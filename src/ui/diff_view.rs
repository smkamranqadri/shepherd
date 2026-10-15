//! A diff tab's view: one change's diff (`files::diff`), read-only, in the pane grid's place like the file viewer.
//! Unified (one column, old and new numbers side by side in the gutter) or split (old left, new right), as the
//! settings keep it; changed lines on a 12% wash of the theme's success or danger colour and their changed words on a
//! stronger one. Unchanged lines between hunks fold into a row that shows twenty more a click, read from the new
//! version when it still matches the diff. Each side is highlighted like the viewer (`files::highlight`, its budget),
//! the new version for context and added lines, the old one for removed lines.
//!
//! git runs on the runtime through `files::runner` (30 s, 8 MiB), one read at a time, each tagged with a generation
//! so a read for another change or scope is dropped when it lands. The Shell asks again on every Changes poll while
//! the tab shows (no sooner than five times the last read took): the same output as last time does nothing; a new one is parsed, marked and highlighted off the main
//! thread. A hidden tab lets go of a large diff and reads it again when shown.
//!
//! No text selection (the viewer's needs every row's layout and its own hit-testing): a right-click on a row copies
//! its line or its hunk.
use crate::files::content::{self, Text};
use crate::files::diff::{self, Diff, DiffLine, Entries, Gap, LineKind, Scope};
use crate::files::highlight::{self, Highlights, Span, Token};
use crate::files::runner::{self, Host, Limits, RunError};
use crate::files::status::{Change, Group};
use crate::shell::widgets::{menu_card, menu_choice, tip, titlebar_button};
use crate::theme::Theme;
use crate::ui::viewer::{Drawn, columns, token_color};
use gpui::{
    AnyElement, App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, Hsla, InteractiveElement, IntoElement, KeyDownEvent, ListHorizontalSizingBehavior,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement, Styled, StyledText, TextRun, UniformListScrollHandle,
    Window, div, point, prelude::*, px,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

/// `git rev-parse` and the like: a line.
const QUICK: Limits = Limits { timeout: Duration::from_secs(5), max_bytes: 64 << 10 };
/// The diff itself.
const DIFF: Limits = Limits { timeout: diff::TIMEOUT, max_bytes: diff::MAX_BYTES };
/// A version read whole for highlighting and unchanged lines: the viewer's text cap.
const SIDE: Limits = Limits { timeout: diff::TIMEOUT, max_bytes: content::MAX_TEXT as usize };
/// How long reading the working tree's file may take: a hung mount never answers.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The lines kept worked out for drawing; past this many the cache starts over.
const MAX_DRAWN: usize = 2048;
/// A hidden tab keeps a diff (and its new version) up to this size; a larger one is read again when shown.
const KEEP_HIDDEN: usize = 512 << 10;
/// Lines highlighted past the last one a hunk shows: the unchanged lines a few clicks open below it.
const HIGHLIGHT_PAST: usize = 400;
/// What the header says when a version has lone CR line ends, which git does not split lines at.
const LONE_CR: &str = "No unchanged lines or colours: the file ends lines with a lone CR, which git does not split at";

/// A Changes poll reads the shown diff again only once five times the last read's time has passed since it ended:
/// a diff git takes seconds over is not read back to back.
fn poll_due(now: Instant, finished: Option<Instant>, took: Duration) -> bool {
    finished.is_none_or(|at| now >= at + took.saturating_mul(5))
}

pub enum DiffEvent {
    /// "Open file": a pinned file tab at `line`.
    OpenFile { path: PathBuf, line: Option<u32> },
    /// "Open in editor", at `line`.
    OpenInEditor { path: PathBuf, line: Option<u32> },
}

/// The change a diff tab shows: its file in the root, the root (git runs from its repository's top), and the
/// change as the Changes list has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSpec {
    pub root: PathBuf,
    pub path: PathBuf,
    pub change: Change,
}

/// What a read found, worked out on the runtime.
struct Loaded {
    /// git's output as read: the next poll's same output changes nothing.
    raw: Arc<Vec<u8>>,
    diff: Diff,
    /// The new version, when it matches the diff: the unchanged lines between hunks come from it.
    new_side: Option<Arc<Text>>,
    new_hl: Arc<Highlights>,
    /// The old version's highlight, for removed lines, and a digest of its bytes (kept when they did not change).
    old_hl: Arc<Highlights>,
    old_digest: u64,
    /// The widest line in columns: the unified view's horizontal extent.
    columns: usize,
    /// The largest line number either side shows, for the gutters' width.
    last_line: u32,
    /// Why a version was not used for unchanged lines and colours, when that is worth saying.
    note: Option<&'static str>,
}

impl Loaded {
    /// What it holds, roughly, in bytes.
    fn weight(&self) -> usize {
        self.raw.len() * 2 + self.new_side.as_ref().map_or(0, |t| t.text.len())
    }
}

enum Answer {
    /// The same output as last time.
    Same,
    Loaded(Box<Loaded>),
    Failed(String),
}

/// What a read found: the repository's top (once found), whether the file is on disk, and the diff.
struct Read {
    top: Option<PathBuf>,
    exists: bool,
    answer: Answer,
}

/// What a read needs, moved to the runtime.
struct Ask {
    spec: DiffSpec,
    /// The file's entries in the list: each scope's rename source.
    entries: Entries,
    scope: Scope,
    top: Option<PathBuf>,
    raw: Option<Arc<Vec<u8>>>,
    old: Option<(u64, Arc<Highlights>)>,
    cancel: Arc<AtomicBool>,
}

/// One row of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// Unchanged lines folded away: `hidden` of them in gap `gap` (`diff::gaps`); a click shows more.
    Gap { gap: usize, hidden: u32 },
    /// An unchanged line shown from a gap, by its numbers in the new and old versions.
    Unchanged { new: u32, old: u32 },
    /// A hunk's `@@` line.
    Header(usize),
    /// A hunk's line, unified.
    Line { hunk: usize, line: usize },
    /// A hunk's old and new lines side by side (the same line for context).
    Pair { hunk: usize, old: Option<usize>, new: Option<usize> },
}

/// The rows of `diff`: each gap's revealed lines around its fold (when the new version is there to expand from),
/// then each hunk's header and lines.
fn build_rows(diff: &Diff, gaps: &[Gap], revealed: &[(u32, u32)], expandable: bool, split: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    for (i, gap) in gaps.iter().enumerate() {
        if expandable && !gap.is_empty() {
            let len = gap.len();
            let (top, bottom) = revealed.get(i).copied().unwrap_or_default();
            let top = top.min(len);
            let bottom = bottom.min(len - top);
            let line = |k: u32| Row::Unchanged { new: gap.from + k, old: gap.old_from + k };
            rows.extend((0..top).map(line));
            if top + bottom < len {
                rows.push(Row::Gap { gap: i, hidden: len - top - bottom });
            }
            rows.extend((len - bottom..len).map(line));
        }
        let Some(hunk) = diff.hunks.get(i) else { continue };
        rows.push(Row::Header(i));
        if split {
            rows.extend(diff::split_rows(&hunk.lines).into_iter().map(|(old, new)| Row::Pair { hunk: i, old, new }));
        } else {
            rows.extend((0..hunk.lines.len()).map(|line| Row::Line { hunk: i, line }));
        }
    }
    rows
}

