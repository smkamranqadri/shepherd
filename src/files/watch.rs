//! Live updates for the right panel: the shown root watched for changes on disk (`notify`: inotify on Linux,
//! FSEvents on macOS), so the Files tree, the Changes list and an open diff refresh when something changes rather
//! than at the next poll. As wooloo's WorkspaceFileWatcher: the root, plus the repository's git folder and common
//! folder (a linked worktree's lie outside it); git's lock files, objects and logs are left out, and its index,
//! HEAD and refs say "status changed". Events are batched over `WINDOW`, then handed to the panel as one `Batch`:
//! the folders to read again, whether git is to be asked, whether `.gitignore` changed.
//!
//! inotify watches a folder at a time, one watch each, from a per-user budget (`max_user_watches`), so on Linux every
//! folder is watched by itself, each before it is read: a `.git`, a `node_modules`, a cache folder (`CACHEDIR.TAG`,
//! as cargo's `target` has), anything git ignores and any link (one to `..` would watch everything above) are not
//! (`walk`). A folder deleted or moved away is let go of, so the same path made again is watched afresh and the
//! count stays true; past `MAX_WATCHED` folders, or at the system's limit, watching gives up and the panel polls as
//! before. FSEvents watches a tree with one stream: there the root is
//! watched whole and the same folders' events are dropped instead (`classify`).
//!
//! Everything here runs on the runtime: the walk and the watches under `spawn_blocking`, the batching in a task.
//! The panel hears `Message`s, and drops the task (and the watcher with it) when the root changes or the panel
//! closes.
use crate::files::runner::{self, Host, Limits};
use ::notify::event::{ModifyKind, RenameMode};
use ::notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{BTreeSet, HashSet};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

/// Events are gathered this long after the first of a batch before the panel hears of them: a save, a checkout or
/// a build's burst is one refresh. Bounded, not reset by each event, so a steady stream still lands.
pub const WINDOW: Duration = Duration::from_millis(300);
/// While watching works, the polls are only a safety net: this often.
pub const SAFETY_POLL: Duration = Duration::from_secs(30);
/// Folders watched at most (inotify only; each is a watch, about 1 KiB of kernel memory, from a budget other apps
/// share). Past this the panel polls.
pub const MAX_WATCHED: usize = 10_000;
const TOO_MANY: &str = "more than 10,000 folders to watch";
/// A batch names at most this many paths of each kind: past it the panel reads all its open folders (and the task
/// walks the root again) rather than carry a checkout's worth of paths.
pub const MAX_BATCH: usize = 2000;
/// What a walk returns when its watch was dropped meanwhile (a root switched): no one hears it.
const CANCELLED: &str = "cancelled";
const LIMIT: &str = "the system's limit on watched folders is reached (fs.inotify.max_user_watches)";
/// `git rev-parse`: two lines.
const LOCATE: Limits = Limits { timeout: Duration::from_secs(5), max_bytes: 64 << 10 };
/// `git ls-files` of the ignored folders, and `git check-ignore` of new ones.
const IGNORED: Limits = Limits { timeout: Duration::from_secs(10), max_bytes: 8 << 20 };

/// Each folder watched by itself (inotify), rather than the root's tree by one stream (FSEvents).
const PER_FOLDER: bool = cfg!(target_os = "linux");

/// What the panel hears from a watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Watching started: events from here on are heard.
    Live,
    /// What changed over a `WINDOW`.
    Batch(Batch),
    /// Watching could not start, or stopped: why, for "Live updates off: <why>". The panel polls instead.
    Off(String),
}

/// What a batch of events asks of the panel.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Batch {
    /// Folders whose listing may have changed: an event's folder, and the entry itself (a folder of its own, maybe).
    pub dirs: BTreeSet<PathBuf>,
    /// git's answer may have changed: something in the work tree, or the index, HEAD or a ref.
    pub status: bool,
    /// A `.gitignore` changed: what the tree marks as ignored may have.
    pub rules: bool,
    /// Every open folder is read again: events were lost (the kernel's queue overflowed), or more than `MAX_BATCH`
    /// folders came.
    pub rescan: bool,
    /// Folders that appeared, to be watched too (per-folder watching). This and the next two are the task's, cleared
    /// before the panel hears the batch.
    pub new_dirs: BTreeSet<PathBuf>,
    /// Watched folders deleted or moved away: they and what is under them are not watched any more (a folder made
    /// again at the same path is watched afresh).
    pub gone: BTreeSet<PathBuf>,
    /// The root is walked again for folders to watch: events were lost, or more than `MAX_BATCH` folders came or went.
    pub rewalk: bool,
}

impl Batch {
    /// Everything read again and git asked: a poll's batch, and what watching's start catches up on.
    pub fn everything() -> Batch {
        Batch { status: true, rescan: true, ..Batch::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && !self.status && !self.rules && !self.rescan && self.new_dirs.is_empty() && self.gone.is_empty() && !self.rewalk
    }

    /// `other`'s asks added to these; past `MAX_BATCH` paths, a rescan (a rewalk) instead of them.
    pub fn merge(&mut self, other: Batch) {
        self.status |= other.status;
        self.rules |= other.rules;
        self.rescan |= other.rescan;
        if !self.rescan {
            self.dirs.extend(other.dirs);
        }
        if self.dirs.len() > MAX_BATCH {
            self.rescan = true;
        }
        if self.rescan {
            self.dirs.clear();
        }
        self.rewalk |= other.rewalk;
        if !self.rewalk {
            self.new_dirs.extend(other.new_dirs);
            self.gone.extend(other.gone);
        }
        if self.new_dirs.len() > MAX_BATCH || self.gone.len() > MAX_BATCH {
            self.rewalk = true;
        }
        if self.rewalk {
            self.new_dirs.clear();
            self.gone.clear();
        }
    }
}

/// Gathers events into batches over `window` from the first, on a clock it is handed.
#[derive(Debug)]
pub struct Batcher {
    window: Duration,
    since: Option<Instant>,
    batch: Batch,
}

impl Batcher {
    pub fn new(window: Duration) -> Batcher {
        Batcher { window, since: None, batch: Batch::default() }
    }

