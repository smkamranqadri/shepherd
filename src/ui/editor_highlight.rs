//! The editor's highlighting: gpui-base's `InputHighlighter` seam over the viewer's syntect parser
//! (`files::highlight`), so an edited file reads in the colours its read-only view had.
//!
//! The parse goes top to bottom and keeps each line's spans, and the parser's state every `CHECKPOINT` lines. An edit
//! throws away what it touched and parses again from the checkpoint at or above it; once the parse below the edit
//! reaches a checkpoint of the old parse with the same state, the rest of the old parse holds and is kept (typing in
//! a function re-parses tens of lines, not the file). Lines below the edit show their old spans until then.
//!
//! What a frame draws is parsed in the frame (a first look at it `SYNC_AHEAD` lines further); the rest is parsed on the background
//! executor in runs of `RUN_LINES`, each checked against the edits since (a run started before one is dropped), and
//! the editor is drawn again after each.
use crate::files::highlight::{self, Span, Token};
use crate::theme::Theme;
use gpui::{AppContext, Context, HighlightStyle, Hsla, SharedString, Task, Window};
use gpui_base::input::{EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputEditorStyle, InputHighlighter, Rope, RopeExt};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference};

/// The parser's state is kept every this many lines: an edit parses again from at most this far above it.
pub const CHECKPOINT: usize = 64;
/// A frame parses what it draws and this many lines further, so a scroll does not wait for the background.
const SYNC_AHEAD: usize = 2000;
/// The lines a background run parses before it hands them over (and the editor is drawn again).
const RUN_LINES: usize = 4000;
/// A background run waits this long first: the frame of a keystroke parses what it shows before the run starts.
const RUN_DELAY: Duration = Duration::from_millis(30);

/// The parser's state before line `line`.
#[derive(Clone)]
pub struct Checkpoint {
    pub line: usize,
    state: ParseState,
    stack: ScopeStack,
}

/// A stretch of lines parsed from `start`: each line's spans, the checkpoints passed (after `start`), and the state
/// after the last line. `converged`: it stopped at an old checkpoint whose state it matched.
pub struct Run {
    start: usize,
    generation: u64,
    lines: Vec<Box<[Span]>>,
    checkpoints: Vec<Checkpoint>,
    end: (ParseState, ScopeStack),
    converged: bool,
}

/// What a background run starts from, taken on the main thread.
pub struct Job {
    syntax: &'static SyntaxReference,
    rope: Rope,
    start: usize,
    generation: u64,
    state: (ParseState, ScopeStack),
    /// The old parse's checkpoints the run may meet, to stop on one it matches.
    old: Vec<Checkpoint>,
    converge_from: usize,
}

impl Job {
    pub fn run(self) -> Run {
        parse_run(self.syntax, &self.rope, self.start, self.generation, self.state, RUN_LINES, &self.old, self.converge_from)
    }
}

/// Line `ix` of `rope`, without its line end, into `buf`.
fn line_into(rope: &Rope, ix: usize, buf: &mut String) {
    buf.clear();
    for chunk in rope.slice_line(ix).chunks() {
        buf.push_str(chunk);
    }
    if buf.ends_with('\r') {
        buf.pop();
    }
}

