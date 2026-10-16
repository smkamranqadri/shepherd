//! The Search tab: the root's files searched for fixed text as you type (`files::search`), 250 ms after the last
//! keystroke, with ripgrep, else git grep, else grep; the matches grouped by file under headers that fold, at most
//! `MAX_MATCHES` of them, within rg's deadline and output cap. Each search has a generation: a keystroke starts a new
//! one and drops the last (its task, and the process with it), and what a dropped one still sends is not heard.
//!
//! A file with unsaved changes in a tab is searched in its buffer rather than on disk, off the main thread and
//! within the same cap; its disk matches are dropped.
//!
//! A click on a match opens its file as a preview tab at the match, selected, and leaves the keyboard here (Up and
//! Down move through the matches); Enter or a double-click opens it pinned and gives it the keyboard. The query, its
//! switches and its globs are kept for each workspace while Shepherd runs.
use super::{SidePanel, SidePanelEvent};
use crate::files::icons::material_icon;
use crate::files::runner::{self, Cmd, Host, Item, RunError};
use crate::files::search::{self, Ended, Event, Hit, MAX_MATCHES, PathFilter, Query};
use crate::shell::widgets::tip;
use crate::theme::Theme;
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, Focusable, HighlightStyle, InteractiveElement, IntoElement, ParentElement, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, StyledText, Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px,
};
use gpui_base::input::{Escape, Input, InputEvent, InputState, MoveDown, Rope};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::AbortHandle;

/// How long typing must pause before a search starts.
const DEBOUNCE: Duration = Duration::from_millis(250);
const ROW_HEIGHT: f32 = 22.;
/// How long the selection must rest under Up and Down before its match opens as a preview: holding an arrow
/// reads no file for each match passed.
const PREVIEW_AFTER: Duration = Duration::from_millis(100);
/// The places rg could not read that are listed; the rest are counted.
const MAX_PROBLEMS: usize = 5;

/// The program a search ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Rg,
    GitGrep,
    Grep,
}

/// How a search ended, or that it has not.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Nothing asked (an empty query).
    Idle,
    Running,
    Done,
    /// It stopped before the end: why, as the line over the results says it.
    Capped(&'static str),
    /// It ended, but these places could not be read (`search::problems`).
    Problems(Vec<String>),
    Failed(String),
    /// Neither rg, git nor grep is installed.
    NoTool,
}

/// A match as its row shows it, worked out off the main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    line: u32,
    /// The first match's bytes in the line as it is on disk (or in the buffer): what is selected when it opens.
    first: Range<usize>,
    /// The line cut around its first match (`search::excerpt`), and the matches in it.
    shown: SharedString,
    marks: Vec<Range<usize>>,
}

impl Found {
    fn of(hit: Hit) -> Found {
        let (shown, marks) = search::excerpt(&hit.text, &hit.ranges);
        // a tab would draw as nothing: a space keeps the offsets
        let shown = shown.replace('\t', " ");
        Found { line: hit.line, first: hit.ranges.first().cloned().unwrap_or(0..0), shown: shown.into(), marks }
    }
}

/// What a search's task sends the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Update {
    /// rg is not installed: the program that searches instead.
    Tool(Tool),
    /// Matches, by their files' paths from the root; `unsaved` when they are from buffers with unsaved changes.
    Found { found: Vec<(String, Found)>, unsaved: bool },
    /// Lines over `search::MAX_RECORD` (a minified bundle's) left out.
    Skipped(usize),
    Done(Status),
}

/// One file's matches.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Group {
    path: String,
    found: Vec<Found>,
    unsaved: bool,
}

/// One row of the results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A file's header: its group.
    File(usize),
    /// A match: its group and its place in it.
    Match(usize, usize),
}

/// The last search's results, which only that search's updates land on.
#[derive(Debug)]
struct Results {
    generation: u64,
    groups: Vec<Group>,
    index: HashMap<String, usize>,
    matches: usize,
    /// Lines too long to read (`Update::Skipped`).
    skipped: usize,
    status: Status,
    tool: Tool,
}

impl Default for Results {
    fn default() -> Results {
        Results { generation: 0, groups: Vec::new(), index: HashMap::new(), matches: 0, skipped: 0, status: Status::Idle, tool: Tool::Rg }
    }
}

impl Results {
    /// A new search: the last one's results go, and whatever it still sends is not heard. Its generation.
    fn restart(&mut self, status: Status) -> u64 {
        let generation = self.generation + 1;
        *self = Results { generation, status, ..Default::default() };
        generation
    }

    /// `update` from the search `generation`; false (and nothing changed) when a newer search has started since.
    fn land(&mut self, generation: u64, update: Update) -> bool {
        if generation != self.generation {
            return false;
        }
        match update {
            Update::Tool(tool) => self.tool = tool,
            Update::Skipped(n) => self.skipped += n,
            Update::Done(status) => self.status = status,
            Update::Found { found, unsaved } => {
                for (path, f) in found {
                    let at = match self.index.get(&path) {
                        Some(&at) => at,
                        None => {
                            self.groups.push(Group { path: path.clone(), found: Vec::new(), unsaved });
                            self.index.insert(path, self.groups.len() - 1);
                            self.groups.len() - 1
                        }
                    };
                    self.groups[at].found.push(f);
                    self.matches += 1;
                }
            }
        }
        true
    }

