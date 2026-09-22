// Music playback tracking, queue, and UI integration.
//
// Holds the now-playing queue (so next/previous work) and exposes playback
// controls invoked from the Operations music row. Like operations.rs, the
// current track persists even when the search window is hidden, and is surfaced
// as a rich result row (cover art + transport controls + progress).
//
// Control entry points (toggle/next/previous/auto_advance/play_queue) must run
// on the GTK main thread — they read the shared player handle via app state and
// spawn their own worker threads for any blocking work (YouTube download).

use crate::music::Track;
use crate::search::{Action, ResultKind, SearchResult};
use gtk::glib;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone)]
pub struct MusicOperation {
    pub id: u64,
    pub queue: Vec<Track>,
    pub index: usize,
    pub duration_ms: u64,
    pub elapsed_ms: u64,
    pub state: PlaybackState,
    pub cover_path: Option<PathBuf>,
    pub started_at: Instant,
}

impl MusicOperation {
    pub fn current_track(&self) -> Option<&Track> {
        self.queue.get(self.index)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

fn registry() -> &'static Mutex<Option<MusicOperation>> {
    static R: OnceLock<Mutex<Option<MusicOperation>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(None))
}

/// When `Some(dir)`, the current session has been dismissed from the UI: it is
/// paused and hidden, but still loaded so it can resume exactly where it left
/// off. `dir` is the swipe direction it was dismissed in, so a reverse swipe can
/// restore it.
fn dismissed_dir_state() -> &'static Mutex<Option<f64>> {
    static D: OnceLock<Mutex<Option<f64>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(None))
}

fn next_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn nudge_ui() {
    // Defer (rather than `MainContext::invoke`, which runs synchronously on the
    // main thread) so a UI callback that changes playback can't re-enter a
    // popover/results rebuild while it's still on the stack.
    glib::idle_add_once(|| {
        crate::app::refresh_search_window();
        // Keep GNOME's media controls (MPRIS) in sync with track/state changes.
        crate::mpris::notify();
    });
}

/// Begin playing a queue of tracks starting at `index`. Replaces any current
/// playback. Call on the GTK main thread.
pub fn play_queue(queue: Vec<Track>, index: usize) {
    if queue.is_empty() {
        return;
    }
    let index = index.min(queue.len() - 1);
    let op = MusicOperation {
        id: next_id(),
        queue,
        index,
        duration_ms: 0,
        elapsed_ms: 0,
        state: PlaybackState::Playing,
        cover_path: None,
        started_at: Instant::now(),
    };
    *registry().lock().unwrap() = Some(op);
    *dismissed_dir_state().lock().unwrap() = None;
    play_current();
}

/// (Re)start playback of the track at the current queue index.
fn play_current() {
    let Some(track) = current().and_then(|op| op.current_track().cloned()) else {
        return;
    };

    record_recent_play(&track);

    // Optimistically reflect the new track immediately; duration is known up
    // front for YouTube, filled in by the decoder for local files.
    {
        let mut guard = registry().lock().unwrap();
        if let Some(op) = guard.as_mut() {
            op.duration_ms = track.duration_ms;
            op.elapsed_ms = 0;
            op.state = PlaybackState::Playing;
            op.cover_path = None;
        }
    }
    nudge_ui();

    // Cover art (async): download then refresh the row.
    if !track.cover_url.is_empty() {
        let url = track.cover_url.clone();
        let src = track.source.clone();
        std::thread::spawn(move || {
            if let Some(path) = crate::youtube_music::ensure_cover(&url) {
                if let Some(op) = registry().lock().unwrap().as_mut() {
                    // Only apply if still the same track.
                    if op.current_track().map(|t| t.source == src).unwrap_or(false) {
                        op.cover_path = Some(path);
                    }
                }
                nudge_ui();
            }
        });
    }

    // Resolve audio + start the embedded player on a worker thread.
    let player = crate::app::with_state(|s| s.music_player.clone());
    std::thread::spawn(move || {
        if track.is_youtube {
            match crate::youtube_music::download_audio(&track.video_id) {
                Some(path) => {
                    if let Err(e) = player.play(&path, track.title.clone()) {
                        log::error!("music: playback failed: {e}");
                        glib::MainContext::default().invoke(stop_music);
                    }
                }
                None => {
                    log::warn!("music: download failed for {}", track.video_id);
                    glib::MainContext::default().invoke(stop_music);
                }
            }
        } else if let Err(e) = player.play(&track.local_path, track.title.clone()) {
            log::error!("music: failed to play local file: {e}");
        }
    });
}

