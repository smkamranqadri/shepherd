//! The Files tab: the workspace root's tree, one folder read when it is opened (`files::tree`), what git ignores
//! asked of `git check-ignore`, and the open folders read again when the disk says they changed (`files::watch`),
//! or every few seconds while the tab is in view when it cannot say (every `watch::SAFETY_POLL` when it can). Every
//! read runs on the runtime, never on the main thread (a folder on a hung network mount blocks its reader), and
//! carries the generation its folder had when it started: an answer that a newer read of the folder overtook is
//! dropped, as is one for a tree since replaced by another root's. One poll or watch read is out at a time; the
//! changes heard meanwhile are read together when it lands.
use super::{SidePanel, SidePanelEvent};
use crate::files::icons::material_icon;
use crate::files::runner::{self, Host, Limits, RunError};
use crate::files::tree::{self, Listing, Read, Row};
use crate::files::watch::{self, Batch, Loads};
use crate::settings::SettingsGlobal;
use crate::shell::widgets::{menu_card, menu_choice};
use crate::theme::Theme;
use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, ScrollStrategy,
    StatefulInteractiveElement, Styled, UniformListScrollHandle, Window, div, prelude::*, px,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How often the open folders are read again while the tab is in view and the window active, when the root is not
/// watched (the watcher did not start, or gave up): just the folders on screen.
pub(super) const POLL: Duration = Duration::from_secs(3);
/// A poll reads at most this many open folders: the root, then the open ones in the order they are drawn.
const MAX_POLLED: usize = 200;
const ROW_HEIGHT: f32 = 22.;
/// The list's padding above its first row (and below its last).
const LIST_PAD: f32 = 4.;
const INDENT: f32 = 12.;
/// `git check-ignore` gets 20k names at most (a folder's cap); it answers with as many.
const CHECK_IGNORE: Limits = Limits { timeout: Duration::from_secs(10), max_bytes: 1 << 20 };

/// Workspaces whose Files tree is remembered when another root replaces it (`FilesMemories`).
const FILES_MEMORY: usize = 64;

/// What a workspace's Files tree showed when another root replaced it: the folders open, the row selected and the
/// row at the top. Given back only to a tree of the same root.
#[derive(Debug, Clone, PartialEq)]
pub struct FilesMemory {
    pub root: PathBuf,
    pub expanded: Vec<PathBuf>,
    pub selected: Option<PathBuf>,
    pub top: Option<usize>,
}

/// Each workspace's [`FilesMemory`] by its key (`roots::root_key`), most recently left first, at most
/// `FILES_MEMORY`.
#[derive(Debug, Default)]
pub(super) struct FilesMemories(Vec<(String, FilesMemory)>);

impl FilesMemories {
    /// Keeps `memory` for `key` as the most recent; a tree with no workspace (`""`) is not kept.
    pub(super) fn stash(&mut self, key: &str, memory: FilesMemory) {
        if key.is_empty() {
            return;
        }
        self.0.retain(|(k, _)| k != key);
        self.0.insert(0, (key.to_string(), memory));
        self.0.truncate(FILES_MEMORY);
    }

    /// `key`'s memory, when it was of `root`: a workspace given another root starts afresh.
    pub(super) fn recall(&self, key: &str, root: &Path) -> Option<&FilesMemory> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, m)| m).filter(|m| m.root == root)
    }

    /// Kept from the last run: only where this run has none, behind this run's.
    pub(super) fn seed(&mut self, key: String, memory: FilesMemory) {
        if !key.is_empty() && self.0.len() < FILES_MEMORY && !self.0.iter().any(|(k, _)| *k == key) {
            self.0.push((key, memory));
        }
    }

    pub(super) fn forget(&mut self, keys: &[String]) {
        self.0.retain(|(k, _)| !keys.contains(k));
    }

    pub(super) fn entries(&self) -> &[(String, FilesMemory)] {
        &self.0
    }
}

/// One root's tree: what was read of it, what is open, and the rows that makes.
pub(super) struct FileTree {
    /// Told apart from the tree of a root chosen since, whose reads must not land here.
    id: u64,
    pub(super) root: PathBuf,
    /// The workspace it shows (`roots::root_key`; `""` for none), whose memory it is kept as.
    memory_key: String,
    /// The row to scroll to the top once the remembered folders' reads land.
    pending_top: Option<usize>,
    reads: HashMap<PathBuf, Read>,
    /// Each read folder's `Listing::fingerprint` (or its error's): a poll hands these over and hears back only
    /// what changed.
    fingerprints: HashMap<PathBuf, u64>,
    expanded: HashSet<PathBuf>,
    /// Each folder's generation, bumped by every read asked for it (a poll does not bump: it is dropped by any read
    /// that started after it).
    generations: HashMap<PathBuf, u64>,
    next_generation: u64,
    /// The root is in a git work tree; `None` until git says. `Some(false)` stops asking.
    git: Option<bool>,
    rows: Vec<Row>,
    /// The hidden and ignored toggles `rows` were made with.
    flags: (bool, bool),
    selected: Option<PathBuf>,
    pub(super) scroll: UniformListScrollHandle,
    /// A poll or a watch's read is out: the next tick waits for it rather than pile up behind a slow disk, and what
    /// the watch hears meanwhile is read once it lands.
    loads: Loads,
    /// When the last poll started: while watching, the next waits `watch::SAFETY_POLL`.
    pub(super) polled_at: Option<Instant>,
    /// Folders whose last read timed out (a hung mount): polls skip them, an expand or Refresh tries again.
    stalled: HashSet<PathBuf>,
}

