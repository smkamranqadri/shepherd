//! The Changes tab: the root's git changes as VS Code's Source Control lists them (merge conflicts, staged, changes,
//! untracked, each under a header that folds), the branch over them, and their count on the tab's icon. `git status`
//! runs while the panel is open in the active window, on whichever tab, so the badge stays true: when the root's
//! watch (`files::watch`) hears the work tree, the index, HEAD or a ref change, and otherwise every
//! `super::files::POLL` while the Changes tab shows, every `BADGE_POLL` while only its badge does (every
//! `watch::SAFETY_POLL` while the root is watched), never sooner than five times the last run took (`poll_delay`), so
//! a repository where git takes seconds is not kept busy. One run at a time, on the runtime, under `files::runner`'s
//! deadline and output cap; a change heard while it is out asks for one more. An answer lands only on the list of the
//! root it was asked for (a root changed meanwhile drops it, and the run with it), and only a different answer
//! redraws.
use super::{SidePanel, SidePanelEvent};
use crate::files::git_ops::{self, Op};
use crate::files::icons::material_icon;
use crate::files::runner::{self, Host, Limits, Output, RunError};
use crate::files::status::{self, Change, GitError, Group, Status};
use crate::shell::widgets::{menu_card, menu_choice, tip};
use crate::theme::Theme;
use gpui::{
    AnyElement, ClickEvent, Context, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, StatefulInteractiveElement, Styled,
    UniformListScrollHandle, Window, div, prelude::*, px,
};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::task::AbortHandle;

/// How often git is asked while only the Changes tab's badge is in view (the panel open on another tab): the count
/// may lag a little; the list itself, once shown, is asked at once and then every `POLL`.
const BADGE_POLL: Duration = Duration::from_secs(12);

/// How long after a run that took `took` the next poll may start: the tab's pace (`POLL` while the Changes tab shows,
/// `BADGE_POLL` while only its badge does, `SAFETY_POLL` either way while the root is `watched`), but never sooner
/// than `floor(took)`. The polls double on `battery` (`power`); the safety net is slow already.
pub(super) fn poll_delay(took: Duration, tab_shown: bool, watched: bool, battery: bool) -> Duration {
    let pace = match (watched, tab_shown) {
        (true, _) => crate::files::watch::SAFETY_POLL,
        (false, true) => super::files::poll_on(battery),
        (false, false) => crate::power::stretch_on(BADGE_POLL, battery),
    };
    pace.max(floor(took))
}

/// How long after a run that took `took` any next one may start, a change heard or not: five times `took`, so git
/// gets at most a fifth of a core however slow the repository.
fn floor(took: Duration) -> Duration {
    took.saturating_mul(5)
}

/// How long after a run that took `took` one asked for by a change the watch heard may start: `WATCH_PACE` while the
/// Changes tab shows, `BADGE_POLL` while only its badge does, never sooner than `floor(took)`. Files git ignores
/// (a log written line by line, an editor's swap file, `*.pyc`) are heard too; this keeps their churn from running
/// git back to back.
fn watch_delay(took: Duration, tab_shown: bool) -> Duration {
    let pace = if tab_shown { WATCH_PACE } else { BADGE_POLL };
    pace.max(floor(took))
}

/// The least time between runs a change heard asks for while the Changes tab shows.
const WATCH_PACE: Duration = Duration::from_millis(1500);

/// A failure that says nothing about the changes themselves (git slow this once, a lock, a hiccup): the last good
/// list and badge stay, marked as not refreshed, rather than flap to an error and back.
fn transient(e: &GitError) -> bool {
    matches!(e, GitError::Timeout | GitError::Failed(_))
}

/// A failure that will only repeat at the next tick, at the same cost: polling stops until the tab is shown again
/// (or the root changes).
fn halts(e: &GitError) -> bool {
    matches!(e, GitError::Timeout | GitError::TooLarge)
}

/// `git status` on a big work tree (a monorepo, a checked-in `node_modules`) takes seconds; past this it is reported
/// rather than waited on, and its 8 MiB of output is about 80k changed paths, more than a list is any use for.
const STATUS: Limits = Limits { timeout: Duration::from_secs(15), max_bytes: 8 << 20 };
/// `git rev-parse --show-prefix`: one line.
const PREFIX: Limits = Limits { timeout: Duration::from_secs(5), max_bytes: 64 << 10 };
const ROW_HEIGHT: f32 = 22.;

/// What a run found: the status, and the root's place in the repository (`rev-parse --show-prefix`), which the rows
/// take off the paths git prints from the repository's top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Answer {
    pub(super) status: Status,
    pub(super) prefix: String,
    /// What the commit box needs: a first commit, a merge or a rebase under way.
    pub(super) repo: RepoState,
}

/// The repository's state beside its status, read with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RepoState {
    /// No commit yet: Amend has nothing to amend, and unstaging takes files out of the index.
    pub(super) initial: bool,
    /// A merge, cherry-pick or revert is under way: the `MERGE_MSG` git wrote for it, comments left out, is what the
    /// message starts from.
    pub(super) merge_msg: Option<String>,
    pub(super) merging: bool,
    /// A cherry-pick or revert stopped on a conflict, which a commit here concludes: its name.
    pub(super) picking: Option<&'static str>,
    /// A rebase is under way: a commit here goes into it.
    pub(super) rebasing: bool,
}

/// One row of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeRow {
    Header { group: Group, count: usize, folded: bool },
    /// An index into the status's changes.
    Change(usize),
    /// "N more changes outside this folder", last.
    Outside(usize),
}

/// One root's changes: the last answer git gave, and the run out for the next.
pub(super) struct ChangeList {
    /// Told apart from the list of a root chosen since, whose answers must not land here.
    id: u64,
    root: PathBuf,
    /// `None` until the first run answers.
    result: Option<Result<Answer, GitError>>,
    rows: Vec<ChangeRow>,
    /// The run out; a tick while it is waits for it rather than pile up behind a slow repository.
    job: Option<AbortHandle>,
    /// Which run is the latest asked (`job`'s): the answer of an earlier one, stopped for an op or replaced, is no one's.
    run: u64,
    /// The next poll may not start before this (`poll_delay`); `None`: at the next chance.
    pub(super) next_at: Option<Instant>,
    /// When the last run ended and how long it took: a run for a change heard waits `watch_delay` after it.
    ran: Option<(Instant, Duration)>,
    /// The watch heard a change since the last run started: one more is due once `watch_delay` has passed.
    wanted: bool,
    /// A timer is out for that run.
    armed: bool,
    /// The last run timed out or printed too much (`halts`): no more runs until the tab is shown again.
    halted: bool,
    /// The last run failed in passing (`transient`) while `result` holds a good list, which stays: why it was not
    /// refreshed.
    stale: Option<GitError>,
    /// The change last clicked: its group and path.
    selected: Option<(Group, String)>,
    /// The row under the pointer (a change or a header), which shows its actions. Held here rather than a group hover: a hover style turns up
    /// only by paint, too late for a hidden child's prepaint, and gpui panics painting what it never prepainted.
    hovered: Option<usize>,
    scroll: UniformListScrollHandle,
    /// Stages, discards and commits landed: a run started before the last of them answers for the index as it was,
    /// and is dropped for one asked after.
    ops: u64,
}

impl Drop for ChangeList {
    /// A list replaced (another root, a machine's workspace) takes its run with it: the runner kills git on drop.
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.abort();
        }
    }
}

impl ChangeList {
    fn new(root: PathBuf) -> ChangeList {
        static IDS: AtomicU64 = AtomicU64::new(0);
        ChangeList {
            id: IDS.fetch_add(1, Ordering::Relaxed),
            root,
            result: None,
            rows: Vec::new(),
            job: None,
            run: 0,
            next_at: None,
            ran: None,
            wanted: false,
            armed: false,
            halted: false,
            stale: None,
            selected: None,
            hovered: None,
            scroll: UniformListScrollHandle::new(),
            ops: 0,
        }
    }

    fn answer(&self) -> Option<&Answer> {
        self.result.as_ref().and_then(|r| r.as_ref().ok())
    }

    /// The change at `(group, path)`, if the list still has it.
    fn find(&self, group: Group, path: &str) -> Option<&Change> {
        self.answer()?.status.changes.iter().find(|c| c.group == group && c.path == path)
    }
}

/// The right-click menu's change.
pub(super) struct ChangeMenu {
    at: Point<Pixels>,
    change: Change,
}

/// The rows for `changes` (sorted by group, as `parse_status_v2` leaves them): each group that has any under its
/// header, its changes unless it is folded, and the count of those outside the root last.
fn change_rows(changes: &[Change], outside: usize, folded: &HashSet<Group>) -> Vec<ChangeRow> {
    let mut rows = Vec::new();
    for group in Group::ALL {
        let members: Vec<usize> = changes.iter().enumerate().filter(|(_, c)| c.group == group).map(|(i, _)| i).collect();
        if members.is_empty() {
            continue;
        }
        let folded = folded.contains(&group);
        rows.push(ChangeRow::Header { group, count: members.len(), folded });
        if !folded {
            rows.extend(members.into_iter().map(ChangeRow::Change));
        }
    }
    if outside > 0 {
        rows.push(ChangeRow::Outside(outside));
    }
    rows
}

/// What a row's hover buttons do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowAction {
    Stage,
    Unstage,
    Discard,
    MarkResolved,
}

/// A row's buttons, left to right: a conflict is marked resolved, a staged change unstaged, a change or an untracked
/// file discarded (when `git_ops::discardable`) or staged. An untracked nested repository has none: git would embed
/// it, and a discard would delete a whole repository.
fn row_actions(change: &Change) -> Vec<RowAction> {
    let discard = git_ops::discardable(change).is_ok().then_some(RowAction::Discard);
    match change.group {
        Group::Conflicts => vec![RowAction::MarkResolved],
        Group::Staged => vec![RowAction::Unstage],
        Group::Untracked if change.is_folder() => Vec::new(),
        Group::Changes | Group::Untracked => discard.into_iter().chain([RowAction::Stage]).collect(),
    }
}

/// `path` (from the repository's top) as the root sees it.
fn relative<'a>(path: &'a str, prefix: &str) -> &'a str {
    path.strip_prefix(prefix).unwrap_or(path)
}

/// A change's letter in VS Code's colors: added and untracked green, deleted and conflicted red, the rest (modified,
/// renamed, copied, type changed) yellow.
fn code_color(code: char, theme: &Theme) -> Hsla {
    match code {
        'A' | 'U' | '?' => theme.success,
        'D' | '!' => theme.danger,
        _ => theme.warning,
    }
}

