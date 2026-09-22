// Music search: search local music library and YouTube Music.
//
// Supports:
// - Local music files (MP3, FLAC, OGG, WAV) from ~/Music and custom paths
// - YouTube Music integration (works without login, enhanced with optional OAuth)
//
// Playing music shows progress in the Operations panel with skip/pause/previous controls.

use super::{Action, ResultKind, SearchResult};
use std::path::{Path, PathBuf};

/// Search local music library for tracks matching the query.
pub fn search_local(query: &str, paths: &[PathBuf]) -> Vec<SearchResult> {
    let ql = query.to_lowercase();
    let mut results = Vec::new();

    for path in paths {
        if !path.exists() {
            log::debug!("music: library path does not exist: {:?}", path);
            continue;
        }

        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                        if matches!(
                            ext.to_lowercase().as_str(),
                            "mp3" | "flac" | "ogg" | "m4a" | "wav" | "mp4"
                        ) {
                            if let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) {
                                let file_lower = file_name.to_lowercase();
                                if file_lower.contains(&ql) {
                                    results.push(SearchResult {
                                        kind: ResultKind::System,
                                        title: file_name.to_string(),
                                        subtitle: Some(format!(
                                            "Local Music • {}",
                                            ext.to_uppercase()
                                        )),
                                        icon: Some("media-playback-start-symbolic".into()),
                                        action: Action::PlayMusic {
                                            // Plain absolute path; the handler treats any
                                            // non-"ytmusic" source as a local file.
                                            source: path.to_string_lossy().to_string(),
                                            title: file_name.to_string(),
                                        },
                                        score: 85_000 - results.len() as i32,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    results.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.title.cmp(&b.title))
    });

    results
}

/// Default recommendations for the music trigger with an empty query:
/// recently played tracks first (newest first, YouTube included when
/// `include_youtube`), then the rest of the local library alphabetically.
pub fn recommendations(paths: &[PathBuf], include_youtube: bool) -> Vec<SearchResult> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Recent plays, newest first.
    for (source, title) in crate::music_operations::recent_plays() {
        if !include_youtube && source.starts_with("ytmusic\u{1f}") {
            continue;
        }
        if !seen.insert(source.clone()) {
            continue;
        }
        let is_yt = source.starts_with("ytmusic\u{1f}");
        out.push(SearchResult {
            kind: ResultKind::System,
            title: title.clone(),
            subtitle: Some(if is_yt {
                "Recently played · YouTube Music".into()
            } else {
                "Recently played · Local".into()
            }),
            icon: Some("media-playback-start-symbolic".into()),
            action: Action::PlayMusic {
                source,
                title,
            },
            score: 100_000 - out.len() as i32,
        });
    }

    // Full local library, alphabetically, minus anything already listed.
    for r in search_local("", paths) {
        if let Action::PlayMusic { source, .. } = &r.action {
            if seen.insert(source.clone()) {
                out.push(r);
            }
        }
    }

    out.truncate(20);
    out
}

/// Field separator used to pack a YouTube Music track into the `PlayMusic`
/// action `source` string (parsed back via [`crate::music::Track`]).
pub const YT_SEP: char = '\u{1f}';

/// Encode a YouTube Music track into a `PlayMusic` source string:
/// `ytmusic␟<video_id>␟<title>␟<artist>␟<duration_ms>␟<cover_url>`.
pub fn encode_yt_source(
    video_id: &str,
    title: &str,
    artist: &str,
    duration_ms: u64,
    cover_url: &str,
) -> String {
    format!(
        "ytmusic{s}{id}{s}{title}{s}{artist}{s}{dur}{s}{cover}",
        s = YT_SEP,
        id = video_id,
        title = title,
        artist = artist,
        dur = duration_ms,
        cover = cover_url,
    )
}

/// Search YouTube Music for real tracks. No login required — results come from
/// the public InnerTube API. The first keystrokes return a "Searching…" row
/// while the background fetch runs; the window refreshes itself when results
/// land. The optional `auth_token` is currently unused (public access only).
pub fn search_youtube_music(query: &str, _auth_token: Option<&str>) -> Vec<SearchResult> {
    if query.trim().is_empty() {
        return vec![];
    }

    match crate::youtube_music::lookup(query) {
        crate::youtube_music::Status::Ready(tracks) if !tracks.is_empty() => {
            let mut score = 80_000;
            tracks
                .into_iter()
                .map(|t| {
                    let subtitle = if t.duration_ms > 0 {
                        format!("{} • {}", t.artist, format_ms(t.duration_ms))
                    } else {
                        t.artist.clone()
                    };
                    let res = SearchResult {
                        kind: ResultKind::System,
                        title: t.title.clone(),
                        subtitle: Some(format!("{} · YouTube Music", subtitle)),
                        icon: Some("media-playback-start-symbolic".into()),
                        action: Action::PlayMusic {
                            source: encode_yt_source(
                                &t.video_id,
                                &t.title,
                                &t.artist,
                                t.duration_ms,
                                &t.cover_url,
                            ),
                            title: t.title,
                        },
                        score,
                    };
                    score -= 1;
                    res
                })
                .collect()
        }
        crate::youtube_music::Status::Loading => vec![SearchResult {
            kind: ResultKind::System,
            title: format!("Searching '{}' on YouTube Music…", query.trim()),
            subtitle: Some("Fetching results…".into()),
            icon: Some("content-loading-symbolic".into()),
            // No-op: pressing Enter while still loading does nothing so the
            // user can't accidentally open the browser.
            action: Action::EnterMode("music".to_string()),
            score: 70_000,
        }],
        // Ready but empty: genuine no-results.
        crate::youtube_music::Status::Ready(_) => vec![SearchResult {
            kind: ResultKind::System,
            title: format!("No YouTube Music results for '{}'", query.trim()),
            subtitle: Some("Try a different search term".into()),
            icon: Some("system-search-symbolic".into()),
            action: Action::EnterMode("music".to_string()),
            score: 70_000,
        }],
    }
}

fn format_ms(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Check if a given path is a supported audio format.
pub fn is_supported_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_lowercase().as_str(),
                "mp3" | "flac" | "ogg" | "m4a" | "wav" | "mp4"
            )
        })
        .unwrap_or(false)
}
