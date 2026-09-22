// Real audio playback backed by rodio.
//
// rodio/cpal output streams are not `Send`, so the actual audio device lives on
// a dedicated thread that owns it for its whole lifetime. `MusicPlayer` (held in
// `AppState` and shared across threads via `Arc`) talks to that thread over an
// mpsc channel. Playback position and state are mirrored into atomics/mutexes
// so any thread can read "now playing" info cheaply.

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::Source;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

enum Cmd {
    Play(PathBuf),
    Pause,
    Resume,
    Stop,
    SetVolume(f32),
    Seek(u64),
}

pub struct MusicPlayer {
    tx: Mutex<Option<Sender<Cmd>>>,
    state: Arc<Mutex<PlaybackState>>,
    current_track: Arc<Mutex<Option<String>>>,
    elapsed_ms: Arc<AtomicU64>,
    duration_ms: Arc<AtomicU64>,
}

impl MusicPlayer {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(MusicPlayer {
            tx: Mutex::new(None),
            state: Arc::new(Mutex::new(PlaybackState::Stopped)),
            current_track: Arc::new(Mutex::new(None)),
            elapsed_ms: Arc::new(AtomicU64::new(0)),
            duration_ms: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Lazily start the audio thread and return a command sender.
    fn ensure_thread(&self) -> Result<Sender<Cmd>, Box<dyn std::error::Error>> {
        let mut guard = self.tx.lock().unwrap();
        if let Some(tx) = guard.as_ref() {
            return Ok(tx.clone());
        }
        let (tx, rx) = mpsc::channel::<Cmd>();
        let state = self.state.clone();
        let elapsed = self.elapsed_ms.clone();
        let duration = self.duration_ms.clone();
        std::thread::Builder::new()
            .name("spotty-audio".into())
            .spawn(move || audio_loop(rx, state, elapsed, duration))?;
        *guard = Some(tx.clone());
        Ok(tx)
    }

    /// Begin playing a local audio file. Returns an error only if the file
    /// cannot be opened or the audio thread cannot start; decode errors are
    /// surfaced asynchronously via logs and a Stopped state.
    pub fn play(&self, path: &PathBuf, title: String) -> Result<(), Box<dyn std::error::Error>> {
        // Verify readability up front so callers get immediate feedback.
        File::open(path)?;
        let tx = self.ensure_thread()?;
        *self.current_track.lock().unwrap() = Some(title);
        *self.state.lock().unwrap() = PlaybackState::Playing;
        self.elapsed_ms.store(0, Ordering::SeqCst);
        self.duration_ms.store(0, Ordering::SeqCst);
        tx.send(Cmd::Play(path.clone())).map_err(|e| {
            Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            ))
        })?;
        Ok(())
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
        }
    }

    pub fn pause(&self) {
        *self.state.lock().unwrap() = PlaybackState::Paused;
        self.send(Cmd::Pause);
    }

    pub fn resume(&self) {
        *self.state.lock().unwrap() = PlaybackState::Playing;
        self.send(Cmd::Resume);
    }

    pub fn stop(&self) {
        *self.state.lock().unwrap() = PlaybackState::Stopped;
        *self.current_track.lock().unwrap() = None;
        self.elapsed_ms.store(0, Ordering::SeqCst);
        self.send(Cmd::Stop);
    }

    pub fn set_volume(&self, volume: f32) {
        self.send(Cmd::SetVolume(volume));
    }

    /// Seek to an absolute position (milliseconds). Updates the shared elapsed
    /// mirror immediately; the audio thread performs the actual seek.
    pub fn seek(&self, ms: u64) {
        self.elapsed_ms.store(ms, Ordering::SeqCst);
        self.send(Cmd::Seek(ms));
    }

    pub fn state(&self) -> PlaybackState {
        *self.state.lock().unwrap()
    }

    pub fn current_track(&self) -> Option<String> {
        self.current_track.lock().unwrap().clone()
    }

    pub fn is_playing(&self) -> bool {
        *self.state.lock().unwrap() == PlaybackState::Playing
    }

    pub fn set_elapsed(&self, ms: u64) {
        self.elapsed_ms.store(ms, Ordering::SeqCst);
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms.load(Ordering::SeqCst)
    }

    /// Best-effort total duration of the current track (0 if unknown).
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms.load(Ordering::SeqCst)
    }
}

impl Default for MusicPlayer {
    fn default() -> Self {
        Self::new().expect("Failed to initialize music player")
    }
}