/// A drawn line's runs cut where its changed words start and end (`emphasis`, bytes of the line): (drawn bytes,
/// token, emphasised).
fn emphasised(drawn: &Drawn, emphasis: &[Range<usize>]) -> Vec<(Range<usize>, Token, bool)> {
    let marks: Vec<Range<usize>> = emphasis.iter().map(|r| drawn.to_shown(r.start)..drawn.to_shown(r.end)).filter(|r| !r.is_empty()).collect();
    let mut out = Vec::with_capacity(drawn.runs.len() + marks.len() * 2);
    for (run, token) in &drawn.runs {
        let mut cuts = vec![run.start, run.end];
        for m in &marks {
            cuts.extend([m.start, m.end].into_iter().filter(|c| run.start < *c && *c < run.end));
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let inside = marks.iter().any(|m| m.start <= pair[0] && pair[1] <= m.end);
            out.push((pair[0]..pair[1], *token, inside));
        }
    }
    out
}

fn digest(bytes: &[u8]) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// `git <args>` at `cwd` with literal pathspecs, its exit codes in `ok` a success; failures as the tab says them.
async fn git(cwd: &Path, args: &[String], limits: Limits, ok: &[i32]) -> Result<Vec<u8>, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut cmd = runner::git_cmd(cwd, &args);
    cmd.env.push(("GIT_LITERAL_PATHSPECS".into(), "1".into()));
    match runner::run(&Host::Local, cmd, limits).await {
        Ok(out) if out.truncated => Err(format!("git printed more than {}", content::human_size(limits.max_bytes as u64))),
        Ok(out) if out.code.is_some_and(|c| ok.contains(&c)) => Ok(out.stdout),
        Ok(out) => Err(out.stderr.lines().find(|l| !l.trim().is_empty()).map_or_else(|| format!("git exited with status {:?}", out.code), str::to_string)),
        Err(RunError::NotFound(_)) => Err("Git isn't installed".into()),
        Err(RunError::Timeout) => Err(format!("git didn't answer within {} seconds", limits.timeout.as_secs())),
        Err(e) => Err(e.to_string()),
    }
}

/// A version of the file in git (`git cat-file blob <spec>`) as text; none when it is missing, binary, not UTF-8
/// or past the viewer's cap.
async fn blob(top: &Path, spec: String) -> Option<Vec<u8>> {
    git(top, &["cat-file".into(), "blob".into(), spec], SIDE, &[0]).await.ok()
}

/// The working tree's file, under the viewer's cap and a deadline.
async fn working_file(path: PathBuf) -> Option<Vec<u8>> {
    let read = tokio::task::spawn_blocking(move || {
        let meta = std::fs::metadata(&path).ok()?;
        (meta.is_file() && meta.len() <= content::MAX_TEXT).then(|| std::fs::read(&path).ok()).flatten()
    });
    tokio::time::timeout(READ_TIMEOUT, read).await.ok()?.ok()?
}

/// `bytes` as text the viewer would show: not binary, UTF-8.
fn text_of(bytes: &[u8]) -> Option<Text> {
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return None;
    }
    content::decode(bytes)
}

/// A `\r` not before a `\n`: the viewer ends a line there, git does not, so the numbers would part.
fn lone_cr(bytes: &[u8]) -> bool {
    bytes.iter().enumerate().any(|(i, &b)| b == b'\r' && bytes.get(i + 1) != Some(&b'\n'))
}

/// `text` has each `(number, line)` the diff shows of its version: the diff was made from it. The text has lost
/// its byte order mark (`content::decode`); git's first line keeps it.
fn matches<'a>(text: &Text, mut lines: impl Iterator<Item = (u32, &'a str)>) -> bool {
    lines.all(|(n, line)| {
        let line = if n == 1 { line.strip_prefix('\u{feff}').unwrap_or(line) } else { line };
        n >= 1 && (n as usize) <= text.len() && text.line(n as usize - 1) == line
    })
}

/// Reads the diff of `ask` at the repository's top (found once, then kept by the view), and whether its file is on
/// disk (a delete, `git rm`'s `D.` among them, has none to open).
async fn read(ask: Ask) -> Read {
    let exists = tokio::time::timeout(Duration::from_secs(2), tokio::fs::metadata(ask.spec.path.clone())).await;
    let exists = matches!(exists, Ok(Ok(meta)) if meta.is_file());
    let top = match ask.top.clone() {
        Some(top) => top,
        None => match git(&ask.spec.root, &["rev-parse".into(), "--show-toplevel".into()], QUICK, &[0]).await {
            Ok(out) => PathBuf::from(String::from_utf8_lossy(&out).trim_end_matches(['\n', '\r'])),
            Err(e) => return Read { top: None, exists, answer: Answer::Failed(e) },
        },
    };
    let answer = read_at(&top, ask).await;
    Read { top: Some(top), exists, answer }
}

/// git's diff of the change in its scope, a delete's too (`git diff -- path`, `--cached` when staged): a file staged
/// then deleted (`MD`) or added then deleted (`AD`) shows each scope's own part.
async fn read_at(top: &Path, ask: Ask) -> Answer {
    let scope = ask.scope;
    // each scope with its own rename source: a staged rename's on the Staged side, the work tree's on the other
    let change = ask.entries.change_for(&ask.spec.change, scope);
    let base = if scope == Scope::All {
        match git(top, &["rev-parse".into(), "--verify".into(), "--quiet".into(), "HEAD".into()], QUICK, &[0]).await {
            Ok(_) => "HEAD".to_string(),
            // before the first commit: the empty tree, of the repository's hash
            Err(_) => match git(top, &["hash-object".into(), "-t".into(), "tree".into(), "/dev/null".into()], QUICK, &[0]).await {
                Ok(out) => String::from_utf8_lossy(&out).trim().to_string(),
                Err(_) => diff::EMPTY_TREE.to_string(),
            },
        }
    } else {
        "HEAD".to_string()
    };
    let ok: &[i32] = if scope == Scope::Untracked { &[0, 1] } else { &[0] };
    let raw = match git(top, &diff::diff_args(&change, scope, &base), DIFF, ok).await {
        Ok(raw) => raw,
        Err(e) => return Answer::Failed(e),
    };
    if ask.raw.as_deref().is_some_and(|last| *last == raw) {
        return Answer::Same;
    }
    // the versions it compares, for highlighting and unchanged lines; a deleted file has no new one
    let (old_spec, new_spec) = diff::sides(&change, scope, &base);
    let new_bytes = match new_spec {
        Some(spec) => blob(top, spec).await,
        None => working_file(top.join(&change.path)).await,
    };
    let old_bytes = match old_spec {
        Some(spec) => blob(top, spec).await,
        None => None,
    };
    let path = ask.spec.path.clone();
    let (cancel, old) = (ask.cancel.clone(), ask.old.clone());
    let built = tokio::task::spawn_blocking(move || build(raw, &path, new_bytes, old_bytes, old, &cancel)).await;
    match built {
        Ok(loaded) => Answer::Loaded(Box::new(loaded)),
        Err(_) => Answer::Failed("Could not read the diff".into()),
    }
}

