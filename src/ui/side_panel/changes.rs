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
/// than `floor(took)`.
pub(super) fn poll_delay(took: Duration, tab_shown: bool, watched: bool) -> Duration {
    let pace = match (watched, tab_shown) {
        (true, _) => crate::files::watch::SAFETY_POLL,
        (false, true) => super::files::POLL,
        (false, false) => BADGE_POLL,
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
    status: Status,
    prefix: String,
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
    scroll: UniformListScrollHandle,
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
            next_at: None,
            ran: None,
            wanted: false,
            armed: false,
            halted: false,
            stale: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
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

/// The root's place in its repository, then its status.
async fn read_status(root: PathBuf) -> Result<Answer, GitError> {
    let prefix = git(&root, &["rev-parse", "--show-prefix"], PREFIX).await?;
    let prefix = String::from_utf8_lossy(&prefix.stdout).trim_end_matches(['\n', '\r']).to_string();
    let out = git(&root, &["-c", "status.renames=true", "status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"], STATUS).await?;
    Ok(Answer { status: status::parse_status_v2(&out.stdout, &prefix), prefix })
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
        let (id, root) = (list.id, list.root.clone());
        let task = self.rt.spawn(read_status(root));
        list.job = Some(task.abort_handle());
        cx.spawn(async move |this, cx| {
            // an aborted run (its list replaced) answers nothing
            let answer = task.await.ok();
            let _ = this.update(cx, |this, cx| this.take_status(id, now, answer, cx));
        })
        .detach();
    }

    fn take_status(&mut self, id: u64, started: Instant, answer: Option<Result<Answer, GitError>>, cx: &mut Context<Self>) {
        if self.changes.as_ref().is_none_or(|l| l.id != id) {
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
        list.next_at = Some(now + poll_delay(took, tab_shown, watched));
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
        list.rows = match list.answer() {
            Some(a) => change_rows(&a.status.changes, a.status.outside_root, &self.changes_folded),
            None => Vec::new(),
        };
        if let Some((group, path)) = list.selected.clone()
            && list.find(group, &path).is_none()
        {
            list.selected = None;
        }
        if let Some(menu) = self.change_menu.as_mut() {
            match list.find(menu.value.change.group, &menu.value.change.path) {
                Some(found) => menu.value.change = found.clone(),
                None => self.change_menu = None,
            }
        }
        cx.notify();
    }

    fn toggle_group(&mut self, group: Group, cx: &mut Context<Self>) {
        if !self.changes_folded.remove(&group) {
            self.changes_folded.insert(group);
        }
        self.rebuild_changes(cx);
    }

    /// `change`'s file in the root.
    fn change_path(&self, change: &Change) -> Option<PathBuf> {
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
                .child(crate::ui::edge_fade::edge_faded(12., &scroll, rows))
                .child(crate::ui::scrollbar("changes-scrollbar", &list.scroll, theme))
                .into_any_element()
        };
        let menu = self.change_menu(theme, cx);
        div().relative().size_full().flex().flex_col().child(branch_line).child(body).children(menu).into_any_element()
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
        range
            .filter_map(|ix| Some((ix, *list.rows.get(ix)?)))
            .map(|(ix, row)| match row {
                ChangeRow::Header { group, count, folded } => {
                    let turn = gpui::Transformation::rotate(gpui::percentage(if folded { 0. } else { 0.25 }));
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
                        .child(div().flex_none().text_size(px(11.)).text_color(theme.text_faint).child(count.to_string()))
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
                    let icon = material_icon(name, change.is_folder(), false);
                    let deleted = change.absent();
                    let tooltip = match (&change.orig, &change.moved_to) {
                        (_, Some(to)) => format!("{rel}\nRenamed to {to}, outside this folder"),
                        (Some(orig), None) => format!("{rel}\nRenamed from {}", relative(orig, &answer.prefix)),
                        (None, None) => rel.to_string(),
                    };
                    let (click, menu_change) = (change.clone(), change.clone());
                    div()
                        .id(("change-row", ix))
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
        assert_eq!(poll_delay(ms(40), true, false), ms(3000));
        assert_eq!(poll_delay(ms(40), false, false), ms(12_000));
        assert_eq!(poll_delay(ms(1000), true, false), ms(5000));
        assert_eq!(poll_delay(ms(1000), false, false), ms(12_000));
        assert_eq!(poll_delay(ms(4000), false, false), ms(20_000));
        assert_eq!(poll_delay(Duration::MAX, true, false), Duration::MAX);
        // watched: the polls are a safety net, the floor still holds for a change heard
        assert_eq!(poll_delay(ms(40), true, true), ms(30_000));
        assert_eq!(poll_delay(ms(8000), false, true), ms(40_000));
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