/// Owns the rodio device sink and one `Player`, servicing commands and mirroring
/// playback position into the shared atomics roughly every 200 ms.
fn audio_loop(
    rx: mpsc::Receiver<Cmd>,
    state: Arc<Mutex<PlaybackState>>,
    elapsed: Arc<AtomicU64>,
    duration: Arc<AtomicU64>,
) {
    let sink = match rodio::DeviceSinkBuilder::open_default_sink() {
        Ok(s) => s,
        Err(e) => {
            log::error!("music: cannot open audio output: {e}");
            *state.lock().unwrap() = PlaybackState::Stopped;
            return;
        }
    };
    let mut player = rodio::Player::connect_new(sink.mixer());
    // Cache extracted audio (MP4 → M4A) by source path so repeat plays don't
    // re-run ffmpeg.
    let extracted: std::collections::HashMap<PathBuf, PathBuf> = std::collections::HashMap::new();
    let extracted = std::sync::Mutex::new(extracted);

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Cmd::Play(path)) => {
                // Fresh player so the previous track is fully discarded.
                player.stop();
                player = rodio::Player::connect_new(sink.mixer());
                // Video containers (e.g. MP4) can't be decoded directly — rodio's
                // symphonia picks the video track first. Extract the audio stream
                // with the bundled ffmpeg and decode the M4A instead.
                let play_path = match resolve_audio_path(&path, &extracted) {
                    Ok(p) => p,
                    Err(e) => {
                        log::error!("music: failed to prepare {:?}: {e}", path);
                        *state.lock().unwrap() = PlaybackState::Stopped;
                        continue;
                    }
                };
                match File::open(&play_path)
                    .map_err(|e| e.to_string())
                    .and_then(|f| rodio::Decoder::try_from(f).map_err(|e| e.to_string()))
                {
                    Ok(source) => {
                        if let Some(d) = source.total_duration() {
                            duration.store(d.as_millis() as u64, Ordering::SeqCst);
                        }
                        player.append(source);
                        player.play();
                        *state.lock().unwrap() = PlaybackState::Playing;
                    }
                    Err(e) => {
                        log::error!("music: failed to decode {:?}: {e}", play_path);
                        *state.lock().unwrap() = PlaybackState::Stopped;
                    }
                }
            }
            Ok(Cmd::Pause) => player.pause(),
            Ok(Cmd::Resume) => player.play(),
            Ok(Cmd::Stop) => {
                player.stop();
                player = rodio::Player::connect_new(sink.mixer());
                elapsed.store(0, Ordering::SeqCst);
                duration.store(0, Ordering::SeqCst);
            }
            Ok(Cmd::SetVolume(v)) => player.set_volume(v),
            Ok(Cmd::Seek(ms)) => {
                if let Err(e) = player.try_seek(Duration::from_millis(ms)) {
                    log::warn!("music: seek to {ms}ms failed: {e}");
                } else {
                    elapsed.store(ms, Ordering::SeqCst);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let playing = *state.lock().unwrap() == PlaybackState::Playing;
                if playing {
                    let pos = player.get_pos().as_millis() as u64;
                    elapsed.store(pos, Ordering::SeqCst);
                    crate::music_operations::update_elapsed(pos);
                    crate::music_operations::update_duration(duration.load(Ordering::SeqCst));
                    if player.empty() {
                        // Track finished on its own: advance the queue.
                        *state.lock().unwrap() = PlaybackState::Stopped;
                        gtk::glib::MainContext::default()
                            .invoke(crate::music_operations::auto_advance);
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Return the file to decode for `path`: the path itself for plain audio, or a
/// cached ffmpeg-extracted M4A for video containers like MP4.
fn resolve_audio_path(
    path: &PathBuf,
    cache: &std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>,
) -> Result<PathBuf, String> {
    let is_video = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| matches!(e.to_lowercase().as_str(), "mp4" | "m4v" | "mov" | "mkv" | "webm"))
        .unwrap_or(false);
    if !is_video {
        return Ok(path.clone());
    }
    if let Some(p) = cache.lock().unwrap().get(path) {
        if p.exists() {
            return Ok(p.clone());
        }
    }
    let ffmpeg = crate::preview::resolve_tool("ffmpeg")
        .ok_or_else(|| "ffmpeg not found".to_string())?;
    let dir = std::env::temp_dir().join("spotty-music");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(format!(
        "{}.m4a",
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("track")
    ));
    let status = std::process::Command::new(&ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(path)
        .args(["-vn", "-acodec", "copy"])
        .arg(&out)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() || !out.exists() {
        return Err("ffmpeg audio extraction failed".into());
    }
    cache.lock().unwrap().insert(path.clone(), out.clone());
    Ok(out)
}