    /// An event's batch, at `now`; an empty one (an event that matters to nothing) opens no window.
    pub fn push(&mut self, now: Instant, batch: Batch) {
        if batch.is_empty() {
            return;
        }
        self.since.get_or_insert(now);
        self.batch.merge(batch);
    }

    /// When the open batch is due; `None` while none is.
    pub fn deadline(&self) -> Option<Instant> {
        self.since.map(|at| at + self.window)
    }

    /// The batch, once due at `now`.
    pub fn take(&mut self, now: Instant) -> Option<Batch> {
        if self.deadline().is_none_or(|at| at > now) {
            return None;
        }
        self.since = None;
        Some(mem::take(&mut self.batch))
    }
}

/// One load out at a time: a batch that comes while one is waits, merged with any others, for the next.
#[derive(Debug, Default)]
pub struct Loads {
    busy: bool,
    queued: Option<Batch>,
}

impl Loads {
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// A load wants to start: the batch to start it with when none is out (one is now), else `None` and it waits.
    pub fn start(&mut self, batch: Batch) -> Option<Batch> {
        if self.busy {
            match &mut self.queued {
                Some(queued) => queued.merge(batch),
                None => self.queued = Some(batch),
            }
            return None;
        }
        self.busy = true;
        Some(batch)
    }

    /// The load out ended: what came meanwhile, to start now (a load is out again for it), if anything did.
    pub fn finish(&mut self) -> Option<Batch> {
        let next = self.queued.take();
        self.busy = next.is_some();
        next
    }
}

/// Where a root's events come from: the root as the panel has it, as the file system spells it (FSEvents reports
/// that: `/private/tmp` for `/tmp`), and its repository's git folders, the worktree's own first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot {
    pub root: PathBuf,
    pub real_root: PathBuf,
    pub git_dirs: Vec<PathBuf>,
}

impl Spot {
    pub fn new(root: PathBuf, git_dirs: Vec<PathBuf>) -> Spot {
        let real_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let mut dirs: Vec<PathBuf> = Vec::new();
        for dir in git_dirs.into_iter().map(|d| d.canonicalize().unwrap_or(d)) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        Spot { root, real_root, git_dirs: dirs }
    }

    /// The root and its git folders, from `git rev-parse` in it (none outside a repository).
    pub async fn locate(root: PathBuf) -> Spot {
        let cmd = runner::git_cmd(&root, &["rev-parse", "--absolute-git-dir", "--git-common-dir"]);
        let dirs = match runner::run(&Host::Local, cmd, LOCATE).await {
            // the common folder may be printed relative to the root
            Ok(out) if out.code == Some(0) => String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).map(|l| root.join(l)).collect(),
            _ => Vec::new(),
        };
        Spot::new(root, dirs)
    }

    /// `path` as under the panel's root, when it came spelled as the file system has it.
    fn shown(&self, path: &Path) -> PathBuf {
        match path.strip_prefix(&self.real_root) {
            Ok(rest) if self.real_root != self.root && !path.starts_with(&self.root) => self.root.join(rest),
            _ => path.to_path_buf(),
        }
    }

    /// `path` is inside a folder that is not watched: one in `skip`, a `.git`, a `node_modules`.
    fn unwatched(&self, skip: &HashSet<PathBuf>, path: &Path) -> bool {
        let mut at = path.parent();
        while let Some(dir) = at {
            if dir == self.root || !dir.starts_with(&self.root) {
                return false;
            }
            if skip.contains(dir) || skipped_name(dir) {
                return true;
            }
            at = dir.parent();
        }
        false
    }
}

/// A folder never watched by its name: git's own (a nested repository's too), and npm's.
fn skipped_name(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == ".git" || n == "node_modules")
}

/// A change at `rel` in a git folder says the status changed: the index, HEAD (and a merge's or a rebase's), a ref.
/// Lock files come and go around every write, objects and logs grow with every commit; neither says more.
fn git_status_changed(rel: &Path) -> bool {
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else { return false };
    if name.ends_with(".lock") {
        return false;
    }
    let mut parts = rel.components().map(|c| c.as_os_str());
    match parts.next().and_then(|p| p.to_str()) {
        Some("refs") => true,
        Some(first) if parts.next().is_none() => matches!(first, "index" | "HEAD" | "packed-refs" | "MERGE_HEAD" | "REBASE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD"),
        _ => false,
    }
}

/// An entry appeared at the event's path (made, or moved there): a folder is watched.
fn appeared(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Any | EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Any)))
}

/// The entry at the event's path went (deleted, or moved away): a folder watched there is not any more.
fn departed(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Remove(_) | EventKind::Any | EventKind::Modify(ModifyKind::Name(RenameMode::From | RenameMode::Any)))
}