/// Parses up to `max` lines of `rope` from `start` on from `state`, stopping early at a checkpoint of `old` (its
/// line at or past `converge_from`) whose state it matches. Checkpoints are made every `CHECKPOINT` lines and where
/// `old` has one, so a later edit's parse can stop on them.
#[allow(clippy::too_many_arguments)]
fn parse_run(
    syntax: &'static SyntaxReference,
    rope: &Rope,
    start: usize,
    generation: u64,
    state: (ParseState, ScopeStack),
    max: usize,
    old: &[Checkpoint],
    converge_from: usize,
) -> Run {
    let total = rope.lines_len();
    let (mut state, mut stack) = state;
    let (mut lines, mut checkpoints, mut converged) = (Vec::new(), Vec::new(), false);
    let (mut text, mut buf, mut spans) = (String::new(), String::new(), Vec::new());
    let mut ix = start;
    while ix < total && ix - start < max {
        if ix > start {
            let old_here = old.binary_search_by_key(&ix, |c| c.line).ok().map(|at| &old[at]);
            if let Some(cp) = old_here.filter(|_| ix >= converge_from)
                && cp.state == state
                && cp.stack == stack
            {
                converged = true;
                break;
            }
            if ix.is_multiple_of(CHECKPOINT) || old_here.is_some() {
                checkpoints.push(Checkpoint { line: ix, state: state.clone(), stack: stack.clone() });
            }
        }
        line_into(rope, ix, &mut text);
        spans.clear();
        if !highlight::parse_line(&mut state, &mut stack, &text, &mut buf, &mut spans) {
            // syntect gave up on it: plain, and the rest goes on from a fresh state rather than a broken one
            spans.clear();
            (state, stack) = highlight::start(syntax);
        }
        lines.push(spans.as_slice().into());
        ix += 1;
    }
    Run { start, generation, lines, checkpoints, end: (state, stack), converged }
}

/// The highlighter's state: the text's lines' spans and the checkpoints, as far as parsed.
pub struct Hl {
    syntax: &'static SyntaxReference,
    rope: Rope,
    /// Each line's spans (bytes of the line, in order). Lines from `valid` on are from before the last edits (shown
    /// until parsed again) or empty.
    lines: Vec<Box<[Span]>>,
    /// Lines `0..valid` are parsed for the text as it is.
    valid: usize,
    /// The state before line `valid`.
    head: (ParseState, ScopeStack),
    /// Sorted by line. Those at or before `valid` are right; those after are the old parse's, kept to stop on.
    checkpoints: Vec<Checkpoint>,
    /// Lines `converge_from..known` hold an old parse that is right if the state at a checkpoint in them matches;
    /// `known_head` is the state after line `known`.
    known: usize,
    known_head: Option<(ParseState, ScopeStack)>,
    converge_from: usize,
    /// Bumped by each edit: a run that started before one is dropped.
    generation: u64,
    task: Option<Task<()>>,
}

impl Hl {
    pub fn new(syntax: &'static SyntaxReference, rope: Rope) -> Hl {
        let head = highlight::start(syntax);
        let checkpoints = vec![Checkpoint { line: 0, state: head.0.clone(), stack: head.1.clone() }];
        Hl { syntax, rope, lines: Vec::new(), valid: 0, head, checkpoints, known: 0, known_head: None, converge_from: 0, generation: 0, task: None }
    }

    pub fn valid(&self) -> usize {
        self.valid
    }

    pub fn done(&self) -> bool {
        self.valid >= self.rope.lines_len()
    }

    /// The text changed by `edit` (now `rope`): what it touched is parsed again from the checkpoint at or above it.
    pub fn edit(&mut self, edit: &InputEdit, rope: Rope) {
        self.generation += 1;
        self.rope = rope;
        let (r, o, n) = (edit.start_position.row, edit.old_end_position.row, edit.new_end_position.row);
        let delta = n as isize - o as isize;
        let shift = |line: usize| (line as isize + delta).max(0) as usize;
        let before = self.converge_from;
        if self.valid > o + 1 {
            // the old parse below the edit may still hold, if the parse after it comes back to the same state
            (self.known, self.known_head) = (shift(self.valid), Some(self.head.clone()));
            self.converge_from = n + 1;
        } else if self.known_head.is_some() && self.known > o + 1 {
            // still parsing towards an old parse kept from an edit before: it must also pass that edit
            self.known = shift(self.known);
            self.converge_from = (n + 1).max(if before > o { shift(before) } else { 0 });
        } else {
            (self.known, self.known_head, self.converge_from) = (0, None, n + 1);
        }
        // checkpoints after the edit's first line had its old lines above them: they go, those below move with them
        self.checkpoints.retain(|c| c.line <= r || c.line > o);
        for c in self.checkpoints.iter_mut().filter(|c| c.line > o) {
            c.line = shift(c.line);
        }
        if self.lines.len() > r {
            let end = (o + 1).min(self.lines.len());
            self.lines.splice(r..end, std::iter::repeat_with(Box::default).take(n - r + 1));
        }
        // the lines parsed reach into the edit: parsed again from the checkpoint at or above its first line
        if self.valid > r {
            let cp = self.checkpoints.iter().rev().find(|c| c.line <= r).unwrap_or(&self.checkpoints[0]);
            self.valid = cp.line;
            self.head = (cp.state.clone(), cp.stack.clone());
        }
        if self.known <= self.converge_from {
            (self.known, self.known_head) = (0, None);
        }
    }

