//! The editor's find bar and Go to Line without gpui: the matches of a query in a buffer (fixed text as the Search
//! tab finds it, or a regex), what Replace and Replace All write, a typed `line:column` as a place, and which match
//! is the current one.
//!
//! Case: fixed text folds it as the Search tab does (char by char, `to_lowercase`, ASCII bytes alike when the query is
//! ASCII); a regex as `regex` does (Unicode simple case folding). They differ only for the few characters whose folds
//! differ, such as the Kelvin sign and the long s, which a regex `k` or `s` finds and fixed text does not.
use crate::files::search::{find_ranges, find_ranges_capped};
use regex::{Regex, RegexBuilder};
use std::ops::Range;

/// The most matches a find counts and marks: "10,000+" past it.
pub const MAX_MATCHES: usize = 10_000;

/// What the find bar asks: its text and the Aa, ab and .* switches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindQuery {
    pub text: String,
    /// Match case (Aa).
    pub case: bool,
    /// Whole word (ab): no word character (alphanumeric or `_`) either side of a match.
    pub word: bool,
    /// Regular expression (.*): `regex`'s syntax, multi-line (`^` and `$` at each line), `$1` in a replacement.
    pub regex: bool,
}

/// The matches found, at most the cap of them, in order and not overlapping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Matches {
    /// Byte offsets on char boundaries.
    pub ranges: Vec<Range<usize>>,
    /// There were more than the cap.
    pub capped: bool,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// No word character touches `range` of `text` on either side.
fn whole_word(text: &str, range: &Range<usize>) -> bool {
    !text[..range.start].chars().next_back().is_some_and(is_word) && !text[range.end..].chars().next().is_some_and(is_word)
}

/// The query as a compiled regex, or why it is not one (the message's last line, as the field's tooltip says it).
fn compile(q: &FindQuery) -> Result<Regex, String> {
    RegexBuilder::new(&q.text).case_insensitive(!q.case).multi_line(true).build().map_err(|e| {
        let text = e.to_string();
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("Invalid regular expression");
        last.trim().trim_start_matches("error: ").to_string()
    })
}

/// A regex's matches as the bar counts them: an empty match (`^`, `a*` between the a's) is none, and whole word
/// keeps only those no word character touches.
fn regex_ranges<'a>(re: &'a Regex, text: &'a str, word: bool) -> impl Iterator<Item = Range<usize>> + 'a {
    re.find_iter(text).map(|m| m.range()).filter(move |r| !r.is_empty() && (!word || whole_word(text, r)))
}

/// The matches of `q` in `text`, at most `cap` of them; an invalid regex says why.
pub fn find_all(text: &str, q: &FindQuery, cap: usize) -> Result<Matches, String> {
    if q.text.is_empty() {
        return Ok(Matches::default());
    }
    let mut ranges: Vec<Range<usize>> = if q.regex {
        let re = compile(q)?;
        regex_ranges(&re, text, q.word).take(cap + 1).collect()
    } else {
        find_ranges_capped(text, &q.text, q.case, q.word, cap.saturating_add(1))
    };
    let capped = ranges.len() > cap;
    ranges.truncate(cap);
    Ok(Matches { ranges, capped })
}

/// What Replace writes over `range`, a match of `q` in `text` when it was found: `with` itself, or with a regex its
/// `$1` and `${name}` filled from the match (`$$` a `$`). `None` when `range` is no longer a match (the text moved
/// since).
pub fn replace_one(text: &str, range: Range<usize>, q: &FindQuery, with: &str) -> Option<String> {
    if range.is_empty() || range.end > text.len() || !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
        return None;
    }
    if !q.regex {
        let found = find_ranges(&text[range.clone()], &q.text, q.case, false);
        return (found.len() == 1 && found[0] == (0..range.len()) && (!q.word || whole_word(text, &range))).then(|| with.to_string());
    }
    let re = compile(q).ok()?;
    let caps = re.captures_at(text, range.start)?;
    let m = caps.get(0)?;
    if m.range() != range || (q.word && !whole_word(text, &range)) {
        return None;
    }
    let mut out = String::new();
    caps.expand(with, &mut out);
    Some(out)
}

/// One replacement Replace All made: the bytes it replaced, and how long what it wrote there is.
pub type Edit = (Range<usize>, usize);

