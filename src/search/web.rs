use crate::config::Config;
use crate::search::{Action, ResultKind, SearchResult};

pub fn result(q: &str, cfg: &Config) -> SearchResult {
    let engine_name = if cfg.search_engine == crate::config::SearchEngine::BrowserDefault {
        crate::search::browser_engine::engine_name()
            .unwrap_or_else(|| "default browser".into())
    } else if cfg.search_engine == crate::config::SearchEngine::Custom {
        "custom web search".into()
    } else {
        cfg.search_engine.display_name().to_string()
    };
    let url = cfg.web_search_url_for(q);
    let icon = domain_of(&url)
        .map(|d| format!("favicon:{d}"))
        .unwrap_or_else(|| "web-browser-symbolic".into());
    SearchResult {
        kind: ResultKind::Web,
        title: format!("Search \"{q}\" on {engine_name}"),
        subtitle: Some("Open in browser".into()),
        icon: Some(icon),
        action: Action::OpenUrl(url),
        score: 100,
    }
}

/// Extract the host (e.g. "duckduckgo.com") from a URL, including custom
/// search URLs, so the search-engine's own favicon can be shown — even for a
/// user-entered custom search engine.
fn domain_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}