/// A folder, not a link to one (a link is never walked nor watched: one to `..` would watch everything above).
pub fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// What one event at `path` asks for. `is_dir` is asked only of an entry that appeared (a new folder is watched).
pub fn classify(spot: &Spot, skip: &HashSet<PathBuf>, kind: &EventKind, path: &Path, is_dir: &dyn Fn(&Path) -> bool) -> Batch {
    let mut batch = Batch::default();
    // opens and reads: our own reads and git's would come back as changes
    if matches!(kind, EventKind::Access(_)) {
        return batch;
    }
    // a save's own temp file (`files::save`), made and renamed over its file: the file's own event says it changed
    if path.file_name().is_some_and(super::save::is_temp_name) {
        return batch;
    }
    if departed(kind) {
        // only those watched are kept (`run`): a deleted file costs nothing
        batch.gone.insert(path.to_path_buf());
    }
    // the worktree's git folder first: it lies inside the common one (both as the file system spells them, as
    // they are watched)
    if let Some(rel) = spot.git_dirs.iter().find_map(|d| path.strip_prefix(d).ok()) {
        batch.status = git_status_changed(rel);
        // a new folder of refs (`refs/heads/feat/`, a new remote's) is watched too
        if appeared(kind) && rel.starts_with("refs") && is_dir(path) {
            batch.new_dirs.insert(path.to_path_buf());
        }
        return batch;
    }
    let path = spot.shown(path);
    let Ok(rel) = path.strip_prefix(&spot.root) else { return batch };
    if spot.unwatched(skip, &path) {
        batch.gone.clear();
        return batch;
    }
    batch.status = true;
    if let Some(parent) = path.parent().filter(|p| p.starts_with(&spot.root) && !rel.as_os_str().is_empty()) {
        batch.dirs.insert(parent.to_path_buf());
    }
    batch.rules = path.file_name().is_some_and(|n| n == ".gitignore");
    if appeared(kind) && !rel.as_os_str().is_empty() && !skip.contains(&path) && !skipped_name(&path) && is_dir(&path) {
        batch.new_dirs.insert(path.clone());
    }
    batch.dirs.insert(path);
    batch
}

/// What a whole event asks for: each of its paths', or everything when events were lost.
pub fn classify_event(spot: &Spot, skip: &HashSet<PathBuf>, event: &Event, is_dir: &dyn Fn(&Path) -> bool) -> Batch {
    let mut batch = Batch::default();
    if event.need_rescan() {
        batch.merge(Batch { rewalk: true, ..Batch::everything() });
    }
    // a rename both ends of which were heard: the first path is where it went from, the second where to
    let both = EventKind::Modify(ModifyKind::Name(RenameMode::Both));
    let (from, to) = (EventKind::Modify(ModifyKind::Name(RenameMode::From)), EventKind::Modify(ModifyKind::Name(RenameMode::To)));
    for (i, path) in event.paths.iter().enumerate() {
        let kind = match (event.kind == both, i) {
            (true, 0) => &from,
            (true, _) => &to,
            (false, _) => &event.kind,
        };
        batch.merge(classify(spot, skip, kind, path, is_dir));
    }
    batch
}

/// Walks the folders to watch from `start` down, `start` first: all but a `.git`, a `node_modules`, a cache folder
/// (tagged `CACHEDIR.TAG`) and those in `skip` (git ignores them), the folders found to be caches joining `skip`.
/// Each is handed to `watch` before it is read, so a folder made in it meanwhile is either listed or heard of. Links
/// are not followed (nor is `start`, unless `link_ok`: the root may be one). Stops at `watch`'s error, or at
/// `cancel`. Blocking.
pub fn walk(start: &Path, link_ok: bool, skip: &mut HashSet<PathBuf>, cancel: &AtomicBool, watch: &mut dyn FnMut(&Path) -> Result<(), String>) -> Result<(), String> {
    if !link_ok && !real_dir(start) {
        return Ok(());
    }
    let mut stack = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        watch(&dir)?;
        // a folder gone or locked meanwhile is not watched; the rest still are
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            if skipped_name(&path) || skip.contains(&path) {
                continue;
            }
            if path.join("CACHEDIR.TAG").is_file() {
                skip.insert(path);
                continue;
            }
            stack.push(path);
        }
    }
    Ok(())
}

/// The folders git ignores in `out` (`git ls-files -z --others --ignored --directory`, paths from `root`: a folder
/// ends in `/`), under `root`.
pub fn parse_ignored(root: &Path, out: &[u8]) -> HashSet<PathBuf> {
    out.split(|&b| b == 0)
        .filter_map(|p| p.strip_suffix(b"/"))
        .filter(|p| !p.is_empty())
        .map(|p| root.join(crate::platform::os_from_bytes(p)))
        .collect()
}

/// The folders git ignores under `root`, each once (git does not look inside them); none outside a repository.
async fn ignored_dirs(root: &Path) -> HashSet<PathBuf> {
    let cmd = runner::git_cmd(root, &["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"]);
    match runner::run(&Host::Local, cmd, IGNORED).await {
        // a cut answer still names the ones it got to
        Ok(out) if out.code == Some(0) || out.truncated => parse_ignored(root, &out.stdout),
        _ => HashSet::new(),
    }
}