/// The right-click menu's row: its path and whether it is a folder.
pub(super) struct FileMenu {
    at: Point<Pixels>,
    path: PathBuf,
    file: bool,
}

/// One folder's answer from the runtime.
struct Answer {
    dir: PathBuf,
    generation: u64,
    read: Read,
    fingerprint: u64,
    /// The read timed out (`READ_TIMEOUT`).
    stalled: bool,
}

impl FileTree {
    fn new(root: PathBuf, flags: (bool, bool), memory_key: String) -> FileTree {
        static IDS: AtomicU64 = AtomicU64::new(0);
        FileTree {
            id: IDS.fetch_add(1, Ordering::Relaxed),
            root,
            memory_key,
            pending_top: None,
            reads: HashMap::new(),
            fingerprints: HashMap::new(),
            expanded: HashSet::new(),
            generations: HashMap::new(),
            next_generation: 0,
            git: None,
            rows: Vec::new(),
            flags,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            loads: Loads::default(),
            polled_at: None,
            stalled: HashSet::new(),
        }
    }

    fn rebuild(&mut self, flags: (bool, bool)) {
        self.flags = flags;
        self.rows = tree::visible_rows(&self.root, &self.reads, &self.expanded, flags.0, flags.1);
        // the remembered folders came in one read with the root: the rows are as they were left
        if self.reads.contains_key(&self.root)
            && let Some(top) = self.pending_top.take()
            && !self.rows.is_empty()
        {
            self.scroll.scroll_to_item(top.min(self.rows.len() - 1), ScrollStrategy::Top);
        }
    }

    /// What it shows, to give back to its workspace's next tree: the open folders that read (not those gone).
    fn memory(&self) -> FilesMemory {
        let mut expanded: Vec<PathBuf> = self.expanded.iter().filter(|d| matches!(self.reads.get(*d), Some(Read::Listed(_)))).cloned().collect();
        expanded.sort();
        let top = (!self.rows.is_empty()).then(|| self.top_row());
        FilesMemory { root: self.root.clone(), expanded, selected: self.selected.clone(), top: self.pending_top.or(top) }
    }

    /// The row at the top of the list: one asked for and not yet scrolled to, else the scroll offset's.
    fn top_row(&self) -> usize {
        let state = self.scroll.0.borrow();
        let scrolled = -f32::from(state.base_handle.offset().y) - LIST_PAD;
        state.deferred_scroll_to_item.as_ref().map_or((scrolled.max(0.) / ROW_HEIGHT) as usize, |d| d.item_index)
    }

    /// Opens what `memory` had open (inside the root only); returns the folders to read with the root.
    fn recall(&mut self, memory: &FilesMemory) -> Vec<PathBuf> {
        let inside = |p: &PathBuf| p != &self.root && p.starts_with(&self.root);
        self.expanded = memory.expanded.iter().filter(|p| inside(p)).cloned().collect();
        self.selected = memory.selected.clone().filter(|p| inside(p));
        self.pending_top = memory.top;
        let mut dirs: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        dirs.sort();
        dirs.truncate(MAX_POLLED - 1);
        dirs
    }

    /// Nothing opened or picked yet: a memory from the last run may still be given to it.
    fn untouched(&self) -> bool {
        self.expanded.is_empty() && self.selected.is_none()
    }

    /// The row of the entry at `path`, if it is drawn.
    fn index_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|r| matches!(r, Row::Entry { path: p, .. } if p == path))
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected.as_deref().and_then(|p| self.index_of(p))
    }
}

/// The hidden and ignored toggles, as saved.
fn flags(cx: &App) -> (bool, bool) {
    let s = &SettingsGlobal::of(cx).settings;
    (s.panel_show_hidden, s.panel_show_ignored)
}

/// An error reading a folder, as the row under it says it.
fn describe(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "This folder no longer exists".into(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".into(),
        _ => e.to_string(),
    }
}