    /// The whole text is new: everything is parsed again.
    pub fn reset(&mut self, rope: Rope) {
        let task = self.task.take();
        *self = Hl::new(self.syntax, rope);
        self.task = task;
        self.generation += 1;
    }

    /// The old parse's checkpoints a run from `valid` over `max` lines may meet.
    fn old_checkpoints(&self, max: usize) -> &[Checkpoint] {
        if self.known_head.is_none() {
            return &[];
        }
        let from = self.checkpoints.partition_point(|c| c.line <= self.valid);
        let to = self.checkpoints.partition_point(|c| c.line < self.known.min(self.valid + max + 1));
        &self.checkpoints[from..to.max(from)]
    }

    /// Parses on the main thread until line `until` is parsed (a frame shows it).
    pub fn parse_until(&mut self, until: usize) {
        let syntax = self.syntax;
        let until = until.min(self.rope.lines_len());
        while self.valid < until {
            let max = until - self.valid;
            let run = parse_run(syntax, &self.rope, self.valid, self.generation, self.head.clone(), max, self.old_checkpoints(max), self.converge_from);
            if !self.apply(run) {
                return;
            }
        }
    }

    /// The next background run's start, if there is more to parse.
    pub fn job(&self) -> Option<Job> {
        if self.done() {
            return None;
        }
        Some(Job {
            syntax: self.syntax,
            rope: self.rope.clone(),
            start: self.valid,
            generation: self.generation,
            state: self.head.clone(),
            old: self.old_checkpoints(RUN_LINES).to_vec(),
            converge_from: self.converge_from,
        })
    }

    /// Takes in a run's lines, if it started where the parse is now and no edit came since. True when it did.
    pub fn apply(&mut self, run: Run) -> bool {
        if run.generation != self.generation || run.start != self.valid {
            return false;
        }
        let (start, end) = (run.start, run.start + run.lines.len());
        let mut fresh = run.lines.into_iter();
        for (slot, line) in self.lines.iter_mut().skip(start).zip(fresh.by_ref()) {
            *slot = line;
        }
        self.lines.extend(fresh);
        // the run's checkpoints replace what was kept after its start; one it stopped on (same state) stays
        let from = self.checkpoints.partition_point(|c| c.line <= start);
        let to = self.checkpoints.partition_point(|c| if run.converged { c.line < end } else { c.line <= end });
        let mut checkpoints = run.checkpoints;
        if !run.converged && end > start && end.is_multiple_of(CHECKPOINT) {
            checkpoints.push(Checkpoint { line: end, state: run.end.0.clone(), stack: run.end.1.clone() });
        }
        self.checkpoints.splice(from..to, checkpoints);
        if run.converged
            && let Some(head) = self.known_head.take()
        {
            // the rest of the old parse holds
            (self.valid, self.head, self.known) = (self.known, head, 0);
            return true;
        }
        if end == start {
            // nothing parsed: the text ended
            return false;
        }
        (self.valid, self.head) = (end, run.end);
        if self.valid >= self.known {
            (self.known, self.known_head) = (0, None);
        }
        true
    }