/// Of `dirs` (new folders under `root`), those git ignores.
async fn ignored_of(root: &Path, dirs: &[PathBuf]) -> HashSet<PathBuf> {
    let mut input = Vec::new();
    for dir in dirs {
        if let Ok(rel) = dir.strip_prefix(root) {
            input.extend_from_slice(&crate::platform::git_path(rel));
            input.push(0);
        }
    }
    let mut cmd = runner::git_cmd(root, &["check-ignore", "-z", "--stdin"]);
    cmd.stdin = Some(input);
    match runner::run(&Host::Local, cmd, IGNORED).await {
        Ok(out) if out.code == Some(0) => out.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| root.join(crate::platform::os_from_bytes(p))).collect(),
        _ => HashSet::new(),
    }
}

/// The watcher, what it watches and what it leaves out; shared by the task and the blocking work that adds watches.
struct Registry {
    watcher: RecommendedWatcher,
    watched: HashSet<PathBuf>,
    skip: HashSet<PathBuf>,
    /// Set when the watch is dropped: a walk out stops at its next folder.
    cancel: Arc<AtomicBool>,
}

/// Why a watch failed, when it means watching must stop; `None` for a folder gone or locked (the rest still work).
fn fatal(e: &::notify::Error) -> Option<String> {
    match &e.kind {
        ::notify::ErrorKind::MaxFilesWatch => Some(LIMIT.into()),
        ::notify::ErrorKind::PathNotFound | ::notify::ErrorKind::WatchNotFound => None,
        ::notify::ErrorKind::Io(io) if matches!(io.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied) => None,
        _ => Some(e.to_string()),
    }
}

/// Watches `dir`, once, within `MAX_WATCHED` all told. `must`: its failure is watching's (the root's).
fn add_watch(watcher: &mut RecommendedWatcher, watched: &mut HashSet<PathBuf>, dir: &Path, mode: RecursiveMode, must: bool) -> Result<(), String> {
    if watched.contains(dir) {
        return Ok(());
    }
    if watched.len() >= MAX_WATCHED {
        return Err(TOO_MANY.into());
    }
    match watcher.watch(dir, mode) {
        Ok(()) => {
            watched.insert(dir.to_path_buf());
            Ok(())
        }
        Err(e) => match fatal(&e) {
            Some(why) => Err(why),
            None if must => Err(e.to_string()),
            None => Ok(()),
        },
    }
}

impl Registry {
    /// The watcher, its events to `tx`, and the root's watches (blocking).
    fn start(spot: &Spot, skip: HashSet<PathBuf>, cancel: Arc<AtomicBool>, tx: tokio::sync::mpsc::UnboundedSender<::notify::Result<Event>>) -> Result<Registry, String> {
        let watcher = ::notify::recommended_watcher(move |event: ::notify::Result<Event>| {
            let _ = tx.send(event);
        })
        .map_err(|e| format!("the file watcher didn't start: {e}"))?;
        let mut registry = Registry { watcher, watched: HashSet::new(), skip, cancel };
        if PER_FOLDER {
            registry.add_tree(&spot.root, true)?;
            for dir in &spot.git_dirs {
                add_watch(&mut registry.watcher, &mut registry.watched, dir, RecursiveMode::NonRecursive, false)?;
                registry.add_tree(&dir.join("refs"), false)?;
            }
        } else {
            add_watch(&mut registry.watcher, &mut registry.watched, &spot.root, RecursiveMode::Recursive, true)?;
            for dir in spot.git_dirs.iter().filter(|d| !d.starts_with(&spot.real_root)) {
                add_watch(&mut registry.watcher, &mut registry.watched, dir, RecursiveMode::Recursive, false)?;
            }
        }
        Ok(registry)
    }

    /// Watches each folder from `start` down (`walk`). `root`: it is the root, whose failure is watching's, and
    /// which may be a link.
    fn add_tree(&mut self, start: &Path, root: bool) -> Result<(), String> {
        let (watcher, watched) = (&mut self.watcher, &mut self.watched);
        let mut first = root;
        walk(start, root, &mut self.skip, &self.cancel, &mut |dir| {
            let must = mem::take(&mut first);
            add_watch(watcher, watched, dir, RecursiveMode::NonRecursive, must)
        })
    }

    /// `gone` folders, and every watched one under them, are not watched any more (inotify drops a deleted
    /// folder's watch itself; one moved away is let go of here).
    fn forget(&mut self, gone: &BTreeSet<PathBuf>) {
        if gone.is_empty() {
            return;
        }
        let dropped: Vec<PathBuf> = self.watched.iter().filter(|d| d.ancestors().any(|a| gone.contains(a))).cloned().collect();
        for dir in dropped {
            let _ = self.watcher.unwatch(&dir);
            self.watched.remove(&dir);
        }
    }

    /// After lost events: the watched folders no longer there are let go of.
    fn prune(&mut self) {
        let gone: BTreeSet<PathBuf> = self.watched.iter().filter(|d| !d.is_dir()).cloned().collect();
        self.forget(&gone);
    }
}

/// A root being watched; dropping it stops the watching.
pub struct Watch {
    task: AbortHandle,
    /// Stops a walk under `spawn_blocking`, which aborting the task does not: roots switched quickly would leave
    /// several walks, and their watchers, at work.
    cancel: Arc<AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}

impl Watch {
    /// Starts watching `root` on the runtime; `out` hears `Live` or `Off`, then the batches.
    pub fn start(rt: &Handle, root: PathBuf, out: futures::channel::mpsc::UnboundedSender<Message>) -> Watch {
        let cancel = Arc::new(AtomicBool::new(false));
        Watch { task: rt.spawn(run(root, cancel.clone(), out)).abort_handle(), cancel }
    }
}