    /// The rows: each file's header, then its matches unless it is `folded`.
    fn rows(&self, folded: &HashSet<String>) -> Vec<Row> {
        let mut rows = Vec::with_capacity(self.groups.len() + self.matches);
        for (g, group) in self.groups.iter().enumerate() {
            rows.push(Row::File(g));
            if !folded.contains(&group.path) {
                rows.extend((0..group.found.len()).map(|m| Row::Match(g, m)));
            }
        }
        rows
    }
}

/// A workspace's search, put back when it shows again.
#[derive(Debug, Clone, Default)]
struct Remembered {
    query: Query,
    filters: bool,
}

/// The query's inputs, made the first time the tab shows (they need the window).
struct Inputs {
    query: Entity<InputState>,
    include: Entity<InputState>,
    exclude: Entity<InputState>,
    _events: Vec<Subscription>,
}

/// The Search tab's state, in the side panel.
pub(super) struct SearchState {
    inputs: Option<Inputs>,
    case: bool,
    word: bool,
    /// The include and exclude inputs show (the "…" disclosure).
    filters: bool,
    /// The workspace (`memory_key`) and root the inputs and results are for.
    shown_for: Option<(String, PathBuf)>,
    memory: HashMap<String, Remembered>,
    results: Results,
    rows: Vec<Row>,
    folded: HashSet<String>,
    /// The root the results' paths are from.
    root: PathBuf,
    /// The timer of the next search, its task and what lands its updates; replaced by the next keystroke.
    debounce: Option<Task<()>>,
    job: Option<AbortHandle>,
    receive: Option<Task<()>>,
    /// The match row selected: its group and place.
    selected: Option<(usize, usize)>,
    /// The timer of the preview an arrow asked for (`PREVIEW_AFTER`); a click or Enter opens at once, dropping it.
    preview_wait: Option<Task<()>>,
    scroll: UniformListScrollHandle,
}

