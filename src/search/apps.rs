use crate::index::AppEntry;
use crate::search::{Action, ResultKind, SearchResult};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32String};

pub fn search(query: &str, apps: &[AppEntry]) -> Vec<SearchResult> {
    if apps.is_empty() || query.is_empty() {
        return vec![];
    }
    let ql = query.to_lowercase();
    let mut matcher = Matcher::default();
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut results = Vec::new();

    for app in apps {
        // Use pre-computed lowercase name — avoids a String allocation per app
        // per keystroke (the hottest path in app search).
        let nl = app.name_lower.as_str();
        let mut score: i32 = 0;

        // Tier 1: exact name match
        if nl == ql {
            score = 10000;
        }
        // Tier 2: name starts with query
        else if nl.starts_with(&ql) {
            score = 5000;
        }
        // Tier 3: name contains query as substring
        else if nl.contains(&ql) {
            // Shorter names rank higher (less noise)
            score = 2500 - (app.name.len() as i32).min(500);
        }
        // Tier 4: keyword match (from .desktop file Keywords field)
        else if app.keywords.iter().any(|k| {
            k.eq_ignore_ascii_case(&ql) || k.to_ascii_lowercase().starts_with(&ql)
        }) {
            score = 1500;
        }
        // Tier 5: stronger fuzzy on the NAME only (no description/comment).
        else if ql.len() >= 2 {
            let hay = Utf32String::from(app.name.as_str());
            if let Some(fuzzy) = pattern.score(hay.slice(..), &mut matcher) {
                // Lower floor to tolerate more typos and partial matches.
                if fuzzy >= (ql.len() as u32) * 25 {
                    score = (fuzzy as i32).min(1800);
                }
            }
            // Tier 6: keyboard-layout typo detection (e.g. "frefox" → "Firefox")
            if score == 0 {
                if let Some(sim) = crate::search::typo::keyboard_similarity(&ql, nl) {
                    score = (sim as i32).min(1400);
                } 
            }
        }

        if score == 0 {
            continue;
        }
        results.push(mk(app, score));
    }

    // Apply frequency bonus from history
    for r in &mut results {
        r.score += crate::history::frequency_bonus_for(query, &r.title);
    }

    results.sort_by(|a, b| b.score.cmp(&a.score));
    results
}

fn mk(app: &AppEntry, score: i32) -> SearchResult {
    SearchResult {
        kind: ResultKind::App,
        title: app.name.clone(),
        // Keep subtitle for display only - NOT used in matching
        subtitle: app.comment.clone().or_else(|| app.generic_name.clone()),
        icon: app.icon.clone(),
        action: Action::LaunchDesktopFile(app.desktop_file.clone()),
        score,
    }
}