/// Toggle play/pause on the current track.
pub fn toggle() {
    let player = crate::app::with_state(|s| s.music_player.clone());
    let mut guard = registry().lock().unwrap();
    let Some(op) = guard.as_mut() else { return };
    match op.state {
        PlaybackState::Playing => {
            op.state = PlaybackState::Paused;
            player.pause();
        }
        PlaybackState::Paused | PlaybackState::Stopped => {
            op.state = PlaybackState::Playing;
            player.resume();
        }
    }
    drop(guard);
    nudge_ui();
}

/// Pause playback (MPRIS Pause). No-op if nothing is playing.
pub fn pause() {
    let player = crate::app::with_state(|s| s.music_player.clone());
    {
        let mut guard = registry().lock().unwrap();
        let Some(op) = guard.as_mut() else { return };
        op.state = PlaybackState::Paused;
    }
    player.pause();
    nudge_ui();
}

/// Resume playback (MPRIS Play). No-op if nothing is loaded.
pub fn resume() {
    let player = crate::app::with_state(|s| s.music_player.clone());
    {
        let mut guard = registry().lock().unwrap();
        let Some(op) = guard.as_mut() else { return };
        op.state = PlaybackState::Playing;
    }
    player.resume();
    nudge_ui();
}

/// Seek forward/back by `delta_ms` (negative = back) on the current track.
pub fn seek(delta_ms: i64) {
    let (target, duration) = {
        let guard = registry().lock().unwrap();
        let Some(op) = guard.as_ref() else { return };
        let dur = op.duration_ms.max(1);
        let cur = op.elapsed_ms as i64;
        ((cur + delta_ms).clamp(0, dur as i64) as u64, dur)
    };
    seek_to(target.min(duration));
}

/// Seek to an absolute position (ms) on the current track.
pub fn seek_to(ms: u64) {
    let player = crate::app::with_state(|s| s.music_player.clone());
    {
        let mut guard = registry().lock().unwrap();
        let Some(op) = guard.as_mut() else { return };
        op.elapsed_ms = ms.min(op.duration_ms.max(ms));
    }
    player.seek(ms);
    nudge_ui();
}

// ── Recently played log (for music-trigger recommendations) ─────────────

fn recent_path() -> std::path::PathBuf {
    dirs::config_dir().unwrap().join("spotty/recent_music.json")
}