/// `text` with every match of `q` replaced (no cap), and the replacements, in order.
pub fn replace_all_text(text: &str, q: &FindQuery, with: &str) -> Result<(String, Vec<Edit>), String> {
    if q.text.is_empty() {
        return Ok((text.to_string(), Vec::new()));
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut edits = Vec::new();
    if q.regex {
        let re = compile(q)?;
        for caps in re.captures_iter(text) {
            let Some(m) = caps.get(0) else { continue };
            let r = m.range();
            if r.is_empty() || (q.word && !whole_word(text, &r)) {
                continue;
            }
            out.push_str(&text[at..r.start]);
            let len = out.len();
            caps.expand(with, &mut out);
            edits.push((r.clone(), out.len() - len));
            at = r.end;
        }
    } else {
        for r in find_ranges(text, &q.text, q.case, q.word) {
            out.push_str(&text[at..r.start]);
            out.push_str(with);
            edits.push((r.clone(), with.len()));
            at = r.end;
        }
    }
    out.push_str(&text[at..]);
    Ok((out, edits))
}

/// Where `offset` of the old text is after `edits` (in order, not overlapping): moved by those before it, and inside
/// a replaced stretch kept as far in as the new text there reaches.
pub fn map_offset(offset: usize, edits: &[Edit]) -> usize {
    let mut shift: isize = 0;
    for (range, len) in edits {
        if range.start >= offset {
            break;
        }
        if offset < range.end {
            return (range.start as isize + shift) as usize + (offset - range.start).min(*len);
        }
        shift += *len as isize - range.len() as isize;
    }
    (offset as isize + shift) as usize
}

/// A place typed into Go to Line, 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GotoTarget {
    pub line: u32,
    /// In characters; `None` is the line's start.
    pub column: Option<u32>,
}

/// `12`, `12:5` or `:5` (a column of the `current` line), clamped to the `line_count` lines; `None` when it is
/// none of those.
pub fn parse_goto(s: &str, current: u32, line_count: u32) -> Option<GotoTarget> {
    let s = s.trim();
    let (line, column) = match s.split_once(':') {
        Some((l, c)) => (l.trim(), Some(c.trim())),
        None => (s, None),
    };
    let number = |t: &str| (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.parse::<u64>().unwrap_or(u64::MAX));
    let line = if line.is_empty() {
        // `:5` alone; a bare `:` is nothing
        column?;
        current as u64
    } else {
        number(line)?
    };
    let column = match column {
        Some(c) => Some(number(c)?.clamp(1, u32::MAX as u64) as u32),
        None => None,
    };
    Some(GotoTarget { line: line.clamp(1, line_count.max(1) as u64) as u32, column })
}

/// The byte offset of 1-based character `column` in `line` (its text, without the line end), at most its end.
pub fn column_offset(line: &str, column: u32) -> usize {
    line.char_indices().nth(column.saturating_sub(1) as usize).map_or(line.len(), |(i, _)| i)
}

/// The match current for a cursor at `cursor`: the first starting at or after it, else the first (the search wraps).
pub fn nearest(matches: &[Range<usize>], cursor: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(matches.partition_point(|r| r.start < cursor) % matches.len())
}

/// The match Previous goes to from a selection starting at `at`: the last starting before it, else the last (the
/// search wraps).
pub fn previous(matches: &[Range<usize>], at: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(matches.partition_point(|r| r.start < at).checked_sub(1).unwrap_or(matches.len() - 1))
}

/// "3 of 120", "3 of 10,000+", "No results" (only once there is a query).
pub fn count_label(current: Option<usize>, total: usize, capped: bool) -> String {
    if total == 0 {
        return "No results".into();
    }
    let total = format!("{}{}", group(total), if capped { "+" } else { "" });
    match current {
        Some(i) => format!("{} of {total}", group(i + 1)),
        None => format!("{total} results"),
    }
}

/// The text a selection prefills the query with: one line, not empty.
pub fn prefill(selected: &str) -> Option<&str> {
    (!selected.is_empty() && !selected.contains('\n')).then_some(selected)
}

/// Whether the replace row shows: asked for, in a file that can be written.
pub fn shows_replace(asked: bool, readonly: bool) -> bool {
    asked && !readonly
}

