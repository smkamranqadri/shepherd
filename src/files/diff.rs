//! A change's diff, for its diff tab: the git arguments that print it per Changes group (and per scope when a file
//! is both staged and changed again), and a parser of git's unified output that keeps each line's numbers in the
//! old and new versions. Pure, but for nothing: the tab runs git through `files::runner` and hands the bytes here.
//!
//! The parser follows uherdr's (`GitDiff.swift`): git is told `diff.suppressBlankEmpty=false`, so a context line
//! always has its space, and a bare empty line (a user's config, a mail client's patch) is skipped without moving
//! the numbers; a trailing `\r` goes, as the viewer drops it; `\ No newline at end of file` marks the line before
//! it. A combined diff (`@@@`, a merge conflict's) is not parsed: the tab opens the file with its markers instead.
//! A deleted file's diff is git's own too (`git diff -- path`, `--cached` when staged), so a file staged and then
//! deleted (`MD`) or added and then deleted (`AD`) shows each group's part of it, not one guess at its last version.
//! Word emphasis follows wooloo's (`WorkspaceDiffView.swift`): each run of removed lines pairs with the run of
//! added lines after it, line by line, and each pair is diffed by words with `similar`; a pair that keeps less than
//! 40% of its text gets no emphasis, as whole changed lines read better than confetti.
use super::status::{Change, Group};
use std::ops::Range;
use std::time::{Duration, Instant};

