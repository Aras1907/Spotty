use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::config::Config;
use crate::index::Snapshot;
use crate::search::browse;
use crate::search::inline_file_mode;
use crate::search::merge_pinned;
use crate::search::SearchResult;

static PENDING: OnceLock<Mutex<Option<(u64, String, Vec<SearchResult>)>>> = OnceLock::new();
static GEN: AtomicU64 = AtomicU64::new(0);
static INFLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn pending() -> &'static Mutex<Option<(u64, String, Vec<SearchResult>)>> {
    PENDING.get_or_init(|| Mutex::new(None))
}

fn inflight_set() -> &'static Mutex<HashSet<String>> {
    INFLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Build a key from mode + query so different modes with the same text
/// don't collide and the pending match is exact.
pub fn key(mode: Option<&str>, query: &str) -> String {
    match mode {
        Some(m) => format!("{}\u{1}{}", m, query),
        None => query.to_string(),
    }
}

pub fn job_inflight(k: &str) -> bool {
    inflight_set().lock().unwrap().contains(k)
}

pub fn has_pending(k: &str) -> bool {
    pending().lock().unwrap().as_ref().map_or(false, |(_, q, _)| q == k)
}

pub fn take_pending(k: &str) -> Option<Vec<SearchResult>> {
    let mut g = pending().lock().ok()?;
    if let Some((_, q, _)) = g.as_ref() {
        if q == k {
            return g.take().map(|(_, _, r)| r);
        }
    }
    None
}

/// Spawn a search job for the given key/mode/query. The worker computes
/// results on a background thread; when done it stores them in the pending
/// slot and invokes `refresh_search_window`.
pub fn spawn(
    k: String,
    query: String,
    mode: Option<String>,
    config: Config,
    snap: Arc<RwLock<crate::index::Snapshot>>,
) -> u64 {
    inflight_set().lock().unwrap().insert(k.clone());
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        // Guard: always remove from inflight on exit, even on panic.
        struct InflightGuard(String);
        impl Drop for InflightGuard {
            fn drop(&mut self) {
                inflight_set().lock().unwrap().remove(&self.0);
            }
        }
        let _guard = InflightGuard(k.clone());
        let results = compute(&query, mode.as_deref(), &config, &snap);
        if GEN.load(Ordering::SeqCst) == gen {
            *pending().lock().unwrap() = Some((gen, k, results));
            glib::MainContext::default().invoke(crate::app::refresh_search_window);
        }
    });
    gen
}

/// Compute search results. Mode routing mirrors the old synchronous
/// `search::search` / `search_mode` dispatch.
fn compute(
    query: &str,
    mode: Option<&str>,
    config: &Config,
    snap: &Arc<RwLock<crate::index::Snapshot>>,
) -> Vec<SearchResult> {
    // 1. Active mode → dispatch directly (handles empty query too).
    if let Some(m) = mode {
        return crate::search::search_mode(m, query, config, snap);
    }

    let query = query.trim();
    if query.is_empty() {
        return vec![];
    }

    // 2. Inline "find foo" etc.
    if let Some((mode_word, rest)) = inline_file_mode(query, config) {
        return crate::search::search_mode(&mode_word, &rest, config, snap);
    }

    // 3. Universal search.
    // Path browsing
    if config.enable_root_browsing
        && (query.starts_with('/') || query.starts_with("~/") || query == "~")
    {
        return browse::browse(query);
    }
    let ql = query.to_lowercase();
    let mut r = Vec::with_capacity(32);
    // Trigger suggestions
    let trigger_kws = crate::triggers::keywords();
    let kw_iter = config
        .command_keywords
        .iter()
        .chain(trigger_kws.iter());
    if ql.len() >= 1 && ql.len() <= 8 && ql.chars().all(|c| c.is_alphabetic()) {
        for kw in kw_iter {
            let dn = kw.display_name().to_lowercase();
            let word_match = kw.word.starts_with(&ql);
            let name_match = dn.starts_with(&ql);
            if word_match || name_match {
                let score = if kw.word == ql || dn == ql {
                    100_000
                } else if word_match {
                    50_000
                } else {
                    45_000
                };
                r.push(SearchResult {
                    kind: crate::search::ResultKind::System,
                    title: crate::search::capitalize(&kw.word),
                    subtitle: Some(kw.description.clone()),
                    icon: Some(if kw.icon.is_empty() {
                        "folder-symbolic".into()
                    } else {
                        kw.icon.clone()
                    }),
                    action: crate::search::Action::EnterMode(kw.word.clone()),
                    score,
                });
            }
        }
    }
    if ql == "update" || ql == "upgrade" || ql.starts_with("upd") || ql.starts_with("upg") {
        let mut update_results = crate::search::cmd::update_all_result();
        let score = if ql == "update" || ql == "upgrade" {
            100_000
        } else if ql.starts_with("upd") || ql.starts_with("upg") {
            50_000
        } else {
            30_000
        };
        for r in &mut update_results {
            r.score = r.score.max(score);
        }
        r.extend(update_results);
    }
    r.extend(crate::search::system::search(query, config));
    r.extend(crate::search::settings_panels::search(query));
    if config.enable_calculator {
        if let Some(calc) = crate::search::calculator::evaluate(query) {
            r.push(calc);
        }
    }
    if config.enable_apps {
        let snap_guard = snap.read().unwrap();
        r.extend(crate::search::apps::search(query, &snap_guard.apps));
    }
    if config.enable_new_apps && query.chars().count() >= 3 {
        r.extend(crate::search::cmd::universal_install(query, config.package_manager, 4));
    }
    if config.enable_web {
        r.push(crate::search::web::result(query, config));
    }
    r = merge_pinned(r, &ql, &config.pinned_results);
    r.extend(crate::operations::running_result_rows());
    r.sort_by(|a, b| b.score.cmp(&a.score));
    r.truncate(20);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileEntry;

    fn snap_with(files: Vec<FileEntry>) -> Arc<RwLock<crate::index::Snapshot>> {
        Arc::new(RwLock::new(crate::index::Snapshot { apps: vec![], files }))
    }

    #[test]
    fn key_distinguishes_modes() {
        assert_ne!(key(Some("find"), "x"), key(None, "x"));
        assert_ne!(key(Some("find"), "x"), key(Some("app"), "x"));
        assert_eq!(key(Some("find"), "x"), key(Some("find"), "x"));
    }

    #[test]
    fn find_mode_routes_to_file_search() {
        let dir = std::env::temp_dir().join(format!("spotty_jobs_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("report_q4.txt");
        std::fs::write(&path, "quarter four report").unwrap();

        let entry = FileEntry {
            name: "report_q4.txt".into(),
            name_lower: "report_q4.txt".into(),
            path: path.clone(),
            is_dir: false,
        };
        let snap = snap_with(vec![entry]);
        let cfg = Config::default();

        let results = compute("report_q4", Some("find"), &cfg, &snap);
        assert!(
            results.iter().any(|r| r.title == "report_q4.txt"),
            "find mode should return the file, got: {:?}",
            results.iter().map(|r| r.title.clone()).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_routing_happens_before_empty_query_return() {
        let snap = snap_with(vec![]);
        let cfg = Config::default();
        let results = compute("", Some("emoji"), &cfg, &snap);
        assert!(
            !results.is_empty(),
            "an empty query in a mode must still be routed to that mode"
        );
    }
}