/// `n` with thousands apart: 10,000.
pub fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> FindQuery {
        FindQuery { text: text.into(), ..FindQuery::default() }
    }

    fn re(text: &str) -> FindQuery {
        FindQuery { regex: true, ..q(text) }
    }

    /// One match (clippy reads `[0..3]` as a mistake for a range's numbers).
    fn one(r: Range<usize>) -> Vec<Range<usize>> {
        vec![r]
    }

    fn count((text, edits): (String, Vec<Edit>)) -> (String, usize) {
        (text, edits.len())
    }

    fn found(text: &str, query: &FindQuery) -> Vec<Range<usize>> {
        find_all(text, query, MAX_MATCHES).unwrap().ranges
    }

    #[test]
    fn literal_finds_as_the_search_tab_does() {
        let text = "Foo foo food _foo";
        assert_eq!(found(text, &q("foo")), [0..3, 4..7, 8..11, 14..17], "any case by default");
        assert_eq!(found(text, &FindQuery { case: true, ..q("foo") }), [4..7, 8..11, 14..17]);
        assert_eq!(found(text, &FindQuery { word: true, ..q("foo") }), [0..3, 4..7], "food and _foo are not the word");
        // regex characters are plain text here
        assert_eq!(found("a.b axb", &q("a.b")), one(0..3));
        assert_eq!(found("anything", &q("")), Vec::<Range<usize>>::new());
    }

    #[test]
    fn a_regex_skips_empty_matches() {
        assert_eq!(found("a\n\nb", &re("^")), Vec::<Range<usize>>::new(), "line starts match nothing");
        assert_eq!(found("baab", &re("a*")), one(1..3), "only the a's, not the empty matches between");
        assert_eq!(found("x\ny", &re("^.$")), [0..1, 2..3], "multi-line: ^ and $ at each line");
    }

    #[test]
    fn a_regex_honours_case_and_whole_word() {
        let text = "Cat cat concat cats";
        assert_eq!(found(text, &re("cat")), [0..3, 4..7, 11..14, 15..18]);
        assert_eq!(found(text, &FindQuery { case: true, ..re("C.t") }), one(0..3));
        assert_eq!(found(text, &FindQuery { word: true, ..re("cats?") }), [0..3, 4..7, 15..19]);
        // Unicode case folding
        assert_eq!(found("ÉCOLE école", &re("école")), [0..6, 7..13]);
    }

    #[test]
    fn an_invalid_regex_says_why() {
        let err = find_all("abc", &re("a("), MAX_MATCHES).unwrap_err();
        assert!(!err.is_empty() && !err.contains('\n'), "{err:?}");
        assert!(find_all("abc", &q("a("), MAX_MATCHES).is_ok(), "fixed text is never invalid");
    }

    #[test]
    fn matches_stop_at_the_cap() {
        let text = "x".repeat(10);
        let m = find_all(&text, &q("x"), 3).unwrap();
        assert_eq!(m.ranges, [0..1, 1..2, 2..3]);
        assert!(m.capped);
        let m = find_all(&text, &re("x"), 3).unwrap();
        assert_eq!((m.ranges.len(), m.capped), (3, true));
        let m = find_all(&text, &q("x"), 10).unwrap();
        assert_eq!((m.ranges.len(), m.capped), (10, false), "exactly the cap is not past it");
        assert_eq!(MAX_MATCHES, 10_000);
    }

    #[test]
    fn replace_fills_groups_in_a_regex_only() {
        let text = "let a = 1; let b = 2;";
        let pattern = re(r"let (\w) = (\d)");
        assert_eq!(replace_one(text, 0..9, &pattern, "$2 -> $1").as_deref(), Some("1 -> a"));
        assert_eq!(replace_one(text, 0..9, &pattern, "$$1").as_deref(), Some("$1"), "$$ is a dollar");
        assert_eq!(replace_one(text, 11..20, &pattern, "${1}x").as_deref(), Some("bx"));
        // fixed text writes the replacement as typed, $ and all
        assert_eq!(replace_one(text, 0..3, &q("let"), "$1").as_deref(), Some("$1"));
        let (all, n) = count(replace_all_text(text, &pattern, "$1=$2").unwrap());
        assert_eq!((all.as_str(), n), ("a=1; b=2;", 2));
        let (all, n) = count(replace_all_text("a$b a$b", &q("$b"), "$1").unwrap());
        assert_eq!((all.as_str(), n), ("a$1 a$1", 2));
    }

    #[test]
    fn replace_checks_the_match_is_still_there() {
        let text = "foo bar foo";
        assert_eq!(replace_one(text, 8..11, &q("foo"), "x").as_deref(), Some("x"));
        assert_eq!(replace_one(text, 4..7, &q("foo"), "x"), None, "the text moved: not a match now");
        assert_eq!(replace_one(text, 8..11, &FindQuery { case: true, ..q("FOO") }, "x"), None);
        assert_eq!(replace_one("foods", 0..3, &FindQuery { word: true, ..q("foo") }, "x"), None);
        assert_eq!(replace_one(text, 8..12, &q("foo"), "x"), None, "past the end");
        assert_eq!(replace_one(text, 4..7, &re("b.r"), "x").as_deref(), Some("x"));
        assert_eq!(replace_one(text, 4..6, &re("b.r"), "x"), None, "part of a match is not one");
    }

    #[test]
    fn replace_all_skips_what_find_skips() {
        let (all, n) = count(replace_all_text("baab", &re("a*"), "-").unwrap());
        assert_eq!((all.as_str(), n), ("b-b", 1), "no empty matches");
        let (all, n) = count(replace_all_text("cat cats", &FindQuery { word: true, ..re("cat") }, "dog").unwrap());
        assert_eq!((all.as_str(), n), ("dog cats", 1));
        let (all, n) = count(replace_all_text("Foo foo", &FindQuery { case: true, ..q("foo") }, "x").unwrap());
        assert_eq!((all.as_str(), n), ("Foo x", 1));
        assert!(replace_all_text("x", &re("("), "y").is_err());
        assert_eq!(replace_all_text("x", &q(""), "y").unwrap(), ("x".to_string(), Vec::new()));
    }

    #[test]
    fn replace_all_carries_the_cursor_across() {
        // "ab cd ab" with ab -> xyz: 0..2 and 6..8 become three bytes each
        let (out, edits) = replace_all_text("ab cd ab", &q("ab"), "xyz").unwrap();
        assert_eq!(out, "xyz cd xyz");
        assert_eq!(edits, [(0..2, 3), (6..8, 3)]);
        assert_eq!(map_offset(0, &edits), 0, "at a replacement's start");
        assert_eq!(map_offset(3, &edits), 4, "after the first: moved by its one byte more");
        assert_eq!(map_offset(5, &edits), 6, "just before the second");
        assert_eq!(map_offset(8, &edits), 10, "the end");
        assert_eq!(map_offset(1, &edits), 1, "inside one: as far in");
        // shorter replacements pull it back, and inside one it stays within the new text
        let (_, edits) = replace_all_text("abcd abcd x", &q("abcd"), "z").unwrap();
        assert_eq!(map_offset(10, &edits), 4);
        assert_eq!(map_offset(8, &edits), 3, "inside the second: at most its end");
        assert_eq!(map_offset(4, &[]), 4);
    }

    #[test]
    fn go_to_line_reads_line_and_column_and_clamps() {
        let t = |line, column| Some(GotoTarget { line, column });
        assert_eq!(parse_goto("12", 1, 100), t(12, None));
        assert_eq!(parse_goto(" 12 : 5 ", 1, 100), t(12, Some(5)));
        assert_eq!(parse_goto(":5", 7, 100), t(7, Some(5)), "a column of the current line");
        assert_eq!(parse_goto("500", 1, 100), t(100, None), "past the end: the last line");
        assert_eq!(parse_goto("0", 1, 100), t(1, None));
        assert_eq!(parse_goto("3:0", 1, 100), t(3, Some(1)));
        assert_eq!(parse_goto("99999999999999999999", 1, 100), t(100, None));
        assert_eq!(parse_goto("1", 1, 0), t(1, None), "an empty buffer has its one line");
        for bad in ["", ":", "x", "-3", "3:x", "1.5", "+2"] {
            assert_eq!(parse_goto(bad, 1, 100), None, "{bad:?}");
        }
    }

    #[test]
    fn columns_count_characters() {
        assert_eq!(column_offset("héllo", 1), 0);
        assert_eq!(column_offset("héllo", 3), 3, "é is two bytes");
        assert_eq!(column_offset("héllo", 99), 6, "past the end: the end");
        assert_eq!(column_offset("", 4), 0);
    }

    #[test]
    fn the_current_match_is_the_first_at_or_after_the_cursor() {
        let m = [2..4, 6..8, 10..12];
        assert_eq!(nearest(&m, 0), Some(0));
        assert_eq!(nearest(&m, 2), Some(0), "at a match's start: that one");
        assert_eq!(nearest(&m, 3), Some(1));
        assert_eq!(nearest(&m, 10), Some(2));
        assert_eq!(nearest(&m, 11), Some(0), "past the last: wraps to the first");
        assert_eq!(nearest(&[], 5), None);
    }

    #[test]
    fn previous_is_the_last_before_the_selection_and_wraps() {
        let m = [2..4, 6..8, 10..12];
        assert_eq!(previous(&m, 6), Some(0), "selected 6..8: the one before");
        assert_eq!(previous(&m, 7), Some(1), "inside a match: that one");
        assert_eq!(previous(&m, 12), Some(2));
        assert_eq!(previous(&m, 2), Some(2), "at the first: wraps to the last");
        assert_eq!(previous(&m, 0), Some(2));
        assert_eq!(previous(&[], 5), None);
        // next from a selected match is the one after it
        assert_eq!(nearest(&m, 8), Some(2));
    }

    #[test]
    fn the_count_reads_as_its_place_of_the_total() {
        assert_eq!(count_label(Some(2), 120, false), "3 of 120");
        assert_eq!(count_label(Some(0), 10_000, true), "1 of 10,000+");
        assert_eq!(count_label(None, 4, false), "4 results");
        assert_eq!(count_label(None, 0, false), "No results");
    }

    #[test]
    fn a_one_line_selection_prefills_the_query() {
        assert_eq!(prefill("needle"), Some("needle"));
        assert_eq!(prefill("two\nlines"), None);
        assert_eq!(prefill(""), None);
    }

    #[test]
    fn a_read_only_file_hides_replace() {
        assert!(shows_replace(true, false));
        assert!(!shows_replace(true, true));
        assert!(!shows_replace(false, false));
    }
}