/// How long git may take over a diff, and how much of it is read: a diff past 8 MiB is not one to read in a tab.
pub const TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_BYTES: usize = 8 << 20;
/// A line longer than this (in bytes) gets no word emphasis: minified code would be all confetti, and slow.
pub const MAX_EMPHASIS: usize = 1000;
/// The time all of a diff's word emphasis may take; the pairs past it show whole lines.
pub const EMPHASIS_BUDGET: Duration = Duration::from_millis(150);
/// git's empty tree: the base of a file's whole change ("All") in a repository without a commit yet (SHA-1 ones;
/// the tab asks git for it in a SHA-256 one).
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's number in the old version; `None` for an added line.
    pub old: Option<u32>,
    /// In the new version; `None` for a removed line.
    pub new: Option<u32>,
    /// Without its marker, its line end or a trailing `\r`.
    pub text: String,
    /// `\ No newline at end of file` followed it.
    pub no_newline: bool,
    /// The changed words of a paired removed or added line (byte ranges of `text`), from `mark_words`.
    pub emphasis: Vec<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@ context` line as git wrote it.
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// The first line of the new version the hunk covers; with no new lines (a pure removal), the one after the
    /// line its start names (git names the line before an empty side).
    pub fn new_first(&self) -> u32 {
        if self.new_count == 0 { self.new_start + 1 } else { self.new_start }
    }

    pub fn old_first(&self) -> u32 {
        if self.old_count == 0 { self.old_start + 1 } else { self.old_start }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub hunks: Vec<Hunk>,
    /// git called it binary: there are no lines to show.
    pub binary: bool,
    /// A combined diff (`@@@`), as git prints an unmerged file: not parsed, the file is opened instead.
    pub combined: bool,
    /// The header says a new file (`new file mode`) or a deleted one (`deleted file mode`).
    pub new_file: bool,
    pub deleted_file: bool,
    /// The file's modes before and after, as the header has them (`old mode`/`new mode`, or a new or deleted
    /// file's one).
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    /// A rename's paths and how alike its two versions are (`similarity index 100%`: not a line changed).
    pub rename_from: Option<String>,
    pub rename_to: Option<String>,
    pub similarity: Option<u32>,
}

impl Diff {
    /// Lines added and removed.
    pub fn counts(&self) -> (usize, usize) {
        let lines = self.hunks.iter().flat_map(|h| &h.lines);
        lines.fold((0, 0), |(a, r), l| match l.kind {
            LineKind::Added => (a + 1, r),
            LineKind::Removed => (a, r + 1),
            LineKind::Context => (a, r),
        })
    }

    /// What a diff without hunks (and not binary) is, from its header: a title and a line under it.
    pub fn without_hunks(&self) -> (&'static str, String) {
        let mode = match (&self.old_mode, &self.new_mode) {
            (Some(a), Some(b)) if a != b && !self.new_file && !self.deleted_file => Some(format!("Its mode changed from {a} to {b}.")),
            _ => None,
        };
        if self.new_file {
            return ("Empty file", "A new file with nothing in it.".into());
        }
        if self.deleted_file {
            return ("Empty file deleted", "The file had nothing in it.".into());
        }
        if let (Some(from), Some(to)) = (&self.rename_from, &self.rename_to) {
            let mode = mode.map(|m| format!(" {m}")).unwrap_or_default();
            return ("Renamed", format!("From {from} to {to}, its text unchanged.{mode}"));
        }
        match mode {
            Some(mode) => ("Mode changed", mode),
            None => ("No changes", "The two versions have the same text.".into()),
        }
    }

    /// The new version's line where the first change is, for "Open file": its first added line, or where its first
    /// removal was.
    pub fn first_change(&self) -> Option<u32> {
        self.hunks.iter().find_map(|h| {
            let first = h.lines.iter().find(|l| l.kind != LineKind::Context)?;
            Some(first.new.unwrap_or_else(|| {
                // a removal: the new line that follows it, which is where it was
                let before = h.lines.iter().take_while(|l| !std::ptr::eq(*l, first)).filter(|l| l.new.is_some()).count() as u32;
                h.new_first() + before
            }))
        })
    }
}

/// `@@ -a[,b] +c[,d] @@…` as (a, b, c, d); a missing count is 1.
fn hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let mut parts = line.strip_prefix("@@ ")?.split(' ');
    let side = |part: Option<&str>, sign: char| -> Option<(u32, u32)> {
        let part = part?.strip_prefix(sign)?;
        match part.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((part.parse().ok()?, 1)),
        }
    };
    let (a, b) = side(parts.next(), '-')?;
    let (c, d) = side(parts.next(), '+')?;
    Some((a, b, c, d))
}

/// git's unified diff of one file (or of a rename's two paths) as hunks; headers (`diff --git`, `index`, `---`,
/// `+++`, modes) are skipped. A hunk ends when its counts are used up or at a line no hunk line starts with.
pub fn parse_unified(out: &[u8]) -> Diff {
    let mut diff = Diff::default();
    let (mut old, mut new) = (0u32, 0u32);
    // the lines each side of the hunk still has to come; both zero: not in a hunk
    let (mut old_left, mut new_left) = (0u32, 0u32);
    let mut pieces: Vec<&[u8]> = out.split(|&b| b == b'\n').collect();
    if pieces.last().is_some_and(|p| p.is_empty()) {
        pieces.pop();
    }
    for raw in pieces {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = String::from_utf8_lossy(raw);
        let in_hunk = old_left > 0 || new_left > 0;
        if line.starts_with("@@@") {
            diff.combined = true;
            (old_left, new_left) = (0, 0);
            continue;
        }
        if line.starts_with("@@ ") {
            match hunk_header(&line) {
                Some((a, b, c, d)) => {
                    (old, new, old_left, new_left) = (a, c, b, d);
                    diff.hunks.push(Hunk { header: line.into_owned(), old_start: a, old_count: b, new_start: c, new_count: d, lines: Vec::new() });
                }
                None => (old_left, new_left) = (0, 0),
            }
            continue;
        }
        let Some(hunk) = diff.hunks.last_mut().filter(|_| in_hunk || line.starts_with('\\')) else {
            header_fact(&mut diff, &line);
            continue;
        };
        let Some(marker) = line.chars().next() else {
            // a bare empty line: not a hunk line, and not a number
            continue;
        };
        let text = line[1..].to_string();
        let push = |kind, old, new| DiffLine { kind, old, new, text, no_newline: false, emphasis: Vec::new() };
        match marker {
            ' ' if old_left > 0 && new_left > 0 => {
                hunk.lines.push(push(LineKind::Context, Some(old), Some(new)));
                (old, new, old_left, new_left) = (old + 1, new + 1, old_left - 1, new_left - 1);
            }
            '+' if new_left > 0 => {
                hunk.lines.push(push(LineKind::Added, None, Some(new)));
                (new, new_left) = (new + 1, new_left - 1);
            }
            '-' if old_left > 0 => {
                hunk.lines.push(push(LineKind::Removed, Some(old), None));
                (old, old_left) = (old + 1, old_left - 1);
            }
            '\\' => {
                if let Some(last) = hunk.lines.last_mut() {
                    last.no_newline = true;
                }
            }
            _ => {
                (old_left, new_left) = (0, 0);
                if line.starts_with("Binary files ") {
                    diff.binary = true;
                }
            }
        }
    }
    diff
}

/// A header line outside the hunks: what it says of the file, kept in `diff`.
fn header_fact(diff: &mut Diff, line: &str) {
    if line.starts_with("Binary files ") || line == "GIT binary patch" {
        diff.binary = true;
    } else if let Some(mode) = line.strip_prefix("new file mode ") {
        (diff.new_file, diff.new_mode) = (true, Some(mode.to_string()));
    } else if let Some(mode) = line.strip_prefix("deleted file mode ") {
        (diff.deleted_file, diff.old_mode) = (true, Some(mode.to_string()));
    } else if let Some(mode) = line.strip_prefix("old mode ") {
        diff.old_mode = Some(mode.to_string());
    } else if let Some(mode) = line.strip_prefix("new mode ") {
        diff.new_mode = Some(mode.to_string());
    } else if let Some(path) = line.strip_prefix("rename from ") {
        diff.rename_from = Some(path.to_string());
    } else if let Some(path) = line.strip_prefix("rename to ") {
        diff.rename_to = Some(path.to_string());
    } else if let Some(score) = line.strip_prefix("similarity index ") {
        diff.similarity = score.trim_end_matches('%').parse().ok();
    }
}

/// What a diff tab compares. A change's own group picks it (`Scope::of`); a file both staged and changed again
/// offers the other two of All, Staged and Unstaged as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// HEAD against the working tree: the file's whole change.
    All,
    /// HEAD against the index.
    Staged,
    /// The index against the working tree.
    Unstaged,
    /// Nothing against the working tree (`--no-index /dev/null`): exit 1 is a diff, not a failure.
    Untracked,
}

impl Scope {
    pub fn of(group: Group) -> Scope {
        match group {
            Group::Staged => Scope::Staged,
            Group::Untracked => Scope::Untracked,
            Group::Changes | Group::Conflicts => Scope::Unstaged,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "All",
            Scope::Staged => "Staged",
            Scope::Unstaged => "Unstaged",
            Scope::Untracked => "Untracked",
        }
    }
}

/// `git <args>` (run at the repository's top, with `GIT_LITERAL_PATHSPECS=1`) that print `change`'s diff in `scope`.
/// `base` is what All compares the working tree with: `HEAD`, or the empty tree before the first commit. A rename
/// names its source too (`-M` pairs them), staged or not.
pub fn diff_args(change: &Change, scope: Scope, base: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-c", "diff.suppressBlankEmpty=false", "diff", "--no-ext-diff", "--no-textconv", "--no-color", "-U3"].map(String::from).to_vec();
    if scope == Scope::Untracked {
        args.extend(["--no-index", "--", "/dev/null"].map(String::from));
        args.push(change.path.clone());
        return args;
    }
    args.push("-M".into());
    match scope {
        Scope::Staged => args.push("--cached".into()),
        Scope::All => args.push(base.to_string()),
        Scope::Unstaged | Scope::Untracked => {}
    }
    args.push("--".into());
    args.extend(change.orig.iter().filter(|o| **o != change.path).cloned());
    args.push(change.path.clone());
    args
}

/// A file's entries in the Changes list: its Staged one and its Changes one, either or both. A file staged and
/// changed again offers each scope, and each scope takes its rename's source from the entry it belongs to: a staged
/// rename edited since (`RM`) has its source on the Staged entry only, a rename in the work tree (`.R`, after
/// `git add -N`) on the Changes one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entries {
    pub staged: Option<Change>,
    pub changes: Option<Change>,
}

impl Entries {
    pub fn both(&self) -> bool {
        self.staged.is_some() && self.changes.is_some()
    }

    /// The rename's source `scope` compares from: the index's rename for Staged, the work tree's for Unstaged,
    /// either for All (HEAD to the work tree).
    pub fn orig(&self, scope: Scope) -> Option<String> {
        let orig = |c: &Option<Change>| c.as_ref().and_then(|c| c.orig.clone());
        match scope {
            Scope::Staged => orig(&self.staged),
            Scope::Unstaged => orig(&self.changes),
            Scope::All => orig(&self.staged).or_else(|| orig(&self.changes)),
            Scope::Untracked => None,
        }
    }

    /// `change` as `scope` compares it: with that scope's rename source, when the list has the file's entries.
    pub fn change_for(&self, change: &Change, scope: Scope) -> Change {
        let mut change = change.clone();
        if self.staged.is_some() || self.changes.is_some() {
            change.orig = self.orig(scope);
        }
        change
    }
}

/// The versions a scope compares, as `git cat-file blob` specs; `None` for the working tree's file. The old one is
/// at a rename's source.
pub fn sides(change: &Change, scope: Scope, base: &str) -> (Option<String>, Option<String>) {
    let old = change.orig.as_deref().unwrap_or(&change.path);
    match scope {
        Scope::All => (Some(format!("{base}:{old}")), None),
        Scope::Staged => (Some(format!("HEAD:{old}")), Some(format!(":0:{}", change.path))),
        Scope::Unstaged => (Some(format!(":0:{old}")), None),
        Scope::Untracked => (None, None),
    }
}

/// Each change block of `lines`: a run of removed lines and the run of added lines right after it (either may be
/// empty), as index ranges.
pub fn change_blocks(lines: &[DiffLine]) -> Vec<(Range<usize>, Range<usize>)> {
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == LineKind::Context {
            i += 1;
            continue;
        }
        let removed = i;
        while i < lines.len() && lines[i].kind == LineKind::Removed {
            i += 1;
        }
        let added = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        blocks.push((removed..added, added..i));
    }
    blocks
}

/// The side-by-side rows of `lines` (indexes into them): context beside itself, each block's removed lines beside
/// its added ones in order, the shorter side padded.
pub fn split_rows(lines: &[DiffLine]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut rows = Vec::with_capacity(lines.len());
    let mut blocks = change_blocks(lines).into_iter().peekable();
    let mut i = 0;
    while i < lines.len() {
        match blocks.peek() {
            Some((removed, added)) if removed.start == i => {
                for k in 0..removed.len().max(added.len()) {
                    rows.push(((k < removed.len()).then(|| removed.start + k), (k < added.len()).then(|| added.start + k)));
                }
                i = added.end;
                blocks.next();
            }
            _ => {
                rows.push((Some(i), Some(i)));
                i += 1;
            }
        }
    }
    rows
}

/// A token's class: a word (letters, digits, `_`), a run of whitespace, or one other character on its own.
fn class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        1
    } else if c.is_whitespace() {
        2
    } else {
        3
    }
}

/// `line` cut into words, whitespace runs and single punctuation characters.
fn tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut last) = (0, 0u8);
    for (i, c) in line.char_indices() {
        let k = class(c);
        if i > start && (k != last || k == 3) {
            out.push(&line[start..i]);
            start = i;
        }
        last = k;
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}

/// The changed tokens' byte ranges, joined into one where only whitespace separates them.
fn ranges(tokens: &[&str], changed: &[bool]) -> Vec<Range<usize>> {
    let mut flags = changed.to_vec();
    for i in 1..tokens.len().saturating_sub(1) {
        if !flags[i] && flags[i - 1] && flags[i + 1] && tokens[i].chars().all(char::is_whitespace) {
            flags[i] = true;
        }
    }
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut at = 0;
    for (token, flag) in tokens.iter().zip(flags) {
        let end = at + token.len();
        if flag {
            match out.last_mut() {
                Some(last) if last.end == at => last.end = end,
                _ => out.push(at..end),
            }
        }
        at = end;
    }
    out
}

/// The changed words of a pair of lines: the old line's byte ranges, then the new one's.
pub type Marks = (Vec<Range<usize>>, Vec<Range<usize>>);

/// The changed words of `old` and `new` (byte ranges of each), or `None` when they share too little for emphasis
/// to help (under 40% of the visible text kept), are equal, are too long, or the deadline passed.
pub fn word_diff(old: &str, new: &str, deadline: Instant) -> Option<Marks> {
    if old == new || old.len() > MAX_EMPHASIS || new.len() > MAX_EMPHASIS || Instant::now() >= deadline {
        return None;
    }
    let (a, b) = (tokens(old), tokens(new));
    let ops = similar::capture_diff_slices_deadline(similar::Algorithm::Myers, &a, &b, Some(deadline));
    let (mut a_changed, mut b_changed) = (vec![false; a.len()], vec![false; b.len()]);
    for op in ops {
        let (tag, o, n) = op.as_tag_tuple();
        if tag != similar::DiffTag::Equal {
            a_changed[o].fill(true);
            b_changed[n].fill(true);
        }
    }
    let visible = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
    let kept: usize = a.iter().zip(&a_changed).filter(|(_, changed)| !**changed).map(|(t, _)| visible(t)).sum();
    let shown = visible(old).max(visible(new)).max(1);
    if kept * 10 < shown * 4 {
        return None;
    }
    Some((ranges(&a, &a_changed), ranges(&b, &b_changed)))
}

/// Marks the changed words of each removed line paired with an added one (`change_blocks`), within `budget`.
pub fn mark_words(diff: &mut Diff, budget: Duration) {
    let deadline = Instant::now() + budget;
    for hunk in &mut diff.hunks {
        for (removed, added) in change_blocks(&hunk.lines) {
            for k in 0..removed.len().min(added.len()) {
                let (o, n) = (removed.start + k, added.start + k);
                if let Some((a, b)) = word_diff(&hunk.lines[o].text, &hunk.lines[n].text, deadline) {
                    hunk.lines[o].emphasis = a;
                    hunk.lines[n].emphasis = b;
                }
            }
        }
    }
}

/// Unchanged lines of the new version that no hunk shows: the ones before hunk `i` (`gaps[i]`), and after the last
/// (`gaps[hunks.len()]`, known only with the new version's length). Lines `from..=to`, empty when `from > to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub from: u32,
    pub to: u32,
    /// The old version's number of line `from`: unchanged lines move together.
    pub old_from: u32,
}