    /// Style runs for `range` (whole lines of the text, as the editor asks): ordered, not overlapping, covering it,
    /// unstyled text as `HighlightStyle::default()`. Lines not parsed yet draw plain (or with their old spans).
    pub fn styles(&self, range: &Range<usize>, resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut out = Vec::new();
        if range.is_empty() {
            return out;
        }
        let rope = &self.rope;
        let end = range.end.min(rope.len());
        let first = rope.offset_to_point(range.start).row;
        let last = rope.offset_to_point(end.saturating_sub(1).max(range.start)).row;
        let mut at = range.start;
        for row in first..=last {
            let Some(spans) = self.lines.get(row) else { break };
            let line_start = rope.line_start_offset(row);
            let line_len = rope.line_len(row);
            for span in spans.iter() {
                let s = (line_start + (span.start as usize).min(line_len)).max(at);
                let e = (line_start + (span.end as usize).min(line_len)).min(end);
                if e <= s {
                    continue;
                }
                let Some(style) = token_name(span.token).and_then(|n| resolver.style(n)) else { continue };
                if s > at {
                    out.push((at..s, HighlightStyle::default()));
                }
                out.push((s..e, style));
                at = e;
            }
        }
        if at < range.end {
            out.push((at..range.end, HighlightStyle::default()));
        }
        out
    }
}

/// The name a token's colour is asked for by (`TokenStyles`).
pub fn token_name(token: Token) -> Option<&'static str> {
    Some(match token {
        Token::Plain => return None,
        Token::Comment => "comment",
        Token::String => "string",
        Token::Keyword => "keyword",
        Token::Number => "number",
        Token::Function => "function",
        Token::Type => "type",
        Token::Constant => "constant",
        Token::Punct => "punctuation",
        Token::Tag => "tag",
        Token::Attr => "attribute",
    })
}

const TOKENS: [Token; 10] = [Token::Comment, Token::String, Token::Keyword, Token::Number, Token::Function, Token::Type, Token::Constant, Token::Punct, Token::Tag, Token::Attr];

/// The viewer's token colours (`viewer::token_color`), by name, for gpui-base's painter.
pub struct TokenStyles {
    colors: Vec<(&'static str, Hsla)>,
}

impl TokenStyles {
    pub fn new(theme: &Theme) -> TokenStyles {
        TokenStyles { colors: TOKENS.iter().filter_map(|t| Some((token_name(*t)?, crate::ui::viewer::token_color(*t, theme)))).collect() }
    }
}

impl HighlightStyleResolver for TokenStyles {
    fn style(&self, name: &str) -> Option<HighlightStyle> {
        let (_, color) = self.colors.iter().find(|(n, _)| *n == name)?;
        Some(HighlightStyle { color: Some(*color), ..Default::default() })
    }
}

/// The editor's colours from the theme: the terminal's palette, as the viewer and the panes beside it.
pub fn editor_style(theme: &Theme) -> InputEditorStyle {
    InputEditorStyle {
        foreground: theme.terminal.foreground,
        muted_foreground: theme.text_faint,
        background: theme.terminal.background,
        border: theme.border,
        selection: theme.terminal.selection,
        caret: theme.cursor,
        highlight_styles: Arc::new(TokenStyles::new(theme)),
        editor_active_line: Some(theme.wash(0.04)),
        editor_gutter_background: Some(theme.terminal.background),
        ..Default::default()
    }
}

/// The `InputHighlighter` the editor drives: `Hl`, shared with its background runs.
pub struct SyntectHighlighter {
    inner: Rc<RefCell<Hl>>,
}

impl SyntectHighlighter {
    /// For `syntax` (resolved, and its contexts loaded, off the main thread: `files::highlight::syntax_for`).
    pub fn new(syntax: &'static SyntaxReference) -> SyntectHighlighter {
        SyntectHighlighter { inner: Rc::new(RefCell::new(Hl::new(syntax, Rope::new()))) }
    }

