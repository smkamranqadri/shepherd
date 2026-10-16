//! The editor gutter's change bars: the buffer's lines diffed against git's index blob (what `git diff` would show),
//! and the index against HEAD for what is staged, mapped onto the buffer's lines (drawn as outlines). Pure: the
//! editor fetches both blobs in one `git cat-file --batch` and runs this off the main thread.
use std::ops::Range;
use std::time::Instant;

/// Past this many changed stretches the bars say only that much changed: one stretch over all of them.
pub const MAX_HUNKS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Added,
    Modified,
    /// Lines went from above `rows.start` (`rows` is empty).
    Deleted,
}

/// A stretch of the buffer's lines (0-based rows) and how it differs; `staged`: the change is in the index already
/// (HEAD to index), the buffer's lines are as the index has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChange {
    pub rows: Range<usize>,
    pub mark: Mark,
    pub staged: bool,
}

/// One diff's stretches: the old side's lines and the new side's.
pub type Hunk = (Range<usize>, Range<usize>);

/// `old` against `new`, line by line, under `deadline` (a slow diff gives up into fewer, larger stretches), and the
/// lines equal on both sides as (old start, new start, length).
fn hunks(old: &[&str], new: &[&str], deadline: Instant) -> (Vec<Hunk>, Vec<(usize, usize, usize)>) {
    let ops = similar::capture_diff_slices_deadline(similar::Algorithm::Myers, old, new, Some(deadline));
    let (mut out, mut equal): (Vec<Hunk>, Vec<_>) = (Vec::new(), Vec::new());
    for op in ops {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == similar::DiffTag::Equal {
            equal.push((o.start, n.start, o.len()));
            continue;
        }
        // neighbouring ops make one stretch (a delete next to an insert is a modification)
        match out.last_mut() {
            Some((lo, ln)) if lo.end == o.start && ln.end == n.start => {
                lo.end = o.end;
                ln.end = n.end;
            }
            _ => out.push((o, n)),
        }
    }
    (out, equal)
}

fn mark_of(old: &Range<usize>, new: &Range<usize>) -> Mark {
    if new.is_empty() {
        Mark::Deleted
    } else if old.is_empty() {
        Mark::Added
    } else {
        Mark::Modified
    }
}

/// One stretch over all of `hunks` when there are more than `MAX_HUNKS` of them.
fn capped(hunks: Vec<Hunk>) -> Vec<Hunk> {
    if hunks.len() <= MAX_HUNKS {
        return hunks;
    }
    let (first, last) = (&hunks[0], &hunks[hunks.len() - 1]);
    vec![(first.0.start..last.0.end, first.1.start..last.1.end)]
}

/// Lines as diffed: split at line ends, a `\r` before one left out (the index may hold CRLF, the buffer has `\n`).
pub fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// What is staged: HEAD's blob against the index's (`None`: the file is not in HEAD, all of the index is), as
/// stretches of index lines. Worked out once when the blobs come, so HEAD's blob need not be kept.
pub fn staged(head: Option<&str>, index: &str, deadline: Instant) -> Vec<Hunk> {
    let index = lines(index);
    match head {
        Some(head) => capped(hunks(&lines(head), &index, deadline).0),
        // added to the index, not in HEAD: all of it is staged
        None if !index.is_empty() => vec![(0..0, 0..index.len())],
        None => Vec::new(),
    }
}

/// The buffer's change bars: `buffer` against the `index` blob, and what is staged (`staged`) where the buffer's
/// lines are still the index's, in order of their first row. No index blob (an untracked file,
/// no repo): none.
pub fn line_changes(staged: &[Hunk], index: Option<&str>, buffer: &str, deadline: Instant) -> Vec<LineChange> {
    let Some(index) = index else { return Vec::new() };
    let (index, buffer) = (lines(index), lines(buffer));
    let (unstaged, equal) = hunks(&index, &buffer, deadline);
    let mut out: Vec<LineChange> = capped(unstaged).into_iter().map(|(o, n)| LineChange { mark: mark_of(&o, &n), rows: n, staged: false }).collect();
    // an index line's row in the buffer, where the buffer still has it
    let row_of = |i: usize| -> Option<usize> {
        let at = equal.partition_point(|(o, _, len)| o + len <= i);
        equal.get(at).filter(|(o, _, _)| *o <= i).map(|(o, n, _)| n + (i - o))
    };
    for (o, n) in staged.iter().cloned() {
        let mark = mark_of(&o, &n);
        if mark == Mark::Deleted {
            // lines staged out from above index line `n.start`: above that line's row, or the buffer's end
            let row = if n.start >= index.len() { Some(buffer.len()) } else { row_of(n.start) };
            if let Some(row) = row {
                out.push(LineChange { rows: row..row, mark, staged: true });
            }
            continue;
        }
        // the stretch's lines the buffer still has, in runs of consecutive rows
        let mut run: Option<Range<usize>> = None;
        for row in n.filter_map(row_of) {
            match &mut run {
                Some(r) if r.end == row => r.end += 1,
                _ => {
                    out.extend(run.take().map(|rows| LineChange { rows, mark, staged: true }));
                    run = Some(row..row + 1);
                }
            }
        }
        out.extend(run.map(|rows| LineChange { rows, mark, staged: true }));
    }
    out.sort_by_key(|c| (c.rows.start, c.rows.end, c.staged));
    out
}

