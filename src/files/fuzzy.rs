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

/// Reused rows of `score_ascii`'s table.
#[derive(Default)]
pub struct Scratch(Vec<i32>, Vec<i32>);

/// `score` of a lowercase ASCII `query` without whitespace in an ASCII `text`, the same number without allocating
/// past `scratch`: Quick Open scores 200,000 paths a keystroke.
pub fn score_ascii(query: &[u8], text: &[u8], scratch: &mut Scratch) -> Option<i32> {
    const NONE: i32 = i32::MIN;
    let (m, n) = (query.len(), text.len());
    if m == 0 {
        return Some(0);
    }
    if m > n {
        return None;
    }
    let lower = |i: usize| text[i].to_ascii_lowercase();
    let word_start = |i: usize| i == 0 || !text[i - 1].is_ascii_alphanumeric();
    let mut anywhere = false;
    for i in 0..=n - m {
        if (0..m).all(|j| lower(i + j) == query[j]) {
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
    // `matched`'s table a row at a time: `last[i]` scores the letters so far with the latest at text[i]
    let bonus = |i: usize| if word_start(i) { 10 } else { 0 };
    let Scratch(last, row) = scratch;
    last.clear();
    last.extend((0..n).map(|i| if lower(i) == query[0] { bonus(i) - i as i32 } else { NONE }));
    for &c in &query[1..] {
        row.clear();
        row.resize(n, NONE);
        // the best (score + j) among the previous letter's places before i
        let mut prev = NONE;
        for i in 0..n {
            if prev != NONE && lower(i) == c {
                row[i] = prev + bonus(i) - i as i32 + 1;
            }
            if last[i] != NONE && (prev == NONE || last[i] + i as i32 > prev) {
                prev = last[i] + i as i32;
            }
        }
        std::mem::swap(last, row);
    }
    last.iter().copied().filter(|&s| s != NONE).max().map(|s| s.min(999))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ascii_score_is_score() {
        let texts = ["", "a", "src/main.rs", "Src/Widgets/FooBar.ts", "foo_bar.py", "a_b/c-d/x.y.z", "README.md", "aaaa", "abab_ab", "x/yy/zzz/q.rs", "Split right"];
        let queries = ["a", "ab", "aa", "main", "mr", "src/", "fb", "foobar", "readme", "xyz", "/", "_", "-", "q", "zzzz", "srm", "abab", "spr", "rs"];
        let mut scratch = Scratch::default();
        let mut found = 0;
        for text in texts {
            for query in queries {
                let want = score(query, text);
                assert_eq!(score_ascii(query.as_bytes(), text.as_bytes(), &mut scratch), want, "{query:?} in {text:?}");
                found += usize::from(want.is_some_and(|s| s < 1000));
            }
        }
        assert!(found > 10, "scattered letters were scored too: {found}");
    }
}