/// `args` in `root` under the runner's limits, its failures as the tab tells them.
async fn git(root: &Path, args: &[&str], limits: Limits) -> Result<Output, GitError> {
    match runner::run(&Host::Local, runner::git_cmd(root, args), limits).await {
        Ok(out) if out.truncated => Err(GitError::TooLarge),
        Ok(out) if out.code == Some(0) => Ok(out),
        Ok(out) => Err(status::classify(&out.stderr)),
        Err(RunError::NotFound(_)) => Err(GitError::GitMissing),
        Err(RunError::Timeout) => Err(GitError::Timeout),
        Err(RunError::Io(e)) => {
            // the runner's spawn fails alike for a missing program and a missing folder; it tells them apart by
            // the folder, which a hung mount could stall: asked under a deadline, here off the main thread
            let gone = tokio::time::timeout(Duration::from_secs(2), tokio::fs::metadata(root.to_path_buf())).await;
            Err(if matches!(gone, Ok(Err(_))) { GitError::FolderMissing } else { GitError::Failed(e) })
        }
        Err(e) => Err(GitError::Failed(e.to_string())),
    }
}

/// `path` is there, asked under a deadline (a hung mount answers no).
async fn exists(path: &Path) -> bool {
    matches!(tokio::time::timeout(Duration::from_secs(2), tokio::fs::metadata(path.to_path_buf())).await, Ok(Ok(_)))
}

/// The root's place in its repository and where a merge or rebase would keep its state, then its status.
async fn read_status(root: PathBuf) -> Result<Answer, GitError> {
    let paths = ["MERGE_HEAD", "MERGE_MSG", "rebase-merge", "rebase-apply", "CHERRY_PICK_HEAD", "REVERT_HEAD"];
    let mut args = vec!["rev-parse", "--show-prefix"];
    for p in paths {
        args.extend(["--git-path", p]);
    }
    let out = git(&root, &args, PREFIX).await?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = text.split('\n').map(|l| l.trim_end_matches('\r'));
    let prefix = lines.next().unwrap_or_default().to_string();
    // relative to the root (a linked worktree's are absolute)
    let found: Vec<PathBuf> = lines.take(paths.len()).map(|l| root.join(l)).collect();
    let out = git(&root, &["-c", "status.renames=true", "status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"], STATUS).await?;
    let mut repo = RepoState { initial: git_ops::initial_in_status(&out.stdout), ..Default::default() };
    if let [merge_head, merge_msg, rebase_merge, rebase_apply, cherry_pick, revert] = &found[..] {
        repo.merging = exists(merge_head).await;
        repo.rebasing = exists(rebase_merge).await || exists(rebase_apply).await;
        repo.picking = if exists(cherry_pick).await {
            Some("cherry-pick")
        } else if exists(revert).await {
            Some("revert")
        } else {
            None
        };
        if repo.merging || repo.picking.is_some() {
            let text = tokio::time::timeout(Duration::from_secs(2), tokio::fs::read_to_string(merge_msg.clone())).await;
            repo.merge_msg = text.ok().and_then(Result::ok).map(|t| git_ops::merge_message(&t)).filter(|m| !m.is_empty());
        }
    }
    Ok(Answer { status: status::parse_status_v2(&out.stdout, &prefix), prefix, repo })
}

/// `git config --global --add safe.directory <path>`, quoted for a shell when the path needs it.
fn safe_directory_command(path: &str) -> String {
    let plain = !path.is_empty() && path.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+@:~".contains(&b));
    let quoted = if plain { path.to_string() } else { format!("'{}'", path.replace('\'', r"'\''")) };
    format!("git config --global --add safe.directory {quoted}")
}

impl SidePanel {
    /// Keeps the list on the context's root: a new root gets a new list (asked at once while live); no root, or a
    /// machine's, none.
    pub(super) fn sync_changes(&mut self, cx: &mut Context<Self>) {
        let root = self.context.root.as_deref().filter(|_| !self.context.remote).map(PathBuf::from);
        if self.changes.as_ref().map(|l| &l.root) == root.as_ref() {
            return;
        }
        let counted = self.changes_count();
        self.changes = root.map(ChangeList::new);
        self.change_menu = None;
        if counted.is_some() {
            cx.emit(SidePanelEvent::ChangesCounted);
        }
        self.poll_changes(cx);
    }

    /// The Changes tab's badge, once git said: the rows the list has, so a file staged and edited again counts twice
    /// (once in Staged, once in Changes), as VS Code's Source Control count does.
    pub fn changes_count(&self) -> Option<usize> {
        self.changes.as_ref()?.answer().map(|a| a.status.changes.len())
    }

    /// The change at `(group, path)` (from the repository's top) as the last status listed it, if it still does.
    pub fn change_now(&self, group: Group, path: &str) -> Option<Change> {
        self.changes.as_ref()?.find(group, path).cloned()
    }

    /// The file at `path`'s Staged and Changes entries: both, and its diff tab offers All, Staged and Unstaged, each
    /// with its own rename source.
    pub fn entries(&self, path: &str) -> crate::files::diff::Entries {
        let Some(answer) = self.changes.as_ref().and_then(ChangeList::answer) else { return Default::default() };
        let entry = |group| answer.status.changes.iter().find(|c| c.group == group && c.path == path).cloned();
        crate::files::diff::Entries { staged: entry(Group::Staged), changes: entry(Group::Changes) }
    }

    /// The Shell's word on whether the panel (whose tab strip carries the badge) is open, whether it shows the
    /// Changes tab, and whether the window is active. Coming into view asks git at once, as does the tab coming
    /// into view (which also lifts a halt after a timeout); the tab going out of view takes the menu down.
    pub fn set_changes_shown(&mut self, panel_shown: bool, tab_shown: bool, active: bool, cx: &mut Context<Self>) {
        if !tab_shown && self.change_menu.as_ref().is_some_and(|m| m.is_open()) {
            self.change_menu = None;
            cx.notify();
        }
        let tab_woke = tab_shown && !self.changes_tab_shown;
        self.changes_tab_shown = tab_shown;
        if tab_woke && let Some(list) = self.changes.as_mut() {
            list.halted = false;
            list.next_at = None;
        }
        if self.panel_shown != panel_shown {
            self.panel_shown = panel_shown;
            self.sync_watch(cx);
        }
        let live = panel_shown && active;
        let woke = live && !self.changes_live;
        self.changes_live = live;
        if woke && self.watching() {
            // what the watch heard while the window was in the background is asked about now, not at the safety net
            self.want_changes(cx);
        } else if woke || tab_woke {
            self.poll_changes(cx);
        }
    }

    /// The watch heard something that may change git's answer: a run once `watch_delay` has passed (one
    /// more, when a run is out).
    pub(super) fn want_changes(&mut self, cx: &mut Context<Self>) {
        if let Some(list) = self.changes.as_mut() {
            list.wanted = true;
            self.poll_changes(cx);
        }
    }

    /// After a stage, discard or commit: git asked at once, past the pace, the floor and a halt. A run out was asked
    /// before the op landed, so what it will answer is the index as it was: it is stopped (the runner kills its git),
    /// not waited for, which a slow repository would make seconds of the old rows. The answer asked for now refreshes the
    /// shown diff and the gutters (`ChangesPolled`).
    pub(super) fn refresh_changes_now(&mut self, cx: &mut Context<Self>) {
        if let Some(list) = self.changes.as_mut() {
            list.ops += 1;
            if let Some(job) = list.job.take() {
                job.abort();
            }
            list.next_at = None;
            list.ran = None;
            list.halted = false;
            list.wanted = true;
            self.poll_changes(cx);
        }
    }