/// Parses, marks the changed words and highlights both versions: the CPU part of a read, on the blocking pool.
fn build(raw: Vec<u8>, path: &Path, new_bytes: Option<Vec<u8>>, old_bytes: Option<Vec<u8>>, old: Option<(u64, Arc<Highlights>)>, cancel: &AtomicBool) -> Loaded {
    let mut d = diff::parse_unified(&raw);
    diff::mark_words(&mut d, diff::EMPHASIS_BUDGET);
    // a version with lone CRs numbers its lines otherwise than git: it is not used, and the header says why
    let lone = |b: &Option<Vec<u8>>| b.as_deref().is_some_and(lone_cr);
    let note = (lone(&new_bytes) || lone(&old_bytes)).then_some(LONE_CR);
    let (new_bytes, old_bytes) = (new_bytes.filter(|b| !lone_cr(b)), old_bytes.filter(|b| !lone_cr(b)));
    let lines = || d.hunks.iter().flat_map(|h| &h.lines);
    let new_side = new_bytes.as_deref().and_then(text_of).filter(|t| matches(t, lines().filter_map(|l| Some((l.new?, l.text.as_str())))));
    let old_raw = old_bytes.as_deref();
    let old_side = old_raw.and_then(text_of).filter(|t| matches(t, lines().filter_map(|l| Some((l.old?, l.text.as_str())))));
    let last_new = lines().filter_map(|l| l.new).max().unwrap_or(0) as usize;
    let last_old = lines().filter_map(|l| l.old).max().unwrap_or(0) as usize;
    let first = new_side.as_ref().or(old_side.as_ref()).map_or("", |t| t.line(0));
    let syntax = highlight::syntax_for(path, first);
    let new_hl = match (syntax, &new_side) {
        (Some(syntax), Some(text)) => highlight::highlight(syntax, text.lines().take(last_new + HIGHLIGHT_PAST), highlight::BUDGET, cancel),
        _ => Highlights::default(),
    };
    let old_digest = old_raw.map_or(0, digest);
    let has_removed = lines().any(|l| l.kind == LineKind::Removed);
    let old_hl = match (syntax, &old_side, old) {
        (_, _, Some((was, hl))) if was == old_digest && old_side.is_some() && hl.lines() >= last_old.min(old_side.as_ref().map_or(0, |t| t.len())) => hl,
        (Some(syntax), Some(text), _) if has_removed => Arc::new(highlight::highlight(syntax, text.lines().take(last_old), highlight::BUDGET, cancel)),
        _ => Arc::default(),
    };
    let side_columns = new_side.as_ref().map_or(0, |t| t.lines().map(columns).max().unwrap_or(0));
    let columns = lines().map(|l| columns(&l.text)).max().unwrap_or(0).max(side_columns);
    let last_line = (last_new.max(last_old) as u32).max(new_side.as_ref().map_or(0, |t| t.len() as u32));
    Loaded { raw: Arc::new(raw), diff: d, new_side: new_side.map(Arc::new), new_hl: Arc::new(new_hl), old_hl, old_digest, columns, last_line, note }
}

/// The right-click menu's row: its line's text, and its hunk.
pub struct RowMenu {
    at: Point<Pixels>,
    line: Option<String>,
    hunk: Option<usize>,
}

pub struct DiffView {
    focus: FocusHandle,
    rt: Handle,
    spec: DiffSpec,
    /// What it compares; its change's group's own unless the switch picked another.
    scope: Scope,
    /// The file's entries in the list (both: the switch shows), each scope's rename source.
    entries: Entries,
    /// The file is on disk, as the last read found: "Open file" and the editor need one.
    exists: bool,
    /// When the last read ended and how long it took: a poll waits five times that (`poll_due`).
    finished: Option<Instant>,
    took: Duration,
    started: Instant,
    /// The repository's top, once found: git runs there with paths from it.
    top: Option<PathBuf>,
    /// Bumped by every read started: an older one's answer is dropped when it lands.
    generation: u64,
    /// The read out; a poll while it is waits for the next.
    job: Option<AbortHandle>,
    /// Set to stop the highlight of a read no longer wanted.
    cancel: Arc<AtomicBool>,
    /// `None` until the first read answers (and after a hidden tab let go of it).
    result: Option<Result<Loaded, String>>,
    /// As the settings had it at the last render.
    split: bool,
    rows: Vec<Row>,
    gaps: Vec<Gap>,
    /// Each gap's lines shown under the hunk above it and over the one below.
    revealed: Vec<(u32, u32)>,
    drawn: HashMap<(u8, u32, u32), Rc<Drawn>>,
    scroll: UniformListScrollHandle,
    menu: Option<crate::ui::Popup<RowMenu>>,
}

impl EventEmitter<DiffEvent> for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for DiffView {
    fn drop(&mut self) {
        self.stop();
    }
}

impl DiffView {
    pub fn new(rt: Handle, spec: DiffSpec, entries: Entries, cx: &mut Context<Self>) -> DiffView {
        let scope = Scope::of(spec.change.group);
        let exists = !spec.change.absent();
        let split = crate::settings::SettingsGlobal::of(cx).settings.diff_split;
        let mut view = DiffView {
            focus: cx.focus_handle(),
            rt,
            spec,
            scope,
            entries: Entries::default(),
            exists,
            finished: None,
            took: Duration::ZERO,
            started: Instant::now(),
            top: None,
            generation: 0,
            job: None,
            cancel: Arc::default(),
            result: None,
            split,
            rows: Vec::new(),
            gaps: Vec::new(),
            revealed: Vec::new(),
            drawn: HashMap::new(),
            scroll: UniformListScrollHandle::new(),
            menu: None,
        };
        view.set_entries(entries);
        view.load(cx);
        view
    }

    pub fn path(&self) -> &Path {
        &self.spec.path
    }

    /// Shows `spec`'s change: the same one again reads it afresh, keeping the place; another (a preview replaced)
    /// starts over.
    pub fn open(&mut self, spec: DiffSpec, entries: Entries, cx: &mut Context<Self>) {
        let same = spec.path == self.spec.path && spec.change.group == self.spec.change.group;
        if !same {
            self.stop();
            if spec.root != self.spec.root {
                self.top = None;
            }
            self.scope = Scope::of(spec.change.group);
            (self.result, self.rows, self.gaps, self.revealed) = (None, Vec::new(), Vec::new(), Vec::new());
            self.drawn.clear();
            self.menu = None;
            self.scroll = UniformListScrollHandle::new();
            self.exists = !spec.change.absent();
        }
        self.spec = spec;
        self.set_entries(entries);
        self.load(cx);
        cx.notify();
    }

    /// A Changes poll answered (`poll`), or the tab came into view: reads the diff again, with the change as the
    /// list has it now. A read still out is waited for, not doubled; a poll waits for `poll_due`.
    pub fn reload(&mut self, change: Option<Change>, entries: Entries, poll: bool, cx: &mut Context<Self>) {
        if let Some(change) = change {
            self.spec.change = change;
        }
        let scope = self.scope;
        self.set_entries(entries);
        if scope != self.scope {
            self.load(cx);
            cx.notify();
        } else if self.job.is_none() && (!poll || poll_due(Instant::now(), self.finished, self.took)) {
            self.load(cx);
        }
    }

