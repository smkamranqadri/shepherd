//! Fuzzy matching without gpui: a query's letters in a text, in order and ignoring case, scored so a prefix beats a
//! word start beats a substring beats scattered letters. The command panel, the sidebar's search and Quick Open
//! rank with it.

/// How well `query` matches `text`, ignoring case, and which chars of `text` it matched; None if it does not:
/// `text` starting with it ranks first, then it starting a word, then anywhere, then its letters in order (more
/// of them on word starts, fewer gaps, higher).
pub fn matched(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query.trim().to_lowercase().chars().collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let word_start = |i: usize| i == 0 || !t[i - 1].is_alphanumeric();
    let hits: Vec<usize> = (0..t.len()).filter(|&i| t[i..].starts_with(&q)).collect();
    let run = |at: usize| (at..at + q.len()).collect();
    if hits.first() == Some(&0) {
        return Some((3000, run(0)));
    }
    if let Some(&at) = hits.iter().find(|&&i| word_start(i)) {
        return Some((2000, run(at)));
    }
    if let Some(&at) = hits.first() {
        return Some((1000, run(at)));
    }
    // The best placement of the letters: +10 for each on a word start, -1 for each char skipped before it.
    // best[k][i] scores q[..=k] with q[k] at t[i]; a running max over the previous row keeps it O(q·t).
    let q: Vec<char> = q.into_iter().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return None;
    }
    let bonus = |i: usize| if word_start(i) { 10 } else { 0 };
    let mut best: Vec<Vec<Option<(i32, usize)>>> = Vec::with_capacity(q.len());
    for (k, &c) in q.iter().enumerate() {
        let mut row = vec![None; t.len()];
        // the best (score + j, j) among the previous letter's places before i
        let mut prev: Option<(i32, usize)> = None;
        for i in 0..t.len() {
            if t[i] == c {
                row[i] = match k {
                    0 => Some((bonus(i) - i as i32, 0)),
                    _ => prev.map(|(s, j)| (s + bonus(i) - i as i32 + 1, j)),
                };
            }
            if k > 0
                && let Some((s, _)) = best[k - 1][i]
                && prev.is_none_or(|(p, _)| s + i as i32 > p)
            {
                prev = Some((s + i as i32, i));
            }
        }
        best.push(row);
    }
    let (mut i, score) = best[q.len() - 1].iter().enumerate().filter_map(|(i, c)| c.map(|(s, _)| (i, s))).max_by_key(|&(i, s)| (s, std::cmp::Reverse(i)))?;
    let mut at = vec![i];
    for k in (1..q.len()).rev() {
        i = best[k][i]?.1;
        at.push(i);
    }
    at.reverse();
    Some((score.min(999), at))
}

pub fn score(query: &str, text: &str) -> Option<i32> {
    matched(query, text).map(|(score, _)| score)
}

/// Reused buffers of `score_ascii`.
#[derive(Default)]
pub struct Scratch {
    lower: Vec<u8>,
    last: Vec<i32>,
    row: Vec<i32>,
    /// Each letter's first and last place in the text that a whole match can use.
    window: Vec<(usize, usize)>,
}

/// `score` of a lowercase ASCII `query` without whitespace in an ASCII `text`, the same number without allocating
/// past `scratch`: Quick Open scores 200,000 paths a keystroke. The table only runs over the places each letter can
/// take in a whole match (after the earliest the letters before it allow, before the latest the ones after it do).
pub fn score_ascii(query: &[u8], text: &[u8], scratch: &mut Scratch) -> Option<i32> {
    const NONE: i32 = i32::MIN;
    let (m, n) = (query.len(), text.len());
    if m == 0 {
        return Some(0);
    }
    if m > n {
        return None;
    }
    let Scratch { lower, last, row, window } = scratch;
    lower.clear();
    lower.extend(text.iter().map(u8::to_ascii_lowercase));
    let t = &lower[..];
    let word_start = |i: usize| i == 0 || !t[i - 1].is_ascii_alphanumeric();
    let mut anywhere = false;
    for i in 0..=n - m {
        if t[i] == query[0] && &t[i..i + m] == query {
            if i == 0 {
                return Some(3000);
            }
            if word_start(i) {
                return Some(2000);
            }
            anywhere = true;
        }
    }
    if anywhere {
        return Some(1000);
    }
    // the windows: greedy from the front for each letter's first place, from the back for its last
    window.clear();
    let mut at = 0;
    for &c in query {
        let found = at + t[at..].iter().position(|&b| b == c)?;
        window.push((found, 0));
        at = found + 1;
    }
    let mut end = n;
    for (k, &c) in query.iter().enumerate().rev() {
        let found = t[..end].iter().rposition(|&b| b == c)?;
        window[k].1 = found;
        end = found;
    }
    // `matched`'s table a row at a time: `last[i]` scores the letters so far with the latest at t[i]
    let bonus = |i: usize| if word_start(i) { 10 } else { 0 };
    // only ever read inside the windows each row writes first, so never cleared
    if last.len() < n {
        last.resize(n, NONE);
        row.resize(n, NONE);
    }
    let (lo, hi) = window[0];
    for i in lo..=hi {
        last[i] = if t[i] == query[0] { bonus(i) - i as i32 } else { NONE };
    }
    for k in 1..m {
        let ((plo, phi), (lo, hi), c) = (window[k - 1], window[k], query[k]);
        // the best (score + j) among the previous letter's places before i
        let (mut prev, mut j) = (NONE, plo);
        for i in lo..=hi {
            while j < i && j <= phi {
                if last[j] != NONE && (prev == NONE || last[j] + j as i32 > prev) {
                    prev = last[j] + j as i32;
                }
                j += 1;
            }
            row[i] = if t[i] == c && prev != NONE { prev + bonus(i) - i as i32 + 1 } else { NONE };
        }
        std::mem::swap(last, row);
    }
    let (lo, hi) = window[m - 1];
    last[lo..=hi].iter().copied().filter(|&s| s != NONE).max().map(|s| s.min(999))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ascii_score_is_score() {
        // a seeded xorshift: the same 20,000 texts and queries every run, from letters, cases, digits and the
        // separators that make word starts
        let mut seed: u64 = 0x5eed_1234_abcd_ef01;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        const ALPHABET: &[u8] = b"abcABC_xyz/-.09 Q";
        let mut scratch = Scratch::default();
        let (mut scattered, mut none) = (0, 0);
        for _ in 0..20_000 {
            let text: String = (0..next(40)).map(|_| ALPHABET[next(ALPHABET.len())] as char).collect();
            let query: String = (0..1 + next(5)).map(|_| ALPHABET[next(ALPHABET.len())].to_ascii_lowercase() as char).filter(|c| *c != ' ').collect();
            let want = score(&query, &text);
            assert_eq!(score_ascii(query.as_bytes(), text.as_bytes(), &mut scratch), want, "{query:?} in {text:?}");
            scattered += usize::from(want.is_some_and(|s| s < 1000));
            none += usize::from(want.is_none());
        }
        assert!(scattered > 1000 && none > 1000, "both kinds came up: {scattered} scattered, {none} not found");
    }
}