    /// Parses what is left on the background executor, a run at a time, drawing the editor again after each. The
    /// task is the highlighter's: a new one (the next edit) replaces it, and it ends with the highlighter.
    fn run_rest(&self, cx: &mut Context<EditorState>) {
        if self.inner.borrow().done() {
            self.inner.borrow_mut().task = None;
            return;
        }
        let weak: Weak<RefCell<Hl>> = Rc::downgrade(&self.inner);
        let task = cx.spawn(async move |editor, cx| {
            cx.background_executor().timer(RUN_DELAY).await;
            loop {
                let Some(job) = weak.upgrade().and_then(|hl| hl.borrow().job()) else { return };
                let run = cx.background_spawn(async move { job.run() }).await;
                let Some(hl) = weak.upgrade() else { return };
                // a frame's own parse moved past its start meanwhile: the next run starts from there
                let applied = hl.borrow_mut().apply(run);
                if applied && editor.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        });
        self.inner.borrow_mut().task = Some(task);
    }
}

impl InputHighlighter for SyntectHighlighter {
    fn language(&self) -> SharedString {
        SharedString::from(self.inner.borrow().syntax.name.clone())
    }

    fn update(&mut self, edit: Option<InputEdit>, text: &Rope, _folding: bool, _: &mut Window, cx: &mut Context<EditorState>) {
        {
            let mut hl = self.inner.borrow_mut();
            match &edit {
                // the first call (a new highlighter for a new text) has nothing parsed to keep
                Some(edit) if !(hl.lines.is_empty() && hl.valid == 0) => hl.edit(edit, text.clone()),
                _ => hl.reset(text.clone()),
            }
        }
        self.run_rest(cx);
    }

    fn styles(&self, range: &Range<usize>, resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut hl = self.inner.borrow_mut();
        if !hl.done() {
            let last = hl.rope.offset_to_point(range.end.saturating_sub(1)).row;
            if hl.valid <= last {
                // after an edit the lines below still have their old spans to show: a keystroke parses only what
                // is on screen (and the background the rest); a first look parses a little further for the scroll
                let ahead = if hl.lines.len() > hl.valid { 0 } else { SYNC_AHEAD };
                hl.parse_until(last + 1 + ahead);
            }
        }
        hl.styles(range, resolver)
    }

    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_base::input::Point;
    use std::path::Path;

    struct Named;

    impl HighlightStyleResolver for Named {
        fn style(&self, name: &str) -> Option<HighlightStyle> {
            // a distinct weight per name, to tell the runs apart
            let weight = gpui::FontWeight(100. + name.len() as f32);
            Some(HighlightStyle { font_weight: Some(weight), ..Default::default() })
        }
    }

    fn rust() -> &'static SyntaxReference {
        highlight::syntax_for(Path::new("a.rs"), "").expect("rust")
    }

    fn text(lines: usize) -> String {
        (0..lines).map(|i| format!("fn f{i}() {{ let s = \"x{i}\"; // c\n")).collect()
    }

    /// The edit replacing `old` (byte range of `before`) with `new`.
    fn edit_of(before: &str, old: Range<usize>, new: &str) -> (InputEdit, String) {
        let after = format!("{}{new}{}", &before[..old.start], &before[old.end..]);
        let (a, b) = (Rope::from_str(before), Rope::from_str(&after));
        let point = |r: &Rope, at: usize| {
            let p = r.offset_to_point(at);
            Point::new(p.row, p.column)
        };
        let edit = InputEdit {
            start_byte: old.start,
            old_end_byte: old.end,
            new_end_byte: old.start + new.len(),
            start_position: point(&a, old.start),
            old_end_position: point(&a, old.end),
            new_end_position: point(&b, old.start + new.len()),
        };
        (edit, after)
    }

    fn assert_covers(styles: &[(Range<usize>, HighlightStyle)], range: &Range<usize>) {
        let mut at = range.start;
        for (r, _) in styles {
            assert_eq!(r.start, at, "in order, no gap or overlap: {styles:?}");
            assert!(r.end > r.start, "no empty run");
            at = r.end;
        }
        assert_eq!(at, range.end, "the whole range");
    }

