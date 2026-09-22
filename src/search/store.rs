// Spotty Store: Extension marketplace and integrations management.
//
// Shows available extensions (YouTube Music, custom services, etc.) with:
// - Logo/icon
// - Short description
// - Authentication status
// - Enable/disable toggle

use super::{Action, ResultKind, SearchResult};

#[derive(Debug, Clone)]
pub struct Extension {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub icon: &'static str,
    pub category: &'static str,
}

pub static EXTENSIONS: &[Extension] = &[
    Extension {
        id: "youtube-music",
        name: "YouTube Music",
        description: "Stream millions of songs from YouTube Music with browser-based OAuth",
        icon: "media-playback-start-symbolic",
        category: "Music",
    },
    Extension {
        id: "local-music",
        name: "Local Music Library",
        description: "Search and play music from ~/Music and custom library paths",
        icon: "folder-music-symbolic",
        category: "Music",
    },
];

/// Search for available extensions (no filtering, shows all).
pub fn search(_query: &str) -> Vec<SearchResult> {
    EXTENSIONS
        .iter()
        .enumerate()
        .map(|(i, ext)| SearchResult {
            kind: ResultKind::System,
            title: ext.name.to_string(),
            subtitle: Some(ext.description.to_string()),
            icon: Some(ext.icon.to_string()),
            action: Action::EnterMode(format!("extension:{}", ext.id)),
            score: 90_000 - i as i32,
        })
        .collect()
}

/// Get details about a specific extension.
pub fn get_extension(id: &str) -> Option<&'static Extension> {
    EXTENSIONS.iter().find(|ext| ext.id == id)
}