fn load_recent() -> Vec<(String, String)> {
    std::fs::read_to_string(recent_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_recent(v: &[(String, String)]) {
    let p = recent_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string(v) {
        let _ = std::fs::write(p, s);
    }
}

/// Persist a played track (source, title) at the front of the log, deduped by
/// source, capped at 30. YouTube and local entries both recorded.
fn record_recent_play(track: &Track) {
    let mut v = load_recent();
    v.retain(|(s, _)| s != &track.source);
    v.insert(0, (track.source.clone(), track.title.clone()));
    v.truncate(30);
    save_recent(&v);
}

/// Recently played tracks, newest first (source, title).
pub fn recent_plays() -> Vec<(String, String)> {
    load_recent()
}

/// Skip to the next track in the queue (no-op at the end).
pub fn next() {
    let advance = {
        let mut guard = registry().lock().unwrap();
        match guard.as_mut() {
            Some(op) if op.index + 1 < op.queue.len() => {
                op.index += 1;
                true
            }
            _ => false,
        }
    };
    if advance {
        play_current();
    }
}

/// Skip to the previous track in the queue (no-op at the start).
pub fn previous() {
    let advance = {
        let mut guard = registry().lock().unwrap();
        match guard.as_mut() {
            Some(op) if op.index > 0 => {
                op.index -= 1;
                true
            }
            _ => false,
        }
    };
    if advance {
        play_current();
    }
}

/// Called when a track finishes on its own: play the next one, or stop.
pub fn auto_advance() {
    let has_next = current()
        .map(|op| op.index + 1 < op.queue.len())
        .unwrap_or(false);
    if has_next {
        next();
    } else {
        stop_music();
    }
}

/// Stop playback and clear the queue entirely (used when a queue finishes or on
/// an explicit MPRIS Stop — not for UI dismissals, which pause + hide instead).
pub fn stop_music() {
    let player = crate::app::with_state(|s| s.music_player.clone());
    player.stop();
    registry().lock().unwrap().take();
    *dismissed_dir_state().lock().unwrap() = None;
    nudge_ui();
}

/// Dismiss the current session from the UI: pause playback (keeping the track
/// loaded and its position) and hide it. `dir` records the swipe direction so a
/// reverse swipe can restore it. No-op if nothing is playing.
pub fn dismiss(dir: f64) {
    let player = crate::app::with_state(|s| s.music_player.clone());
    {
        let mut guard = registry().lock().unwrap();
        let Some(op) = guard.as_mut() else { return };
        op.state = PlaybackState::Paused;
    }
    player.pause();
    *dismissed_dir_state().lock().unwrap() = Some(dir);
    nudge_ui();
}

/// Restore a dismissed session and resume playback from where it left off.
/// Returns true if there was something to restore.
pub fn restore() -> bool {
    if dismissed_dir_state().lock().unwrap().take().is_none() {
        return false;
    }
    let player = crate::app::with_state(|s| s.music_player.clone());
    {
        let mut guard = registry().lock().unwrap();
        let Some(op) = guard.as_mut() else {
            return false;
        };
        op.state = PlaybackState::Playing;
    }
    player.resume();
    nudge_ui();
    true
}

/// Whether the current session is dismissed (paused + hidden from the UI).
pub fn is_dismissed() -> bool {
    dismissed_dir_state().lock().unwrap().is_some()
}

/// Direction the current session was dismissed in, if any (for the undo bar's
/// reverse-swipe-to-restore gesture).
pub fn dismissed_dir() -> Option<f64> {
    *dismissed_dir_state().lock().unwrap()
}

/// Update elapsed time for the current track (called from the audio thread).
pub fn update_elapsed(elapsed_ms: u64) {
    if let Some(op) = registry().lock().unwrap().as_mut() {
        op.elapsed_ms = elapsed_ms;
    }
}

/// Fill in the track duration once the decoder determines it (local files).
pub fn update_duration(duration_ms: u64) {
    if duration_ms == 0 {
        return;
    }
    if let Some(op) = registry().lock().unwrap().as_mut() {
        if op.duration_ms == 0 {
            op.duration_ms = duration_ms;
        }
    }
}

/// Snapshot of the current music operation, if any.
pub fn current() -> Option<MusicOperation> {
    registry().lock().unwrap().clone()
}

/// Build the now-playing result row (rendered specially as a transport bar).
pub fn music_result_row() -> Option<SearchResult> {
    // Dismissed sessions are paused and hidden from the UI.
    if is_dismissed() {
        return None;
    }
    let op = registry().lock().unwrap();
    let op = op.as_ref()?;
    let track = op.current_track()?;

    let progress_frac = if op.duration_ms > 0 {
        (op.elapsed_ms as f64 / op.duration_ms as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let state_str = match op.state {
        PlaybackState::Playing => "playing",
        PlaybackState::Paused => "paused",
        PlaybackState::Stopped => "stopped",
    };
    let artist = if track.artist.is_empty() {
        track.source_label().to_string()
    } else {
        track.artist.clone()
    };
    let subtitle = format!(
        "{} · {} / {} · {}",
        artist,
        format_duration(op.elapsed_ms),
        format_duration(op.duration_ms),
        track.source_label(),
    );

    // Render hints for result_row::build_music_row:
    //   "__music__␟<frac>␟<state>␟<yt>␟<has_prev>␟<has_next>␟<cover_path>"
    let sep = '\u{1f}';
    let action = Action::EnterMode(format!(
        "__music__{s}{frac}{s}{state}{s}{yt}{s}{prev}{s}{next}{s}{cover}",
        s = sep,
        frac = progress_frac,
        state = state_str,
        yt = if track.is_youtube { 1 } else { 0 },
        prev = if op.index > 0 { 1 } else { 0 },
        next = if op.index + 1 < op.queue.len() { 1 } else { 0 },
        cover = op
            .cover_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    ));

    Some(SearchResult {
        kind: ResultKind::System,
        title: track.title.clone(),
        subtitle: Some(subtitle),
        icon: Some("music-row".to_string()),
        action,
        score: 100_000,
    })
}

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Whether music is currently playing.
pub fn is_playing() -> bool {
    registry()
        .lock()
        .unwrap()
        .as_ref()
        .map(|op| op.state == PlaybackState::Playing)
        .unwrap_or(false)
}