impl Gap {
    pub fn len(&self) -> u32 {
        (self.to + 1).saturating_sub(self.from)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn gaps(hunks: &[Hunk], new_len: Option<u32>) -> Vec<Gap> {
    let mut out = Vec::with_capacity(hunks.len() + 1);
    let (mut new_at, mut old_at) = (1, 1);
    for h in hunks {
        out.push(Gap { from: new_at, to: h.new_first().saturating_sub(1), old_from: old_at });
        new_at = h.new_first() + h.new_count;
        old_at = h.old_first() + h.old_count;
    }
    out.push(Gap { from: new_at, to: new_len.unwrap_or(0), old_from: old_at });
    out
}

/// A gap clicked: twenty more of its lines show under the hunk above and twenty over the one below (all of it when
/// that leaves few hidden); the first gap has no hunk above, the last none below. `shown` is (top, bottom).
pub fn reveal(gap: usize, gaps: usize, len: u32, shown: (u32, u32)) -> (u32, u32) {
    const STEP: u32 = 20;
    let (top, bottom) = shown;
    let (first, last) = (gap == 0, gap + 1 == gaps);
    // the first and last gaps grow by twice the step on their one side
    let (up, down) = match (first, last) {
        (true, false) => (0, STEP * 2),
        (false, true) => (STEP * 2, 0),
        _ => (STEP, STEP),
    };
    if len.saturating_sub(top + bottom) <= up + down || (first && last) {
        let bottom = bottom.min(len);
        return (len - bottom, bottom);
    }
    (top + up, bottom + down)
}

/// A hunk as patch text, for the menu's "Copy hunk".
pub fn hunk_text(hunk: &Hunk) -> String {
    let mut out = hunk.header.clone();
    out.push('\n');
    for l in &hunk.lines {
        out.push(match l.kind {
            LineKind::Context => ' ',
            LineKind::Added => '+',
            LineKind::Removed => '-',
        });
        out.push_str(&l.text);
        out.push('\n');
        if l.no_newline {
            out.push_str("\\ No newline at end of file\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line's kind, old and new numbers, and text.
    type Numbered<'a> = (LineKind, Option<u32>, Option<u32>, &'a str);

    fn numbers(d: &Diff) -> Vec<Numbered<'_>> {
        d.hunks.iter().flat_map(|h| &h.lines).map(|l| (l.kind, l.old, l.new, l.text.as_str())).collect()
    }

    use LineKind::{Added as A, Context as C, Removed as R};

    #[test]
    fn a_new_file_starts_at_line_one() {
        let d = parse_unified(b"diff --git a/x b/x\nnew file mode 100644\n--- /dev/null\n+++ b/x\n@@ -0,0 +1,2 @@\n+one\n+two\n");
        assert_eq!(numbers(&d), [(A, None, Some(1), "one"), (A, None, Some(2), "two")]);
        assert_eq!(d.hunks[0].new_first(), 1);
        assert_eq!(d.hunks[0].old_first(), 1);
        assert_eq!(d.counts(), (2, 0));
    }

    #[test]
    fn a_missing_count_is_one() {
        let d = parse_unified(b"@@ -10 +11 @@ fn x()\n-a\n+b\n");
        assert_eq!((d.hunks[0].old_count, d.hunks[0].new_count), (1, 1));
        assert_eq!(d.hunks[0].header, "@@ -10 +11 @@ fn x()");
        assert_eq!(numbers(&d), [(R, Some(10), None, "a"), (A, None, Some(11), "b")]);
    }

    #[test]
    fn crlf_lines_lose_their_cr() {
        let d = parse_unified(b"@@ -1,2 +1,2 @@\r\n same\r\n-old\r\n+new\r\n");
        assert_eq!(numbers(&d), [(C, Some(1), Some(1), "same"), (R, Some(2), None, "old"), (A, None, Some(2), "new")]);
    }

    #[test]
    fn no_newline_marks_the_line_before() {
        let d = parse_unified(b"@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+a\n");
        let lines = &d.hunks[0].lines;
        assert!(lines[0].no_newline && !lines[1].no_newline);
        assert_eq!(lines.len(), 2);
        assert!(hunk_text(&d.hunks[0]).contains("-a\n\\ No newline at end of file\n+a\n"));
    }

    #[test]
    fn binary_files_have_no_hunks() {
        let d = parse_unified(b"diff --git a/i.png b/i.png\nindex 1..2 100644\nBinary files a/i.png and b/i.png differ\n");
        assert!(d.binary && d.hunks.is_empty());
        assert!(parse_unified(b"diff --git a/b b/b\nGIT binary patch\nliteral 3\n").binary);
    }

    #[test]
    fn a_bare_empty_line_does_not_count() {
        let d = parse_unified(b"@@ -1,3 +1,3 @@\n a\n\n \n-b\n+c\n");
        assert_eq!(numbers(&d), [(C, Some(1), Some(1), "a"), (C, Some(2), Some(2), ""), (R, Some(3), None, "b"), (A, None, Some(3), "c")]);
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert_eq!(parse_unified(b""), Diff::default());
        assert_eq!(parse_unified(b"\n"), Diff::default());
        assert_eq!(Diff::default().first_change(), None);
    }

    #[test]
    fn two_hunks_keep_their_own_numbers() {
        let out = b"@@ -1,3 +1,3 @@ head\n a\n-b\n+B\n c\n@@ -20,2 +20,3 @@ fn tail()\n x\n+y\n z\n";
        let d = parse_unified(out);
        assert_eq!(d.hunks.len(), 2);
        assert_eq!(d.hunks[1].header, "@@ -20,2 +20,3 @@ fn tail()");
        assert_eq!(numbers(&d)[4..], [(C, Some(20), Some(20), "x"), (A, None, Some(21), "y"), (C, Some(21), Some(22), "z")]);
        assert_eq!(d.first_change(), Some(2));
        let g = gaps(&d.hunks, Some(30));
        assert_eq!(g, [Gap { from: 1, to: 0, old_from: 1 }, Gap { from: 4, to: 19, old_from: 4 }, Gap { from: 23, to: 30, old_from: 22 }]);
        assert!(g[0].is_empty() && g[1].len() == 16);
        assert!(gaps(&d.hunks, None)[2].is_empty());
    }

    #[test]
    fn rename_headers_are_skipped_and_a_pure_removal_points_where_it_was() {
        let out = b"diff --git a/old name.rs b/new name.rs\nsimilarity index 90%\nrename from old name.rs\nrename to new name.rs\nindex 1..2 100644\n--- a/old name.rs\n+++ b/new name.rs\n@@ -5,3 +5,2 @@\n keep\n-gone\n keep\n";
        let d = parse_unified(out);
        assert_eq!(numbers(&d), [(C, Some(5), Some(5), "keep"), (R, Some(6), None, "gone"), (C, Some(7), Some(6), "keep")]);
        assert_eq!(d.first_change(), Some(6));
        // a removal with no new lines: git names the line before
        let d = parse_unified(b"@@ -3,1 +2,0 @@\n-x\n");
        assert_eq!((d.hunks[0].new_first(), d.first_change()), (3, Some(3)));
        // the rest of a hunk's header block: `---`/`+++` of a second file after a hunk's counts are spent
        let two = b"@@ -1 +1 @@\n-a\n+b\ndiff --git a/c b/c\n--- a/c\n+++ b/c\n@@ -1 +1 @@\n-c\n+d\n";
        assert_eq!(numbers(&parse_unified(two)).len(), 4);
        assert!(parse_unified(b"diff --cc f\n@@@ -1,1 -1,1 +1,5 @@@\n++<<<<<<< ours\n").combined);
    }

    #[test]
    fn removed_runs_pair_with_the_added_runs_after_them() {
        let d = parse_unified(b"@@ -1,4 +1,5 @@\n-a\n-b\n+A\n c\n+d\n-e\n+E\n+F\n");
        let lines = &d.hunks[0].lines;
        assert_eq!(change_blocks(lines), [(0..2, 2..3), (4..4, 4..5), (5..6, 6..8)]);
        assert_eq!(
            split_rows(lines),
            [(Some(0), Some(2)), (Some(1), None), (Some(3), Some(3)), (None, Some(4)), (Some(5), Some(6)), (None, Some(7))]
        );
    }

    #[test]
    fn changed_words_are_marked_unless_too_little_is_kept() {
        let far = Instant::now() + Duration::from_secs(5);
        let (a, b) = word_diff("let total = price * count;", "let total = price * amount;", far).unwrap();
        assert_eq!((&"let total = price * count;"[a[0].clone()], &"let total = price * amount;"[b[0].clone()]), ("count", "amount"));
        assert_eq!((a.len(), b.len()), (1, 1));
        // two changed words with a space between are one run
        let (_, b) = word_diff("a b c d e f", "a X Y d e f", far).unwrap();
        assert_eq!((b.len(), b.first()), (1, Some(&(2..5))));
        // under 40% kept: whole lines read better
        assert_eq!(word_diff("fn alpha(beta: u32)", "struct Gamma { delta }", far), None);
        assert_eq!(word_diff("same", "same", far), None);
        assert_eq!(word_diff(&"x".repeat(MAX_EMPHASIS + 1), "y", far), None);
        assert_eq!(word_diff("a b", "a c", Instant::now() - Duration::from_secs(1)), None, "past the deadline");

        let mut d = parse_unified(b"@@ -1,2 +1,2 @@\n-let x = 1;\n-gone entirely here\n+let x = 2;\n");
        mark_words(&mut d, EMPHASIS_BUDGET);
        let lines = &d.hunks[0].lines;
        assert_eq!(lines[0].emphasis, lines[2].emphasis);
        assert_eq!((lines[0].emphasis.len(), lines[0].emphasis.first()), (1, Some(&(8..9))));
        assert!(lines[1].emphasis.is_empty(), "no partner");
    }

    fn change(group: Group, path: &str, orig: Option<&str>) -> Change {
        Change { group, path: path.into(), orig: orig.map(Into::into), code: 'M', xy: ['.', 'M'], submodule: false, moved_to: None }
    }

    #[test]
    fn each_group_and_scope_has_its_args() {
        let head = "-c diff.suppressBlankEmpty=false diff --no-ext-diff --no-textconv --no-color -U3";
        let joined = |c: &Change, s: Scope| diff_args(c, s, "HEAD").join(" ");
        let staged = change(Group::Staged, "new name.rs", Some("old name.rs"));
        assert_eq!(joined(&staged, Scope::of(Group::Staged)), format!("{head} -M --cached -- old name.rs new name.rs"));
        let changed = change(Group::Changes, "a.rs", None);
        assert_eq!(joined(&changed, Scope::of(Group::Changes)), format!("{head} -M -- a.rs"));
        assert_eq!(joined(&changed, Scope::All), format!("{head} -M HEAD -- a.rs"));
        assert_eq!(diff_args(&changed, Scope::All, EMPTY_TREE).join(" "), format!("{head} -M {EMPTY_TREE} -- a.rs"));
        let renamed = change(Group::Changes, "b.rs", Some("a.rs"));
        assert_eq!(joined(&renamed, Scope::Unstaged), format!("{head} -M -- a.rs b.rs"), "an unstaged rename names its source");
        let new = change(Group::Untracked, "-n.rs", None);
        assert_eq!(joined(&new, Scope::of(Group::Untracked)), format!("{head} --no-index -- /dev/null -n.rs"));
        assert_eq!(Scope::of(Group::Conflicts), Scope::Unstaged);
        // an `RM` file's Changes entry takes the Staged entry's source for Staged and All, none for Unstaged
        let entries = Entries { staged: Some(staged.clone()), changes: Some(change(Group::Changes, "new name.rs", None)) };
        assert!(entries.both());
        let tab = entries.changes.clone().unwrap();
        assert_eq!(entries.change_for(&tab, Scope::Staged).orig.as_deref(), Some("old name.rs"));
        assert_eq!(entries.change_for(&tab, Scope::All).orig.as_deref(), Some("old name.rs"));
        assert_eq!(entries.change_for(&tab, Scope::Unstaged).orig, None);
        // a `.R` file: the work tree's rename
        let worktree = Entries { staged: None, changes: Some(renamed.clone()) };
        assert_eq!(worktree.change_for(&renamed, Scope::Unstaged).orig.as_deref(), Some("a.rs"));
        assert_eq!(Entries::default().change_for(&renamed, Scope::Unstaged), renamed, "no list: the change as it came");
        assert_eq!(sides(&staged, Scope::Staged, "HEAD"), (Some("HEAD:old name.rs".into()), Some(":0:new name.rs".into())));
        assert_eq!(sides(&changed, Scope::Unstaged, "HEAD"), (Some(":0:a.rs".into()), None));
    }

    #[test]
    fn a_diff_without_hunks_says_what_it_is() {
        let empty = parse_unified(b"diff --git a/e b/e\nnew file mode 100644\nindex 0000000..e69de29\n");
        assert!(empty.new_file && empty.hunks.is_empty());
        assert_eq!(empty.without_hunks().0, "Empty file");
        let moved = parse_unified(b"diff --git a/a b/b\nsimilarity index 100%\nrename from a\nrename to b\n");
        assert_eq!((moved.similarity, moved.rename_from.as_deref(), moved.rename_to.as_deref()), (Some(100), Some("a"), Some("b")));
        assert_eq!(moved.without_hunks(), ("Renamed", "From a to b, its text unchanged.".to_string()));
        let mode = parse_unified(b"diff --git a/s b/s\nold mode 100644\nnew mode 100755\n");
        assert_eq!(mode.without_hunks(), ("Mode changed", "Its mode changed from 100644 to 100755.".to_string()));
        assert_eq!(parse_unified(b"diff --git a/g b/g\ndeleted file mode 100644\n").without_hunks().0, "Empty file deleted");
        assert_eq!(Diff::default().without_hunks().0, "No changes");
    }

    #[test]
    fn a_gap_opens_twenty_lines_a_side_then_all() {
        assert_eq!(reveal(1, 3, 100, (0, 0)), (20, 20));
        assert_eq!(reveal(1, 3, 100, (20, 20)), (40, 40));
        assert_eq!(reveal(1, 3, 100, (40, 40)), (60, 40), "few left: all of it");
        assert_eq!(reveal(0, 3, 100, (0, 0)), (0, 40), "the first gap grows over the first hunk only");
        assert_eq!(reveal(2, 3, 100, (0, 0)), (40, 0), "the last under the last hunk only");
        assert_eq!(reveal(1, 3, 30, (0, 0)), (30, 0));
    }

    /// git's own output for the cases above, from a temp repository: what the parser meets for real.
    #[test]
    fn real_git_output_parses() {
        let has_git = std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success());
        if !has_git {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
                .args(args)
                .current_dir(dir.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("f.txt"), "one\n\nthree\nfour\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "first"]);
        std::fs::write(dir.path().join("f.txt"), "one\n\nTHREE\nfour").unwrap();
        let c = change(Group::Changes, "f.txt", None);
        let out = git(&diff_args(&c, Scope::Unstaged, "HEAD").iter().map(String::as_str).collect::<Vec<_>>());
        let d = parse_unified(&out.stdout);
        assert_eq!(
            numbers(&d),
            [(C, Some(1), Some(1), "one"), (C, Some(2), Some(2), ""), (R, Some(3), None, "three"), (R, Some(4), None, "four"), (A, None, Some(3), "THREE"), (A, None, Some(4), "four")]
        );
        assert!(d.hunks[0].lines[5].no_newline);
        std::fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        let out = git(&diff_args(&change(Group::Untracked, "new.txt", None), Scope::Untracked, "HEAD").iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(out.status.code(), Some(1), "a difference");
        assert_eq!(numbers(&parse_unified(&out.stdout)), [(A, None, Some(1), "x")]);
    }
}