    #[test]
    fn styles_are_ordered_and_cover_the_range() {
        let source = text(300);
        let mut hl = Hl::new(rust(), Rope::from_str(&source));
        let rope = Rope::from_str(&source);
        for rows in [0..1, 3..40, 250..300, 299..300] {
            let range = rope.line_start_offset(rows.start)..rope.line_start_offset(rows.end);
            hl.parse_until(rows.end);
            let styles = hl.styles(&range, &Named);
            assert_covers(&styles, &range);
            assert!(styles.iter().any(|(_, s)| s.font_weight.is_some()), "some of it is highlighted");
        }
        // a range not parsed yet draws plain, still covered
        let fresh = Hl::new(rust(), Rope::from_str(&source));
        let range = 0..rope.line_start_offset(5);
        assert_covers(&fresh.styles(&range, &Named), &range);
        // a range past the text's end too
        let all = 0..source.len() + 3;
        assert_covers(&hl.styles(&all, &Named), &all);
    }

    #[test]
    fn an_edit_parses_again_from_the_checkpoint_above_it() {
        let source = text(300);
        let mut hl = Hl::new(rust(), Rope::from_str(&source));
        hl.parse_until(300);
        assert_eq!(hl.valid(), 300);
        assert!(hl.checkpoints.iter().any(|c| c.line == 128));
        // typing on line 130: from the checkpoint at 128
        let at = Rope::from_str(&source).line_start_offset(130) + 3;
        let (edit, after) = edit_of(&source, at..at, "x");
        hl.edit(&edit, Rope::from_str(&after));
        assert_eq!(hl.valid(), 128);
        // the parse meets the old checkpoint at 192 in the same state: the old parse holds past it
        hl.parse_until(200);
        assert_eq!(hl.valid(), 300, "converged at 192, not parsed to the end");
        // typing on line 10: from line 0
        let at = Rope::from_str(&after).line_start_offset(10) + 3;
        let (edit, after2) = edit_of(&after, at..at, "y");
        hl.edit(&edit, Rope::from_str(&after2));
        assert_eq!(hl.valid(), 0);
        // a new line on line 70 moves the checkpoints below; the parse still stops on them
        let at = Rope::from_str(&after2).line_start_offset(70);
        let (edit, after3) = edit_of(&after2, at..at, "let z = 1;\n");
        hl.parse_until(300);
        hl.edit(&edit, Rope::from_str(&after3));
        assert_eq!(hl.valid(), 64);
        assert!(hl.checkpoints.iter().any(|c| c.line == 129), "128 moved down a line");
        hl.parse_until(130);
        assert_eq!(hl.valid(), 301, "converged on the checkpoint moved to 129");
        let range = 0..after3.len();
        assert_covers(&hl.styles(&range, &Named), &range);
    }

    #[test]
    fn opening_a_comment_reparses_what_follows() {
        let source = text(200);
        let mut hl = Hl::new(rust(), Rope::from_str(&source));
        hl.parse_until(200);
        let (edit, after) = edit_of(&source, 0..0, "/*");
        hl.edit(&edit, Rope::from_str(&after));
        hl.parse_until(10);
        assert_eq!(hl.valid(), 10, "no checkpoint matches inside the comment");
        let line = Rope::from_str(&after).line_start_offset(5);
        let styles = hl.styles(&(line..line + 3), &Named);
        let comment = gpui::FontWeight(100. + "comment".len() as f32);
        assert!(styles.iter().all(|(_, s)| s.font_weight == Some(comment)), "{styles:?}");
        // the background's way: jobs run and applied
        let stale = hl.job().expect("more to parse");
        while let Some(job) = hl.job() {
            assert!(hl.apply(job.run()));
        }
        assert!(hl.done());
        assert!(!hl.apply(stale.run()), "a run from where the parse no longer is");
        // a run from before an edit is dropped
        let (edit, after2) = edit_of(&after, 0..2, "");
        hl.edit(&edit, Rope::from_str(&after2));
        let before_edit = hl.job().expect("more to parse");
        let (edit, after3) = edit_of(&after2, 0..0, " ");
        hl.edit(&edit, Rope::from_str(&after3));
        assert!(!hl.apply(before_edit.run()));
    }
}
