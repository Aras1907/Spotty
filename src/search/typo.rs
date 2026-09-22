// Keyboard-layout-aware fuzzy matching.
// Detects typos where the user hit a key adjacent to the intended one
// (e.g. "dicuments" for "documents" because o and i are near on QWERTY).

/// Returns the set of physically-adjacent keys for a given lowercase char
/// on a standard QWERTY layout.
fn neighbors(c: char) -> &'static [char] {
    match c {
        'q' => &['w', 'a', 's'],
        'w' => &['q', 'e', 'a', 's', 'd'],
        'e' => &['w', 'r', 's', 'd', 'f'],
        'r' => &['e', 't', 'd', 'f', 'g'],
        't' => &['r', 'y', 'f', 'g', 'h'],
        'y' => &['t', 'u', 'g', 'h', 'j'],
        'u' => &['y', 'i', 'h', 'j', 'k'],
        'i' => &['u', 'o', 'j', 'k', 'l'],
        'o' => &['i', 'p', 'k', 'l'],
        'p' => &['o', 'l'],
        'a' => &['q', 'w', 's', 'z'],
        's' => &['a', 'w', 'e', 'd', 'z', 'x'],
        'd' => &['s', 'e', 'r', 'f', 'x', 'c'],
        'f' => &['d', 'r', 't', 'g', 'c', 'v'],
        'g' => &['f', 't', 'y', 'h', 'v', 'b'],
        'h' => &['g', 'y', 'u', 'j', 'b', 'n'],
        'j' => &['h', 'u', 'i', 'k', 'n', 'm'],
        'k' => &['j', 'i', 'o', 'l', 'm'],
        'l' => &['k', 'o', 'p'],
        'z' => &['a', 's', 'x'],
        'x' => &['z', 's', 'd', 'c'],
        'c' => &['x', 'd', 'f', 'v'],
        'v' => &['c', 'f', 'g', 'b'],
        'b' => &['v', 'g', 'h', 'n'],
        'n' => &['b', 'h', 'j', 'm'],
        'm' => &['n', 'j', 'k'],
        _ => &[],
    }
}

/// True if two chars are equal OR physically adjacent on QWERTY.
fn near(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    neighbors(a).contains(&b)
}

/// Damerau-Levenshtein-style distance but where a SUBSTITUTION between
/// keyboard-adjacent keys costs only 0.5 instead of 1.0.
/// Returns a similarity score in 0..=1000 (higher = more similar).
/// Returns None if the strings are too dissimilar to be a typo.
pub fn keyboard_similarity(query: &str, target: &str) -> Option<u32> {
    // Cheap early bail BEFORE allocating Vecs or running the DP matrix:
    // a keyboard typo changes characters, not length much, so if the lengths
    // differ wildly there's no point computing an edit distance.
    let qn = query.chars().count();
    let tn = target.chars().count();
    if qn == 0 || tn == 0 {
        return None;
    }
    let len_budget = (qn as f32 * 0.4).ceil().max(1.0) as usize;
    if tn.abs_diff(qn) > len_budget {
        return None;
    }

    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = target.to_lowercase().chars().collect();
    if q.is_empty() || t.is_empty() {
        return None;
    }

    // For substring-style matching against longer targets, slide a window.
    // We want "dicuments" to match "documents", and also "dicu" to match
    // the start of "documents".
    let qlen = q.len();
    let tlen = t.len();

    // Distance must stay small relative to query length to count as a typo.
    let max_allowed = (qlen as f32 * 0.4).ceil().max(1.0) as f32;

    // Cost matrix using f32 (adjacency substitution = 0.5)
    let best = weighted_distance(&q, &t);
    // Also try prefix match: compare query against the same-length prefix of target
    let prefix_best = if tlen >= qlen {
        weighted_distance(&q, &t[..qlen])
    } else {
        f32::MAX
    };

    let dist = best.min(prefix_best);
    if dist > max_allowed {
        return None;
    }

    // Convert distance to a score: lower distance = higher score
    // Scale: 0 distance → 900, max_allowed distance → ~300
    let score = (900.0 - (dist / max_allowed.max(0.001)) * 600.0).max(0.0);
    Some(score as u32)
}

/// Weighted edit distance where adjacent-key substitutions cost 0.5.
fn weighted_distance(a: &[char], b: &[char]) -> f32 {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0f32; m + 1]; n + 1];
    for i in 0..=n {
        dp[i][0] = i as f32;
    }
    for j in 0..=m {
        dp[0][j] = j as f32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub_cost = if a[i - 1] == b[j - 1] {
                0.0
            } else if near(a[i - 1], b[j - 1]) {
                0.5 // keyboard-adjacent typo
            } else {
                1.0
            };
            let mut best = dp[i - 1][j - 1] + sub_cost;
            best = best.min(dp[i - 1][j] + 1.0); // deletion
            best = best.min(dp[i][j - 1] + 1.0); // insertion
                                                 // Transposition (swapped adjacent chars)
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(dp[i - 2][j - 2] + 0.5);
            }
            dp[i][j] = best;
        }
    }
    dp[n][m]
}