    /// Each tick of the panel's clock while live, and each change the watch hears: git asked again once
    /// `poll_delay` has passed, or a change was heard and `watch_delay` has, unless the last run is still out or polling
    /// is halted.
    pub(super) fn poll_changes(&mut self, cx: &mut Context<Self>) {
        if !self.changes_live {
            return;
        }
        let now = Instant::now();
        let Some(list) = self.changes.as_mut().filter(|l| l.job.is_none() && !l.halted) else { return };
        let paced = list.next_at.is_none_or(|at| at <= now);
        let tab_shown = self.changes_tab_shown;
        let floor_at = list.ran.map(|(at, took)| at + watch_delay(took, tab_shown)).filter(|&at| at > now);
        if !paced && !(list.wanted && floor_at.is_none()) {
            if let Some(at) = floor_at.filter(|_| list.wanted && !list.armed) {
                // the change heard is asked about as soon as that allows, not at the next tick
                list.armed = true;
                let id = list.id;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(at - now).await;
                    let _ = this.update(cx, |this, cx| {
                        if let Some(list) = this.changes.as_mut().filter(|l| l.id == id) {
                            list.armed = false;
                            this.poll_changes(cx);
                        }
                    });
                })
                .detach();
            }
            return;
        }
        list.wanted = false;
        list.run += 1;
        let (id, run, ops, root) = (list.id, list.run, list.ops, list.root.clone());
        let task = self.rt.spawn(read_status(root));
        list.job = Some(task.abort_handle());
        cx.spawn(async move |this, cx| {
            // an aborted run (its list replaced) answers nothing
            let answer = task.await.ok();
            let _ = this.update(cx, |this, cx| this.take_status(id, run, ops, now, answer, cx));
        })
        .detach();
    }

    fn take_status(&mut self, id: u64, run: u64, ops: u64, started: Instant, answer: Option<Result<Answer, GitError>>, cx: &mut Context<Self>) {
        let Some(list) = self.changes.as_mut().filter(|l| l.id == id) else { return };
        if list.run != run {
            // a run stopped for an op, or already replaced by the one asked since: that one is out, and is `job`
            return;
        }
        if list.ops != ops {
            // asked before an op landed and not stopped (the panel was not live then): its rows may be the index's
            // before it; the one asked for since goes out now
            list.job = None;
            list.wanted = true;
            self.poll_changes(cx);
            return;
        }
        self.land_status(started, answer, cx);
        // a change heard while the run was out: one more, once `watch_delay` allows
        if self.changes.as_ref().is_some_and(|l| l.wanted) {
            self.poll_changes(cx);
        }
    }

    fn land_status(&mut self, started: Instant, answer: Option<Result<Answer, GitError>>, cx: &mut Context<Self>) {
        let counted = self.changes_count();
        let (tab_shown, watched) = (self.changes_tab_shown, self.watching());
        let Some(list) = self.changes.as_mut() else { return };
        list.job = None;
        let Some(answer) = answer else { return };
        let (now, took) = (Instant::now(), started.elapsed());
        list.next_at = Some(now + poll_delay(took, tab_shown, watched, crate::power::on_battery()));
        list.ran = Some((now, took));
        if let Err(e) = &answer {
            list.halted |= halts(e);
            if transient(e) && list.answer().is_some() {
                // the good list stays; only the note that it was not refreshed changes
                if list.stale.as_ref() != Some(e) {
                    list.stale = Some(e.clone());
                    cx.notify();
                }
                return;
            }
        }
        // the shown diff tab reads its diff again, changed or not: git's status may not see a second edit of a file
        // already listed, nor a diff's lines move
        if answer.is_ok() {
            cx.emit(SidePanelEvent::ChangesPolled);
        }
        let was_stale = list.stale.take().is_some();
        if list.result.as_ref() == Some(&answer) {
            if was_stale {
                cx.notify();
            }
            return;
        }
        list.result = Some(answer);
        self.rebuild_changes(cx);
        if self.changes_count() != counted {
            cx.emit(SidePanelEvent::ChangesCounted);
        }
    }

    /// The rows again, after an answer or a fold. The selection goes if its change did; the menu goes with its
    /// change, and follows it when it changed in place (a delete staged, say: its menu no longer offers Open).
    fn rebuild_changes(&mut self, cx: &mut Context<Self>) {
        let Some(list) = self.changes.as_mut() else { return };
        let hovered_kind = list.hovered.and_then(|ix| list.rows.get(ix)).map(std::mem::discriminant);
        list.rows = match list.answer() {
            Some(a) => change_rows(&a.status.changes, a.status.outside_root, &self.changes_folded),
            None => Vec::new(),
        };
        if let Some((group, path)) = list.selected.clone()
            && list.find(group, &path).is_none()
        {
            list.selected = None;
        }
        // a row of another kind where the hovered one was is another element, which gpui counts as unhovered until
        // the pointer moves: so does the list
        if list.hovered.and_then(|ix| list.rows.get(ix)).map(std::mem::discriminant) != hovered_kind {
            list.hovered = None;
        }
        if let Some(menu) = self.change_menu.as_mut() {
            match list.find(menu.value.change.group, &menu.value.change.path) {
                Some(found) => menu.value.change = found.clone(),
                None => self.change_menu = None,
            }
        }
        cx.notify();
    }

    /// The pointer came onto row `ix` or left it.
    fn hover_change(&mut self, ix: usize, on: bool, cx: &mut Context<Self>) {
        let Some(list) = self.changes.as_mut() else { return };
        let hovered = match (on, list.hovered) {
            (true, _) => Some(ix),
            // another row's hover may have come first
            (false, Some(at)) if at != ix => Some(at),
            (false, _) => None,
        };
        if list.hovered != hovered {
            list.hovered = hovered;
            cx.notify();
        }
    }

    fn toggle_group(&mut self, group: Group, cx: &mut Context<Self>) {
        if !self.changes_folded.remove(&group) {
            self.changes_folded.insert(group);
        }
        self.rebuild_changes(cx);
    }

    /// The shown list's root, while it has one.
    pub(super) fn changes_root(&self) -> Option<PathBuf> {
        self.changes.as_ref().map(|l| l.root.clone())
    }

    /// The shown list's last good answer.
    pub(super) fn changes_answer(&self) -> Option<&Answer> {
        self.changes.as_ref()?.answer()
    }

    /// `change`'s file in the root.
    pub(super) fn change_path(&self, change: &Change) -> Option<PathBuf> {
        let list = self.changes.as_ref()?;
        Some(list.root.join(relative(&change.path, &list.answer()?.prefix)))
    }

    /// A change to the Shell; `preview` for a single click (replaced by the next), not for a double click or the
    /// menu's Open file.
    fn open_change(&mut self, change: Change, preview: bool, cx: &mut Context<Self>) {
        if let Some(path) = self.change_path(&change) {
            cx.emit(SidePanelEvent::OpenChange { path, change, preview });
        }
    }

    /// The Changes tab's body: the branch and the groups, or why there are none.
    pub(super) fn render_changes(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        crate::ui::reap_popup(&mut self.change_menu, window);
        let Some(list) = self.changes.as_ref() else { return div().into_any_element() };
        let answer = match &list.result {
            // the first run is out
            None => return div().into_any_element(),
            Some(Err(e)) => return self.change_error(theme, e, cx),
            Some(Ok(answer)) => answer,
        };
        let s = &answer.status;
        let branch = match (&s.branch, &s.detached) {
            (Some(b), _) => b.clone(),
            (None, Some(oid)) => format!("Detached at {oid}"),
            (None, None) => "No branch".into(),
        };
        let ahead_behind = s.ahead_behind.filter(|&(a, b)| a > 0 || b > 0).map(|(a, b)| match (a, b) {
            (a, 0) => format!("↑{a}"),
            (0, b) => format!("↓{b}"),
            (a, b) => format!("↑{a} ↓{b}"),
        });
        let branch_line = div()
            .h(px(28.))
            .flex_none()
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(gpui::svg().path("icons/git-branch.svg").size(px(13.)).flex_none().text_color(theme.text_muted))
            .child(div().min_w_0().truncate().text_size(px(12.)).text_color(theme.text_muted).child(branch))
            .children(ahead_behind.map(|t| div().flex_none().text_size(px(11.5)).text_color(theme.text_faint).child(t)))
            .children(list.stale.as_ref().map(|e| {
                let why = match e {
                    GitError::Timeout => "git status didn't answer within 15 seconds; asked again when this tab is shown again".to_string(),
                    GitError::Failed(e) => format!("git status failed: {e}"),
                    e => format!("{e:?}"),
                };
                div()
                    .id("changes-stale")
                    .flex_none()
                    .ml_auto()
                    .text_size(px(11.5))
                    .text_color(theme.warning)
                    .child("Not refreshed")
                    .tooltip(tip(why, None))
            }));
        // nothing listed may still leave the row counting the changes outside the root
        let body = if s.changes.is_empty() {
            let hint = match s.outside_root {
                0 => "The working tree matches the last commit.".to_string(),
                1 => "1 change elsewhere in the repository.".to_string(),
                n => format!("{n} changes elsewhere in the repository."),
            };
            super::notice(theme, "No changes", hint).into_any_element()
        } else {
            let rows = gpui::uniform_list("changes", list.rows.len(), cx.processor(|this: &mut SidePanel, range: Range<usize>, window, cx| this.change_rows(range, window, cx)))
                .track_scroll(&list.scroll)
                .size_full()
                .pb(px(4.));
            let scroll = list.scroll.0.borrow().base_handle.clone();
            div()
                .relative()
                .flex_1()
                .min_h_0()
                // the rows move under a still pointer, which hovers none of them again until it moves
                .on_scroll_wheel(cx.listener(|this, _: &gpui::ScrollWheelEvent, _, cx| {
                    if let Some(list) = this.changes.as_mut()
                        && list.hovered.take().is_some()
                    {
                        cx.notify();
                    }
                }))
                .child(crate::ui::edge_fade::edge_faded(12., &scroll, rows))
                .child(crate::ui::scrollbar("changes-scrollbar", &list.scroll, theme))
                .into_any_element()
        };
        let menu = self.change_menu(theme, cx);
        let commit = self.render_commit(theme, window, cx);
        div().relative().size_full().flex().flex_col().child(branch_line).children(commit).child(body).children(menu).into_any_element()
    }

    /// Why git gave no list, and what to do about it.
    fn change_error(&self, theme: &Theme, error: &GitError, cx: &mut Context<Self>) -> AnyElement {
        let (title, hint) = match error {
            GitError::GitMissing => ("Git isn't installed", "Install git to see this folder's changes.".to_string()),
            GitError::FolderMissing => ("This folder no longer exists", "Choose another from the … menu above.".to_string()),
            GitError::NotRepo => ("Not a git repository", "This folder isn't in a git repository.".to_string()),
            GitError::DubiousOwnership(path) => {
                let command = safe_directory_command(path);
                let copied = command.clone();
                return div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(super::notice(theme, "Git doesn't trust this repository", "Another user owns it. To trust it, run:"))
                    .child(div().pt(px(8.)).px(px(12.)).text_size(px(11.5)).font_family(theme.font_terminal.clone()).text_color(theme.text_muted).child(command))
                    .child(div().pt(px(12.)).child(super::notice_button(theme, "changes-copy-safe", "Copy command").on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone()));
                    }))))
                    .into_any_element();
            }
            GitError::Timeout => ("Git took too long", "git status didn't answer within 15 seconds.".to_string()),
            GitError::TooLarge => ("Too many changes to list", "git status printed more than 8 MiB.".to_string()),
            GitError::Failed(e) => ("Git failed", e.clone()),
        };
        super::notice(theme, title, hint).into_any_element()
    }

    /// The rows the list shows of `range`.
    fn change_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let Some(list) = self.changes.as_ref() else { return Vec::new() };
        let Some(answer) = list.answer() else { return Vec::new() };
        let menu = self.change_menu.as_ref().filter(|m| m.is_open()).map(|m| (m.value.change.group, m.value.change.path.clone()));
        let hover = theme.hover;
        let busy = self.git_busy();
        let outside = answer.status.outside_root;
        let untracked: Vec<String> = answer.status.changes.iter().filter(|c| c.group == Group::Untracked).map(|c| c.path.clone()).collect();
        range
            .filter_map(|ix| Some((ix, *list.rows.get(ix)?)))
            .map(|(ix, row)| match row {
                ChangeRow::Header { group, count, folded } => {
                    let turn = gpui::Transformation::rotate(gpui::percentage(if folded { 0. } else { 0.25 }));
                    let elsewhere = if outside > 0 { "; changes outside this folder are left alone" } else { "" };
                    let action = match group {
                        Group::Changes => Some(("icons/solar/add.svg", format!("Stage all changes in this folder{elsewhere}"), Op::StageTracked)),
                        Group::Untracked => Some(("icons/solar/add.svg", format!("Stage all untracked files in this folder{elsewhere}"), Op::StageUntracked(untracked.clone()))),
                        Group::Staged => Some(("icons/solar/minus.svg", format!("Unstage all in this folder{elsewhere}"), Op::UnstageAll)),
                        Group::Conflicts => None,
                    };
                    // shown while hovered, as a change's actions
                    let action = action.filter(|_| list.hovered == Some(ix)).map(|(icon, label, op)| {
                        super::commit::action_button(&theme, ("change-group-action", ix), icon, label, busy)
                            .debug_selector(|| format!("change-group-action-{ix}"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.run_git_op(op.clone(), window, cx);
                            }))
                    });
                    div()
                        .id(("change-group", ix))
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
                        .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme.text_muted).child(group.label()))
                        .children(action)
                        .child(div().flex_none().text_size(px(11.)).text_color(theme.text_faint).child(count.to_string()))
                        .on_hover(cx.listener(move |this, on: &bool, _, cx| this.hover_change(ix, *on, cx)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_group(group, cx)))
                        .into_any_element()
                }
                ChangeRow::Outside(n) => div()
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .pl(px(28.))
                    .pr(px(12.))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .text_color(theme.text_faint)
                    .child(div().truncate().child(if n == 1 { "1 more change outside this folder".to_string() } else { format!("{n} more changes outside this folder") }))
                    .into_any_element(),
                ChangeRow::Change(i) => {
                    let change = &answer.status.changes[i];
                    // a nested repository's path ends in `/`: its name is the folder's
                    let rel = relative(&change.path, &answer.prefix).trim_end_matches('/');
                    let (dir, name) = match rel.rsplit_once('/') {
                        Some((dir, name)) => (dir, name),
                        None => ("", rel),
                    };
                    let key = (change.group, change.path.clone());
                    let selected = list.selected.as_ref() == Some(&key) || menu.as_ref() == Some(&key);
                    let hovered = list.hovered == Some(ix);
                    let icon = material_icon(name, change.is_folder(), false);
                    let deleted = change.absent();
                    let tooltip = match (&change.orig, &change.moved_to) {
                        (_, Some(to)) => format!("{rel}\nRenamed to {to}, outside this folder"),
                        (Some(orig), None) => format!("{rel}\nRenamed from {}", relative(orig, &answer.prefix)),
                        (None, None) => rel.to_string(),
                    };
                    let (click, menu_change) = (change.clone(), change.clone());
                    let actions = row_actions(change).into_iter().enumerate().map(|(n, action)| {
                        let (icon, label) = match action {
                            RowAction::Stage => ("icons/solar/add.svg", "Stage"),
                            RowAction::Unstage => ("icons/solar/minus.svg", "Unstage"),
                            RowAction::Discard if change.group == Group::Untracked => ("icons/solar/undo-left.svg", "Delete file"),
                            RowAction::Discard => ("icons/solar/undo-left.svg", "Discard changes"),
                            RowAction::MarkResolved => ("icons/solar/check-circle.svg", "Mark resolved"),
                        };
                        let target = change.clone();
                        super::commit::action_button(&theme, gpui::ElementId::NamedInteger(format!("change-action-{n}").into(), ix as u64), icon, label, busy).on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                let target = target.clone();
                                match action {
                                    RowAction::Stage => this.run_git_op(Op::Stage(target), window, cx),
                                    RowAction::Unstage => this.run_git_op(Op::Unstage(target), window, cx),
                                    RowAction::Discard => this.ask_discard(target, window, cx),
                                    RowAction::MarkResolved => this.mark_resolved(target, window, cx),
                                }
                            },
                        ))
                    });
                    let actions = (selected || hovered).then(|| div().debug_selector(|| format!("change-actions-{ix}")).flex_none().flex().gap(px(2.)).children(actions));
                    div()
                        .id(("change-row", ix))
                        .debug_selector(|| format!("change-row-{ix}"))
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .pl(px(28.))
                        .pr(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(selected, |el| el.bg(theme.selected))
                        .when(!selected, |el| el.hover(move |s| s.bg(hover)))
                        // a coloured SVG, so img(), not the tinted svg() (as the Files tree)
                        .child(gpui::img(icon).size(px(16.)).flex_none().object_fit(gpui::ObjectFit::Contain).when(deleted, |el| el.opacity(0.5)))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.5))
                                .text_color(if deleted { theme.text_muted } else { theme.text })
                                .when(deleted, |el| el.line_through())
                                .child(name.to_string()),
                        )
                        .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).text_color(theme.text_faint).child(dir.to_string()))
                        .children(actions)
                        .child(
                            div()
                                .w(px(12.))
                                .flex_none()
                                .text_size(px(11.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(code_color(change.code, &theme))
                                .child(change.code.to_string()),
                        )
                        .tooltip(tip(tooltip, None))
                        .on_hover(cx.listener(move |this, on: &bool, _, cx| this.hover_change(ix, *on, cx)))
                        .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if let Some(list) = this.changes.as_mut() {
                                list.selected = Some((click.group, click.path.clone()));
                            }
                            // the first click of a double click opens a preview, the second pins it
                            this.open_change(click.clone(), e.click_count() < 2, cx);
                            cx.notify();
                        }))
                        // Capture, so a right-click on another row while the menu is up moves the menu there rather
                        // than the menu's click-outside closing the new one (as the Files tab's rows).
                        .capture_any_mouse_down(cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            if e.button != MouseButton::Right {
                                return;
                            }
                            cx.stop_propagation();
                            this.change_menu = Some(crate::ui::Popup::new(ChangeMenu { at: e.position, change: menu_change.clone() }));
                            cx.notify();
                        }))
                        .into_any_element()
                }
            })
            .collect()
    }

    /// The right-click menu, while it is up.
    fn change_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.change_menu.as_ref()?;
        let list = self.changes.as_ref()?;
        let change = menu.value.change.clone();
        let path = self.change_path(&change)?;
        let relative = path.strip_prefix(&list.root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let close = |this: &mut SidePanel| {
            if let Some(m) = this.change_menu.as_mut() {
                m.close()
            }
        };
        let item = |id: &'static str, icon: &'static str, label: &'static str| menu_choice(theme, id, icon, label, false);
        let copy = |text: String| move |this: &mut SidePanel, _: &ClickEvent, _: &mut Window, cx: &mut Context<SidePanel>| {
            close(this);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            cx.notify();
        };
        // a change opens its diff (a deleted file's too, its lines removed); a conflict its file, with the markers
        let conflict = change.group == Group::Conflicts;
        let openable = if conflict { change.openable() } else { !change.is_folder() };
        let open = if conflict { "Open file" } else { "Open changes" };
        let card = menu_card(theme, "change-menu", 200.)
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                close(this);
                cx.notify();
            }))
            .when(openable, |el| {
                el.child(item("change-menu-open", "icons/solar/file-text.svg", open).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    close(this);
                    this.open_change(change.clone(), false, cx);
                    cx.notify();
                })))
            })
            .child(item("change-menu-copy", "icons/solar/copy.svg", "Copy path").on_click(cx.listener(copy(path.to_string_lossy().into_owned()))))
            .child(item("change-menu-copy-relative", "icons/solar/copy.svg", "Copy relative path").on_click(cx.listener(copy(relative))));
        let anchored = gpui::anchored().position(menu.value.at).snap_to_window_with_margin(px(8.)).child(crate::ui::menu_motion("change-menu-in", menu, card));
        Some(gpui::deferred(anchored).with_priority(1).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(group: Group, path: &str) -> Change {
        Change { group, path: path.into(), orig: None, code: 'M', xy: ['.', 'M'], submodule: false, moved_to: None }
    }

    #[test]
    fn groups_get_headers_in_order_and_a_folded_one_hides_its_changes() {
        let changes = [change(Group::Staged, "a"), change(Group::Changes, "a"), change(Group::Changes, "b"), change(Group::Untracked, "c")];
        let rows = change_rows(&changes, 2, &HashSet::new());
        assert_eq!(
            rows,
            [
                ChangeRow::Header { group: Group::Staged, count: 1, folded: false },
                ChangeRow::Change(0),
                ChangeRow::Header { group: Group::Changes, count: 2, folded: false },
                ChangeRow::Change(1),
                ChangeRow::Change(2),
                ChangeRow::Header { group: Group::Untracked, count: 1, folded: false },
                ChangeRow::Change(3),
                ChangeRow::Outside(2),
            ]
        );
        let rows = change_rows(&changes, 0, &HashSet::from([Group::Changes]));
        assert_eq!(rows[2], ChangeRow::Header { group: Group::Changes, count: 2, folded: true });
        assert_eq!(rows[3], ChangeRow::Header { group: Group::Untracked, count: 1, folded: false });
        assert!(change_rows(&[], 0, &HashSet::new()).is_empty());
    }

    /// The tab's pace, slower for the badge alone, and never sooner than five times what the last run took.
    #[test]
    fn the_next_run_waits_for_the_pace_or_five_times_the_last() {
        let ms = Duration::from_millis;
        assert_eq!(poll_delay(ms(40), true, false, false), ms(3000));
        assert_eq!(poll_delay(ms(40), false, false, false), ms(12_000));
        assert_eq!(poll_delay(ms(1000), true, false, false), ms(5000));
        assert_eq!(poll_delay(ms(1000), false, false, false), ms(12_000));
        assert_eq!(poll_delay(ms(4000), false, false, false), ms(20_000));
        assert_eq!(poll_delay(Duration::MAX, true, false, false), Duration::MAX);
        // watched: the polls are a safety net, the floor still holds for a change heard
        assert_eq!(poll_delay(ms(40), true, true, false), ms(30_000));
        assert_eq!(poll_delay(ms(8000), false, true, false), ms(40_000));
        // on battery the polls slow to half; the safety net and the floor stay
        assert_eq!(poll_delay(ms(40), true, false, true), ms(6000));
        assert_eq!(poll_delay(ms(40), false, false, true), ms(24_000));
        assert_eq!(poll_delay(ms(40), true, true, true), ms(30_000));
        assert_eq!(poll_delay(ms(8000), true, false, true), ms(40_000));
        assert_eq!(floor(ms(40)), ms(200));
        // a change heard: 1.5 s apart while the tab shows, the badge's pace while only it does, and the floor
        assert_eq!(watch_delay(ms(40), true), ms(1500));
        assert_eq!(watch_delay(ms(40), false), ms(12_000));
        assert_eq!(watch_delay(ms(1000), true), ms(5000));
        assert_eq!(watch_delay(ms(4000), false), ms(20_000));
        assert!(transient(&GitError::Timeout) && transient(&GitError::Failed("index.lock".into())));
        assert!(!transient(&GitError::NotRepo) && !transient(&GitError::TooLarge));
        assert!(halts(&GitError::Timeout) && halts(&GitError::TooLarge) && !halts(&GitError::Failed(String::new())));
    }

    #[test]
    fn the_safe_directory_command_quotes_what_a_shell_would_split() {
        assert_eq!(safe_directory_command("/srv/repo"), "git config --global --add safe.directory /srv/repo");
        assert_eq!(safe_directory_command("/srv/my repo"), "git config --global --add safe.directory '/srv/my repo'");
        assert_eq!(safe_directory_command("/srv/it's"), r"git config --global --add safe.directory '/srv/it'\''s'");
    }

    #[test]
    fn letters_take_their_colors() {
        let theme = Theme::for_appearance(gpui::WindowAppearance::Dark);
        assert_eq!(code_color('U', &theme), theme.success);
        assert_eq!(code_color('D', &theme), theme.danger);
        assert_eq!(code_color('R', &theme), theme.warning);
    }

    #[tokio::test]
    async fn a_run_reads_a_subfolder_and_tells_a_plain_folder_and_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("app")).unwrap();
        std::fs::write(repo.join("app/x.rs"), "").unwrap();
        std::fs::write(repo.join("y.rs"), "").unwrap();
        let init = std::process::Command::new("git").args(["init", "-q", "-b", "trunk"]).current_dir(&repo).status().unwrap();
        assert!(init.success());

        let answer = read_status(repo.join("app")).await.unwrap();
        assert_eq!(answer.prefix, "app/");
        assert_eq!(answer.status.branch.as_deref(), Some("trunk"));
        let paths: Vec<_> = answer.status.changes.iter().map(|c| (c.group, c.path.as_str())).collect();
        assert_eq!(paths, [(Group::Untracked, "app/x.rs")]);
        assert_eq!(answer.status.outside_root, 1);

        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(read_status(plain).await, Err(GitError::NotRepo));
        assert_eq!(read_status(dir.path().join("gone")).await, Err(GitError::FolderMissing));
    }

    // Real repositories, made in a temp folder: what git itself prints for each case, not a fixture's idea of it.

    fn has_git() -> bool {
        std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    /// `git <args>` in `dir`, as a user would run it, with no `GIT_*` from the test's environment (the GIT_DIR test
    /// sets one) and no global config to lean on.
    fn sh(dir: &Path, args: &[&str]) -> String {
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
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write(dir: &Path, path: &str, text: &str) {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A repository at `<tmp>/repo` with `files` committed on `main`.
    fn repo(files: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q"]);
        for f in files {
            // each its own text: alike files would pair a delete with a move as git's rename
            write(&repo, f, &format!("{f}\nline two\nline three\n"));
        }
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-qm", "first"]);
        (dir, repo)
    }

    fn listed(answer: &Answer) -> Vec<(Group, &str, char)> {
        answer.status.changes.iter().map(|c| (c.group, c.path.as_str(), c.code)).collect()
    }

    /// An edit, a `git mv` with spaces, deletes staged and not, a file staged then edited again, and a path with
    /// glob characters beside the file its glob would match (nothing is passed as a pathspec: both stay apart).
    #[tokio::test]
    async fn edits_moves_deletes_and_a_file_in_both_groups() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = repo(&["a.txt", "old name.txt", "gone1.txt", "gone2.txt", "both.txt", "src/a.swift", "src/[ab].swift"]);
        write(&repo, "a.txt", "two\n");
        sh(&repo, &["mv", "old name.txt", "new name.txt"]);
        std::fs::remove_file(repo.join("gone1.txt")).unwrap();
        sh(&repo, &["rm", "-q", "gone2.txt"]);
        write(&repo, "both.txt", "two\n");
        sh(&repo, &["add", "both.txt"]);
        write(&repo, "both.txt", "three\n");
        write(&repo, "src/[ab].swift", "two\n");
        // a repository inside the root: listed as its folder, not looked into
        write(&repo, "nested/n.txt", "");
        sh(&repo.join("nested"), &["init", "-q"]);
        let answer = read_status(repo.clone()).await.unwrap();
        assert_eq!(answer.status.branch.as_deref(), Some("main"));
        assert_eq!(
            listed(&answer),
            [
                (Group::Staged, "both.txt", 'M'),
                (Group::Staged, "gone2.txt", 'D'),
                (Group::Staged, "new name.txt", 'R'),
                (Group::Changes, "a.txt", 'M'),
                (Group::Changes, "both.txt", 'M'),
                (Group::Changes, "gone1.txt", 'D'),
                (Group::Changes, "src/[ab].swift", 'M'),
                (Group::Untracked, "nested/", 'U'),
            ]
        );
        let renamed = answer.status.changes.iter().find(|c| c.code == 'R').unwrap();
        assert_eq!(renamed.orig.as_deref(), Some("old name.txt"));
        let openable: Vec<_> = answer.status.changes.iter().filter(|c| !c.openable()).map(|c| c.path.as_str()).collect();
        assert_eq!(openable, ["gone1.txt", "nested/"]);
    }

    /// A rename staged then edited (`RM`) keeps its source on the Staged entry; a rename in the work tree of a file
    /// added with `git add -N` (`.R`) on its Changes entry.
    #[tokio::test]
    async fn rename_sources_go_with_their_entries() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = repo(&["a.txt", "c.txt"]);
        sh(&repo, &["mv", "a.txt", "b.txt"]);
        write(&repo, "b.txt", "a.txt\nline two\nline three\nmore\n");
        std::fs::rename(repo.join("c.txt"), repo.join("d.txt")).unwrap();
        sh(&repo, &["add", "-N", "d.txt"]);
        let answer = read_status(repo).await.unwrap();
        let find = |group, path: &str| answer.status.changes.iter().find(|c| c.group == group && c.path == path).cloned();
        assert_eq!(find(Group::Staged, "b.txt").and_then(|c| c.orig).as_deref(), Some("a.txt"));
        assert_eq!(find(Group::Changes, "b.txt").map(|c| c.orig), Some(None));
        assert_eq!(find(Group::Changes, "d.txt").and_then(|c| c.orig).as_deref(), Some("c.txt"), "{:?}", answer.status.changes);
    }

    #[tokio::test]
    async fn a_merge_conflict_is_listed_first() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = repo(&["f.txt", "calm.txt"]);
        sh(&repo, &["checkout", "-qb", "other"]);
        write(&repo, "f.txt", "theirs\n");
        sh(&repo, &["commit", "-qam", "theirs"]);
        sh(&repo, &["checkout", "-q", "main"]);
        write(&repo, "f.txt", "ours\n");
        sh(&repo, &["commit", "-qam", "ours"]);
        sh(&repo, &["merge", "-q", "other"]);
        write(&repo, "calm.txt", "two\n");
        let answer = read_status(repo).await.unwrap();
        assert_eq!(listed(&answer), [(Group::Conflicts, "f.txt", '!'), (Group::Changes, "calm.txt", 'M')]);
    }

    /// A linked worktree reads as its own checkout, on its own detached HEAD; the main one is ahead of and behind
    /// its local upstream.
    #[tokio::test]
    async fn a_linked_worktree_on_a_detached_head_and_ahead_behind_a_local_upstream() {
        if !has_git() {
            return;
        }
        let (dir, repo) = repo(&["f.txt"]);
        sh(&repo, &["branch", "base"]);
        let wt = dir.path().join("wt");
        sh(&repo, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap(), "HEAD"]);
        write(&wt, "new.txt", "");
        let head = sh(&wt, &["rev-parse", "HEAD"]);
        let answer = read_status(wt).await.unwrap();
        assert_eq!((answer.status.branch.as_deref(), answer.status.detached.as_deref()), (None, Some(&head[..7])));
        assert_eq!(listed(&answer), [(Group::Untracked, "new.txt", 'U')]);
        // the main checkout does not see the worktree's file
        assert!(read_status(repo.clone()).await.unwrap().status.changes.is_empty());

        write(&repo, "f.txt", "ahead\n");
        sh(&repo, &["commit", "-qam", "ahead"]);
        sh(&repo, &["checkout", "-q", "base"]);
        write(&repo, "g.txt", "behind\n");
        sh(&repo, &["add", "g.txt"]);
        sh(&repo, &["commit", "-qm", "behind"]);
        sh(&repo, &["checkout", "-q", "main"]);
        sh(&repo, &["branch", "-q", "--set-upstream-to=base"]);
        let answer = read_status(repo).await.unwrap();
        assert_eq!(answer.status.ahead_behind, Some((1, 1)));
    }

    /// A `GIT_DIR` in Shepherd's environment (a git hook that started it, a pane's leftovers) would point every
    /// run at another repository; the runner drops it. Checked in a child test process that has it set, so this
    /// process's other tests never see it.
    #[test]
    fn a_git_dir_in_the_environment_does_not_redirect_the_read() {
        if !has_git() {
            return;
        }
        let (_other, other) = repo(&["elsewhere.txt"]);
        write(&other, "elsewhere.txt", "changed\n");
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args(["--ignored", "--exact", "ui::side_panel::changes::tests::read_under_an_inherited_git_dir", "--test-threads=1"])
            .env("GIT_DIR", other.join(".git"))
            .env("GIT_WORK_TREE", &other)
            .env("SHEPHERD_GIT_DIR_TEST", "1")
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && printed.contains("1 passed"), "{printed}{}", String::from_utf8_lossy(&out.stderr));
    }

    // Stage, unstage, discard and commit through `git_ops::run`, as the tab runs them, in temp repositories. Their own
    // config names a fictional author, turns signing off and points the hooks at the repository's own, so the
    // machine's global config cannot change what they do.

    fn ours(repo: &Path) {
        let hooks = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for (k, v) in [("user.name", "Fictional Tester"), ("user.email", "tester@example.invalid"), ("commit.gpgsign", "false"), ("core.hooksPath", hooks.to_str().unwrap())] {
            sh(repo, &["config", k, v]);
        }
    }

    /// `repo()` with the tab's config.
    fn our_repo(files: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let (dir, repo) = repo(files);
        ours(&repo);
        (dir, repo)
    }

    async fn find(repo: &Path, group: Group, path: &str) -> Change {
        let answer = read_status(repo.to_path_buf()).await.unwrap();
        answer.status.changes.iter().find(|c| c.group == group && c.path == path).cloned().unwrap_or_else(|| panic!("no {group:?} {path} in {:?}", listed(&answer)))
    }

    async fn run(repo: &Path, op: Op) -> Result<(), git_ops::Failure> {
        git_ops::run(repo.to_path_buf(), op).await
    }

    async fn listing(repo: &Path) -> Vec<(Group, String, char)> {
        listed(&read_status(repo.to_path_buf()).await.unwrap()).into_iter().map(|(g, p, c)| (g, p.to_string(), c)).collect()
    }

    fn rows(rows: &[(Group, &str, char)]) -> Vec<(Group, String, char)> {
        rows.iter().map(|&(g, p, c)| (g, p.to_string(), c)).collect()
    }

    /// A file named like a glob is staged alone (literal pathspecs), one named like an option is a path (the `--`).
    #[tokio::test]
    async fn a_stage_takes_the_file_named_and_nothing_its_name_would_match() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt"]);
        write(&repo, "a.txt", "edited\n");
        write(&repo, "*.txt", "glob\n");
        write(&repo, "-n.txt", "dash\n");
        run(&repo, Op::Stage(find(&repo, Group::Untracked, "*.txt").await)).await.unwrap();
        run(&repo, Op::Stage(find(&repo, Group::Untracked, "-n.txt").await)).await.unwrap();
        assert_eq!(listing(&repo).await, rows(&[(Group::Staged, "*.txt", 'A'), (Group::Staged, "-n.txt", 'A'), (Group::Changes, "a.txt", 'M')]));
        // Stage All in a subfolder root leaves the rest of the repository alone
        write(&repo, "app/x.txt", "x\n");
        let app = repo.join("app");
        let answer = read_status(app.clone()).await.unwrap();
        let untracked: Vec<String> = answer.status.changes.iter().filter(|c| c.group == Group::Untracked).map(|c| c.path.clone()).collect();
        run(&app, Op::StageUntracked(untracked)).await.unwrap();
        run(&app, Op::StageTracked).await.unwrap();
        assert_eq!(listing(&repo).await, rows(&[(Group::Staged, "*.txt", 'A'), (Group::Staged, "-n.txt", 'A'), (Group::Staged, "app/x.txt", 'A'), (Group::Changes, "a.txt", 'M')]));
    }

    #[tokio::test]
    async fn an_unstage_puts_a_file_and_both_sides_of_a_rename_back() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt", "old name.txt"]);
        write(&repo, "a.txt", "edited\n");
        sh(&repo, &["add", "a.txt"]);
        sh(&repo, &["mv", "old name.txt", "new name.txt"]);
        run(&repo, Op::Unstage(find(&repo, Group::Staged, "a.txt").await)).await.unwrap();
        run(&repo, Op::Unstage(find(&repo, Group::Staged, "new name.txt").await)).await.unwrap();
        assert_eq!(listing(&repo).await, rows(&[(Group::Changes, "a.txt", 'M'), (Group::Changes, "old name.txt", 'D'), (Group::Untracked, "new name.txt", 'U')]));
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "edited\n");
        sh(&repo, &["add", "-A"]);
        run(&repo, Op::UnstageAll).await.unwrap();
        assert_eq!(listing(&repo).await.iter().filter(|r| r.0 == Group::Staged).count(), 0);
    }

    /// Before the first commit there is no HEAD to reset to: the file leaves the index and keeps its edits.
    #[tokio::test]
    async fn an_unstage_before_the_first_commit_keeps_the_file() {
        if !has_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q"]);
        ours(&repo);
        write(&repo, "first.txt", "one\n");
        write(&repo, "second.txt", "two\n");
        sh(&repo, &["add", "-A"]);
        write(&repo, "first.txt", "one, edited\n");
        let answer = read_status(repo.clone()).await.unwrap();
        assert!(answer.repo.initial);
        run(&repo, Op::Unstage(find(&repo, Group::Staged, "first.txt").await)).await.unwrap();
        assert_eq!(listing(&repo).await, rows(&[(Group::Staged, "second.txt", 'A'), (Group::Untracked, "first.txt", 'U')]));
        assert_eq!(std::fs::read_to_string(repo.join("first.txt")).unwrap(), "one, edited\n");
        run(&repo, Op::UnstageAll).await.unwrap();
        assert_eq!(listing(&repo).await, rows(&[(Group::Untracked, "first.txt", 'U'), (Group::Untracked, "second.txt", 'U')]));
        // and the first commit itself
        run(&repo, Op::Stage(find(&repo, Group::Untracked, "second.txt").await)).await.unwrap();
        run(&repo, Op::Commit { message: "Start the fictional project".into(), amend: false }).await.unwrap();
        assert_eq!(sh(&repo, &["log", "--format=%s"]), "Start the fictional project");
        assert!(!read_status(repo).await.unwrap().repo.initial);
    }

    /// A discard goes through its prompt: an edit back to the index's version, a delete back on disk, an untracked
    /// file deleted.
    #[tokio::test]
    async fn a_discard_restores_tracked_files_and_deletes_an_untracked_one() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt", "gone.txt", "*.txt"]);
        let committed = std::fs::read_to_string(repo.join("a.txt")).unwrap();
        write(&repo, "a.txt", "edited\n");
        write(&repo, "*.txt", "edited too\n");
        std::fs::remove_file(repo.join("gone.txt")).unwrap();
        write(&repo, "scratch.txt", "fictional notes\n");
        let discard = |c: &Change| git_ops::confirm_discard(c, &c.path, false).unwrap().answered(0).unwrap();
        run(&repo, discard(&find(&repo, Group::Changes, "*.txt").await)).await.unwrap();
        // the glob's name touched only its own file
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "edited\n");
        run(&repo, discard(&find(&repo, Group::Changes, "a.txt").await)).await.unwrap();
        run(&repo, discard(&find(&repo, Group::Changes, "gone.txt").await)).await.unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), committed);
        assert!(repo.join("gone.txt").is_file());
        assert_eq!(listing(&repo).await, rows(&[(Group::Untracked, "scratch.txt", 'U')]));
        run(&repo, discard(&find(&repo, Group::Untracked, "scratch.txt").await)).await.unwrap();
        assert!(!repo.join("scratch.txt").exists());
        assert_eq!(listing(&repo).await, rows(&[]));
    }

    /// A commit takes its message from stdin, as typed; Amend replaces the last commit; nothing staged is said so.
    #[tokio::test]
    async fn a_commit_an_amend_and_nothing_staged() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt"]);
        // git's default would keep `#` lines here too; set, so a global `commit.cleanup=strip` cannot change it
        sh(&repo, &["config", "commit.cleanup", "whitespace"]);
        write(&repo, "a.txt", "edited\n");
        let nothing = run(&repo, Op::Commit { message: "Nothing here".into(), amend: false }).await.unwrap_err();
        assert_eq!(nothing.kind, git_ops::Kind::NothingStaged, "{nothing:?}");
        run(&repo, Op::Stage(find(&repo, Group::Changes, "a.txt").await)).await.unwrap();
        let message = "Tune the fictional widget\n\nIt hums -F now.\n# not a comment";
        run(&repo, Op::Commit { message: message.into(), amend: false }).await.unwrap();
        assert_eq!(sh(&repo, &["log", "-1", "--format=%B"]), message);
        assert_eq!(sh(&repo, &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(git_ops::last_message(repo.clone()).await.unwrap(), message);
        run(&repo, Op::Commit { message: "Tune the widget, reworded".into(), amend: true }).await.unwrap();
        assert_eq!(sh(&repo, &["log", "-1", "--format=%B"]), "Tune the widget, reworded");
        assert_eq!(sh(&repo, &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(listing(&repo).await, rows(&[]));
    }

    /// A pre-commit hook's refusal comes back with its own words, and nothing is committed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_pre_commit_hook_is_reported_with_its_output() {
        use std::os::unix::fs::PermissionsExt;
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt"]);
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'lint: 2 problems in a.txt' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        write(&repo, "a.txt", "edited\n");
        run(&repo, Op::Stage(find(&repo, Group::Changes, "a.txt").await)).await.unwrap();
        let failed = run(&repo, Op::Commit { message: "Blocked".into(), amend: false }).await.unwrap_err();
        assert_eq!(failed, git_ops::Failure { kind: git_ops::Kind::HookFailed, text: "lint: 2 problems in a.txt".into() });
        assert_eq!(sh(&repo, &["rev-list", "--count", "HEAD"]), "1");
    }

    /// A merge under way reads its `MERGE_MSG`, comments left out; the rows offer what their group allows.
    #[tokio::test]
    async fn a_merge_reads_its_message_and_rows_offer_their_actions() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["f.txt"]);
        sh(&repo, &["checkout", "-qb", "other"]);
        write(&repo, "f.txt", "theirs\n");
        sh(&repo, &["commit", "-qam", "theirs"]);
        sh(&repo, &["checkout", "-q", "main"]);
        write(&repo, "f.txt", "ours\n");
        sh(&repo, &["commit", "-qam", "ours"]);
        sh(&repo, &["merge", "-q", "other"]);
        let answer = read_status(repo.clone()).await.unwrap();
        assert!(answer.repo.merging && !answer.repo.rebasing && !answer.repo.initial);
        assert_eq!(answer.repo.merge_msg.as_deref(), Some("Merge branch 'other'"));
        let conflict = find(&repo, Group::Conflicts, "f.txt").await;
        assert_eq!(row_actions(&conflict), [RowAction::MarkResolved]);
        assert!(git_ops::has_conflict_markers(&std::fs::read_to_string(repo.join("f.txt")).unwrap()));
        let mut c = conflict.clone();
        c.group = Group::Untracked;
        assert_eq!(row_actions(&c), [RowAction::Discard, RowAction::Stage]);
        c.path = "nested/".into();
        assert_eq!(row_actions(&c), []);
        c.group = Group::Staged;
        assert_eq!(row_actions(&c), [RowAction::Unstage]);
        run(&repo, Op::Stage(conflict)).await.unwrap();
        assert_eq!(listing(&repo).await, rows(&[(Group::Staged, "f.txt", 'M')]));
    }

    /// An op landing with a run out stops it and has the next run out at once, not once the stopped one has been heard.
    #[gpui::test]
    fn an_op_landing_stops_the_run_out_and_asks_again_at_once(cx: &mut gpui::TestAppContext) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // the run asked for answers from the runtime's threads
        cx.executor().allow_parking();
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let panel = cx.new(|cx| SidePanel::new(rt.handle().clone(), crate::settings::PanelTab::Changes, cx));
        panel.update(cx, |p, cx| {
            p.changes = Some(ChangeList::new("/tmp/fictional-repo".into()));
            p.changes_live = true;
            let out = rt.spawn(std::future::pending::<()>());
            p.changes.as_mut().unwrap().job = Some(out.abort_handle());
            p.refresh_changes_now(cx);
            let list = p.changes.as_ref().unwrap();
            assert_eq!((list.run, list.job.is_some(), list.ops), (1, true, 1), "the run asked after the op is out already");
            assert!(rt.block_on(out).unwrap_err().is_cancelled(), "the run that was out is stopped");
        });
    }

    /// The answer of a run stopped for an op, or replaced by a later one, is no one's: it neither lands nor frees the
    /// `job` of the run out now.
    #[gpui::test]
    fn the_answer_of_a_replaced_run_is_dropped(cx: &mut gpui::TestAppContext) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let panel = cx.new(|cx| SidePanel::new(rt.handle().clone(), crate::settings::PanelTab::Changes, cx));
        let answer = Answer { status: Status { changes: vec![change(Group::Changes, "a.txt")], ..Default::default() }, prefix: String::new(), repo: RepoState::default() };
        panel.update(cx, |p, cx| {
            p.changes = Some(ChangeList::new("/tmp/fictional-repo".into()));
            let list = p.changes.as_mut().unwrap();
            let id = list.id;
            // the third run is the one out
            list.run = 3;
            list.job = Some(rt.spawn(std::future::pending::<()>()).abort_handle());
            p.take_status(id, 2, 0, Instant::now(), Some(Ok(answer.clone())), cx);
            p.take_status(id, 2, 0, Instant::now(), None, cx);
            let list = p.changes.as_ref().unwrap();
            assert_eq!((list.result.is_none(), list.job.is_some(), list.wanted), (true, true, false), "an earlier run's answer changes nothing");
            p.take_status(id, 3, 0, Instant::now(), Some(Ok(answer)), cx);
            let list = p.changes.as_ref().unwrap();
            assert_eq!((list.result.is_some(), list.job.is_none()), (true, true), "the run out lands");
        });
    }

    /// A status asked before a stage landed is dropped, not shown: the rows would be the index's before it.
    #[gpui::test]
    fn a_status_asked_before_an_op_landed_is_dropped(cx: &mut gpui::TestAppContext) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let panel = cx.new(|cx| SidePanel::new(rt.handle().clone(), crate::settings::PanelTab::Changes, cx));
        let answer = Answer { status: Status { changes: vec![change(Group::Changes, "a.txt")], ..Default::default() }, prefix: String::new(), repo: RepoState::default() };
        panel.update(cx, |p, cx| {
            p.changes = Some(ChangeList::new("/tmp/fictional-repo".into()));
            let id = p.changes.as_ref().unwrap().id;
            // the panel not live: nothing runs git here
            p.refresh_changes_now(cx);
            p.take_status(id, 0, 0, Instant::now(), Some(Ok(answer.clone())), cx);
            let list = p.changes.as_ref().unwrap();
            assert_eq!((list.result.is_none(), list.wanted, list.job.is_none()), (true, true, true));
            p.take_status(id, 0, 1, Instant::now(), Some(Ok(answer.clone())), cx);
            assert_eq!(p.changes_count(), Some(1));
        });
    }

    /// A status run is out (the watch heard the index change, say), and a stage, unstage or discard lands: the rows show
    /// the new state as soon as the run asked after it answers, not once the run already out has also run out. The run
    /// out is held in git's clean filter on a file that is never staged and edited to its committed size (6 s, while its
    /// flag is up).
    #[cfg(unix)]
    #[gpui::test]
    fn the_rows_follow_an_op_without_waiting_for_a_status_run_already_out(cx: &mut gpui::TestAppContext) {
        use std::os::unix::fs::PermissionsExt;
        if !has_git() {
            return;
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        // git's answers come from the runtime's threads, the real clock's time
        cx.executor().allow_parking();
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let (dir, repo) = our_repo(&["a.txt", "slow.txt"]);
        let (hold, started) = (dir.path().join("hold"), dir.path().join("started"));
        let filter = dir.path().join("hold-slow.sh");
        std::fs::write(&filter, format!("#!/bin/sh\nif [ -e '{}' ]; then touch '{}'; sleep 6; fi\ncat\n", hold.display(), started.display())).unwrap();
        std::fs::set_permissions(&filter, std::fs::Permissions::from_mode(0o755)).unwrap();
        sh(&repo, &["config", "filter.slow.clean", filter.to_str().unwrap()]);
        write(&repo, ".gitattributes", "slow.txt filter=slow\n");
        sh(&repo, &["add", ".gitattributes"]);
        sh(&repo, &["commit", "-qm", "attributes"]);
        write(&repo, "a.txt", "edited\n");
        // the same size as committed, so git compares the content, through the filter, rather than see the size differ
        write(&repo, "slow.txt", "slow.txt\nline two\nline thref\n");
        let (panel, cx) = cx.add_window_view(|_, cx| SidePanel::new(rt.handle().clone(), crate::settings::PanelTab::Changes, cx));
        panel.update(cx, |p, cx| {
            p.context.root = Some(repo.to_string_lossy().into());
            p.context.workspace_id = Some("w1".into());
            p.changes = Some(ChangeList::new(repo.clone()));
            p.changes_live = true;
            p.poll_changes(cx);
        });
        // Waits (pumping the window) until `done` holds, as long as `limit`; how long it took.
        let wait = |cx: &mut gpui::VisualTestContext, limit: Duration, done: &dyn Fn(&SidePanel) -> bool| {
            let began = Instant::now();
            while began.elapsed() < limit {
                cx.run_until_parked();
                if panel.read_with(cx, |p, _| done(p)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            began.elapsed()
        };
        let listing = |p: &SidePanel| p.changes_answer().map(|a| listed(a).into_iter().map(|(g, path, _)| (g, path.to_string())).collect::<Vec<_>>());
        let rows = |rows: &[(Group, &str)]| Some(rows.iter().map(|&(g, p)| (g, p.to_string())).collect::<Vec<_>>());
        wait(cx, Duration::from_secs(20), &|p| p.changes_count() == Some(2));
        let slow = (Group::Changes, "slow.txt");
        let steps = [
            ("stage", Op::Stage(change(Group::Changes, "a.txt")), rows(&[(Group::Staged, "a.txt"), slow])),
            ("unstage", Op::Unstage(change(Group::Staged, "a.txt")), rows(&[(Group::Changes, "a.txt"), slow])),
            (
                "discard",
                git_ops::confirm_discard(&change(Group::Changes, "a.txt"), "a.txt", false).unwrap().answered(0).unwrap(),
                rows(&[slow]),
            ),
        ];
        for (name, op, expected) in steps {
            let runs = panel.read_with(cx, |p, _| p.changes.as_ref().unwrap().run);
            // the run out, held inside the filter; the flag comes down so the op and the run asked after it are not
            std::fs::write(&hold, "").unwrap();
            let _ = std::fs::remove_file(&started);
            panel.update(cx, |p, cx| {
                let list = p.changes.as_mut().unwrap();
                list.next_at = None;
                list.ran = None;
                p.poll_changes(cx);
            });
            wait(cx, Duration::from_secs(10), &|_| started.exists());
            assert!(started.exists(), "{name}: the run out never reached the filter");
            std::fs::remove_file(&hold).unwrap();
            panel.update_in(cx, |p, window, cx| p.run_git_op(op.clone(), window, cx));
            let took = wait(cx, Duration::from_secs(15), &|p| listing(p) == expected);
            assert_eq!(panel.read_with(cx, |p, _| listing(p)), expected, "{name}: the rows after {took:?}");
            assert!(took < Duration::from_secs(3), "{name}: the new rows took {took:?}, the run out held 6 s");
            // the next step starts from a settled panel: no run out, none wanted, and no run but the held one and the one
            // asked for after the op was ever made (the held one's stopping starts nothing)
            wait(cx, Duration::from_secs(10), &|p| p.changes.as_ref().is_some_and(|l| l.job.is_none() && !l.wanted));
            wait(cx, Duration::from_millis(300), &|_| false);
            assert_eq!(panel.read_with(cx, |p, _| p.changes.as_ref().map(|l| (l.run - runs, l.job.is_none(), l.wanted))), Some((2, true, false)), "{name}: runs made, none out, none wanted");
        }
    }

    /// A row under the pointer shows its actions, and they go when it leaves. A group hover showing them came too late
    /// for their prepaint, and painting them panicked.
    #[gpui::test]
    fn hovering_a_row_shows_its_actions(cx: &mut gpui::TestAppContext) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let (panel, cx) = cx.add_window_view(|_, cx| SidePanel::new(rt.handle().clone(), crate::settings::PanelTab::Changes, cx));
        let answer = Answer { status: Status { changes: vec![change(Group::Changes, "notes.txt")], ..Default::default() }, prefix: String::new(), repo: RepoState::default() };
        panel.update(cx, |p, cx| {
            p.context.root = Some("/tmp/fictional-repo".into());
            p.context.workspace_id = Some("w1".into());
            p.changes = Some(ChangeList::new("/tmp/fictional-repo".into()));
            let id = p.changes.as_ref().unwrap().id;
            p.take_status(id, 0, 0, Instant::now(), Some(Ok(answer)), cx);
            assert_eq!(p.changes.as_ref().unwrap().rows, [ChangeRow::Header { group: Group::Changes, count: 1, folded: false }, ChangeRow::Change(0)]);
        });
        cx.run_until_parked();
        let row = cx.debug_bounds("change-row-1").expect("the change's row drawn");
        let hovered = |cx: &mut gpui::VisualTestContext| panel.read_with(cx, |p, _| p.changes.as_ref().unwrap().hovered);
        assert_eq!(hovered(cx), None);
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(hovered(cx), Some(1));
        let actions = cx.debug_bounds("change-actions-1").expect("the row's actions drawn");
        assert!(row.contains(&actions.center()), "{actions:?} not in {row:?}");
        // off the row: gone, and taking no space
        cx.simulate_mouse_move(row.bottom_left() + gpui::point(px(0.), px(200.)), None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!((hovered(cx), cx.debug_bounds("change-actions-1")), (None, None));
    }

    /// A stage that rebuilds the rows under a still pointer puts a header under it; drawn then, its Stage All shown by a
    /// group hover was never prepainted and painting it panicked.
    #[gpui::test]
    fn a_header_rebuilt_under_a_still_pointer_draws(cx: &mut gpui::TestAppContext) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let (panel, cx) = cx.add_window_view(|_, cx| SidePanel::new(rt.handle().clone(), crate::settings::PanelTab::Changes, cx));
        let answer = |changes| Answer { status: Status { changes, ..Default::default() }, prefix: String::new(), repo: RepoState::default() };
        let before = answer(vec![change(Group::Changes, "notes.txt"), change(Group::Changes, "draft.txt"), change(Group::Untracked, "sketch.txt")]);
        let id = panel.update(cx, |p, cx| {
            p.context.root = Some("/tmp/fictional-repo".into());
            p.context.workspace_id = Some("w1".into());
            p.changes = Some(ChangeList::new("/tmp/fictional-repo".into()));
            let id = p.changes.as_ref().unwrap().id;
            p.take_status(id, 0, 0, Instant::now(), Some(Ok(before)), cx);
            assert_eq!(p.changes.as_ref().unwrap().rows[2], ChangeRow::Change(1));
            id
        });
        cx.run_until_parked();
        let row = cx.debug_bounds("change-row-2").expect("draft.txt's row drawn");
        let hovered = |cx: &mut gpui::VisualTestContext| panel.read_with(cx, |p, _| p.changes.as_ref().unwrap().hovered);
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(hovered(cx), Some(2));
        // draft.txt gone (staged elsewhere, say): the Untracked header lands under the pointer, which has not moved
        let after = answer(vec![change(Group::Changes, "notes.txt"), change(Group::Untracked, "sketch.txt")]);
        panel.update(cx, |p, cx| {
            p.take_status(id, 0, 0, Instant::now(), Some(Ok(after)), cx);
            assert_eq!(p.changes.as_ref().unwrap().rows[2], ChangeRow::Header { group: Group::Untracked, count: 1, folded: false });
        });
        cx.run_until_parked();
        // another row is there now, which gpui counts as unhovered until the pointer moves
        assert_eq!((hovered(cx), cx.debug_bounds("change-group-action-2")), (None, None));
        cx.simulate_mouse_move(row.center() + gpui::point(px(1.), px(0.)), None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(hovered(cx), Some(2));
        let action = cx.debug_bounds("change-group-action-2").expect("the header's Stage All drawn");
        assert!(row.contains(&action.center()), "{action:?} not in {row:?}");
        cx.simulate_mouse_move(row.bottom_left() + gpui::point(px(0.), px(200.)), None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!((hovered(cx), cx.debug_bounds("change-group-action-2")), (None, None));
    }

    /// A stage past its deadline is stopped, not killed: git takes its `index.lock` away and its slow clean filter
    /// ends with it, so a Retry can run.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stage_past_its_deadline_leaves_no_index_lock() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt"]);
        write(&repo, "a.txt", "edited\n");
        let edited = find(&repo, Group::Changes, "a.txt").await;
        sh(&repo, &["config", "filter.slow.clean", "sleep 30; cat"]);
        write(&repo, ".gitattributes", "a.txt filter=slow\n");
        let started = Instant::now();
        let failed = git_ops::run_within(repo.clone(), Op::Stage(edited), Some(Duration::from_millis(1500))).await.unwrap_err();
        assert_eq!(failed.text, "git didn't finish within 1 seconds and was stopped");
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(!repo.join(".git/index.lock").exists(), "index.lock left behind");
    }

    /// A busy index says how to clear a lock a crashed git left.
    #[tokio::test]
    async fn a_held_index_lock_is_reported_with_how_to_clear_it() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt"]);
        write(&repo, "a.txt", "edited\n");
        let edited = find(&repo, Group::Changes, "a.txt").await;
        std::fs::write(repo.join(".git/index.lock"), "").unwrap();
        let failed = run(&repo, Op::Stage(edited)).await.unwrap_err();
        assert_eq!(failed.kind, git_ops::Kind::IndexLocked);
        assert_eq!(failed.text.lines().last(), Some(git_ops::STALE_LOCK));
    }

    /// From a subfolder root a discard reaches the file from the top: an edit restored, an untracked file deleted.
    #[tokio::test]
    async fn a_discard_from_a_subfolder_root() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["app/x.txt", "y.txt"]);
        let committed = std::fs::read_to_string(repo.join("app/x.txt")).unwrap();
        write(&repo, "app/x.txt", "edited\n");
        write(&repo, "app/new.txt", "new\n");
        write(&repo, "y.txt", "edited\n");
        let app = repo.join("app");
        let discard = |c: &Change| git_ops::confirm_discard(c, &c.path, false).unwrap().answered(0).unwrap();
        run(&app, discard(&find(&app, Group::Changes, "app/x.txt").await)).await.unwrap();
        run(&app, discard(&find(&app, Group::Untracked, "app/new.txt").await)).await.unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("app/x.txt")).unwrap(), committed);
        assert!(!repo.join("app/new.txt").exists());
        assert_eq!(listing(&repo).await, rows(&[(Group::Changes, "y.txt", 'M')]));
    }

    /// A real nested repository is listed as its folder and is neither staged by Stage All nor offered a stage or a
    /// discard.
    #[tokio::test]
    async fn a_nested_repository_is_neither_staged_nor_discarded() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt"]);
        write(&repo, "nested/n.txt", "inner\n");
        sh(&repo.join("nested"), &["init", "-q"]);
        write(&repo, "loose.txt", "loose\n");
        let nested = find(&repo, Group::Untracked, "nested/").await;
        assert_eq!(git_ops::confirm_discard(&nested, "nested", false), Err(git_ops::Refusal::Folder));
        assert_eq!(git_ops::stage(&nested), Err(git_ops::Refusal::Folder));
        assert_eq!(row_actions(&nested), []);
        run(&repo, Op::StageUntracked(vec!["nested/".into()])).await.unwrap();
        run(&repo, Op::StageUntracked(vec!["nested/".into(), "loose.txt".into()])).await.unwrap();
        assert_eq!(listing(&repo).await, rows(&[(Group::Staged, "loose.txt", 'A'), (Group::Untracked, "nested/", 'U')]));
        assert!(repo.join("nested/n.txt").is_file());
    }

    /// A conflicted merge: resolved, marked resolved and committed with its `MERGE_MSG`, which concludes it.
    #[tokio::test]
    async fn a_commit_concludes_a_merge() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["f.txt"]);
        sh(&repo, &["checkout", "-qb", "other"]);
        write(&repo, "f.txt", "theirs\n");
        sh(&repo, &["commit", "-qam", "theirs"]);
        sh(&repo, &["checkout", "-q", "main"]);
        write(&repo, "f.txt", "ours\n");
        sh(&repo, &["commit", "-qam", "ours"]);
        sh(&repo, &["merge", "-q", "other"]);
        let message = read_status(repo.clone()).await.unwrap().repo.merge_msg.unwrap();
        write(&repo, "f.txt", "both\n");
        run(&repo, Op::Stage(find(&repo, Group::Conflicts, "f.txt").await)).await.unwrap();
        run(&repo, Op::Commit { message: message.clone(), amend: false }).await.unwrap();
        assert_eq!(sh(&repo, &["log", "-1", "--format=%B"]), message);
        assert_eq!(sh(&repo, &["rev-list", "--parents", "-1", "HEAD"]).split(' ').count(), 3);
        let after = read_status(repo).await.unwrap();
        assert_eq!((after.repo.merging, after.repo.merge_msg, after.status.changes.len()), (false, None, 0));
    }

    /// A rebase stopped on a conflict is seen; a commit there adds to it and leaves it under way.
    #[tokio::test]
    async fn a_commit_during_a_rebase_goes_into_it() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["f.txt"]);
        sh(&repo, &["checkout", "-qb", "topic"]);
        write(&repo, "f.txt", "topic\n");
        sh(&repo, &["commit", "-qam", "topic"]);
        sh(&repo, &["checkout", "-q", "main"]);
        write(&repo, "f.txt", "main\n");
        sh(&repo, &["commit", "-qam", "main"]);
        sh(&repo, &["checkout", "-q", "topic"]);
        sh(&repo, &["rebase", "-q", "main"]);
        assert!(read_status(repo.clone()).await.unwrap().repo.rebasing);
        write(&repo, "f.txt", "both\n");
        run(&repo, Op::Stage(find(&repo, Group::Conflicts, "f.txt").await)).await.unwrap();
        run(&repo, Op::Commit { message: "Topic, over main".into(), amend: false }).await.unwrap();
        assert_eq!(sh(&repo, &["log", "-2", "--format=%s"]), "Topic, over main\nmain");
        assert!(read_status(repo).await.unwrap().repo.rebasing);
    }

    /// A cherry-pick or revert stopped on a conflict is named, and its `MERGE_MSG` is the message's start.
    #[tokio::test]
    async fn a_cherry_pick_and_a_revert_under_way_are_seen_with_their_messages() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["f.txt"]);
        sh(&repo, &["checkout", "-qb", "other"]);
        write(&repo, "f.txt", "theirs\n");
        sh(&repo, &["commit", "-qam", "Pick this fictional fix"]);
        sh(&repo, &["checkout", "-q", "main"]);
        write(&repo, "f.txt", "ours\n");
        sh(&repo, &["commit", "-qam", "ours"]);
        sh(&repo, &["cherry-pick", "other"]);
        let repo_state = read_status(repo.clone()).await.unwrap().repo;
        assert_eq!((repo_state.picking, repo_state.merging), (Some("cherry-pick"), false));
        assert_eq!(repo_state.merge_msg.as_deref(), Some("Pick this fictional fix"));
        sh(&repo, &["cherry-pick", "--abort"]);
        assert_eq!(read_status(repo.clone()).await.unwrap().repo.picking, None);

        write(&repo, "f.txt", "ours, again\n");
        sh(&repo, &["commit", "-qam", "again"]);
        sh(&repo, &["revert", "--no-edit", "HEAD~1"]);
        let repo_state = read_status(repo.clone()).await.unwrap().repo;
        assert_eq!(repo_state.picking, Some("revert"));
        assert!(repo_state.merge_msg.as_deref().is_some_and(|m| m.starts_with("Revert \"ours\"")), "{:?}", repo_state.merge_msg);
    }

    /// With `commit.cleanup=strip`, a message of comments alone is empty, and said so, not blamed on a hook.
    #[tokio::test]
    async fn a_message_empty_after_cleanup_is_said_so() {
        if !has_git() {
            return;
        }
        let (_dir, repo) = our_repo(&["a.txt"]);
        sh(&repo, &["config", "commit.cleanup", "strip"]);
        write(&repo, "a.txt", "edited\n");
        run(&repo, Op::Stage(find(&repo, Group::Changes, "a.txt").await)).await.unwrap();
        let failed = run(&repo, Op::Commit { message: "# only a comment\n".into(), amend: false }).await.unwrap_err();
        assert_eq!(failed.kind, git_ops::Kind::EmptyMessage, "{failed:?}");
        assert_eq!(sh(&repo, &["rev-list", "--count", "HEAD"]), "1");
    }

    /// Run only by `a_git_dir_in_the_environment_does_not_redirect_the_read`, with `GIT_DIR` set.
    #[tokio::test]
    #[ignore]
    async fn read_under_an_inherited_git_dir() {
        if std::env::var_os("SHEPHERD_GIT_DIR_TEST").is_none() {
            return;
        }
        assert!(std::env::var_os("GIT_DIR").is_some());
        let (_dir, repo) = repo(&["mine.txt"]);
        write(&repo, "mine.txt", "changed\n");
        let answer = read_status(repo).await.unwrap();
        assert_eq!(listed(&answer), [(Group::Changes, "mine.txt", 'M')]);
    }
}