impl Default for SearchState {
    fn default() -> SearchState {
        SearchState {
            inputs: None,
            case: false,
            word: false,
            filters: false,
            shown_for: None,
            memory: HashMap::new(),
            results: Results::default(),
            rows: Vec::new(),
            folded: HashSet::new(),
            root: PathBuf::new(),
            debounce: None,
            job: None,
            receive: None,
            selected: None,
            preview_wait: None,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

impl Drop for SearchState {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.abort();
        }
    }
}

/// The buffers with unsaved changes, by their files' real paths: the Shell's, asked as a search starts.
pub type UnsavedBuffers = Box<dyn Fn(&App) -> Vec<(PathBuf, Rope)>>;

/// The program and arguments `tool` searches `q` with, in `root`.
fn command(tool: Tool, root: &Path, q: &Query) -> Cmd {
    match tool {
        Tool::Rg => Cmd { program: crate::ui::statusbar::tool("rg"), args: search::rg_args(q), cwd: root.to_path_buf(), ..Default::default() },
        Tool::GitGrep => {
            let args = search::git_grep_args(q);
            runner::git_cmd(root, &args.iter().map(String::as_str).collect::<Vec<_>>())
        }
        Tool::Grep => Cmd { program: crate::ui::statusbar::tool("grep"), args: search::grep_args(q), cwd: root.to_path_buf(), ..Default::default() },
    }
}

/// `path` from `root` (or its real path, as a tab's files are), `/` between folders; `None` outside it.
fn relative(path: &Path, root: &Path, real_root: Option<&Path>) -> Option<String> {
    let rel = real_root.and_then(|r| path.strip_prefix(r).ok()).or_else(|| path.strip_prefix(root).ok())?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The unsaved buffers of files in `root` that `filter` lets through, by their paths from it (blocking: the real
/// root is asked for).
fn unsaved_in_root(root: &Path, buffers: Vec<(PathBuf, Rope)>, filter: &PathFilter) -> Vec<(String, Rope)> {
    if buffers.is_empty() {
        return Vec::new();
    }
    let real_root = root.canonicalize().ok();
    buffers.into_iter().filter_map(|(path, rope)| Some((relative(&path, root, real_root.as_deref()).filter(|r| filter.allows(r))?, rope))).collect()
}

/// `git check-ignore`: a question per path, and its paths are a few open tabs.
const CHECK_IGNORE: runner::Limits = runner::Limits { timeout: Duration::from_secs(5), max_bytes: 1 << 20 };

/// Which of `paths` (from `root`) git ignores, as rg (and git grep) would skip them; none outside a repository or
/// when git cannot say. Hidden files need no such check: every search here looks in them (rg's `--hidden`).
async fn ignored(root: &Path, paths: &[String]) -> HashSet<String> {
    if paths.is_empty() {
        return HashSet::new();
    }
    let mut stdin = Vec::new();
    for p in paths {
        stdin.extend_from_slice(p.as_bytes());
        stdin.push(0);
    }
    let cmd = Cmd { stdin: Some(stdin), ..runner::git_cmd(root, &["check-ignore", "--stdin", "-z"]) };
    match runner::run(&Host::Local, cmd, CHECK_IGNORE).await {
        // 0: some ignored, 1: none
        Ok(out) if out.code == Some(0) => out.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect(),
        _ => HashSet::new(),
    }
}

/// The buffers' matches, at most `budget`, and the paths of every buffer searched (whose disk matches go).
fn search_buffers(buffers: Vec<(String, Rope)>, q: &Query, budget: usize) -> (Vec<Hit>, HashSet<String>) {
    let (mut hits, mut searched) = (Vec::new(), HashSet::new());
    for (rel, rope) in buffers {
        let mut text = String::with_capacity(rope.len());
        for chunk in rope.chunks() {
            text.push_str(chunk);
        }
        hits.extend(search::search_text(&rel, &text, q, budget.saturating_sub(hits.len())));
        searched.insert(rel);
    }
    (hits, searched)
}

/// The unsaved buffers' matches, as rg would find them in their files: in the root, through the globs, not ignored.
async fn search_unsaved(root: &Path, buffers: Vec<(PathBuf, Rope)>, q: &Query, filter: Arc<PathFilter>, budget: usize) -> (Vec<Hit>, HashSet<String>) {
    let r = root.to_path_buf();
    let inside = tokio::task::spawn_blocking(move || unsaved_in_root(&r, buffers, &filter)).await.unwrap_or_default();
    let paths: Vec<String> = inside.iter().map(|(p, _)| p.clone()).collect();
    let ignored = ignored(root, &paths).await;
    let kept = inside.into_iter().filter(|(p, _)| !ignored.contains(p)).collect();
    let q = q.clone();
    tokio::task::spawn_blocking(move || search_buffers(kept, &q, budget)).await.unwrap_or_default()
}

/// The tool after `tool` when it is not installed (or, for git grep, the root is not in a repository).
fn fallback(tool: Tool) -> Option<Tool> {
    match tool {
        Tool::Rg => Some(Tool::GitGrep),
        Tool::GitGrep => Some(Tool::Grep),
        Tool::Grep => None,
    }
}

/// grep's and git grep's own complaints, a line each without the program's name.
fn complaints(stderr: &str, program: &str) -> Vec<String> {
    let prefix = format!("{program}: ");
    stderr.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| l.strip_prefix(&prefix).unwrap_or(l).to_string()).collect()
}

/// Searches `root` for `q`: the unsaved buffers first, then `first` (rg; the others when it is missing), every match
/// through `filter`, at most `MAX_MATCHES` in all. Runs on the runtime; its updates go to `tx`, and it stops when
/// `tx` is dropped or its task aborted (and its process with it).
async fn run(root: PathBuf, q: Query, filter: Arc<PathFilter>, unsaved: Vec<(PathBuf, Rope)>, first: Tool, tx: mpsc::UnboundedSender<Update>) {
    let send = |u: Update| tx.unbounded_send(u).is_ok();
    let found = |hits: Vec<Hit>| hits.into_iter().map(|h| (h.path.clone(), Found::of(h))).collect::<Vec<_>>();
    let (hits, skip) = search_unsaved(&root, unsaved, &q, filter.clone(), MAX_MATCHES).await;
    let mut budget = MAX_MATCHES - hits.len().min(MAX_MATCHES);
    if !hits.is_empty() && !send(Update::Found { found: found(hits), unsaved: true }) {
        return;
    }
    if budget == 0 {
        send(Update::Done(Status::Capped("Showing the first 1000 matches")));
        return;
    }
    let mut tool = first;
    if tool != Tool::Rg {
        send(Update::Tool(tool));
    }
    let status = 'tools: loop {
        let handle = tokio::runtime::Handle::current();
        let (mut rx, _abort) = runner::stream_lines(&Host::Local, command(tool, &root, &q), search::LIMITS, Some(search::MAX_RECORD), &handle);
        let status = loop {
            let Some(item) = rx.next().await else { break Status::Failed("The search stopped".into()) };
            match item {
                Ok(Item::Lines(lines)) => {
                    let mut hits = Vec::new();
                    for line in lines {
                        let hit = match tool {
                            Tool::Rg => match search::parse_rg_line(&line) {
                                Some(Event::Match(hit)) => Some(hit),
                                _ => None,
                            },
                            Tool::GitGrep => search::parse_git_grep(&line, &q),
                            Tool::Grep => search::parse_grep(&line, &q),
                        };
                        let Some(hit) = hit.filter(|h| !skip.contains(&h.path) && filter.allows(&h.path)) else { continue };
                        hits.push(hit);
                        if hits.len() >= budget {
                            break;
                        }
                    }
                    budget -= hits.len();
                    if !hits.is_empty() && !send(Update::Found { found: found(hits), unsaved: false }) {
                        return;
                    }
                    if budget == 0 {
                        // dropping the stream stops the process
                        break Status::Capped("Showing the first 1000 matches");
                    }
                }
                Ok(Item::Skipped(n)) => {
                    send(Update::Skipped(n));
                }
                Ok(Item::Exit { code, stderr }) => {
                    break match (tool, code) {
                        (Tool::Rg, _) => match search::ended(code, &stderr) {
                            Ended::Ok => Status::Done,
                            Ended::Problems(p) => Status::Problems(p),
                            Ended::Failed(e) => Status::Failed(e),
                        },
                        (_, Some(0 | 1)) => Status::Done,
                        // not a repository (or one git will not read): grep
                        (Tool::GitGrep, Some(c)) if c >= 128 => {
                            tool = Tool::Grep;
                            send(Update::Tool(tool));
                            continue 'tools;
                        }
                        (Tool::Grep, Some(2)) if !complaints(&stderr, "grep").is_empty() => Status::Problems(complaints(&stderr, "grep")),
                        (t, _) => {
                            let program = if t == Tool::Grep { "grep" } else { "git" };
                            Status::Failed(complaints(&stderr, program).into_iter().next().unwrap_or_else(|| format!("{program} failed")))
                        }
                    };
                }
                Err(RunError::NotFound(_)) => match fallback(tool) {
                    Some(next) => {
                        tool = next;
                        send(Update::Tool(tool));
                        continue 'tools;
                    }
                    None => break Status::NoTool,
                },
                Err(RunError::Truncated) => break Status::Capped("Showing what fit in 5 MiB of results"),
                Err(RunError::Timeout) => break Status::Capped("Stopped after 10 seconds"),
                Err(e) => break Status::Failed(e.to_string()),
            }
        };
        break status;
    };
    send(Update::Done(status));
}

/// A workspace's key among the remembered searches.
fn memory_key(context: &super::PanelContext) -> String {
    format!("{}\n{}", context.place, context.workspace_id.as_deref().unwrap_or_default())
}

impl SidePanel {
    /// The Shell's word on where unsaved buffers are.
    pub fn set_unsaved_buffers(&mut self, unsaved: UnsavedBuffers) {
        self.unsaved = Some(unsaved);
    }