async fn run(root: PathBuf, cancel: Arc<AtomicBool>, out: futures::channel::mpsc::UnboundedSender<Message>) {
    let off = |why: String| {
        let _ = out.unbounded_send(Message::Off(why));
    };
    let spot = Spot::locate(root.clone()).await;
    let skip = ignored_dirs(&root).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let started = {
        let spot = spot.clone();
        tokio::task::spawn_blocking(move || Registry::start(&spot, skip, cancel, tx)).await
    };
    let registry = match started {
        Ok(Ok(registry)) => Arc::new(Mutex::new(registry)),
        Ok(Err(why)) => return off(why),
        // the runtime shutting down
        Err(_) => return,
    };
    if out.unbounded_send(Message::Live).is_err() {
        return;
    }
    let mut batcher = Batcher::new(WINDOW);
    loop {
        let event = match batcher.deadline() {
            None => rx.recv().await,
            Some(at) => tokio::select! {
                event = rx.recv() => event,
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(at)) => {
                    let Some(mut batch) = batcher.take(Instant::now()) else { continue };
                    let rewalk = mem::take(&mut batch.rewalk);
                    let gone = mem::take(&mut batch.gone);
                    let starts: Vec<PathBuf> = if rewalk { vec![root.clone()] } else { mem::take(&mut batch.new_dirs).into_iter().collect() };
                    batch.new_dirs.clear();
                    if PER_FOLDER && (rewalk || !starts.is_empty() || !gone.is_empty()) {
                        // a new folder git ignores (a first build's `target`) is left out, as at the start; git is
                        // not asked of its own folders
                        let in_tree: Vec<PathBuf> = starts.iter().filter(|d| !spot.git_dirs.iter().any(|g| d.starts_with(g))).cloned().collect();
                        let ignored = if rewalk || in_tree.is_empty() { HashSet::new() } else { ignored_of(&root, &in_tree).await };
                        let (registry, refs) = (registry.clone(), spot.git_dirs.iter().map(|g| g.join("refs")).collect::<Vec<_>>());
                        let added = tokio::task::spawn_blocking(move || {
                            let mut registry = registry.lock().unwrap_or_else(|e| e.into_inner());
                            // what went first: a folder deleted and made again is watched afresh
                            registry.forget(&gone);
                            if rewalk {
                                registry.prune();
                            }
                            registry.skip.extend(ignored);
                            let starts: Vec<PathBuf> = starts.into_iter().chain(refs.into_iter().filter(|_| rewalk)).filter(|d| !registry.skip.contains(d)).collect();
                            for start in starts {
                                registry.add_tree(&start, false)?;
                            }
                            Ok::<(), String>(())
                        })
                        .await;
                        match added {
                            Ok(Ok(())) => {}
                            Ok(Err(why)) => return off(why),
                            Err(_) => return,
                        }
                    }
                    if out.unbounded_send(Message::Batch(batch)).is_err() {
                        return;
                    }
                    continue;
                }
            },
        };
        // the watcher gone (it holds the only sender)
        let Some(event) = event else { return };
        match event {
            Ok(event) => {
                let is_dir = |p: &Path| PER_FOLDER && real_dir(p);
                let batch = {
                    let registry = registry.lock().unwrap_or_else(|e| e.into_inner());
                    let mut batch = classify_event(&spot, &registry.skip, &event, &is_dir);
                    // a file deleted is nothing to forget
                    batch.gone.retain(|p| registry.watched.contains(p));
                    batch
                };
                batcher.push(Instant::now(), batch);
            }
            Err(e) => {
                if let Some(why) = fatal(&e) {
                    return off(why);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::notify::event::{AccessKind, CreateKind, DataChange, Flag};

    fn spot() -> Spot {
        Spot { root: "/r".into(), real_root: "/r".into(), git_dirs: vec!["/r/.git".into()] }
    }

    fn set(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn modify(path: &str) -> Batch {
        classify(&spot(), &HashSet::new(), &EventKind::Modify(ModifyKind::Data(DataChange::Content)), Path::new(path), &|_| false)
    }

    /// An edit in the work tree: its folder (and the entry) read again, git asked; a `.gitignore` says the rules
    /// changed too.
    #[test]
    fn a_work_tree_change_asks_for_its_folder_and_git() {
        let b = modify("/r/src/a.rs");
        assert_eq!((b.dirs, b.status, b.rules), (set(&["/r/src", "/r/src/a.rs"]), true, false));
        let b = modify("/r/.gitignore");
        assert_eq!((b.dirs, b.status, b.rules), (set(&["/r", "/r/.gitignore"]), true, true));
        // the root itself (its own attributes): no folder above it to read
        assert_eq!(modify("/r").dirs, set(&["/r"]));
        // outside the root, and a read: nothing
        assert!(modify("/elsewhere/x").is_empty());
        let read = classify(&spot(), &HashSet::new(), &EventKind::Access(AccessKind::Read), Path::new("/r/a"), &|_| true);
        assert!(read.is_empty());
    }

    /// In the git folder only the index, HEAD and refs count; locks, objects and logs do not, nor does the rest.
    #[test]
    fn git_folder_events_say_status_only_for_the_index_head_and_refs() {
        for path in ["/r/.git/index", "/r/.git/HEAD", "/r/.git/refs/heads/main", "/r/.git/refs/remotes/origin/feat/x", "/r/.git/packed-refs", "/r/.git/MERGE_HEAD"] {
            let b = modify(path);
            assert!(b.status && b.dirs.is_empty(), "{path}");
        }
        for path in ["/r/.git/index.lock", "/r/.git/refs/heads/main.lock", "/r/.git/objects/ab/cdef", "/r/.git/logs/HEAD", "/r/.git/config", "/r/.git/COMMIT_EDITMSG", "/r/.git", "/r/.git/hooks/HEAD"] {
            assert!(modify(path).is_empty(), "{path}");
        }
        // a linked worktree: its own folder's index counts, another worktree's does not
        let wt = Spot::new("/w".into(), vec!["/c/.git/worktrees/w".into(), "/c/.git".into()]);
        let kind = EventKind::Modify(ModifyKind::Any);
        assert!(classify(&wt, &HashSet::new(), &kind, Path::new("/c/.git/worktrees/w/index"), &|_| false).status);
        assert!(classify(&wt, &HashSet::new(), &kind, Path::new("/c/.git/refs/heads/x"), &|_| false).status);
        assert!(classify(&wt, &HashSet::new(), &kind, Path::new("/c/.git/worktrees/other/index"), &|_| false).is_empty());
    }

    /// Inside an unwatched folder (one git ignores, `node_modules`, a nested `.git`) nothing counts; the folder's
    /// own appearing still re-reads its parent, and is not watched.
    #[test]
    fn the_ignore_filter_drops_what_is_under_unwatched_folders() {
        let skip = HashSet::from([PathBuf::from("/r/target")]);
        let create = EventKind::Create(CreateKind::Folder);
        let at = |path: &str| classify(&spot(), &skip, &create, Path::new(path), &|_| true);
        assert!(at("/r/target/debug/x").is_empty());
        assert!(at("/r/web/node_modules/left-pad/index.js").is_empty());
        assert!(at("/r/vendor/lib/.git/index").is_empty());
        let b = at("/r/target");
        assert_eq!((b.dirs, b.new_dirs.is_empty()), (set(&["/r", "/r/target"]), true));
        let b = at("/r/node_modules");
        assert!(b.status && b.new_dirs.is_empty());
        // a new folder elsewhere is watched; a file is not, nor a folder only modified
        assert_eq!(at("/r/src/new").new_dirs, set(&["/r/src/new"]));
        assert!(classify(&spot(), &skip, &EventKind::Create(CreateKind::File), Path::new("/r/f"), &|_| false).new_dirs.is_empty());
        let moved_in = EventKind::Modify(ModifyKind::Name(RenameMode::To));
        assert_eq!(classify(&spot(), &skip, &moved_in, Path::new("/r/m"), &|_| true).new_dirs, set(&["/r/m"]));
        assert!(modify("/r/src").new_dirs.is_empty());
    }

    /// FSEvents spells a path as the file system does (`/private/tmp`); it is mapped back under the root shown.
    #[test]
    fn a_real_path_maps_back_under_the_shown_root() {
        let s = Spot { root: "/tmp/r".into(), real_root: "/private/tmp/r".into(), git_dirs: vec!["/private/tmp/r/.git".into()] };
        let kind = EventKind::Modify(ModifyKind::Any);
        assert_eq!(classify(&s, &HashSet::new(), &kind, Path::new("/private/tmp/r/a"), &|_| false).dirs, set(&["/tmp/r", "/tmp/r/a"]));
        assert_eq!(classify(&s, &HashSet::new(), &kind, Path::new("/tmp/r/a"), &|_| false).dirs, set(&["/tmp/r", "/tmp/r/a"]));
        assert!(classify(&s, &HashSet::new(), &kind, Path::new("/private/tmp/r/.git/HEAD"), &|_| false).status);
    }

    #[test]
    fn an_overflow_rescans_and_a_rename_counts_both_ends() {
        let lost = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(classify_event(&spot(), &HashSet::new(), &lost, &|_| false), Batch { rewalk: true, ..Batch::everything() });
        let both = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both))).add_path("/r/a/x".into()).add_path("/r/b/x".into());
        let b = classify_event(&spot(), &HashSet::new(), &both, &|_| true);
        assert_eq!(b.dirs, set(&["/r/a", "/r/a/x", "/r/b", "/r/b/x"]));
        // the first path went (its watch is let go of), the second came (watched afresh)
        assert_eq!((b.gone, b.new_dirs), (set(&["/r/a/x"]), set(&["/r/b/x"])));
    }

    /// A save writes a temp file beside its file and renames it over: only the file's own end counts.
    #[test]
    fn a_save_s_temp_file_is_not_a_change() {
        assert!(modify("/r/src/.a.rs.shepherd-41-3~").is_empty());
        let renamed = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both))).add_path("/r/src/.a.rs.shepherd-41-3~".into()).add_path("/r/src/a.rs".into());
        let b = classify_event(&spot(), &HashSet::new(), &renamed, &|_| false);
        assert_eq!((b.dirs, b.status, b.gone), (set(&["/r/src", "/r/src/a.rs"]), true, BTreeSet::new()));
        // a name only like one is a file like any other
        assert!(!modify("/r/src/.a.rs.shepherd-x~").is_empty());
    }

    /// A folder deleted or moved away is forgotten, so the same path made again is watched; one under an
    /// unwatched folder is not.
    #[test]
    fn a_folder_gone_is_forgotten() {
        let remove = EventKind::Remove(::notify::event::RemoveKind::Folder);
        assert_eq!(classify(&spot(), &HashSet::new(), &remove, Path::new("/r/sub"), &|_| true).gone, set(&["/r/sub"]));
        let away = EventKind::Modify(ModifyKind::Name(RenameMode::From));
        assert_eq!(classify(&spot(), &HashSet::new(), &away, Path::new("/r/sub"), &|_| true).gone, set(&["/r/sub"]));
        assert!(classify(&spot(), &HashSet::new(), &remove, Path::new("/r/node_modules/x"), &|_| true).gone.is_empty());
        assert!(modify("/r/sub").gone.is_empty());
    }

    /// A new folder of refs (a branch named `feat/x`, a new remote) is watched; one in objects is not.
    #[test]
    fn new_ref_folders_are_watched() {
        let create = EventKind::Create(CreateKind::Folder);
        let b = classify(&spot(), &HashSet::new(), &create, Path::new("/r/.git/refs/remotes/origin"), &|_| true);
        assert_eq!((b.status, b.new_dirs), (true, set(&["/r/.git/refs/remotes/origin"])));
        assert!(classify(&spot(), &HashSet::new(), &create, Path::new("/r/.git/refs/heads/feat"), &|_| true).new_dirs.contains(Path::new("/r/.git/refs/heads/feat")));
        assert!(classify(&spot(), &HashSet::new(), &create, Path::new("/r/.git/objects/ab"), &|_| true).is_empty());
    }

    /// Past `MAX_BATCH` folders a batch turns into a rescan of the open folders (no list of paths); past it of new
    /// or gone folders, into a walk of the root again.
    #[test]
    fn a_batch_past_its_cap_becomes_a_rescan() {
        let mut b = Batch::default();
        for i in 0..MAX_BATCH / 2 {
            b.merge(modify(&format!("/r/d{i}/f")));
        }
        assert_eq!((b.dirs.len(), b.rescan), (MAX_BATCH, false));
        b.merge(modify("/r/one/more"));
        assert!(b.rescan && b.status && b.dirs.is_empty());
        b.merge(modify("/r/after"));
        assert!(b.dirs.is_empty());
        let create = EventKind::Create(CreateKind::Folder);
        let mut b = Batch::default();
        for i in 0..=MAX_BATCH {
            b.merge(classify(&spot(), &HashSet::new(), &create, Path::new(&format!("/r/n{i}")), &|_| true));
        }
        assert!(b.rewalk && b.new_dirs.is_empty());
        // the batcher merges the same way
        let mut batcher = Batcher::new(WINDOW);
        let t0 = Instant::now();
        for i in 0..=MAX_BATCH {
            batcher.push(t0, modify(&format!("/r/e{i}")));
        }
        assert!(batcher.take(t0 + WINDOW).is_some_and(|b| b.rescan && b.dirs.is_empty()));
    }

    /// A batch is due `WINDOW` after its first event, however many follow (a steady stream still lands); an empty
    /// event opens none.
    #[test]
    fn batches_are_due_a_window_after_their_first_event() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut b = Batcher::new(ms(300));
        b.push(t0, Batch::default());
        assert_eq!(b.deadline(), None);
        b.push(t0, modify("/r/a"));
        b.push(t0 + ms(200), modify("/r/src/b"));
        assert_eq!(b.deadline(), Some(t0 + ms(300)));
        assert_eq!(b.take(t0 + ms(299)), None);
        let batch = b.take(t0 + ms(300)).unwrap();
        assert_eq!(batch.dirs, set(&["/r", "/r/a", "/r/src", "/r/src/b"]));
        assert_eq!((b.deadline(), b.take(t0 + ms(900))), (None, None));
        // the next window starts at the next event
        b.push(t0 + ms(1000), modify("/r/.git/index"));
        assert_eq!(b.deadline(), Some(t0 + ms(1300)));
        assert_eq!(b.take(t0 + ms(1300)).map(|b| (b.status, b.dirs.len())), Some((true, 0)));
    }

    /// One load out: what comes meanwhile is merged into one follow-up, started when the load ends.
    #[test]
    fn loads_coalesce_into_one_follow_up() {
        let mut loads = Loads::default();
        assert_eq!(loads.start(modify("/r/a")), Some(modify("/r/a")));
        assert!(loads.busy());
        assert_eq!(loads.start(modify("/r/b")), None);
        assert_eq!(loads.start(modify("/r/.git/HEAD")), None);
        let follow = loads.finish().unwrap();
        assert_eq!((follow.dirs, follow.status), (set(&["/r", "/r/b"]), true));
        assert!(loads.busy());
        assert_eq!(loads.finish(), None);
        assert!(!loads.busy());
        assert!(loads.start(Batch::default()).is_some());
    }

    #[test]
    fn the_ignored_folders_are_the_lines_ending_in_a_slash() {
        let found = parse_ignored(Path::new("/r"), b"target/\0a.log\0web/node_modules/\0/\0");
        assert_eq!(found, HashSet::from([PathBuf::from("/r/target"), PathBuf::from("/r/web/node_modules")]));
    }

    /// The walk leaves out `.git`, `node_modules`, a tagged cache and what git ignores, follows no link, and stops
    /// past its budget.
    #[cfg(unix)]
    #[test]
    fn the_walk_leaves_out_heavy_folders_and_keeps_to_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for d in ["src/a", ".git/objects", "node_modules/x", "build/out", "ignored/deep", "cache/x"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("cache/CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        // links to a big folder and to `..`: neither is walked
        std::os::unix::fs::symlink(root.join("src"), root.join("link")).unwrap();
        std::os::unix::fs::symlink("..", root.join("up")).unwrap();
        std::os::unix::fs::symlink("/usr", root.join("big")).unwrap();
        let mut skip = HashSet::from([root.join("ignored")]);
        let cancel = AtomicBool::new(false);
        let mut found = Vec::new();
        walk(root, true, &mut skip, &cancel, &mut |d| {
            found.push(d.to_path_buf());
            Ok(())
        })
        .unwrap();
        found.sort();
        let rel: Vec<_> = found.iter().map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(rel, ["", "build", "build/out", "src", "src/a"]);
        assert!(skip.contains(&root.join("cache")));
        // a link handed as a start (a new entry that is one) is not walked either
        let mut none = 0;
        walk(&root.join("up"), false, &mut skip, &cancel, &mut |_| {
            none += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(none, 0);
        assert!(real_dir(&root.join("src")) && !real_dir(&root.join("link")) && !real_dir(&root.join("up")));
        // the watch's error (its budget spent) stops the walk
        let mut count = 0;
        let spent = walk(root, true, &mut skip, &cancel, &mut |_| {
            count += 1;
            if count > 2 { Err(TOO_MANY.into()) } else { Ok(()) }
        });
        assert_eq!((spent, count), (Err(TOO_MANY.into()), 3));
        // and so does the watch being dropped
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(walk(root, true, &mut skip, &cancel, &mut |_| Ok(())), Err(CANCELLED.into()));
    }

    /// Each folder is watched before it is read: one made in it between the two is still found.
    #[test]
    fn a_folder_is_watched_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut found = Vec::new();
        walk(&root, true, &mut HashSet::new(), &AtomicBool::new(false), &mut |d| {
            if d == root {
                // made after the root's watch, before its read
                std::fs::create_dir(root.join("late")).unwrap();
            }
            found.push(d.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert_eq!(found, [root.clone(), root.join("late")]);
    }

    /// A real watcher on a temp repository: an edit, a new folder (then watched), one deleted and made again
    /// (watched again), a link to the folder above (not followed), git's index, and nothing from under
    /// `node_modules`.
    #[cfg(target_os = "linux")]
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_real_watch_hears_edits_new_folders_and_the_index() {
        use futures::StreamExt;
        let git = |dir: &Path, args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(dir).env("GIT_CONFIG_NOSYSTEM", "1").output().is_ok_and(|o| o.status.success())
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        if !git(&root, &["init", "-q"]) {
            return;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let _watch = Watch::start(&Handle::current(), root.clone(), tx);
        let next = async |rx: &mut futures::channel::mpsc::UnboundedReceiver<Message>| tokio::time::timeout(Duration::from_secs(5), rx.next()).await.ok().flatten();
        assert_eq!(next(&mut rx).await, Some(Message::Live));
        // batches until one has `want` among its folders (or git asked, for `None`)
        let until = async |rx: &mut futures::channel::mpsc::UnboundedReceiver<Message>, want: Option<PathBuf>| {
            let mut seen = Vec::new();
            loop {
                match next(&mut *rx).await {
                    Some(Message::Batch(b)) => {
                        let done = match &want {
                            Some(p) => b.dirs.contains(p),
                            None => b.status && b.dirs.is_empty(),
                        };
                        seen.push(b);
                        if done {
                            return seen;
                        }
                    }
                    other => panic!("{other:?} after {seen:?}"),
                }
            }
        };
        std::fs::write(root.join("a.txt"), "one").unwrap();
        let seen = until(&mut rx, Some(root.join("a.txt"))).await;
        assert!(seen.iter().any(|b| b.status && b.dirs.contains(&root)));

        std::fs::create_dir(root.join("sub")).unwrap();
        until(&mut rx, Some(root.join("sub"))).await;
        std::fs::write(root.join("sub/b.txt"), "two").unwrap();
        until(&mut rx, Some(root.join("sub/b.txt"))).await;

        // deleted and made again: watched again
        std::fs::remove_dir_all(root.join("sub")).unwrap();
        until(&mut rx, Some(root.join("sub"))).await;
        std::fs::create_dir(root.join("sub")).unwrap();
        until(&mut rx, Some(root.join("sub"))).await;
        std::fs::write(root.join("sub/c.txt"), "three").unwrap();
        until(&mut rx, Some(root.join("sub/c.txt"))).await;

        // a link to the folder above: not followed, so what changes there is not heard through it
        std::os::unix::fs::symlink(dir.path(), root.join("up")).unwrap();
        until(&mut rx, Some(root.join("up"))).await;
        std::fs::write(dir.path().join("outside.txt"), "").unwrap();
        std::fs::write(root.join("sentinel0"), "").unwrap();
        let seen = until(&mut rx, Some(root.join("sentinel0"))).await;
        assert!(seen.iter().all(|b| !b.dirs.contains(&root.join("up/outside.txt"))), "{seen:?}");

        std::fs::create_dir(root.join("node_modules")).unwrap();
        until(&mut rx, Some(root.join("node_modules"))).await;
        std::fs::write(root.join("node_modules/c.js"), "").unwrap();
        std::fs::write(root.join("sentinel"), "").unwrap();
        let seen = until(&mut rx, Some(root.join("sentinel"))).await;
        assert!(seen.iter().all(|b| !b.dirs.contains(&root.join("node_modules/c.js"))), "{seen:?}");

        assert!(git(&root, &["add", "a.txt"]));
        let seen = until(&mut rx, None).await;
        assert!(seen.last().unwrap().dirs.is_empty());
    }
}