/// `git cat-file --batch`'s answers, one per object asked for: its id and bytes, or `None` for a missing one.
/// Answers past a damaged one are left out.
pub fn parse_batch(mut out: &[u8]) -> Vec<Option<(String, Vec<u8>)>> {
    let mut answers = Vec::new();
    while !out.is_empty() {
        let Some(nl) = out.iter().position(|&b| b == b'\n') else { break };
        let header = String::from_utf8_lossy(&out[..nl]).into_owned();
        out = &out[nl + 1..];
        // `<name> missing`, `<name> ambiguous` (a name may hold spaces)
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            answers.push(None);
            continue;
        }
        let parts: Vec<&str> = header.split(' ').collect();
        match parts.as_slice() {
            [oid, _, size] => {
                let Ok(size) = size.parse::<usize>() else { break };
                if out.len() < size {
                    break;
                }
                answers.push(Some((oid.to_string(), out[..size].to_vec())));
                // the object, then a line end
                out = &out[(size + 1).min(out.len())..];
            }
            _ => break,
        }
    }
    answers
}

/// The bars of `changes` (in order of their first row) on the rows `visible`, for drawing. Bars may overlap (one
/// stretch over many when capped, staged outlines under it), so only their starts are relied on being in order.
pub fn visible(changes: &[LineChange], visible: Range<usize>) -> impl Iterator<Item = &LineChange> {
    let to = changes.partition_point(|c| c.rows.start <= visible.end);
    changes[..to].iter().filter(move |c| {
        if c.rows.is_empty() {
            // a deletion's notch sits on the line between: from the top edge to the bottom one
            (visible.start..=visible.end).contains(&c.rows.start)
        } else {
            c.rows.start < visible.end && c.rows.end > visible.start
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn changes(head: Option<&str>, index: Option<&str>, buffer: &str) -> Vec<(Range<usize>, Mark, bool)> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let staged = index.map(|i| staged(head, i, deadline)).unwrap_or_default();
        line_changes(&staged, index, buffer, deadline).into_iter().map(|c| (c.rows, c.mark, c.staged)).collect()
    }

    #[test]
    fn added_modified_and_deleted_lines_against_the_index() {
        let index = "a\nb\nc\nd\ne\n";
        assert_eq!(changes(Some(index), Some(index), index), []);
        assert_eq!(changes(Some(index), Some(index), "a\nb\nX\nc\nd\ne\n"), [(2..3, Mark::Added, false)]);
        assert_eq!(changes(Some(index), Some(index), "a\nB\nc\nd\ne\n"), [(1..2, Mark::Modified, false)]);
        assert_eq!(changes(Some(index), Some(index), "a\nb\nd\ne\n"), [(2..2, Mark::Deleted, false)]);
        assert_eq!(changes(Some(index), Some(index), "a\nb\nc\nd\n"), [(4..4, Mark::Deleted, false)], "the last line, at the end");
        // CRLF in the index is the same lines
        assert_eq!(changes(None, Some("a\r\nb\r\n"), "a\nb\n").iter().filter(|c| !c.2).count(), 0);
    }

    #[test]
    fn staged_changes_are_mapped_onto_the_buffer_and_outlined() {
        let head = "a\nb\nc\nd\n";
        let index = "a\nB\nc\nd\nnew\n";
        // nothing changed since staging: the staged lines, where they are in the buffer
        assert_eq!(changes(Some(head), Some(index), index), [(1..2, Mark::Modified, true), (4..5, Mark::Added, true)]);
        // a line typed above them moves them down
        let buffer = "top\na\nB\nc\nd\nnew\n";
        assert_eq!(changes(Some(head), Some(index), buffer), [(0..1, Mark::Added, false), (2..3, Mark::Modified, true), (5..6, Mark::Added, true)]);
        // a staged line changed again is the buffer's change, not drawn twice
        let buffer = "a\nBB\nc\nd\nnew\n";
        assert_eq!(changes(Some(head), Some(index), buffer), [(1..2, Mark::Modified, false), (4..5, Mark::Added, true)]);
        // a staged deletion
        assert_eq!(changes(Some(head), Some("a\nc\nd\n"), "a\nc\nd\n"), [(1..1, Mark::Deleted, true)]);
        // added to the index, not in HEAD: all staged
        assert_eq!(changes(None, Some("x\ny\n"), "x\ny\n"), [(0..2, Mark::Added, true)]);
        // not in the index: nothing
        assert_eq!(changes(None, None, "x\n"), []);
    }

    #[test]
    fn many_changes_collapse_into_one() {
        // (the diff's own count depends on how far it got before its deadline: the cap is tested on its own)
        let many: Vec<Hunk> = (0..MAX_HUNKS + 1).map(|i| (i * 2..i * 2 + 1, i * 2..i * 2 + 1)).collect();
        assert_eq!(capped(many), vec![(0..MAX_HUNKS * 2 + 1, 0..MAX_HUNKS * 2 + 1)], "over {MAX_HUNKS} stretches: one");
        let few: Vec<Hunk> = (0..MAX_HUNKS).map(|i| (i * 2..i * 2 + 1, i * 2..i * 2 + 1)).collect();
        assert_eq!(capped(few.clone()), few);
        // and through the diff: a modified first and last line of many make one stretch over all between
        let index: String = (0..3000).map(|i| format!("{i}\n")).collect();
        let buffer: String = (0..3000).map(|i| if i % 2 == 0 { format!("x{i}\n") } else { format!("{i}\n") }).collect();
        let own: Vec<_> = changes(None, Some(&index), &buffer).into_iter().filter(|c| !c.2).collect();
        assert!(!own.is_empty() && own.len() <= MAX_HUNKS, "{}", own.len());
        assert_eq!((own[0].0.start, own[own.len() - 1].0.end), (0, 2999));
    }

    #[test]
    fn batch_answers_parse_with_missing_ones() {
        let out = b"1234 blob 4\nab\nc\nHEAD:./my x missing\n99 blob 0\n\n";
        let answers = parse_batch(out);
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[0], Some(("1234".into(), b"ab\nc".to_vec())));
        assert_eq!(answers[1], None);
        assert_eq!(answers[2], Some(("99".into(), Vec::new())));
        assert!(parse_batch(b"12 blob 40\nshort").is_empty(), "a cut answer is left out");
    }

    #[test]
    fn only_the_rows_on_screen_are_drawn() {
        let c = |rows: Range<usize>, mark| LineChange { rows, mark, staged: false };
        let all = [c(0..2, Mark::Added), c(5..5, Mark::Deleted), c(6..9, Mark::Modified), c(20..21, Mark::Added)];
        let shown: Vec<_> = visible(&all, 5..10).map(|c| (c.rows.start, c.rows.end)).collect();
        assert_eq!(shown, [(5, 5), (6, 9)]);
        assert_eq!(visible(&all, 2..5).map(|c| (c.rows.start, c.rows.end)).collect::<Vec<_>>(), [(5, 5)], "a deletion at the bottom edge");
        assert_eq!(visible(&all, 21..30).count(), 0);
        // a capped stretch over everything, staged outlines inside it: all that touch the rows, whatever their ends
        let s = |rows: Range<usize>| LineChange { rows, mark: Mark::Added, staged: true };
        let overlapping = [c(0..500, Mark::Modified), s(3..4), s(40..41), c(450..450, Mark::Deleted), s(600..601)];
        let shown: Vec<_> = visible(&overlapping, 30..60).map(|c| (c.rows.start, c.rows.end)).collect();
        assert_eq!(shown, [(0, 500), (40, 41)], "the long bar starting above the screen is drawn");
        let shown: Vec<_> = visible(&overlapping, 440..460).map(|c| (c.rows.start, c.rows.end)).collect();
        assert_eq!(shown, [(0, 500), (450, 450)]);
        assert_eq!(visible(&overlapping, 501..700).map(|c| c.rows.start).collect::<Vec<_>>(), [600]);
    }
}