    /// The file's entries now: one no longer both staged and changed loses the switch, and goes back to its group's
    /// own diff.
    fn set_entries(&mut self, entries: Entries) {
        self.entries = entries;
        if !self.both() {
            self.scope = Scope::of(self.spec.change.group);
        }
    }

    /// The switch shows: the file is both staged and changed again, and this is one of those entries' tabs.
    fn both(&self) -> bool {
        self.entries.both() && matches!(self.spec.change.group, Group::Staged | Group::Changes)
    }

    fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            (self.result, self.rows) = (None, Vec::new());
            self.revealed.clear();
            self.menu = None;
            self.load(cx);
            cx.notify();
        }
    }

    /// The read out stops, and its highlight.
    fn stop(&mut self) {
        if let Some(job) = self.job.take() {
            job.abort();
        }
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Starts a read, dropping the one out.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.stop();
        self.generation += 1;
        self.cancel = Arc::default();
        let loaded = self.result.as_ref().and_then(|r| r.as_ref().ok());
        let ask = Ask {
            spec: self.spec.clone(),
            entries: self.entries.clone(),
            scope: self.scope,
            top: self.top.clone(),
            raw: loaded.map(|l| l.raw.clone()),
            old: loaded.map(|l| (l.old_digest, l.old_hl.clone())),
            cancel: self.cancel.clone(),
        };
        let generation = self.generation;
        self.started = Instant::now();
        let task = self.rt.spawn(read(ask));
        self.job = Some(task.abort_handle());
        cx.spawn(async move |this, cx| {
            // an aborted read (another started, the view gone) answers nothing
            if let Ok(answer) = task.await {
                let _ = this.update(cx, |v, cx| v.landed(generation, answer, cx));
            }
        })
        .detach();
    }

    fn landed(&mut self, generation: u64, Read { top, exists, answer }: Read, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        self.job = None;
        self.finished = Some(Instant::now());
        self.took = self.started.elapsed();
        if top.is_some() {
            self.top = top;
        }
        if self.exists != exists {
            self.exists = exists;
            cx.notify();
        }
        match answer {
            Answer::Same => {}
            Answer::Failed(e) => {
                if !matches!(&self.result, Some(Err(was)) if *was == e) {
                    self.result = Some(Err(e));
                    self.rows.clear();
                    self.menu = None;
                    cx.notify();
                }
            }
            Answer::Loaded(loaded) => {
                let gaps = diff::gaps(&loaded.diff.hunks, loaded.new_side.as_ref().map(|t| t.len() as u32));
                // the same folds keep what was opened of them; another shape of diff starts folded
                if gaps.len() != self.gaps.len() {
                    self.revealed = vec![(0, 0); gaps.len()];
                }
                self.gaps = gaps;
                self.result = Some(Ok(*loaded));
                self.drawn.clear();
                self.rebuild();
                cx.notify();
            }
        }
    }

    /// The rows again: after a read, a fold opened, the layout switched. The menu goes with the rows it was for.
    fn rebuild(&mut self) {
        self.menu = None;
        self.rows = match self.loaded() {
            Some(l) => build_rows(&l.diff, &self.gaps, &self.revealed, l.new_side.is_some(), self.split),
            None => Vec::new(),
        };
    }

    fn loaded(&self) -> Option<&Loaded> {
        self.result.as_ref()?.as_ref().ok()
    }

    /// Out of sight: the menu closes, and a large diff is let go of (read again when shown).
    pub fn hide(&mut self) {
        self.menu = None;
        if self.loaded().is_some_and(|l| l.weight() > KEEP_HIDDEN) {
            self.stop();
            (self.result, self.rows) = (None, Vec::new());
            self.drawn.clear();
        }
    }

    /// Leaving for good: the read stops and what it read goes.
    pub fn release(&mut self) {
        self.stop();
        (self.result, self.rows) = (None, Vec::new());
        self.drawn.clear();
        self.menu = None;
    }

    fn reveal(&mut self, gap: usize, cx: &mut Context<Self>) {
        let Some(len) = self.gaps.get(gap).map(Gap::len) else { return };
        if let Some(shown) = self.revealed.get_mut(gap) {
            *shown = diff::reveal(gap, self.gaps.len(), len, *shown);
            self.rebuild();
            cx.notify();
        }
    }

    /// Where "Open file" and the editor go: the first change's line.
    fn first_line(&self) -> Option<u32> {
        self.loaded().and_then(|l| l.diff.first_change())
    }

    fn open_file(&mut self, cx: &mut Context<Self>) {
        cx.emit(DiffEvent::OpenFile { path: self.spec.path.clone(), line: self.first_line() });
    }

    fn open_in_editor(&mut self, cx: &mut Context<Self>) {
        cx.emit(DiffEvent::OpenInEditor { path: self.spec.path.clone(), line: self.first_line() });
    }

    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (key, m) = (e.keystroke.key.as_str(), &e.keystroke.modifiers);
        let primary = if cfg!(target_os = "macos") { m.platform && !m.control } else { m.control && !m.platform };
        if primary && !m.alt && !m.shift && key == "enter" {
            if self.exists {
                self.open_in_editor(cx);
            }
        } else if !m.modified() || (primary && matches!(key, "up" | "down")) {
            if !self.scroll_key(key, primary, window, cx) {
                return;
            }
        } else {
            return;
        }
        cx.stop_propagation();
    }

    /// Arrows, Page Up/Down, Home and End move the view; true when `key` was one of them.
    fn scroll_key(&mut self, key: &str, primary: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let handle = self.scroll.0.borrow().base_handle.clone();
        let theme = Theme::of(cx);
        let line = px((theme.terminal_font_size * theme.terminal_line_height).round());
        let page = handle.bounds().size.height - line * 2.;
        let (offset, max) = (handle.offset(), handle.max_offset());
        let y = match key {
            "up" if primary => px(0.),
            "down" if primary => -max.y,
            "up" => offset.y + line * 3.,
            "down" => offset.y - line * 3.,
            "pageup" => offset.y + page,
            "pagedown" => offset.y - page,
            "home" => px(0.),
            "end" => -max.y,
            _ => return false,
        };
        handle.set_offset(point(offset.x, y.clamp(-max.y, px(0.))));
        window.refresh();
        cx.notify();
        true
    }

    /// The header's path: under the root when the file is in it.
    fn shown_path(&self) -> String {
        match self.spec.path.strip_prefix(&self.spec.root) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => self.spec.path.to_string_lossy().into_owned(),
        }
    }

    fn header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.spec.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let icon = crate::files::icons::material_icon(&name, false, false);
        let tag = |label: SharedString| {
            div().flex_none().px(px(5.)).rounded(px(4.)).border_1().border_color(theme.border).text_size(px(10.5)).text_color(theme.text_faint).child(label)
        };
        let group = match self.spec.change.group {
            Group::Conflicts => "Merge",
            Group::Staged => "Staged",
            Group::Changes => "Changes",
            Group::Untracked => "Untracked",
        };
        let counts = self.loaded().filter(|l| !l.diff.binary && !l.diff.combined).map(|l| l.diff.counts());
        let renamed = self.spec.change.orig.as_ref().filter(|o| **o != self.spec.change.path).map(|o| format!("from {o}"));
        let scopes = self.both().then(|| {
            let choices = [Scope::All, Scope::Staged, Scope::Unstaged].map(|scope| {
                let on = self.scope == scope;
                choice(theme, ("diff-scope", scope as usize), scope.label(), on).on_click(cx.listener(move |v: &mut DiffView, _: &ClickEvent, _, cx| v.set_scope(scope, cx)))
            });
            segmented(theme).children(choices)
        });
        let layouts = [("Unified", false), ("Split", true)].map(|(label, split)| {
            choice(theme, ("diff-layout", split as usize), label, self.split == split).on_click(cx.listener(move |_: &mut DiffView, _: &ClickEvent, _, cx| {
                // kept in the settings, so every diff tab and the next run show it so; not over a file that failed to load
                if crate::settings::SettingsGlobal::of(cx).error.is_none() {
                    crate::ui::settings::update(cx, |s| s.diff_split = split);
                }
                cx.notify();
            }))
        });
        let present = self.exists;
        let note = self.loaded().and_then(|l| l.note);
        div()
            .h(px(36.))
            .flex_none()
            .pl(px(12.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.wash(0.04))
            .font_family(theme.font_ui.clone())
            .text_size(px(12.5))
            .child(gpui::img(icon).size(px(16.)).flex_none().object_fit(gpui::ObjectFit::Contain))
            .child(div().min_w_0().truncate().text_color(theme.text).child(self.shown_path()))
            .children(renamed.map(|r| div().min_w_0().truncate().text_size(px(11.5)).text_color(theme.text_faint).child(r)))
            .child(tag(group.into()))
            .children(counts.map(|(added, removed)| {
                div()
                    .flex_none()
                    .flex()
                    .gap(px(6.))
                    .text_size(px(11.5))
                    .child(div().text_color(theme.success).child(format!("+{added}")))
                    .child(div().text_color(theme.danger).child(format!("−{removed}")))
            }))
            .child(div().flex_1())
            .children(note.map(|why| {
                div().id("diff-note").flex_none().text_size(px(11.5)).text_color(theme.text_faint).child("No folds").tooltip(tip(why, None))
            }))
            .children(scopes)
            .child(segmented(theme).children(layouts))
            .when(present, |el| {
                el.child(
                    titlebar_button(theme, "diff-open-file", "icons/solar/file-text.svg")
                        .tooltip(tip("Open file", None))
                        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_file(cx))),
                )
                .child(
                    titlebar_button(theme, "diff-editor", "icons/solar/pen-new-square.svg")
                        .tooltip(tip("Open in editor", None))
                        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_in_editor(cx))),
                )
            })
    }

    /// A message in place of the diff, with the file offered when there is one to open.
    fn notice(&self, theme: &Theme, title: impl Into<SharedString>, hint: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        let hover = theme.hover;
        let present = self.exists;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .font_family(theme.font_ui.clone())
            .child(div().text_color(theme.text).child(title.into()))
            .child(div().text_size(px(12.)).text_color(theme.text_muted).child(hint.into()))
            .when(present, |el| {
                el.child(
                    div()
                        .id("diff-notice-open")
                        .mt(px(8.))
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.border)
                        .text_size(px(12.))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_file(cx)))
                        .child("Open file"),
                )
            })
            .into_any_element()
    }

    fn body(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let loaded = match &self.result {
            None => return div().into_any_element(),
            Some(Err(e)) => return self.notice(theme, "Could not show the diff", e.clone(), cx),
            Some(Ok(loaded)) => loaded,
        };
        if loaded.diff.combined {
            return self.notice(theme, "This file has merge conflicts", "Open it to resolve them between their markers.", cx);
        }
        if loaded.diff.binary {
            return self.notice(theme, "Binary file", "git doesn't show a binary file's changes as text.", cx);
        }
        if loaded.diff.hunks.is_empty() {
            let (title, hint) = loaded.diff.without_hunks();
            return self.notice(theme, title, hint, cx);
        }
        self.lines(theme, window, cx)
    }

    fn lines(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let font = gpui::font(theme.font_terminal.clone());
        let size = px(theme.terminal_font_size);
        let em = window.text_system().em_advance(window.text_system().resolve_font(&font), size).unwrap_or(size * 0.6);
        let (columns, last) = self.loaded().map_or((0, 0), |l| (l.columns, l.last_line));
        let number = em * last.to_string().len().max(3) as f32 + px(16.);
        let sign = em * 2.;
        let line_h = px((theme.terminal_font_size * theme.terminal_line_height).round());
        let geometry = Geometry { number, sign, line_h, width: number * 2. + sign + em * (columns + 1) as f32 + px(24.) };
        let mut list = gpui::uniform_list("diff-lines", self.rows.len(), cx.processor(move |v: &mut DiffView, range: Range<usize>, _, cx| v.row_elements(range, geometry, cx)))
            .track_scroll(&self.scroll)
            .size_full()
            .pb(px(8.));
        // unified rows are as wide as the widest line and scroll across; split halves cut theirs
        if !self.split {
            list = list.with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained);
        }
        let scroll = self.scroll.clone();
        div()
            .id("diff-code")
            .relative()
            .size_full()
            .font(font)
            .text_size(size)
            .line_height(line_h)
            .text_color(theme.terminal.foreground)
            .child(list)
            .child(crate::ui::scrollbar("diff-scrollbar", &scroll, theme))
            .into_any_element()
    }

    /// A line as drawn, worked out on its first frame on screen: `key` tells it apart (a hunk's line, an unchanged
    /// line's number).
    fn drawn(&mut self, key: (u8, u32, u32), text: &str, spans: &[Span]) -> Rc<Drawn> {
        if self.drawn.len() >= MAX_DRAWN && !self.drawn.contains_key(&key) {
            self.drawn.clear();
        }
        self.drawn.entry(key).or_insert_with(|| Rc::new(Drawn::new(text, spans))).clone()
    }

    /// One side of a row: its number, its sign, and its text with its tokens and changed words.
    fn cell(&mut self, side: Side, g: Geometry, theme: &Theme) -> gpui::Div {
        let font = gpui::font(theme.font_terminal.clone());
        let (wash, strong) = match side.kind {
            Some(LineKind::Added) => (Some(theme.success.opacity(0.12)), theme.success.opacity(0.3)),
            Some(LineKind::Removed) => (Some(theme.danger.opacity(0.12)), theme.danger.opacity(0.3)),
            _ => (None, theme.hover),
        };
        let sign = match side.kind {
            Some(LineKind::Added) => "+",
            Some(LineKind::Removed) => "−",
            _ => "",
        };
        let number = |n: Option<u32>| div().flex_none().w(g.number).pr(px(10.)).flex().justify_end().text_color(theme.text_faint).children(n.map(|n| SharedString::from(n.to_string())));
        let text = side.text.map(|CellText { key, text, spans, emphasis, no_newline }| {
            let drawn = self.drawn(key, &text, &spans);
            let run = |len: usize, color: Hsla, background: Option<Hsla>| TextRun { len, font: font.clone(), color, background_color: background, underline: None, strikethrough: None };
            let mut runs: Vec<TextRun> = emphasised(&drawn, &emphasis).into_iter().map(|(r, token, marked)| run(r.len(), token_color(token, theme), marked.then_some(strong))).collect();
            if drawn.text.len() > drawn.body {
                runs.push(run(drawn.text.len() - drawn.body, theme.text_faint, None));
            }
            div()
                .flex_none()
                .flex()
                .child(StyledText::new(drawn.text.clone()).with_runs(runs))
                .when(no_newline, |el| el.child(div().pl(px(8.)).text_color(theme.text_faint).child("⊘ no newline at end")))
        });
        div()
            .h(g.line_h)
            .flex()
            .flex_none()
            .whitespace_nowrap()
            .when_some(wash, |el, wash| el.bg(wash))
            .children(side.numbers.iter().map(|n| number(*n)))
            .child(div().flex_none().w(g.sign).flex().justify_center().text_color(if wash.is_some() { theme.text_muted } else { theme.text_faint }).child(sign))
            .children(text)
    }

    /// `line` of hunk `hunk` as a cell: its numbers (both, or the one of its side when split).
    fn line_side(&self, hunk: usize, line: usize, numbers: Numbers) -> Side {
        let Some(loaded) = self.loaded() else { return Side::default() };
        let Some(l) = loaded.diff.hunks.get(hunk).and_then(|h| h.lines.get(line)) else { return Side::default() };
        let spans = match (l.kind, l.old, l.new) {
            (LineKind::Removed, Some(old), _) => loaded.old_hl.line(old as usize - 1).to_vec(),
            (_, _, Some(new)) => loaded.new_hl.line(new as usize - 1).to_vec(),
            _ => Vec::new(),
        };
        let numbers = match numbers {
            Numbers::Both => vec![l.old, l.new],
            Numbers::Old => vec![l.old],
            Numbers::New => vec![l.new],
        };
        let text = CellText { key: (0, hunk as u32, line as u32), text: l.text.clone(), spans, emphasis: l.emphasis.clone(), no_newline: l.no_newline };
        Side { kind: Some(l.kind), numbers, text: Some(text) }
    }

    /// An unchanged line opened from a gap, as a cell.
    fn unchanged_side(&self, new: u32, old: u32, numbers: Numbers) -> Side {
        let Some(loaded) = self.loaded() else { return Side::default() };
        let Some(text) = loaded.new_side.as_ref().filter(|t| new >= 1 && (new as usize) <= t.len()) else { return Side::default() };
        let line = text.line(new as usize - 1).to_string();
        let spans = loaded.new_hl.line(new as usize - 1).to_vec();
        let numbers = match numbers {
            Numbers::Both => vec![Some(old), Some(new)],
            Numbers::Old => vec![Some(old)],
            Numbers::New => vec![Some(new)],
        };
        let text = CellText { key: (1, new, 0), text: line, spans, emphasis: Vec::new(), no_newline: false };
        Side { kind: Some(LineKind::Context), numbers, text: Some(text) }
    }

    fn row_elements(&mut self, range: Range<usize>, g: Geometry, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let hover = theme.hover;
        let split = self.split;
        let rows: Vec<(usize, Row)> = range.filter_map(|ix| Some((ix, *self.rows.get(ix)?))).collect();
        rows.into_iter()
            .map(|(ix, row)| {
                let (line, hunk) = self.row_copy(row);
                let el = match row {
                    Row::Header(h) => {
                        let header = self.loaded().and_then(|l| l.diff.hunks.get(h)).map(|h| h.header.clone()).unwrap_or_default();
                        div()
                            .h(g.line_h)
                            .flex()
                            .flex_none()
                            .items_center()
                            .whitespace_nowrap()
                            .bg(theme.accent.opacity(0.07))
                            .text_color(theme.text_muted)
                            .pl(if split { g.number + g.sign } else { g.number * 2. + g.sign })
                            .child(header)
                    }
                    Row::Gap { gap, hidden } => {
                        let label = if hidden == 1 { "1 unchanged line".to_string() } else { format!("{hidden} unchanged lines") };
                        div()
                            .h(g.line_h)
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.))
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .bg(theme.wash(0.03))
                            .hover(move |s| s.bg(hover))
                            .text_color(theme.text_faint)
                            .pl(if split { g.number } else { g.number * 2. })
                            .child(gpui::svg().path("icons/alt-arrow-right.svg").size(px(12.)).text_color(theme.text_faint).with_transformation(gpui::Transformation::rotate(gpui::percentage(0.25))))
                            .child(label)
                            .on_mouse_down(MouseButton::Left, cx.listener(move |v: &mut DiffView, _: &MouseDownEvent, _, cx| v.reveal(gap, cx)))
                    }
                    Row::Unchanged { new, old } if split => {
                        let (left, right) = (self.unchanged_side(new, old, Numbers::Old), self.unchanged_side(new, old, Numbers::New));
                        self.pair(left, right, g, &theme)
                    }
                    Row::Unchanged { new, old } => {
                        let side = self.unchanged_side(new, old, Numbers::Both);
                        self.cell(side, g, &theme)
                    }
                    Row::Line { hunk, line } => {
                        let side = self.line_side(hunk, line, Numbers::Both);
                        self.cell(side, g, &theme)
                    }
                    Row::Pair { hunk, old, new } => {
                        let left = old.map_or_else(Side::default, |l| self.line_side(hunk, l, Numbers::Old));
                        let right = new.map_or_else(Side::default, |l| self.line_side(hunk, l, Numbers::New));
                        self.pair(left, right, g, &theme)
                    }
                };
                let el = if split { el.w_full() } else { el.min_w(g.width) };
                el.id(("diff-row", ix))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |v: &mut DiffView, e: &MouseDownEvent, _, cx| {
                            // a fold row has nothing to copy: no menu
                            if line.is_none() && hunk.is_none() {
                                return;
                            }
                            cx.stop_propagation();
                            v.menu = Some(crate::ui::Popup::new(RowMenu { at: e.position, line: line.clone(), hunk }));
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// Split: the old side left, the new right, each half the width; an empty side is blank.
    fn pair(&mut self, left: Side, right: Side, g: Geometry, theme: &Theme) -> gpui::Div {
        let half = |cell: gpui::Div, divider: bool| {
            div().flex_1().min_w_0().overflow_hidden().when(divider, |el| el.border_l_1().border_color(theme.border)).child(cell)
        };
        let blank = |side: &Side| side.text.is_none();
        let (left_blank, right_blank) = (blank(&left), blank(&right));
        let left = if left_blank { div().h(g.line_h).bg(theme.wash(0.02)) } else { self.cell(left, g, theme) };
        let right = if right_blank { div().h(g.line_h).bg(theme.wash(0.02)) } else { self.cell(right, g, theme) };
        div().h(g.line_h).flex().child(half(left, false)).child(half(right, true))
    }

    /// What a right-click on `row` copies: its line's text (the new side's, of a split pair that has both) and its
    /// hunk.
    fn row_copy(&self, row: Row) -> (Option<String>, Option<usize>) {
        let Some(loaded) = self.loaded() else { return (None, None) };
        let line = |hunk: usize, line: usize| loaded.diff.hunks.get(hunk).and_then(|h| h.lines.get(line)).map(|l: &DiffLine| l.text.clone());
        match row {
            Row::Header(h) => (None, Some(h)),
            Row::Line { hunk, line: l } => (line(hunk, l), Some(hunk)),
            Row::Pair { hunk, old, new } => (new.or(old).and_then(|l| line(hunk, l)), Some(hunk)),
            Row::Unchanged { new, .. } => (loaded.new_side.as_ref().map(|t| t.line(new as usize - 1).to_string()), None),
            Row::Gap { .. } => (None, None),
        }
    }

    /// The right-click menu, while it is up.
    fn row_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let close = |v: &mut DiffView| {
            if let Some(m) = v.menu.as_mut() {
                m.close()
            }
        };
        let hunk = menu.value.hunk.and_then(|h| self.loaded()?.diff.hunks.get(h)).map(diff::hunk_text);
        let copy = |text: String| move |v: &mut DiffView, _: &ClickEvent, _: &mut Window, cx: &mut Context<DiffView>| {
            close(v);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            cx.notify();
        };
        let card = menu_card(theme, "diff-menu", 180.)
            .on_mouse_down_out(cx.listener(move |v, _, _, cx| {
                close(v);
                cx.notify();
            }))
            .children(menu.value.line.clone().map(|line| menu_choice(theme, "diff-menu-copy-line", "icons/solar/copy.svg", "Copy line", false).on_click(cx.listener(copy(line)))))
            .children(hunk.map(|text| menu_choice(theme, "diff-menu-copy-hunk", "icons/solar/copy.svg", "Copy hunk", false).on_click(cx.listener(copy(text)))));
        let anchored = gpui::anchored().position(menu.value.at).snap_to_window_with_margin(px(8.)).child(crate::ui::menu_motion("diff-menu-in", menu, card));
        Some(gpui::deferred(anchored).with_priority(1).into_any_element())
    }
}