    /// The inputs, made once.
    fn search_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.inputs.is_some() {
            return;
        }
        let theme = Theme::of(cx).clone();
        let mut field = |placeholder: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                input.set_editor_style(crate::ui::input_style(&theme));
                input
            })
        };
        let (query, include, exclude) = (field("Search", cx), field("Files to include, e.g. *.rs, src/ui, ./docs", cx), field("Files to exclude", cx));
        let typed = |p: &mut SidePanel, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<SidePanel>| match event {
            InputEvent::Change => p.schedule_search(DEBOUNCE, cx),
            // Enter searches now rather than after the pause
            InputEvent::PressEnter { .. } => p.schedule_search(Duration::ZERO, cx),
            _ => {}
        };
        let events = vec![cx.subscribe_in(&query, window, typed), cx.subscribe_in(&include, window, typed), cx.subscribe_in(&exclude, window, typed)];
        self.search.inputs = Some(Inputs { query, include, exclude, _events: events });
    }

    /// The query as the inputs and switches have it.
    fn query(&self, cx: &App) -> Query {
        let Some(i) = self.search.inputs.as_ref() else { return Query::default() };
        Query {
            text: i.query.read(cx).value().to_string(),
            case: self.search.case,
            word: self.search.word,
            include: i.include.read(cx).value().to_string(),
            exclude: i.exclude.read(cx).value().to_string(),
        }
    }

    /// Keeps the inputs and results on the shown workspace and root: another workspace's search is put back (and
    /// run again), this one's kept for its return; another root searches again.
    fn sync_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.context.root.as_deref().filter(|_| !self.context.remote).map(PathBuf::from) else { return };
        let key = memory_key(&self.context);
        if self.search.shown_for.as_ref().is_some_and(|(k, r)| *k == key && *r == root) {
            return;
        }
        self.search_inputs(window, cx);
        let old = self.search.shown_for.replace((key.clone(), root)).map(|(k, _)| k);
        if old.as_ref() != Some(&key) {
            if let Some(old) = old {
                let kept = Remembered { query: self.query(cx), filters: self.search.filters };
                self.search.memory.insert(old, kept);
            }
            let back = self.search.memory.get(&key).cloned().unwrap_or_default();
            (self.search.case, self.search.word, self.search.filters) = (back.query.case, back.query.word, back.filters);
            self.search.folded.clear();
            if let Some(i) = self.search.inputs.as_ref() {
                let (query, include, exclude) = (i.query.clone(), i.include.clone(), i.exclude.clone());
                query.update(cx, |s, cx| s.set_value(back.query.text.clone(), window, cx));
                include.update(cx, |s, cx| s.set_value(back.query.include.clone(), window, cx));
                exclude.update(cx, |s, cx| s.set_value(back.query.exclude.clone(), window, cx));
            }
        }
        self.schedule_search(Duration::ZERO, cx);
    }

    /// ShowSearch: the tab's query takes the keyboard, its text selected to type over.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_inputs(window, cx);
        self.sync_search(window, cx);
        if let Some(i) = self.search.inputs.as_ref() {
            i.query.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        }
        cx.notify();
    }

    /// The query has the keyboard (ShowSearch again then closes the panel).
    pub fn query_focused(&self, window: &Window, cx: &App) -> bool {
        self.search.inputs.as_ref().is_some_and(|i| i.query.read(cx).focus_handle(cx).is_focused(window))
    }

    /// A search after `delay`: the last one, its timer and its process stop now. It always starts from a timer, so
    /// never inside the Shell's own update (the unsaved buffers are the Shell's to read).
    fn schedule_search(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let generation = self.stop_search();
        self.search.debounce = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let _ = this.update(cx, |p, cx| p.start_search(generation, cx));
        }));
    }

    /// Stops the search out, and makes way for the next one: the generation that one will have.
    fn stop_search(&mut self) -> u64 {
        if let Some(job) = self.search.job.take() {
            job.abort();
        }
        self.search.receive = None;
        self.search.results.generation + 1
    }

    fn start_search(&mut self, generation: u64, cx: &mut Context<Self>) {
        if generation != self.search.results.generation + 1 {
            return;
        }
        self.search.selected = None;
        let Some(root) = self.search.shown_for.as_ref().map(|(_, r)| r.clone()) else { return };
        let q = self.query(cx);
        if q.text.is_empty() {
            self.search.results.restart(Status::Idle);
            self.rebuild_search(cx);
            return;
        }
        let filter = match PathFilter::new(&q) {
            Ok(f) => Arc::new(f),
            Err(e) => {
                self.search.results.restart(Status::Failed(format!("Not a glob: {e}")));
                self.rebuild_search(cx);
                return;
            }
        };
        let unsaved = self.unsaved.as_ref().map(|f| f(cx)).unwrap_or_default();
        let generation = self.search.results.restart(Status::Running);
        self.search.root = root.clone();
        let (tx, mut rx) = mpsc::unbounded();
        let task = self.rt.spawn(run(root, q, filter, unsaved, Tool::Rg, tx));
        self.search.job = Some(task.abort_handle());
        self.search.receive = Some(cx.spawn(async move |this, cx| {
            while let Some(update) = rx.next().await {
                if this.update(cx, |p, cx| p.take_search(generation, update, cx)).is_err() {
                    return;
                }
            }
        }));
        self.rebuild_search(cx);
    }

    fn take_search(&mut self, generation: u64, update: Update, cx: &mut Context<Self>) {
        let done = matches!(update, Update::Done(_));
        if self.search.results.land(generation, update) {
            if done {
                self.search.job = None;
            }
            self.rebuild_search(cx);
        }
    }

    fn rebuild_search(&mut self, cx: &mut Context<Self>) {
        self.search.rows = self.search.results.rows(&self.search.folded);
        cx.notify();
    }

    fn toggle_search_file(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.search.folded.remove(&path) {
            self.search.folded.insert(path);
        }
        self.rebuild_search(cx);
    }

    /// A match to the Shell now: a preview at it (the keyboard stays here), or pinned with the keyboard. A preview
    /// an arrow asked for and not yet opened is dropped.
    fn open_found(&mut self, g: usize, m: usize, preview: bool, cx: &mut Context<Self>) {
        self.search.preview_wait = None;
        self.emit_found(g, m, preview, cx);
    }

    /// The selected match as a preview once the selection has rested `PREVIEW_AFTER` (Up, Down).
    fn preview_soon(&mut self, at: (usize, usize), cx: &mut Context<Self>) {
        self.search.preview_wait = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_AFTER).await;
            let _ = this.update(cx, |p, cx| {
                if p.search.selected == Some(at) {
                    p.emit_found(at.0, at.1, true, cx);
                }
            });
        }));
    }

    fn emit_found(&mut self, g: usize, m: usize, preview: bool, cx: &mut Context<Self>) {
        let Some(group) = self.search.results.groups.get(g) else { return };
        let Some(found) = group.found.get(m) else { return };
        let path = self.search.root.join(&group.path);
        cx.emit(SidePanelEvent::OpenMatch { path, line: found.line, cols: found.first.clone(), preview });
    }

    fn select_found(&mut self, at: (usize, usize), cx: &mut Context<Self>) {
        self.search.selected = Some(at);
        if let Some(ix) = self.search.rows.iter().position(|r| *r == Row::Match(at.0, at.1)) {
            self.search.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// The results' keys while they have the keyboard: Up and Down move through the matches (each shown as a
    /// preview once the selection rests), Enter opens the selected one and gives it the keyboard. True when the key
    /// was theirs.
    pub(super) fn search_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let matches: Vec<(usize, usize)> = self.search.rows.iter().filter_map(|r| if let Row::Match(g, m) = r { Some((*g, *m)) } else { None }).collect();
        let (Some(&first), Some(&last)) = (matches.first(), matches.last()) else { return false };
        let at = self.search.selected.and_then(|s| matches.iter().position(|&m| m == s));
        let to = match key {
            "down" => at.map_or(first, |i| matches.get(i + 1).copied().unwrap_or(last)),
            "up" => at.map_or(first, |i| matches[i.saturating_sub(1)]),
            "home" => first,
            "end" => last,
            "enter" => {
                let at = self.search.selected.unwrap_or(first);
                self.search.selected = Some(at);
                self.open_found(at.0, at.1, false, cx);
                return true;
            }
            _ => return false,
        };
        if Some(to) != self.search.selected {
            self.select_found(to, cx);
            self.preview_soon(to, cx);
        }
        true
    }

    /// Down in the query: the results take the keyboard, at their first match.
    fn enter_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self.search.rows.iter().find_map(|r| if let Row::Match(g, m) = r { Some((*g, *m)) } else { None });
        if let Some(first) = first {
            window.focus(&self.focus, cx);
            self.select_found(first, cx);
            self.preview_soon(first, cx);
        }
    }

    /// Esc in the query clears it; an empty one lets Esc through (the keyboard back to the pane).
    fn clear_query(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(query) = self.search.inputs.as_ref().map(|i| i.query.clone()) else { return false };
        if query.read(cx).value().is_empty() {
            return false;
        }
        query.update(cx, |s, cx| s.set_value("", window, cx));
        self.schedule_search(Duration::ZERO, cx);
        true
    }

    /// The Search tab's body: the query, its switches and globs, a line on how the search went, the results.
    pub(super) fn render_search(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.sync_search(window, cx);
        let Some(inputs) = self.search.inputs.as_ref() else { return div().into_any_element() };
        let field = |input: &Entity<InputState>| {
            div()
                .h(px(26.))
                .flex_1()
                .min_w_0()
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .bg(theme.wash(0.06))
                .text_size(px(12.5))
                .child(div().flex_1().min_w_0().child(Input::new(input)))
        };
        let hover = theme.hover;
        let switch = |id: &'static str, label: &'static str, on: bool, what: &'static str| {
            div()
                .id(id)
                .size(px(22.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(if on { theme.accent } else { theme.text_muted })
                .when(on, |el| el.bg(theme.accent.opacity(0.15)))
                .when(!on, |el| el.hover(move |s| s.bg(hover)))
                .cursor_pointer()
                .child(label)
                .tooltip(tip(what, None))
        };
        let globs = !inputs.include.read(cx).value().is_empty() || !inputs.exclude.read(cx).value().is_empty();
        let (case, word, filters) = (self.search.case, self.search.word, self.search.filters);
        let query_row = div()
            .flex()
            .items_center()
            .gap(px(2.))
            // Down from the query into the results, before the input's own Down
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                if this.search.rows.iter().any(|r| matches!(r, Row::Match(..))) {
                    cx.stop_propagation();
                    this.enter_results(window, cx);
                }
            }))
            .child(div().flex_1().min_w_0().mr(px(2.)).child(field(&inputs.query)))
            .child(switch("search-case", "Aa", case, "Match Case").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.search.case = !this.search.case;
                this.schedule_search(Duration::ZERO, cx);
            })))
            .child(switch("search-word", "ab", word, "Match Whole Word").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.search.word = !this.search.word;
                this.schedule_search(Duration::ZERO, cx);
            })))
            .child(switch("search-filters", "…", filters || globs, "Files to Include and Exclude").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.search.filters = !this.search.filters;
                cx.notify();
            })));
        let header = div()
            .flex_none()
            .px(px(8.))
            .pt(px(8.))
            .pb(px(4.))
            .flex()
            .flex_col()
            .gap(px(4.))
            // Esc clears a query before the input lets it through to the panel (and the panel to the pane)
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.query_focused(window, cx) && this.clear_query(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(query_row)
            .when(filters, |el| el.child(div().flex().child(field(&inputs.include))).child(div().flex().child(field(&inputs.exclude))));
        let body = self.search_body(theme, cx);
        div().size_full().flex().flex_col().child(header).children(self.search_status(theme)).child(body).into_any_element()
    }

    /// The faint lines over the results: how many, why they stopped, what searched, what could not be read.
    fn search_status(&self, theme: &Theme) -> Vec<AnyElement> {
        let r = &self.search.results;
        let line = |text: String, color| div().flex_none().px(px(12.)).py(px(2.)).text_size(px(11.5)).text_color(color).truncate().child(text).into_any_element();
        let mut lines = Vec::new();
        let files = r.groups.len();
        let count = match (r.matches, files) {
            (1, _) => "1 result in 1 file".to_string(),
            (n, 1) => format!("{n} results in 1 file"),
            (n, f) => format!("{n} results in {f} files"),
        };
        match &r.status {
            Status::Running if r.matches == 0 => lines.push(line("Searching…".into(), theme.text_faint)),
            Status::Running | Status::Done if r.matches > 0 => lines.push(line(count, theme.text_muted)),
            Status::Capped(why) => {
                lines.push(line(count, theme.text_muted));
                lines.push(line(why.to_string(), theme.warning));
            }
            Status::Problems(problems) => {
                if r.matches > 0 {
                    lines.push(line(count, theme.text_muted));
                }
                let what = if problems.len() == 1 { "1 place couldn't be searched".to_string() } else { format!("{} places couldn't be searched", problems.len()) };
                lines.push(line(what, theme.warning));
                lines.extend(problems.iter().take(MAX_PROBLEMS).map(|p| line(p.clone(), theme.text_faint)));
                if problems.len() > MAX_PROBLEMS {
                    lines.push(line(format!("and {} more", problems.len() - MAX_PROBLEMS), theme.text_faint));
                }
            }
            _ => {}
        }
        if r.skipped > 0 {
            let what = if r.skipped == 1 { "1 line over 64 KiB skipped".to_string() } else { format!("{} lines over 64 KiB skipped", r.skipped) };
            lines.push(line(what, theme.text_faint));
        }
        match r.tool {
            Tool::GitGrep if r.status != Status::NoTool => lines.push(line("using git grep (ripgrep not installed)".into(), theme.text_faint)),
            Tool::Grep if r.status != Status::NoTool => lines.push(line("using grep (ripgrep not installed)".into(), theme.text_faint)),
            _ => {}
        }
        lines
    }

    fn search_body(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let r = &self.search.results;
        match &r.status {
            Status::Failed(e) => return super::notice(theme, "Search failed", e.clone()).into_any_element(),
            Status::NoTool => {
                return super::notice(theme, "ripgrep isn't installed", "Install ripgrep (the ripgrep package) to search this folder.").into_any_element();
            }
            Status::Done if r.matches == 0 => return super::notice(theme, "No results", "").into_any_element(),
            _ if self.search.rows.is_empty() => return div().into_any_element(),
            _ => {}
        }
        let rows = gpui::uniform_list("search-results", self.search.rows.len(), cx.processor(|this: &mut SidePanel, range: Range<usize>, window, cx| this.search_rows(range, window, cx)))
            .track_scroll(&self.search.scroll)
            .size_full()
            .pb(px(4.));
        let scroll = self.search.scroll.0.borrow().base_handle.clone();
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(crate::ui::edge_fade::edge_faded(12., &scroll, rows))
            .child(crate::ui::scrollbar("search-scrollbar", &self.search.scroll, theme))
            .into_any_element()
    }

    /// The rows the list shows of `range`.
    fn search_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let hover = theme.hover;
        let mark = HighlightStyle { background_color: Some(theme.accent.opacity(0.3)), color: Some(theme.text), ..Default::default() };
        let r = &self.search.results;
        range
            .filter_map(|ix| Some((ix, *self.search.rows.get(ix)?)))
            .filter_map(|(ix, row)| match row {
                Row::File(g) => {
                    let group = r.groups.get(g)?;
                    let folded = self.search.folded.contains(&group.path);
                    let turn = gpui::Transformation::rotate(gpui::percentage(if folded { 0. } else { 0.25 }));
                    let (dir, name) = group.path.rsplit_once('/').unwrap_or(("", &group.path));
                    let path = group.path.clone();
                    let tooltip = if group.unsaved { format!("{}\nSearched in its unsaved changes", group.path) } else { group.path.clone() };
                    Some(
                        div()
                            .id(("search-file", ix))
                            .h(px(ROW_HEIGHT))
                            .w_full()
                            .pl(px(8.))
                            .pr(px(12.))
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .child(
                                div()
                                    .size(px(16.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(gpui::svg().path("icons/alt-arrow-right.svg").size(px(12.)).text_color(theme.text_muted.opacity(0.6)).with_transformation(turn)),
                            )
                            // a coloured SVG, so img(), not the tinted svg() (as the Files tree)
                            .child(gpui::img(material_icon(name, false, false)).size(px(16.)).flex_none().object_fit(gpui::ObjectFit::Contain))
                            .child(div().flex_none().max_w(px(220.)).truncate().text_size(px(12.5)).text_color(theme.text).child(name.to_string()))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).text_color(theme.text_faint).child(dir.to_string()))
                            .when(group.unsaved, |el| el.child(div().flex_none().text_size(px(11.)).text_color(theme.text_faint).child("●")))
                            .child(div().flex_none().text_size(px(11.)).text_color(theme.text_faint).child(group.found.len().to_string()))
                            .tooltip(tip(tooltip, None))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_search_file(path.clone(), cx)))
                            .into_any_element(),
                    )
                }
                Row::Match(g, m) => {
                    let found = r.groups.get(g)?.found.get(m)?;
                    let selected = self.search.selected == Some((g, m));
                    let marks: Vec<_> = found.marks.iter().filter(|m| found.shown.is_char_boundary(m.start) && found.shown.is_char_boundary(m.end)).map(|m| (m.clone(), mark)).collect();
                    Some(
                        div()
                            .id(("search-match", ix))
                            .h(px(ROW_HEIGHT))
                            .w_full()
                            .pl(px(28.))
                            .pr(px(12.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .when(selected, |el| el.bg(theme.selected))
                            .when(!selected, |el| el.hover(move |s| s.bg(hover)))
                            .child(div().flex_none().min_w(px(20.)).text_right().text_size(px(11.)).text_color(theme.text_faint).child(found.line.to_string()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_size(px(12.))
                                    .text_color(theme.text_muted)
                                    .child(StyledText::new(found.shown.clone()).with_highlights(marks)),
                            )
                            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                                // the results keep the keyboard (Up, Down, Enter); a double-click's second gives it
                                // to the file, pinned
                                window.focus(&this.focus, cx);
                                this.search.selected = Some((g, m));
                                this.open_found(g, m, e.click_count() < 2, cx);
                                cx.notify();
                            }))
                            .into_any_element(),
                    )
                }
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn found(line: u32) -> Found {
        Found { line, first: 0..1, shown: "x".into(), marks: vec![0..1] }
    }

    #[test]
    fn a_stale_search_is_not_heard() {
        let mut results = Results::default();
        let old = results.restart(Status::Running);
        let new = results.restart(Status::Running);
        assert!(new > old);
        let update = |path: &str| Update::Found { found: vec![(path.to_string(), found(1))], unsaved: false };
        assert!(!results.land(old, update("old.rs")));
        assert!(!results.land(old, Update::Done(Status::Done)));
        assert_eq!((results.matches, &results.status), (0, &Status::Running));
        assert!(results.land(new, update("a.rs")));
        assert!(results.land(new, update("b.rs")));
        assert!(results.land(new, Update::Found { found: vec![("a.rs".into(), found(9))], unsaved: false }));
        assert!(results.land(new, Update::Done(Status::Done)));
        assert_eq!(results.groups.iter().map(|g| (g.path.as_str(), g.found.len())).collect::<Vec<_>>(), [("a.rs", 2), ("b.rs", 1)]);
        assert_eq!(results.status, Status::Done);
        let rows = results.rows(&HashSet::from(["a.rs".to_string()]));
        assert_eq!(rows, [Row::File(0), Row::File(1), Row::Match(1, 0)]);
        // a restart clears what the last one found
        results.restart(Status::Idle);
        assert!(results.groups.is_empty() && results.matches == 0);
    }

    async fn collect(root: &Path, q: Query, unsaved: Vec<(PathBuf, Rope)>, first: Tool) -> Vec<Update> {
        let filter = Arc::new(PathFilter::new(&q).unwrap());
        let (tx, rx) = mpsc::unbounded();
        run(root.to_path_buf(), q, filter, unsaved, first, tx).await;
        rx.collect().await
    }

    fn paths(updates: &[Update]) -> Vec<(String, u32, bool)> {
        let mut out: Vec<_> = updates
            .iter()
            .flat_map(|u| match u {
                Update::Found { found, unsaved } => found.iter().map(|(p, f)| (p.clone(), f.line, *unsaved)).collect(),
                _ => Vec::new(),
            })
            .collect();
        out.sort();
        out
    }

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn needle() {}\n").unwrap();
        std::fs::write(dir.path().join("src/deep/b.rs"), "x\nNeedle\n").unwrap();
        std::fs::write(dir.path().join("docs/c.md"), "needle\n").unwrap();
        dir
    }

    /// A buffer of a file git ignores is not searched, as rg would not search the file; one of a hidden file is.
    #[tokio::test]
    async fn unsaved_buffers_follow_the_ignore_files() {
        if !has("git") {
            return;
        }
        let dir = tree();
        let init = std::process::Command::new("git").args(["init", "-q"]).current_dir(dir.path()).env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").status().unwrap();
        assert!(init.success());
        std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
        std::fs::create_dir_all(dir.path().join("build")).unwrap();
        std::fs::write(dir.path().join("build/out.rs"), "").unwrap();
        let q = Query { text: "needle".into(), ..Default::default() };
        let buffers = vec![
            (dir.path().join("build/out.rs"), Rope::from("needle")),
            (dir.path().join(".hidden.rs"), Rope::from("needle")),
            (dir.path().join("src/a.rs"), Rope::from("needle")),
        ];
        let (hits, searched) = search_unsaved(dir.path(), buffers, &q, Arc::new(PathFilter::new(&q).unwrap()), 10).await;
        let mut paths: Vec<_> = hits.iter().map(|h| h.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, [".hidden.rs", "src/a.rs"]);
        assert!(!searched.contains("build/out.rs"));
    }

    fn has(program: &str) -> bool {
        std::process::Command::new(program).arg("--version").output().is_ok_and(|o| o.status.success())
    }

    #[tokio::test]
    async fn rg_searches_with_full_path_includes_and_unsaved_buffers() {
        if !has("rg") {
            return;
        }
        let dir = tree();
        let q = Query { text: "needle".into(), include: "src/**/*.rs".into(), ..Default::default() };
        let got = collect(dir.path(), q.clone(), Vec::new(), Tool::Rg).await;
        assert_eq!(paths(&got), [("src/a.rs".into(), 1, false), ("src/deep/b.rs".into(), 2, false)]);
        assert_eq!(got.last(), Some(&Update::Done(Status::Done)));
        // an unsaved buffer of src/a.rs: its lines, not the disk's
        let real = dir.path().canonicalize().unwrap().join("src/a.rs");
        let got = collect(dir.path(), q.clone(), vec![(real, Rope::from("one\ntwo needle\n"))], Tool::Rg).await;
        assert_eq!(paths(&got), [("src/a.rs".into(), 2, true), ("src/deep/b.rs".into(), 2, false)]);
        // case and exclude
        let q = Query { text: "needle".into(), case: true, exclude: "docs/".into(), ..Default::default() };
        assert_eq!(paths(&collect(dir.path(), q, Vec::new(), Tool::Rg).await), [("src/a.rs".into(), 1, false)]);
    }

    #[tokio::test]
    async fn outside_a_repository_git_grep_gives_way_to_grep() {
        if !has("git") || !has("grep") {
            return;
        }
        let dir = tree();
        let q = Query { text: "needle".into(), word: true, include: "*.rs".into(), ..Default::default() };
        let got = collect(dir.path(), q, Vec::new(), Tool::GitGrep).await;
        assert!(got.contains(&Update::Tool(Tool::Grep)), "{got:?}");
        assert_eq!(paths(&got), [("src/a.rs".into(), 1, false), ("src/deep/b.rs".into(), 2, false)]);
        assert_eq!(got.last(), Some(&Update::Done(Status::Done)));
    }

    #[test]
    fn a_buffer_outside_the_root_is_not_searched() {
        let dir = tree();
        let q = Query { text: "needle".into(), ..Default::default() };
        let filter = PathFilter::new(&q).unwrap();
        let elsewhere = std::env::temp_dir().join("shepherd-elsewhere.rs");
        let inside = unsaved_in_root(dir.path(), vec![(elsewhere, Rope::from("needle")), (dir.path().join("docs/c.md"), Rope::from("a\nneedle"))], &filter);
        let (hits, searched) = search_buffers(inside, &q, 10);
        assert_eq!(searched, HashSet::from(["docs/c.md".to_string()]));
        assert_eq!(hits.iter().map(|h| (h.path.as_str(), h.line)).collect::<Vec<_>>(), [("docs/c.md", 2)]);
    }
}
