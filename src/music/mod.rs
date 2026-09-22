pub mod player;

pub use player::MusicPlayer;

use crate::search::music::YT_SEP;
use std::path::PathBuf;

/// A playable track, decoded from a `PlayMusic` action's `source` string. Used
/// for the now-playing queue (next/previous) and the preview overview.
#[derive(Clone, Debug)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub is_youtube: bool,
    /// YouTube video id (empty for local files).
    pub video_id: String,
    /// Absolute path (empty for not-yet-downloaded YouTube tracks).
    pub local_path: PathBuf,
    pub duration_ms: u64,
    /// Remote cover-art URL (YouTube) — empty for local files.
    pub cover_url: String,
    /// The original encoded `source` string, used as a stable identity.
    pub source: String,
}

impl Track {
    /// Parse a `PlayMusic { source, title }` pair into a `Track`. YouTube tracks
    /// are `ytmusic␟id␟title␟artist␟dur␟cover`; anything else is a local path.
    pub fn from_action(source: &str, title: &str) -> Self {
        let yt_prefix = format!("ytmusic{YT_SEP}");
        if let Some(rest) = source.strip_prefix(&yt_prefix) {
            let p: Vec<&str> = rest.split(YT_SEP).collect();
            Track {
                title: p
                    .get(1)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| title.to_string()),
                artist: p.get(2).map(|s| s.to_string()).unwrap_or_default(),
                is_youtube: true,
                video_id: p.first().copied().unwrap_or("").to_string(),
                local_path: PathBuf::new(),
                duration_ms: p.get(3).and_then(|s| s.parse().ok()).unwrap_or(0),
                cover_url: p.get(4).map(|s| s.to_string()).unwrap_or_default(),
                source: source.to_string(),
            }
        } else {
            let path = PathBuf::from(source);
            Track {
                title: title.to_string(),
                artist: String::new(),
                is_youtube: false,
                video_id: String::new(),
                local_path: path,
                duration_ms: 0,
                cover_url: String::new(),
                source: source.to_string(),
            }
        }
    }

    /// Short human label for the track's source.
    pub fn source_label(&self) -> &'static str {
        if self.is_youtube {
            "YouTube Music"
        } else {
            "Local file"
        }
    }
}