/// Which numbers a cell's gutter shows.
#[derive(Clone, Copy)]
enum Numbers {
    Both,
    Old,
    New,
}

/// A cell's content: its kind, its gutter's numbers and its line; none of them for a split row's blank side.
#[derive(Default)]
struct Side {
    kind: Option<LineKind>,
    numbers: Vec<Option<u32>>,
    text: Option<CellText>,
}

/// A cell's line: the drawn cache's key, its text, tokens and changed words, and whether it lacks a final newline.
struct CellText {
    key: (u8, u32, u32),
    text: String,
    spans: Vec<Span>,
    emphasis: Vec<Range<usize>>,
    no_newline: bool,
}

/// The list's measures, worked out once a frame.
#[derive(Clone, Copy)]
struct Geometry {
    /// A line number's column.
    number: Pixels,
    /// The `+`/`−` column.
    sign: Pixels,
    line_h: Pixels,
    /// A unified row's width: the widest line's.
    width: Pixels,
}

/// The header's segmented switch.
fn segmented(theme: &Theme) -> gpui::Div {
    div().flex().flex_none().gap(px(2.)).p(px(2.)).rounded(px(7.)).border_1().border_color(theme.border)
}

fn choice(theme: &Theme, id: (&'static str, usize), label: &'static str, on: bool) -> gpui::Stateful<gpui::Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .px(px(8.))
        .h(px(22.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .text_size(px(12.))
        .cursor_pointer()
        .text_color(if on { theme.text } else { theme.text_muted })
        .when(on, |el| el.bg(theme.selected))
        .when(!on, |el| el.hover(move |s| s.bg(hover)))
        .child(label)
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::reap_popup(&mut self.menu, window);
        let split = crate::settings::SettingsGlobal::of(cx).settings.diff_split;
        if split != self.split {
            self.split = split;
            self.rebuild();
        }
        let theme = &Theme::of(cx).clone();
        let body = self.body(theme, window, cx);
        let menu = self.row_menu(theme, cx);
        div()
            .id("diff-view")
            .track_focus(&self.focus)
            .key_context("DiffView")
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| v.key(e, window, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| window.focus(&v.focus, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.terminal.background)
            .child(self.header(theme, cx))
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_waits_five_times_the_last_read() {
        let now = Instant::now();
        assert!(poll_due(now, None, Duration::from_secs(9)), "never read: at once");
        let ended = now - Duration::from_secs(2);
        assert!(!poll_due(now, Some(ended), Duration::from_secs(1)), "a 1 s read waits 5 s");
        assert!(poll_due(now, Some(ended), Duration::from_millis(300)));
    }

    #[test]
    fn versions_with_lone_crs_or_a_bom_are_matched_as_git_numbers_them() {
        assert!(lone_cr(b"a\rb") && !lone_cr(b"a\r\nb\n") && !lone_cr(b""));
        let text = content::decode(b"\xEF\xBB\xBFfirst\nsecond\n").unwrap();
        assert!(matches(&text, [(1, "\u{feff}first"), (2, "second")].into_iter()));
        assert!(!matches(&text, [(3, "x")].into_iter()));
        let d = diff::parse_unified(b"@@ -1,2 +1,2 @@\n a\n-b\n+c\n");
        let built = build(b"@@ -1,2 +1,2 @@\n a\n-b\n+c\n".to_vec(), Path::new("x.txt"), Some(b"a\rc".to_vec()), Some(b"a\nb\n".to_vec()), None, &AtomicBool::new(false));
        assert_eq!(built.diff.hunks, d.hunks);
        assert!(built.new_side.is_none() && built.note.is_some(), "a lone CR: no folds, and the header says why");
    }

    // Real repositories, as in the Changes tab's tests: what git prints for each group and scope.

    fn has_git() -> bool {
        std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    fn sh(dir: &Path, args: &[&str]) {
        let mut cmd = std::process::Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(key);
            }
        }
        let out = cmd
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn repo(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q"]);
        for (path, text) in files {
            std::fs::write(repo.join(path), text).unwrap();
        }
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-qm", "first"]);
        (dir, repo)
    }

    fn entry(group: Group, path: &str, xy: [char; 2], orig: Option<&str>) -> Change {
        Change { group, path: path.into(), orig: orig.map(Into::into), code: xy[if group == Group::Staged { 0 } else { 1 }], xy, submodule: false, moved_to: None }
    }

    /// The tab of `change` in `scope`: whether its file is on disk, and the diff.
    async fn read_tab(repo: &Path, change: &Change, entries: &Entries, scope: Scope) -> (bool, Diff) {
        let spec = DiffSpec { root: repo.to_path_buf(), path: repo.join(&change.path), change: change.clone() };
        let ask = Ask { spec, entries: entries.clone(), scope, top: None, raw: None, old: None, cancel: Arc::default() };
        let read = read(ask).await;
        match read.answer {
            Answer::Loaded(l) => (read.exists, l.diff),
            Answer::Same => panic!("nothing asked before"),
            Answer::Failed(e) => panic!("{scope:?}: {e}"),
        }
    }

    fn texts(d: &Diff) -> Vec<(LineKind, &str)> {
        d.hunks.iter().flat_map(|h| &h.lines).map(|l| (l.kind, l.text.as_str())).collect()
    }

    /// `MD`: edited, staged, then deleted. Staged shows the edit, Unstaged the staged version removed, All HEAD's.
    #[tokio::test]
    async fn a_file_staged_then_deleted_shows_each_scope() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = repo(&[("f.txt", "a\nb\n")]);
        std::fs::write(repo.join("f.txt"), "a\nB\n").unwrap();
        sh(&repo, &["add", "f.txt"]);
        std::fs::remove_file(repo.join("f.txt")).unwrap();
        let (staged, changes) = (entry(Group::Staged, "f.txt", ['M', 'D'], None), entry(Group::Changes, "f.txt", ['M', 'D'], None));
        let entries = Entries { staged: Some(staged.clone()), changes: Some(changes.clone()) };
        use LineKind::{Added as A, Context as C, Removed as R};
        let (exists, d) = read_tab(&repo, &staged, &entries, Scope::Staged).await;
        assert!(!exists, "nothing to open");
        assert_eq!(texts(&d), [(C, "a"), (R, "b"), (A, "B")]);
        let (_, d) = read_tab(&repo, &changes, &entries, Scope::Unstaged).await;
        assert_eq!(texts(&d), [(R, "a"), (R, "B")]);
        assert!(d.deleted_file);
        let (_, d) = read_tab(&repo, &staged, &entries, Scope::All).await;
        assert_eq!(texts(&d), [(R, "a"), (R, "b")]);
    }

    /// `AD`: added, then deleted. Staged shows it added, Unstaged removed, All nothing (HEAD and the work tree lack
    /// it alike).
    #[tokio::test]
    async fn a_file_added_then_deleted_shows_each_scope() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = repo(&[("keep.txt", "k\n")]);
        std::fs::write(repo.join("g.txt"), "x\n").unwrap();
        sh(&repo, &["add", "g.txt"]);
        std::fs::remove_file(repo.join("g.txt")).unwrap();
        let (staged, changes) = (entry(Group::Staged, "g.txt", ['A', 'D'], None), entry(Group::Changes, "g.txt", ['A', 'D'], None));
        let entries = Entries { staged: Some(staged.clone()), changes: Some(changes.clone()) };
        let (exists, d) = read_tab(&repo, &staged, &entries, Scope::Staged).await;
        assert!(!exists);
        assert_eq!((d.counts(), d.new_file), ((1, 0), true));
        let (_, d) = read_tab(&repo, &changes, &entries, Scope::Unstaged).await;
        assert_eq!(d.counts(), (0, 1));
        let (_, d) = read_tab(&repo, &changes, &entries, Scope::All).await;
        assert!(d.hunks.is_empty() && !d.binary);
    }

    /// `RM`: the Changes tab's Staged and All take the Staged entry's source; `.R` (`git add -N`): the work tree's
    /// rename is the Changes entry's own.
    #[tokio::test]
    async fn renames_take_each_scope_s_source() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = repo(&[("a.txt", "one\ntwo\nthree\nfour\n"), ("c.txt", "five\nsix\nseven\neight\n")]);
        sh(&repo, &["mv", "a.txt", "b.txt"]);
        std::fs::write(repo.join("b.txt"), "one\ntwo\nthree\nfour\nmore\n").unwrap();
        let staged = entry(Group::Staged, "b.txt", ['R', 'M'], Some("a.txt"));
        let changes = entry(Group::Changes, "b.txt", ['R', 'M'], None);
        let entries = Entries { staged: Some(staged), changes: Some(changes.clone()) };
        let (exists, d) = read_tab(&repo, &changes, &entries, Scope::Staged).await;
        assert!(exists);
        assert_eq!((d.rename_from.as_deref(), d.similarity), (Some("a.txt"), Some(100)));
        assert_eq!(d.without_hunks().0, "Renamed");
        let (_, d) = read_tab(&repo, &changes, &entries, Scope::Unstaged).await;
        assert_eq!((d.rename_from.as_deref(), d.counts()), (None, (1, 0)));
        let (_, d) = read_tab(&repo, &changes, &entries, Scope::All).await;
        assert_eq!((d.rename_from.as_deref(), d.counts()), (Some("a.txt"), (1, 0)));

        std::fs::rename(repo.join("c.txt"), repo.join("d.txt")).unwrap();
        sh(&repo, &["add", "-N", "d.txt"]);
        let moved = entry(Group::Changes, "d.txt", ['.', 'R'], Some("c.txt"));
        let entries = Entries { staged: None, changes: Some(moved.clone()) };
        let (_, d) = read_tab(&repo, &moved, &entries, Scope::Unstaged).await;
        assert_eq!(d.rename_from.as_deref(), Some("c.txt"));
    }

    #[test]
    fn rows_fold_unchanged_lines_and_open_them_around_the_fold() {
        let d = diff::parse_unified(b"@@ -1,2 +1,2 @@\n-a\n+A\n b\n@@ -50,1 +50,1 @@\n-x\n+X\n");
        let gaps = diff::gaps(&d.hunks, Some(60));
        let folded = build_rows(&d, &gaps, &[(0, 0); 3], true, false);
        assert_eq!(
            folded,
            [
                Row::Header(0),
                Row::Line { hunk: 0, line: 0 },
                Row::Line { hunk: 0, line: 1 },
                Row::Line { hunk: 0, line: 2 },
                Row::Gap { gap: 1, hidden: 47 },
                Row::Header(1),
                Row::Line { hunk: 1, line: 0 },
                Row::Line { hunk: 1, line: 1 },
                Row::Gap { gap: 2, hidden: 10 },
            ]
        );
        let opened = build_rows(&d, &gaps, &[(0, 0), (2, 1), (10, 0)], true, false);
        assert_eq!(opened[4..8], [Row::Unchanged { new: 3, old: 3 }, Row::Unchanged { new: 4, old: 4 }, Row::Gap { gap: 1, hidden: 44 }, Row::Unchanged { new: 49, old: 49 }]);
        assert!(!opened.iter().any(|r| matches!(r, Row::Gap { gap: 2, .. })), "all of the last gap shows");
        // nothing to open them from: no folds
        assert!(!build_rows(&d, &gaps, &[(0, 0); 3], false, false).iter().any(|r| matches!(r, Row::Gap { .. })));
        // split: pairs
        let split = build_rows(&d, &gaps, &[(0, 0); 3], false, true);
        assert_eq!(split[..3], [Row::Header(0), Row::Pair { hunk: 0, old: Some(0), new: Some(1) }, Row::Pair { hunk: 0, old: Some(2), new: Some(2) }]);
    }

    #[test]
    fn changed_words_cut_the_runs() {
        let span = |start, end, token| Span { start, end, token };
        let d = Drawn::new("\tlet x = 1;", &[span(1, 4, Token::Keyword)]);
        // "    let x = 1;": the keyword at 4..7
        let cut = emphasised(&d, std::slice::from_ref(&(9..10)));
        assert_eq!(cut, [(0..4, Token::Plain, false), (4..7, Token::Keyword, false), (7..12, Token::Plain, false), (12..13, Token::Plain, true), (13..14, Token::Plain, false)]);
        assert_eq!(emphasised(&d, &[]).len(), d.runs.len());
    }
}