fn digest(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Reads `dirs` (each with the generation it was asked at and, for a poll, the fingerprint the tree has of it),
/// one after the other, and marks what git ignores in each that changed. Returns the folders that changed, and
/// whether the root turned out to be in a git work tree.
async fn read_dirs(root: PathBuf, dirs: Vec<(PathBuf, u64, Option<u64>)>, git: Option<bool>) -> (Vec<Answer>, Option<bool>) {
    read_dirs_with(root, dirs, git, READ_TIMEOUT, read_folder).await
}

/// How long one folder's read may take: a folder on a hung NFS, sshfs or stale FUSE mount never answers, and a
/// poll waiting on it would never end (nor would the polls after it).
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// What the row under a folder says when its read timed out; polls leave such a folder alone after that.
const NOT_RESPONDING: &str = "Not responding";

/// One folder's listing and whether it is where git sees it (blocking: the runtime's blocking pool runs it).
type Reader = fn(&Path, &Path) -> std::io::Result<(Listing, bool)>;

fn read_folder(dir: &Path, root: &Path) -> std::io::Result<(Listing, bool)> {
    let listing = tree::list(dir, root)?;
    // a folder reached through a link inside the root is not where git sees it: git refuses paths "beyond a
    // symbolic link", the whole batch with them
    let direct = match (dir.canonicalize(), root.canonicalize()) {
        (Ok(d), Ok(r)) => dir.strip_prefix(root).is_ok_and(|rel| d == r.join(rel)),
        _ => false,
    };
    Ok((listing, direct))
}

/// `read_dirs` with the deadline and the reader as arguments, for the tests.
async fn read_dirs_with(root: PathBuf, dirs: Vec<(PathBuf, u64, Option<u64>)>, mut git: Option<bool>, deadline: Duration, reader: Reader) -> (Vec<Answer>, Option<bool>) {
    let mut answers = Vec::new();
    for (dir, generation, known) in dirs {
        let listed = {
            let (dir, root) = (dir.clone(), root.clone());
            // ponytail: a read that never ends keeps its blocking thread; polls then leave its folder alone, so
            // only an expand or Refresh starts another
            tokio::time::timeout(deadline, tokio::task::spawn_blocking(move || reader(&dir, &root))).await
        };
        let (mut listing, direct) = match listed {
            Ok(Ok(Ok(listed))) => listed,
            Ok(Ok(Err(e))) => {
                let error = describe(&e);
                let fingerprint = digest(&error);
                if known != Some(fingerprint) {
                    answers.push(Answer { dir, generation, read: Read::Failed(error), fingerprint, stalled: false });
                }
                continue;
            }
            // the runtime shutting down
            Ok(Err(_)) => continue,
            Err(_) => {
                let read = Read::Failed(NOT_RESPONDING.into());
                answers.push(Answer { dir, generation, read, fingerprint: digest(NOT_RESPONDING), stalled: true });
                continue;
            }
        };
        let fingerprint = listing.fingerprint();
        if known == Some(fingerprint) {
            continue;
        }
        if direct {
            git = mark_ignored(&root, &dir, &mut listing, git).await;
        }
        answers.push(Answer { dir, generation, read: Read::Listed(listing), fingerprint, stalled: false });
    }
    (answers, git)
}

/// Asks git which of `listing`'s entries it ignores, when the root is (or may be) in a work tree; returns what
/// that showed of the root.
async fn mark_ignored(root: &Path, dir: &Path, listing: &mut Listing, git: Option<bool>) -> Option<bool> {
    if git == Some(false) || listing.entries.is_empty() {
        return git;
    }
    let Ok(rel) = dir.strip_prefix(root) else { return git };
    let mut cmd = runner::git_cmd(root, &["check-ignore", "-z", "--stdin"]);
    cmd.stdin = Some(tree::check_ignore_input(rel, listing));
    match runner::run(&Host::Local, cmd, CHECK_IGNORE).await {
        // 0: some are ignored (a cut answer still names the ones it got to); 1: none is
        Ok(out) if out.code == Some(0) || out.truncated => {
            tree::mark_ignored(listing, rel, &tree::parse_check_ignore(&out.stdout));
            Some(true)
        }
        Ok(out) if out.code == Some(1) => Some(true),
        Ok(out) if out.stderr.contains("not a git repository") => Some(false),
        Err(RunError::NotFound(_)) => Some(false),
        // a path git will not take (inside a submodule, say), a timeout: nothing marked, asked again next time
        _ => git,
    }
}

impl SidePanel {
    /// Keeps the tree on the context's root: a new root gets a new tree (read when the Files tab shows it); no
    /// root, or a machine's, none.
    /// A tree replaced (by another root or none) is remembered for its workspace, and a workspace shown again with
    /// the same root opens what it had open, at the row it was scrolled to.
    pub(super) fn sync_files(&mut self, cx: &mut Context<Self>) {
        let root = self.context.root.as_deref().filter(|_| !self.context.remote).map(PathBuf::from);
        let key = self.context.workspace_id.as_deref().map_or_else(String::new, |ws| crate::files::roots::root_key(&self.context.place, ws));
        let Some(root) = root else {
            self.stash_files(cx);
            self.files = None;
            self.file_menu = None;
            return;
        };
        if self.tab != crate::settings::PanelTab::Files {
            return;
        }
        if self.files.as_ref().is_some_and(|t| t.root == root) {
            // another workspace on the same root: the tree stays, and is that workspace's from now on
            if self.files.as_ref().is_some_and(|t| t.memory_key != key) {
                self.stash_files(cx);
                if let Some(tree) = self.files.as_mut() {
                    tree.memory_key = key;
                }
            }
            return;
        }
        self.stash_files(cx);
        let mut tree = FileTree::new(root.clone(), flags(cx), key);
        let open = self.files_memory.recall(&tree.memory_key, &root).cloned().map(|m| tree.recall(&m)).unwrap_or_default();
        self.files = Some(tree);
        self.file_menu = None;
        self.read_dirs(std::iter::once(root).chain(open).collect(), false, false, cx);
    }

    /// The shown tree, remembered for its workspace.
    fn stash_files(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = self.files.as_ref().filter(|t| !t.memory_key.is_empty()) {
            self.files_memory.stash(&tree.memory_key, tree.memory());
            cx.emit(SidePanelEvent::FilesRemembered);
        }
    }

    /// Every workspace's open folders as remembered, the shown tree's as it is now: (key, root, open folders), kept
    /// for the next run.
    pub fn files_memory_now(&self) -> Vec<(String, PathBuf, Vec<PathBuf>)> {
        let live = self.files.as_ref().filter(|t| !t.memory_key.is_empty()).map(|t| (t.memory_key.clone(), t.memory()));
        live.iter()
            .chain(self.files_memory.entries().iter().filter(|(k, _)| live.as_ref().is_none_or(|(l, _)| l != k)))
            .map(|(k, m)| (k.clone(), m.root.clone(), m.expanded.clone()))
            .collect()
    }

    /// The open folders kept from the last run: remembered where this run has nothing, and opened in the shown tree
    /// when nothing was opened in it yet.
    pub fn seed_files_memory(&mut self, kept: Vec<(String, PathBuf, Vec<PathBuf>)>, cx: &mut Context<Self>) {
        for (key, root, expanded) in kept {
            self.files_memory.seed(key, FilesMemory { root, expanded, selected: None, top: None });
        }
        let Some(tree) = self.files.as_mut().filter(|t| t.untouched()) else { return };
        let Some(memory) = self.files_memory.recall(&tree.memory_key, &tree.root).cloned() else { return };
        let open = tree.recall(&memory);
        if !open.is_empty() {
            let root = tree.root.clone();
            self.read_dirs(std::iter::once(root).chain(open).collect(), false, false, cx);
        }
    }

    /// Workspaces closed, or ids herdr gave to another: their open folders are forgotten.
    pub fn forget_files(&mut self, keys: &[String]) {
        self.files_memory.forget(keys);
    }

    /// The Shell's word on whether the Files tab is on screen and the window active. Coming into view reads the
    /// open folders again at once; going out of it takes the menu down.
    pub fn set_files_shown(&mut self, shown: bool, active: bool, cx: &mut Context<Self>) {
        if !shown && self.file_menu.as_ref().is_some_and(|m| m.is_open()) {
            self.file_menu = None;
            cx.notify();
        }
        let live = shown && active;
        let woke = live && !self.files_live;
        self.files_live = live;
        if woke {
            self.poll_files(false, cx);
        }
    }

    /// Every `POLL` while the tab is live (`tick`; every `watch::SAFETY_POLL` while the root is watched), and when it
    /// comes into view: the root and the open folders drawn, read again; only a change lands.
    pub(super) fn poll_files(&mut self, tick: bool, cx: &mut Context<Self>) {
        let (live, watching) = (self.files_live, self.watching());
        let Some(tree) = self.files.as_mut().filter(|t| live && !t.loads.busy()) else { return };
        if tick && watching && tree.polled_at.is_some_and(|at| at.elapsed() < watch::SAFETY_POLL) {
            return;
        }
        tree.polled_at = Some(Instant::now());
        if let Some(batch) = tree.loads.start(Batch::everything()) {
            self.load_files(batch, cx);
        }
    }

    /// What the watch heard: the open folders among it read again (all of them after lost events or a
    /// `.gitignore` changed, which also asks git again what it ignores), or once the read out lands.
    pub(super) fn watch_files(&mut self, batch: Batch, cx: &mut Context<Self>) {
        // a tab out of view reads everything when it comes back
        let live = self.files_live;
        let Some(tree) = self.files.as_mut().filter(|_| live) else { return };
        if batch.dirs.is_empty() && !batch.rescan && !batch.rules {
            return;
        }
        if let Some(batch) = tree.loads.start(batch) {
            self.load_files(batch, cx);
        }
    }

    /// Starts `batch`'s read (a load is out for it: `Loads::start`); with nothing to read, the load ends, and the
    /// next one waiting starts.
    fn load_files(&mut self, mut batch: Batch, cx: &mut Context<Self>) {
        loop {
            let Some(tree) = self.files.as_mut() else { return };
            let all = batch.rescan || batch.rules;
            let open = tree.rows.iter().filter_map(|r| match r {
                Row::Entry { path, expanded: true, .. } => Some(path.clone()),
                _ => None,
            });
            // a folder not read yet has its first read out already
            let dirs: Vec<PathBuf> = std::iter::once(tree.root.clone())
                .chain(open)
                .filter(|d| tree.reads.contains_key(d) && !tree.stalled.contains(d) && (all || batch.dirs.contains(d)))
                .take(MAX_POLLED)
                .collect();
            if !dirs.is_empty() {
                // new ignore rules: read afresh, so git is asked again (a poll asks only of folders whose names changed)
                self.read_dirs(dirs, !batch.rules, true, cx);
                return;
            }
            match tree.loads.finish() {
                Some(next) => batch = next,
                None => return,
            }
        }
    }

    /// The refresh button: the root and every open folder read afresh, what git ignores asked again too.
    pub fn refresh_files(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = self.files.as_mut() else { return };
        tree.git = None;
        let open = tree.rows.iter().filter_map(|r| match r {
            Row::Entry { path, expanded: true, .. } => Some(path.clone()),
            _ => None,
        });
        let dirs: Vec<PathBuf> = std::iter::once(tree.root.clone()).chain(open).take(MAX_POLLED).collect();
        self.read_dirs(dirs, false, false, cx);
    }

    /// Reads `dirs` on the runtime. A poll hands over the fingerprints it has and keeps the folders' generations;
    /// any other read bumps them, so a poll still out cannot land over it. `load`: the read is the tree's load out
    /// (`FileTree::loads`), ended when it lands.
    fn read_dirs(&mut self, dirs: Vec<PathBuf>, poll: bool, load: bool, cx: &mut Context<Self>) {
        let Some(tree) = self.files.as_mut() else { return };
        let jobs = dirs
            .into_iter()
            .map(|dir| {
                if poll {
                    let generation = tree.generations.get(&dir).copied().unwrap_or(0);
                    let known = tree.fingerprints.get(&dir).copied();
                    (dir, generation, known)
                } else {
                    tree.next_generation += 1;
                    tree.generations.insert(dir.clone(), tree.next_generation);
                    (dir, tree.next_generation, None)
                }
            })
            .collect();
        let (id, root, git) = (tree.id, tree.root.clone(), tree.git);
        let job = self.rt.spawn(read_dirs(root, jobs, git));
        cx.spawn(async move |this, cx| {
            let answer = job.await.ok();
            let _ = this.update(cx, |this, cx| this.take_reads(id, load, answer, cx));
        })
        .detach();
    }

    fn take_reads(&mut self, id: u64, load: bool, answer: Option<(Vec<Answer>, Option<bool>)>, cx: &mut Context<Self>) {
        let Some(tree) = self.files.as_mut().filter(|t| t.id == id) else { return };
        // what the watch heard while this was out is read next
        let next = if load { tree.loads.finish() } else { None };
        self.land_reads(answer, cx);
        if let Some(next) = next {
            self.load_files(next, cx);
        }
    }

    fn land_reads(&mut self, answer: Option<(Vec<Answer>, Option<bool>)>, cx: &mut Context<Self>) {
        let Some(tree) = self.files.as_mut() else { return };
        let Some((answers, git)) = answer else { return };
        tree.git = git.or(tree.git);
        let mut changed = false;
        for a in answers {
            if tree.generations.get(&a.dir).copied().unwrap_or(0) != a.generation {
                continue;
            }
            if a.stalled {
                tree.stalled.insert(a.dir.clone());
            } else {
                tree.stalled.remove(&a.dir);
            }
            tree.fingerprints.insert(a.dir.clone(), a.fingerprint);
            tree.reads.insert(a.dir, a.read);
            changed = true;
        }
        if changed {
            self.rebuild_files(cx);
        }
    }

    /// The rows again, after a read, a fold or a toggle; the menu goes if its row did.
    fn rebuild_files(&mut self, cx: &mut Context<Self>) {
        self.rebuild_rows(flags(cx));
        cx.notify();
    }

    /// `rebuild_files` without the notify, for the render that finds the toggles changed.
    fn rebuild_rows(&mut self, flags: (bool, bool)) {
        let Some(tree) = self.files.as_mut() else { return };
        tree.rebuild(flags);
        if let Some(menu) = &self.file_menu
            && tree.index_of(&menu.value.path).is_none()
        {
            self.file_menu = None;
        }
    }

    /// Opens or folds the folder at `path`. Opening one read before shows what was read at once and reads it again.
    fn toggle_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(tree) = self.files.as_mut() else { return };
        if !tree.expanded.remove(&path) {
            tree.expanded.insert(path.clone());
            self.read_dirs(vec![path], false, false, cx);
        }
        self.rebuild_files(cx);
        // kept for the next run
        cx.emit(SidePanelEvent::FilesRemembered);
    }

    /// A click or Enter on the entry at `path`: a folder opens or folds, a file (an image too) opens in a file tab, a
    /// preview for a single click (`preview`); a link to a folder outside the root does neither.
    fn activate(&mut self, path: PathBuf, preview: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.files.as_ref().and_then(|t| t.index_of(&path).map(|i| t.rows[i].clone())) else { return };
        let Row::Entry { entry, .. } = entry else { return };
        if entry.expandable() {
            self.toggle_folder(path, cx);
        } else if entry.is_dir() || entry.kind == tree::EntryKind::Dangling {
            // a link out of the root, or to nothing
        } else {
            cx.emit(SidePanelEvent::OpenFile { path, line: None, preview });
        }
    }

    /// Cmd+Enter (Ctrl+Enter on Linux): the selected file to the editor. True when a file was selected.
    pub(super) fn open_selected_in_editor(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(tree) = self.files.as_ref() else { return false };
        let Some(Row::Entry { path, entry, .. }) = tree.selected_index().map(|i| &tree.rows[i]) else { return false };
        if entry.is_dir() || entry.kind == tree::EntryKind::Dangling {
            return false;
        }
        cx.emit(SidePanelEvent::OpenInEditor(path.clone()));
        true
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tree) = self.files.as_mut() else { return };
        let Some(Row::Entry { path, .. }) = tree.rows.get(index) else { return };
        tree.selected = Some(path.clone());
        tree.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// The tree's keys while the panel has keyboard focus: arrows move and open as in Finder's list and VS Code's
    /// tree, Enter opens. True when the key was the tree's.
    pub(super) fn files_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(tree) = self.files.as_ref() else { return false };
        let entries: Vec<usize> = tree.rows.iter().enumerate().filter(|(_, r)| matches!(r, Row::Entry { .. })).map(|(i, _)| i).collect();
        let (Some(&first), Some(&last)) = (entries.first(), entries.last()) else { return false };
        let at = tree.selected_index();
        let selected = at.map(|i| tree.rows[i].clone());
        match key {
            "down" => self.select(at.and_then(|i| entries.iter().copied().find(|&e| e > i)).unwrap_or(if at.is_some() { last } else { first }), cx),
            "up" => self.select(at.and_then(|i| entries.iter().rev().copied().find(|&e| e < i)).unwrap_or(first), cx),
            "home" => self.select(first, cx),
            "end" => self.select(last, cx),
            "left" => match selected {
                Some(Row::Entry { path, expanded: true, .. }) => self.toggle_folder(path, cx),
                Some(Row::Entry { path, .. }) => {
                    let parent = path.parent().and_then(|p| tree.index_of(p));
                    if let Some(parent) = parent {
                        self.select(parent, cx);
                    }
                }
                _ => self.select(first, cx),
            },
            "right" => match selected {
                Some(Row::Entry { path, entry, expanded: false, .. }) if entry.expandable() => self.toggle_folder(path, cx),
                Some(Row::Entry { expanded: true, .. }) => {
                    if let Some(child) = at.and_then(|i| entries.iter().copied().find(|&e| e > i)) {
                        self.select(child, cx);
                    }
                }
                Some(_) => {}
                None => self.select(first, cx),
            },
            "enter" => match selected {
                Some(row) => self.activate(row.path().to_path_buf(), false, cx),
                None => self.select(first, cx),
            },
            _ => return false,
        }
        true
    }

    /// The Files tab's body: the tree, or why there is none to show.
    pub(super) fn render_files(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let flags = flags(cx);
        if self.files.as_ref().is_some_and(|t| t.flags != flags) {
            // a toggle in the header changed the saved settings (this render draws the new rows: no notify)
            self.rebuild_rows(flags);
        }
        crate::ui::reap_popup(&mut self.file_menu, window);
        let Some(tree) = self.files.as_ref() else { return div().into_any_element() };
        if tree.rows.is_empty() {
            return match tree.reads.get(&tree.root) {
                Some(Read::Listed(l)) if l.entries.is_empty() => super::notice(theme, "Empty folder", "").into_any_element(),
                Some(_) => super::notice(theme, "Nothing to show", "Every entry here is hidden or ignored by git.").into_any_element(),
                // the first read is out
                None => div().into_any_element(),
            };
        }
        let list = gpui::uniform_list("files", tree.rows.len(), cx.processor(|this: &mut SidePanel, range: Range<usize>, window, cx| this.file_rows(range, window, cx)))
            .track_scroll(&tree.scroll)
            .size_full()
            .py(px(LIST_PAD));
        let scroll = tree.scroll.0.borrow().base_handle.clone();
        let menu = self.file_menu(theme, cx);
        div()
            .relative()
            .size_full()
            // a press in the tree (between rows too) gives the panel the keyboard, for its arrows; only here, so
            // another tab's inputs keep the focus a press gives them
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
            .child(crate::ui::edge_fade::edge_faded(12., &scroll, list))
            .child(crate::ui::scrollbar("files-scrollbar", &tree.scroll, theme))
            .children(menu)
            .into_any_element()
    }

    /// The rows the list shows of `range`.
    fn file_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let Some(tree) = self.files.as_ref() else { return Vec::new() };
        let menu_path = self.file_menu.as_ref().filter(|m| m.is_open()).map(|m| m.value.path.clone());
        let hover = theme.hover;
        range
            .filter_map(|ix| Some((ix, tree.rows.get(ix)?)))
            .map(|(ix, row)| match row {
                Row::Note { depth, text, .. } => div()
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .pl(px(8. + (depth + 1) as f32 * INDENT + 20.))
                    .pr(px(8.))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .text_color(theme.text_faint)
                    .child(div().truncate().child(text.clone()))
                    .into_any_element(),
                Row::Entry { path, depth, entry, expanded, ignored } => {
                    let selected = tree.selected.as_deref() == Some(path.as_path()) || menu_path.as_deref() == Some(path.as_path());
                    let chevron = entry.expandable().then(|| {
                        let turn = gpui::Transformation::rotate(gpui::percentage(if *expanded { 0.25 } else { 0. }));
                        gpui::svg().path("icons/alt-arrow-right.svg").size(px(12.)).text_color(theme.text_muted.opacity(0.6)).with_transformation(turn)
                    });
                    let dangling = entry.kind == tree::EntryKind::Dangling;
                    let color = if *ignored || dangling { theme.text_faint } else { theme.text };
                    let (is_dir, link) = (entry.is_dir(), matches!(entry.kind, tree::EntryKind::Symlink { .. }));
                    let icon = material_icon(&entry.name, is_dir, *expanded);
                    let outside = matches!(entry.kind, tree::EntryKind::Symlink { dir: true, outside: true });
                    let (click, menu) = (path.clone(), path.clone());
                    let file = !entry.is_dir() && !dangling;
                    div()
                        .id(("file-row", ix))
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .pl(px(8. + *depth as f32 * INDENT))
                        .pr(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(selected, |el| el.bg(theme.selected))
                        .when(!selected, |el| el.hover(move |s| s.bg(hover)))
                        .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().children(chevron))
                        .child(
                            // A coloured SVG, so img(), not the tinted svg(); an ignored or dangling entry is dimmed
                            div()
                                .relative()
                                .size(px(16.))
                                .flex_none()
                                .child(gpui::img(icon).size(px(16.)).object_fit(gpui::ObjectFit::Contain).when(*ignored || dangling, |el| el.opacity(0.5)))
                                .when(link, |el| {
                                    el.child(
                                        div().absolute().right(px(-2.)).bottom(px(-2.)).child(
                                            gpui::svg().path("icons/solar/arrow-right-up.svg").size(px(8.)).text_color(theme.text_muted),
                                        ),
                                    )
                                }),
                        )
                        .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).text_color(color).child(entry.name.clone()))
                        .when(outside, |el| el.tooltip(crate::shell::widgets::tip("A link to a folder outside this one", None)))
                        .when(dangling, |el| el.tooltip(crate::shell::widgets::tip("A link to something that is not there", None)))
                        .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if let Some(tree) = this.files.as_mut() {
                                tree.selected = Some(click.clone());
                            }
                            // the second click of a double-click pins the preview the first opened (a folder's
                            // toggles back, as it always did)
                            this.activate(click.clone(), e.click_count() < 2, cx);
                            cx.notify();
                        }))
                        // Capture, so a right-click on another row while the menu is up moves the menu there rather
                        // than the menu's click-outside closing the new one (as the root row's `…` menu).
                        .capture_any_mouse_down(cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            if e.button != MouseButton::Right {
                                return;
                            }
                            cx.stop_propagation();
                            this.file_menu = Some(crate::ui::Popup::new(FileMenu { at: e.position, path: menu.clone(), file }));
                            cx.notify();
                        }))
                        .into_any_element()
                }
            })
            .collect()
    }

    /// The right-click menu, while it is up.
    fn file_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.file_menu.as_ref()?;
        let root = self.files.as_ref()?.root.clone();
        let (path, file) = (menu.value.path.clone(), menu.value.file);
        if path.as_os_str().is_empty() {
            return None;
        }
        let close = |this: &mut SidePanel| {
            if let Some(m) = this.file_menu.as_mut() {
                m.close()
            }
        };
        let item = |id: &'static str, icon: &'static str, label: &'static str| menu_choice(theme, id, icon, label, false);
        let copy = |text: String| move |this: &mut SidePanel, _: &ClickEvent, _: &mut Window, cx: &mut Context<SidePanel>| {
            close(this);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            cx.notify();
        };
        let relative = path.strip_prefix(&root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let card = menu_card(theme, "file-menu", 220.)
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                close(this);
                cx.notify();
            }))
            .when(file, |el| {
                let (open, editor) = (path.clone(), path.clone());
                el.child(item("file-menu-open", "icons/solar/file-text.svg", "Open").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    close(this);
                    cx.emit(SidePanelEvent::OpenFile { path: open.clone(), line: None, preview: false });
                    cx.notify();
                })))
                .child(item("file-menu-editor", "icons/solar/pen-new-square.svg", "Open in editor").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    close(this);
                    cx.emit(SidePanelEvent::OpenInEditor(editor.clone()));
                    cx.notify();
                })))
            })
            .child(item("file-menu-copy", "icons/solar/copy.svg", "Copy path").on_click(cx.listener(copy(path.to_string_lossy().into_owned()))))
            .child(item("file-menu-copy-relative", "icons/solar/copy.svg", "Copy relative path").on_click(cx.listener(copy(relative))))
            .child(div().mx(px(-4.)).my(px(4.)).h(px(1.)).bg(theme.border))
            .child(item("file-menu-reveal", "icons/folder.svg", "Reveal in file manager").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                close(this);
                let (program, args) = crate::platform::reveal_command(&path);
                this.rt.spawn(async move {
                    let _ = tokio::process::Command::new(program).args(args).status().await;
                });
                cx.notify();
            })));
        let anchored = gpui::anchored().position(menu.value.at).snap_to_window_with_margin(px(8.)).child(crate::ui::menu_motion("file-menu-in", menu, card));
        Some(gpui::deferred(anchored).with_priority(1).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(root: &str, open: &[&str]) -> FilesMemory {
        FilesMemory { root: root.into(), expanded: open.iter().map(PathBuf::from).collect(), selected: None, top: Some(3) }
    }

    #[test]
    fn a_workspace_gets_its_open_folders_back_on_the_same_root_only() {
        let mut m = FilesMemories::default();
        m.stash("s\nw1", memory("/r", &["/r/src"]));
        m.stash("s\nw2", memory("/q", &[]));
        assert_eq!(m.recall("s\nw1", Path::new("/r")), Some(&memory("/r", &["/r/src"])));
        assert_eq!(m.recall("s\nw1", Path::new("/other")), None, "the workspace was given another root");
        assert_eq!(m.recall("s\nw3", Path::new("/r")), None);
        // stashed again: the newer one, at the front
        m.stash("s\nw1", memory("/r", &["/r/src", "/r/docs"]));
        assert_eq!(m.entries()[0].1.expanded.len(), 2);
        assert_eq!(m.entries().len(), 2);
        // the last run's never overrides this run's
        m.seed("s\nw1".into(), memory("/r", &[]));
        assert_eq!(m.recall("s\nw1", Path::new("/r")).unwrap().expanded.len(), 2);
        m.stash("", memory("/r", &[]));
        assert_eq!(m.entries().len(), 2, "no workspace, nothing kept");
        m.forget(&["s\nw2".to_string()]);
        assert_eq!(m.entries().iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["s\nw1"]);
    }

    #[test]
    fn files_memory_keeps_the_most_recent_workspaces() {
        let mut m = FilesMemories::default();
        for i in 0..FILES_MEMORY + 5 {
            m.stash(&format!("s\nw{i}"), memory("/r", &[]));
        }
        assert_eq!(m.entries().len(), FILES_MEMORY);
        assert_eq!(m.entries()[0].0, format!("s\nw{}", FILES_MEMORY + 4));
        assert!(m.recall("s\nw4", Path::new("/r")).is_none(), "the oldest went");
        assert!(m.recall("s\nw5", Path::new("/r")).is_some());
        m.seed("s\nnew".into(), memory("/r", &[]));
        assert!(m.recall("s\nnew", Path::new("/r")).is_none(), "full: the last run's waits");
    }

    #[test]
    fn a_tree_opens_what_it_remembers_inside_its_root() {
        let mut tree = FileTree::new("/r".into(), (false, false), "s\nw1".into());
        let m = FilesMemory { root: "/r".into(), expanded: vec!["/r/src".into(), "/elsewhere".into(), "/r".into()], selected: Some("/r/src/a.rs".into()), top: Some(7) };
        assert_eq!(tree.recall(&m), [PathBuf::from("/r/src")]);
        assert_eq!((tree.selected.clone(), tree.pending_top), (Some(PathBuf::from("/r/src/a.rs")), Some(7)));
        assert!(!tree.untouched());
        // what it gives back: only folders that read, the top it still waits to scroll to
        tree.reads.insert("/r".into(), Read::Listed(Listing::default()));
        tree.reads.insert("/r/src".into(), Read::Listed(Listing::default()));
        tree.expanded.insert("/r/gone".into());
        tree.reads.insert("/r/gone".into(), Read::Failed("This folder no longer exists".into()));
        assert_eq!(tree.memory().expanded, [PathBuf::from("/r/src")]);
        assert_eq!(tree.memory().top, Some(7));
    }

    fn ignored(answer: &Answer) -> Vec<(&str, bool)> {
        match &answer.read {
            Read::Listed(l) => l.entries.iter().map(|e| (e.name.as_str(), e.ignored)).collect(),
            Read::Failed(e) => panic!("{e}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reads_mark_what_git_ignores_and_a_poll_hears_only_of_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(root.join("a.log"), "").unwrap();
        std::fs::write(root.join("src/x.rs"), "").unwrap();
        std::os::unix::fs::symlink(root.join("src"), root.join("linked")).unwrap();
        let init = std::process::Command::new("git").args(["init", "-q"]).current_dir(&root).status().unwrap();
        assert!(init.success());

        let jobs = vec![(root.clone(), 1, None), (root.join("target"), 2, None), (root.join("linked"), 3, None)];
        let (answers, git) = read_dirs(root.clone(), jobs, None).await;
        assert_eq!(git, Some(true));
        assert_eq!(ignored(&answers[0]), [("linked", false), ("src", false), ("target", true), (".gitignore", false), ("a.log", true)]);
        assert_eq!((answers[1].dir.clone(), answers[1].generation), (root.join("target"), 2));
        // through a link: listed, not asked of git (which would refuse the whole batch)
        assert_eq!(ignored(&answers[2]), [("x.rs", false)]);

        let known = answers[0].fingerprint;
        let (answers, _) = read_dirs(root.clone(), vec![(root.clone(), 1, Some(known))], Some(true)).await;
        assert!(answers.is_empty());
        std::fs::write(root.join("b.rs"), "").unwrap();
        let (answers, _) = read_dirs(root.clone(), vec![(root.clone(), 1, Some(known)), (root.join("gone"), 4, None)], Some(true)).await;
        assert_eq!(answers.len(), 2);
        assert!(matches!(&answers[1].read, Read::Failed(e) if e == "This folder no longer exists"));
    }

    /// A folder that never answers (a hung mount) times out as "Not responding", and the folders after it are
    /// still read: the poll ends.
    #[tokio::test]
    async fn a_hung_folder_times_out_and_the_rest_are_read() {
        fn reader(dir: &Path, _: &Path) -> std::io::Result<(Listing, bool)> {
            if dir.ends_with("hung") {
                std::thread::sleep(Duration::from_secs(2));
            }
            Ok((Listing::default(), false))
        }
        let root = PathBuf::from("/r");
        let jobs = vec![(root.join("hung"), 1, None), (root.join("fine"), 2, None)];
        let started = std::time::Instant::now();
        let (answers, _) = read_dirs_with(root.clone(), jobs, Some(false), Duration::from_millis(100), reader).await;
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(answers[0].stalled && matches!(&answers[0].read, Read::Failed(e) if e == NOT_RESPONDING));
        assert!(!answers[1].stalled && matches!(answers[1].read, Read::Listed(_)));
    }

    #[tokio::test]
    async fn a_folder_outside_git_says_so_and_marks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.log"), "").unwrap();
        let root = dir.path().to_path_buf();
        let (answers, git) = read_dirs(root.clone(), vec![(root.clone(), 1, None)], None).await;
        assert_eq!(git, Some(false));
        assert_eq!(ignored(&answers[0]), [("a.log", false)]);
    }
}
