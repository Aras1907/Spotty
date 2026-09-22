//! Live dictionary lookups for the "dict" trigger (id `dictionary`).
//!
//! Two free, keyless APIs, both fetched with the flatpak-spawn-aware curl
//! helper (`triggers::fetch_text`) on a spawned thread:
//!
//! - Word autocompletion: `api.datamuse.com/sug?s=<prefix>` → JSON list of
//!   `{"word": ...}`. Drives the entry's ghost-completion while typing.
//! - Definitions: `api.dictionaryapi.dev/api/v2/entries/en/<word>` → JSON
//!   entry with meanings. 404s (non-words) are cached as a miss so they are
//!   not refetched on every keystroke.
//!
//! Results are cached per session; fetches are debounce-gated (one network
//! round per ~250ms) and re-run the search when they land via
//! `app::refresh_search_window`.
use crate::search::{Action, ResultKind, SearchResult};
use gtk::glib;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};
use std::time::Instant;

/// prefix → datamuse suggestions.
static SUGGEST_CACHE: LazyLock<RwLock<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
/// word → formatted definition; `None` = tried and not found (404).
static DEF_CACHE: LazyLock<RwLock<HashMap<String, Option<String>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

thread_local! {
    /// Last network fetch timestamp (main thread only) — keystroke debounce.
    static LAST_FETCH: RefCell<Instant> = RefCell::new(Instant::now() - std::time::Duration::from_millis(500));
    static PENDING_WORD: RefCell<Option<String>> = const { RefCell::new(None) };
    static PENDING_SUGGEST: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn cache_get<T: Clone>(c: &RwLock<HashMap<String, T>>, k: &str) -> Option<T> {
    c.read().ok()?.get(k).cloned()
}
fn cache_put<T>(c: &RwLock<HashMap<String, T>>, k: String, v: T) {
    if let Ok(mut m) = c.write() {
        m.insert(k, v);
    }
}

/// Debounce gate: allow a fetch at most every 250ms.
fn allow_fetch() -> bool {
    let mut last = LAST_FETCH.with(|l| *l.borrow());
    let now = Instant::now();
    if now.duration_since(last).as_millis() < 250 {
        return false;
    }
    last = now;
    LAST_FETCH.with(|l| *l.borrow_mut() = last);
    true
}

/// Search results for the dict trigger (`rest` = the word being looked up).
pub fn results(rest: &str) -> Vec<SearchResult> {
    let icon = Some("accessories-dictionary-symbolic".into());
    if rest.trim().is_empty() {
        return vec![SearchResult {
            kind: ResultKind::System,
            title: "Type a word to look up".into(),
            subtitle: Some(
                "Start typing — the word autocompletes and its definition appears below.".into(),
            ),
            icon,
            action: Action::EnterMode("dict".into()),
            score: 1000,
        }];
    }
    let word = rest.trim().to_lowercase();
    let mut out = Vec::new();

    // Expanded definition row once the full word has one.
    if let Some(def) = cache_get(&DEF_CACHE, &word) {
        if let Some(def) = def {
            out.push(SearchResult {
                kind: ResultKind::Web,
                title: crate::search::capitalize(&word),
                subtitle: Some(def),
                icon: Some("dict-def".into()),
                action: Action::OpenUrl(format!(
                    "https://www.dictionary.com/browse/{word}"
                )),
                score: 100_000,
            });
        }
    } else if word.chars().count() >= 3 {
        // Not looked up yet: fire a debounced fetch if one isn't in flight.
        let in_flight = PENDING_WORD.with(|p| p.borrow().as_deref() == Some(word.as_str()));
        if !in_flight && allow_fetch() {
            PENDING_WORD.with(|p| *p.borrow_mut() = Some(word.clone()));
            let w2 = word.clone();
            std::thread::spawn(move || {
                let body =
                    crate::triggers::fetch_text(&format!(
                        "https://api.dictionaryapi.dev/api/v2/entries/en/{}",
                        urlencoding::encode(&w2)
                    ));
                glib::MainContext::default().invoke(move || {
                    PENDING_WORD.with(|p| *p.borrow_mut() = None);
                    let def = body.ok().and_then(|t| parse_definition(&t));
                    cache_put(&DEF_CACHE, w2.clone(), def);
                    crate::app::refresh_search_window();
                });
            });
        }
        out.push(SearchResult {
            kind: ResultKind::System,
            title: format!("Looking up \"{word}\"…"),
            subtitle: Some("Fetching the definition".into()),
            icon: icon.clone(),
            action: Action::EnterMode("dict".into()),
            score: 85_000,
        });
    }

    // Word autocompletion from datamuse: the suggestion row's title drives
    // the entry's ghost-completion (candidate_for → title).
    if word.chars().count() >= 2 {
        let sugg = cache_get(&SUGGEST_CACHE, &word);
        match sugg {
            Some(suggestions) => {
                if let Some(s) = suggestions.first() {
                    if !s.eq_ignore_ascii_case(&word) {
                        out.push(SearchResult {
                            kind: ResultKind::Web,
                            title: s.clone(),
                            subtitle: Some(format!(
                                "{} — complete the word with Tab",
                                crate::search::capitalize(s)
                            )),
                            icon: icon.clone(),
                            action: Action::OpenUrl(format!(
                                "https://www.dictionary.com/browse/{s}"
                            )),
                            score: 90_000,
                        });
                    }
                }
            }
            None => {
                let in_flight =
                    PENDING_SUGGEST.with(|p| p.borrow().as_deref() == Some(word.as_str()));
                if !in_flight && allow_fetch() {
                    PENDING_SUGGEST.with(|p| *p.borrow_mut() = Some(word.clone()));
                    let w2 = word.clone();
                    std::thread::spawn(move || {
                        let body = crate::triggers::fetch_text(&format!(
                            "https://api.datamuse.com/sug?s={}",
                            urlencoding::encode(&w2)
                        ));
                        glib::MainContext::default().invoke(move || {
                            PENDING_SUGGEST.with(|p| *p.borrow_mut() = None);
                            let list = body.ok().and_then(|t| parse_suggestions(&t));
                            cache_put(&SUGGEST_CACHE, w2.clone(), list.unwrap_or_default());
                            crate::app::refresh_search_window();
                        });
                    });
                }
            }
        }
    }

    // Plain web-search row as the base entry (always available).
    out.push(SearchResult {
        kind: ResultKind::Web,
        title: format!("Dictionary: {word}"),
        subtitle: Some(format!(
            "Open https://www.dictionary.com/browse/{word}"
        )),
        icon,
        action: Action::OpenUrl(format!("https://www.dictionary.com/browse/{word}")),
        score: 80_000,
    });
    out
}

/// Parse a dictionaryapi.dev response into a multi-line definition:
/// `partOfSpeech — definition` per sense, examples indented below.
fn parse_definition(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let entry = v.as_array()?.first()?;
    let mut lines: Vec<String> = Vec::new();
    for m in entry.get("meanings")?.as_array()? {
        let pos = m.get("partOfSpeech").and_then(|p| p.as_str()).unwrap_or("");
        for d in m.get("definitions")?.as_array()?.iter().take(3) {
            let text = d.get("definition").and_then(|t| t.as_str()).unwrap_or("");
            if text.is_empty() {
                continue;
            }
            let line = if pos.is_empty() {
                format!("• {text}")
            } else {
                format!("{pos} — {text}")
            };
            if !lines.contains(&line) {
                lines.push(line);
            }
            if let Some(ex) = d.get("example").and_then(|e| e.as_str()) {
                if !ex.is_empty() {
                    lines.push(format!("    “{ex}”"));
                }
            }
        }
        if lines.len() >= 8 {
            break;
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Parse a datamuse `sug` response into the suggested words, best first.
fn parse_suggestions(raw: &str) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let mut out: Vec<String> = v
        .as_array()?
        .iter()
        .filter_map(|e| e.get("word").and_then(|w| w.as_str()).map(str::to_string))
        .collect();
    out.truncate(5);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DEF: &str = r#"[
      {
        "word": "serendipity",
        "phonetic": "/ˌsɛ.ɹən.ˈdɪ.pɪ.ti/",
        "meanings": [
          {
            "partOfSpeech": "noun",
            "definitions": [
              {
                "definition": "A combination of events which have come together by chance to make a surprisingly good outcome.",
                "example": "His serendipity made the discovery possible."
              }
            ]
          }
        ]
      }
    ]"#;

    #[test]
    fn formats_definition() {
        let def = parse_definition(SAMPLE_DEF).expect("definitions parse");
        assert!(def.contains("noun — A combination of events"));
        assert!(def.contains("“His serendipity made the discovery possible.”"));
    }

    #[test]
    fn rejects_non_words() {
        assert!(parse_definition("404: Not Found").is_none());
        assert!(parse_definition("[]").is_none());
    }

    #[test]
    fn parses_suggestions() {
        let s = parse_suggestions(r#"[{"word":"serenity","score":224065},{"word":"serene","score":206073}]"#)
            .expect("suggestions parse");
        assert_eq!(s, vec!["serenity", "serene"]);
    }
}
