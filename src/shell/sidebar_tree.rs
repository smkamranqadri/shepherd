//! The sidebar's tree: every machine, its sessions, and each session's workspaces, all at once. A level with one
//! entry hides (one machine: no machine row; one machine with one session: no session rows either, as before the
//! tree). The shown session's workspace rows are its live ones; another session's come from its watcher's last
//! snapshot, and a click there switches to it (`go_to`). A session row is a header over its indented workspaces: a
//! click on it folds them, and its menu switches there. The session and machine menus start, stop and restart
//! sessions and turn machines on and off. The Agents section lists the agents of every session (`cross_agents`).
use super::sessions::Place;
use super::watchers::{Kind, SessionKey, Status, Watchers};
use super::*;
use crate::files::roots::root_key;
use crate::herdr::{Machine, SessionInfo};
use std::collections::HashSet;

/// How long another session's branch, read from its HEAD, is kept before it is read again.
const HEAD_TTL: Duration = Duration::from_secs(60);

/// A checkout's branch, from its HEAD file.
#[derive(Clone, Debug, PartialEq)]
struct Head(String);

fn read_head(dir: &str) -> Option<Head> {
    head_branch(std::path::Path::new(dir)).map(Head)
}

/// The branch checked out at `dir` or the nearest folder above it with a `.git`, read from its HEAD (a linked
/// worktree's `.git` file points to its own); a detached HEAD gives its commit's first 7 digits.
pub(super) fn head_branch(dir: &std::path::Path) -> Option<String> {
    let dot_git = dir.ancestors().map(|d| d.join(".git")).find(|g| g.exists())?;
    let git_dir = if dot_git.is_file() {
        let link = std::fs::read_to_string(&dot_git).ok()?;
        let target = std::path::PathBuf::from(link.trim().strip_prefix("gitdir:")?.trim());
        if target.is_absolute() { target } else { dot_git.parent()?.join(target) }
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(r) => Some(r.trim().strip_prefix("refs/heads/").unwrap_or(r.trim()).to_string()),
        None => (head.len() >= 7).then(|| head[..7].to_string()),
    }
}

/// The machine id the tree gives this computer's sessions.
const LOCAL: &str = "local";

/// How long a goal waits for the session switched to (a stopped one being started) to list its workspace.
const GOAL_EXPIRY: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TreeRow {
    /// A machine: this computer, or an SSH target with its saved sessions. Disabled machines are listed greyed.
    Machine { id: String, label: String, status: Status, enabled: bool },
    Session { key: SessionKey, collapsed: bool, waiting: usize, status: Status },
    /// The session's workspace rows.
    Workspaces { key: SessionKey },
    /// A line under a session that has no workspaces to show: why, and what to do.
    Notice { key: SessionKey, text: String, action: Option<NoticeAction> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NoticeAction {
    Start,
    Retry,
}

/// Where a click in the tree goes once its session is shown.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Goal {
    Workspace(String),
    Pane { tab: String, pane: String },
}

/// How workspace rows behave: the shown session's answer clicks, renames and drags; another session's switch there.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum RowMode {
    Live,
    Other(SessionKey),
}

/// The workspace hints held up: their opacity, and each numbered row's (session, workspace) and chord.
pub(super) type Chords = (f32, Vec<(SessionKey, String, String)>);

/// What the tree keeps beside the watchers.
#[derive(Default)]
pub(super) struct TreeState {
    /// A workspace or agent to show once the session switched to lists it, and since when it waits.
    pending_goal: Option<(SessionKey, Goal, Instant)>,
    /// A machine row's menu: the machine (`LOCAL` or its SSH target) and where it opened.
    pub(super) machine_menu: Option<crate::ui::Popup<(String, gpui::Point<Pixels>)>>,
    /// The footer's + menu (New session, Add SSH machine), where it opened.
    add_menu: Option<crate::ui::Popup<gpui::Point<Pixels>>>,
    /// Bare folded ids (an older Shepherd's) were moved under the shown session's key.
    folds_migrated: bool,
    /// The agent Next agent that needs you last went to (session, pane), so the next press goes on from it while a
    /// switch to its session is still landing, or after you looked elsewhere.
    pub(super) needs_you_last: Option<(SessionKey, String)>,
}

impl TreeState {
    /// A switch to `key`: a goal waiting for another session is dropped, so it never lands after you moved on.
    pub(super) fn switching_to(&mut self, key: &SessionKey) {
        if self.pending_goal.as_ref().is_some_and(|(k, _, _)| k != key) {
            self.pending_goal = None;
        }
    }

    /// A goal waits for the session switched to (its others are dropped on a switch): it wins over what the view
    /// remembers.
    pub(super) fn goal_waiting(&self) -> bool {
        self.pending_goal.is_some()
    }

    pub(super) fn menu_open(&self) -> bool {
        self.machine_menu.as_ref().is_some_and(|m| m.is_open()) || self.add_menu_open()
    }

    pub(super) fn add_menu_open(&self) -> bool {
        self.add_menu.as_ref().is_some_and(|m| m.is_open())
    }
}

/// The machine a place runs on: this computer, or its SSH target.
fn machine_of(place: &Place) -> &str {
    match place {
        Place::Session(_) => LOCAL,
        Place::Machine(m) => &m.target,
    }
}

/// herdr's sessions and machines in the tree's order: this computer's sessions first, its default one leading, then
/// each SSH target's enabled machines together, in the order herdr lists them. The disabled machines go apart.
pub(super) fn tree_order(mut sessions: Vec<SessionInfo>, machines: Vec<Machine>) -> (Vec<Place>, Vec<Machine>) {
    sessions.sort_by_key(|s| !s.default); // stable: herdr's order otherwise
    let (enabled, disabled): (Vec<Machine>, Vec<Machine>) = machines.into_iter().partition(|m| m.enabled);
    let mut targets: Vec<&str> = Vec::new();
    for m in &enabled {
        if !targets.contains(&m.target.as_str()) {
            targets.push(&m.target);
        }
    }
    let grouped: Vec<Place> = targets.iter().flat_map(|t| enabled.iter().filter(move |m| m.target == *t).cloned().map(Place::Machine)).collect();
    (sessions.into_iter().map(Place::Session).chain(grouped).collect(), disabled)
}

/// Agents of the snapshot that wait for you or finished.
pub(super) fn waiting(s: &Snapshot) -> usize {
    s.panes.iter().filter(|p| p.agent.is_some() && crate::notify::wants_attention(p.agent_status)).count()
}

/// The agent that has waited longest (as `since` saw it), else the first that waits: its tab and pane.
pub(super) fn longest_waiting(s: &Snapshot, since: &crate::model::Since) -> Option<(String, String)> {
    s.panes
        .iter()
        .filter(|p| p.agent.is_some() && crate::notify::wants_attention(p.agent_status))
        .min_by_key(|p| {
            let at = crate::model::waiting_since(since, p);
            (at.is_none(), at)
        })
        .map(|p| (p.tab_id.clone(), p.pane_id.clone()))
}

/// Folds the session `key` when it is open, opens it when it is folded.
pub(super) fn toggle_in(collapsed: &mut Vec<String>, key: &str) {
    match collapsed.iter().position(|k| k == key) {
        Some(at) => {
            collapsed.remove(at);
        }
        None => collapsed.push(key.to_string()),
    }
}

/// Whether a header (a session or machine row) gets a divider over it: every one but the first, and none right
/// under its machine's row, which heads the group already.
pub(super) fn divider_above(rows: &[TreeRow], i: usize) -> bool {
    i > 0 && matches!(rows[i], TreeRow::Session { .. } | TreeRow::Machine { .. }) && !matches!(rows[i - 1], TreeRow::Machine { .. })
}

/// One session's agents, as the Agents section reads them: the shown session's live snapshot with Shepherd's own
/// `since`, another's from its watcher, timed by the watcher's.
pub(super) struct SessionAgents<'a> {
    pub key: SessionKey,
    pub name: String,
    pub shown: bool,
    pub snapshot: &'a Snapshot,
    pub since: &'a crate::model::Since,
}

/// A row of the Agents section: the agent, its session, the workspace it is in ("session · workspace" when there
/// is more than one session) and how long it has waited.
#[derive(Clone, Debug)]
pub(super) struct AgentEntry {
    pub key: SessionKey,
    /// The session's place in the list, for the rows' ids: pane ids repeat across sessions.
    pub n: usize,
    pub shown: bool,
    pub pane: crate::model::Pane,
    pub workspace: String,
    pub waited: Option<String>,
}

/// The Agents section's entries across `sessions` (in the tree's order), in `sort`'s order, those that need you
/// alone when `attention`; and whether any session has an agent at all. Status puts what needs you first, every
/// session's together; Waiting the longest wait first; Grouped keeps each session's workspaces together, in turn.
pub(super) fn cross_agents(sessions: &[SessionAgents], sort: AgentSort, attention: bool, now: Instant) -> (Vec<AgentEntry>, bool) {
    let rank = |s: AgentStatus| match s {
        AgentStatus::Blocked => 0,
        AgentStatus::Done => 1,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 3,
        AgentStatus::Unknown => 4,
    };
    let named = sessions.len() > 1;
    let mut any = false;
    let mut out: Vec<(AgentEntry, Option<Instant>)> = Vec::new();
    for (n, s) in sessions.iter().enumerate() {
        for p in s.snapshot.agents_by(sort, s.since) {
            any = true;
            if attention && !crate::notify::wants_attention(p.agent_status) {
                continue;
            }
            let at = crate::model::waiting_since(s.since, p);
            let ws = s.snapshot.workspaces.iter().find(|w| w.workspace_id == p.workspace_id).map(|w| w.label.as_str()).unwrap_or_default();
            let workspace = if named { format!("{} · {ws}", s.name) } else { ws.to_string() };
            let waited = at.map(|at| crate::model::ago(now.saturating_duration_since(at)));
            out.push((AgentEntry { key: s.key.clone(), n, shown: s.shown, pane: p.clone(), workspace, waited }, at));
        }
    }
    // stable sorts: within a rank or a wait, each session's own order, and the sessions in the tree's
    match sort {
        AgentSort::Status => out.sort_by_key(|(e, _)| rank(e.pane.agent_status)),
        AgentSort::Waiting => out.sort_by_key(|(_, at)| (at.is_none(), *at)),
        AgentSort::Grouped => {}
    }
    (out.into_iter().map(|(e, _)| e).collect(), any)
}

/// The tree's rows for `places` (in [`tree_order`]), their watchers and the collapsed sessions; `shown` is the shown
/// session's snapshot, which its watcher does not hold.
pub(super) fn tree(places: &[Place], w: &Watchers, collapsed: &HashSet<String>, shown: Option<&Snapshot>) -> Vec<TreeRow> {
    let mut machines: Vec<(&str, Vec<&Place>)> = Vec::new();
    for place in places {
        match machines.iter_mut().find(|(id, _)| *id == machine_of(place)) {
            Some((_, list)) => list.push(place),
            None => machines.push((machine_of(place), vec![place])),
        }
    }
    let mut off: Vec<&str> = Vec::new();
    for m in &w.disabled {
        if !machines.iter().any(|(id, _)| *id == m.target) && !off.contains(&m.target.as_str()) {
            off.push(&m.target);
        }
    }
    let show_machines = machines.len() + off.len() > 1;
    let show_sessions = show_machines || machines.first().is_some_and(|(_, list)| list.len() > 1);
    let mut rows = Vec::new();
    for (id, list) in &machines {
        let statuses: Vec<Status> = list.iter().map(|p| w.get(&SessionKey::of(p)).map_or(Status::Connecting, |w| w.status)).collect();
        if show_machines {
            let status = if statuses.iter().all(|s| *s == Status::Unreachable) { Status::Unreachable } else { Status::Live };
            let label = if *id == LOCAL { "This computer".to_string() } else { id.to_string() };
            rows.push(TreeRow::Machine { id: id.to_string(), label, status, enabled: true });
        }
        for (place, status) in list.iter().zip(statuses) {
            let key = SessionKey::of(place);
            let watcher = w.get(&key);
            let snapshot = if status == Status::Shown { shown } else { watcher.and_then(|w| w.snapshot.as_ref()) };
            let folded = show_sessions && collapsed.contains(&key.0);
            if show_sessions {
                rows.push(TreeRow::Session { key: key.clone(), collapsed: folded, waiting: snapshot.map_or(0, waiting), status });
            }
            if folded {
                continue;
            }
            match status {
                Status::Stopped => rows.push(TreeRow::Notice {
                    key: key.clone(),
                    text: "Stopped".into(),
                    action: matches!(place, Place::Session(_)).then_some(NoticeAction::Start),
                }),
                Status::Unreachable => rows.push(TreeRow::Notice {
                    key: key.clone(),
                    text: if matches!(place, Place::Machine(_)) { "Can't reach host" } else { "Not answering" }.into(),
                    action: Some(NoticeAction::Retry),
                }),
                _ => {}
            }
            if status == Status::Shown || (status != Status::Stopped && snapshot.is_some()) {
                rows.push(TreeRow::Workspaces { key });
            }
        }
    }
    for target in off {
        rows.push(TreeRow::Machine { id: target.to_string(), label: target.to_string(), status: Status::Stopped, enabled: false });
    }
    rows
}

/// The workspaces Workspace1–9 go to: the tree's visible rows from the top, worktrees of a folded group left out.
pub(super) fn workspace_order<'a>(rows: &[TreeRow], snaps: impl Fn(&SessionKey) -> Option<&'a Snapshot>, folded: &HashSet<String>) -> Vec<(SessionKey, String)> {
    let mut order = Vec::new();
    for row in rows {
        let TreeRow::Workspaces { key } = row else { continue };
        let Some(snapshot) = snaps(key) else { continue };
        for (w, kids) in crate::model::groups(snapshot.workspace_tree()) {
            order.push((key.clone(), w.workspace_id.clone()));
            if !folded.contains(&root_key(&key.0, &w.workspace_id)) {
                order.extend(kids.iter().map(|k| (key.clone(), k.workspace_id.clone())));
            }
        }
    }
    order.truncate(9);
    order
}

/// Moves an older Shepherd's folded workspaces (bare ids) under the shown session's key; true when any moved.
pub(super) fn migrate_folds(folded: &mut HashSet<String>, shown: &str) -> bool {
    let bare: Vec<String> = folded.iter().filter(|f| !f.contains('\n')).cloned().collect();
    for id in &bare {
        folded.remove(id);
        folded.insert(root_key(shown, id));
    }
    !bare.is_empty()
}

impl Shell {
    /// The tree's rows and the shown session's key; a socket of one's own (no session of herdr's list), or the
    /// session shown before the list came, shows its workspaces first.
    fn tree_model(&self, cx: &App) -> (Vec<TreeRow>, SessionKey) {
        let collapsed: HashSet<String> = SettingsGlobal::of(cx).settings.collapsed_sessions.iter().cloned().collect();
        let mut rows = tree(&self.places, &self.watchers, &collapsed, self.snapshot.as_ref());
        let shown = self.shown_key().unwrap_or_default();
        if !self.places.iter().any(|p| SessionKey::of(p) == shown) {
            rows.insert(0, TreeRow::Workspaces { key: shown.clone() });
        }
        (rows, shown)
    }

    /// The workspaces Workspace1–9 go to, in the tree's order.
    fn numbered_workspaces(&self, rows: &[TreeRow], shown: &SessionKey) -> Vec<(SessionKey, String)> {
        workspace_order(rows, |k| if k == shown { self.snapshot.as_ref() } else { self.watchers.snapshot(k) }, &self.folded)
    }

    /// Workspace1–9: the `index`th workspace the tree shows, in whichever session.
    pub(super) fn go_to_tree_workspace(&mut self, index: usize, cx: &mut Context<Self>) {
        let (rows, shown) = self.tree_model(cx);
        if let Some((key, ws)) = self.numbered_workspaces(&rows, &shown).into_iter().nth(index) {
            self.go_to(key, Goal::Workspace(ws), cx);
        }
    }

    /// Shows `goal` in the session `key`: at once in the shown one; else after switching there, from its last
    /// snapshot, and again from its first own (a session being started waits up to 15 s to list it). A machine out
    /// of reach stays where it was, with a toast.
    pub(super) fn go_to(&mut self, key: SessionKey, goal: Goal, cx: &mut Context<Self>) {
        if self.shown_key().unwrap_or_default() == key {
            self.apply_goal(&goal, cx);
            return;
        }
        let Some(place) = self.places.iter().find(|p| SessionKey::of(p) == key).cloned() else { return };
        if let Some(w) = self.watchers.get(&key).filter(|w| w.status == Status::Unreachable && w.kind() == Kind::Remote) {
            let why = w.error.clone().unwrap_or_default();
            let name = place.look(cx).0;
            crate::ui::toast::error(cx, format!("Can't reach {name}: {why}"));
            return;
        }
        self.show_place_from_tree(place.clone(), cx);
        if !self.places_shown(&place) {
            return;
        }
        self.apply_goal(&goal, cx);
        self.tree.pending_goal = Some((key, goal, Instant::now()));
    }

    /// Shows a place from the tree, the list fading over as the keys' switch does.
    fn show_place_from_tree(&mut self, place: Place, cx: &mut Context<Self>) {
        let at = |p: &Place| self.places.iter().position(|q| q.key() == p.key());
        let shown = self.places.iter().position(|p| self.places_shown(p));
        let forward = at(&place) >= shown;
        self.show_place(place, forward, cx);
    }

    /// The goal in the shown session's snapshot: false while it does not list it.
    fn apply_goal(&mut self, goal: &Goal, cx: &mut Context<Self>) -> bool {
        let Some(snapshot) = self.snapshot.as_ref() else { return false };
        // the goal wins over the keyboard pane the view remembers
        self.restore = None;
        match goal {
            Goal::Workspace(id) => {
                let Some(w) = snapshot.workspaces.iter().find(|w| &w.workspace_id == id) else { return false };
                let tab = w.active_tab_id.clone().or_else(|| snapshot.tabs_of(id).first().map(|t| t.tab_id.clone()));
                // the workspace as it was left, a file tab it showed included
                self.show_herdr_tab(tab, false, cx);
            }
            Goal::Pane { tab, pane } => {
                if !snapshot.panes.iter().any(|p| &p.pane_id == pane) {
                    return false;
                }
                self.show_agent(tab.clone(), pane.clone(), cx);
            }
        }
        true
    }

    /// The shown session sent a snapshot of its own: a goal waiting for it is shown, and folds an older Shepherd
    /// kept by bare id move under its key.
    pub(super) fn after_snapshot(&mut self, cx: &mut Context<Self>) {
        // before a goal: the panes a view brought back that this snapshot no longer backs go first
        self.check_parked(cx);
        let shown = self.shown_key();
        if !self.tree.folds_migrated && let Some(key) = &shown {
            self.tree.folds_migrated = true;
            if migrate_folds(&mut self.folded, &key.0) {
                let mut folded: Vec<String> = self.folded.iter().cloned().collect();
                folded.sort();
                save_from_sidebar(cx, |s| s.folded = folded);
            }
        }
        let Some((key, goal, at)) = self.tree.pending_goal.clone() else { return };
        // given up on after a while, else shown once the session lists it
        if at.elapsed() > GOAL_EXPIRY || (shown.as_ref() == Some(&key) && self.apply_goal(&goal, cx)) {
            self.tree.pending_goal = None;
        }
    }

    fn toggle_collapsed(&mut self, key: &SessionKey, cx: &mut Context<Self>) {
        let key = key.0.clone();
        save_from_sidebar(cx, move |s| toggle_in(&mut s.collapsed_sessions, &key));
        cx.notify();
    }

    /// The Agents section's entries from every session: the shown one's live snapshot, the others' from their
    /// watchers; and whether any session has an agent at all.
    pub(super) fn agents_everywhere(&self, attention: bool, cx: &App) -> (Vec<AgentEntry>, bool) {
        cross_agents(&self.session_agents(cx), self.agent_sort, attention, Instant::now())
    }

    /// Every session's agents as the Agents section and Next agent that needs you read them, in the tree's order: a
    /// socket of one's own first, the shown session's live snapshot, the others' from their watchers (a stopped one's
    /// left out). One list for both, so the key never goes where the section does not show.
    pub(super) fn session_agents(&self, cx: &App) -> Vec<SessionAgents<'_>> {
        let shown = self.shown_key();
        let mut sessions: Vec<SessionAgents> = Vec::new();
        let listed = |k: &SessionKey| self.places.iter().any(|p| SessionKey::of(p) == *k);
        if let (Some(key), Some(snapshot)) = (&shown, self.snapshot.as_ref()) {
            // a socket of one's own, outside herdr's list, leads as its workspaces do
            if !listed(key) {
                sessions.push(SessionAgents { key: key.clone(), name: self.place_label(cx), shown: true, snapshot, since: &self.since });
            }
        }
        for place in &self.places {
            let key = SessionKey::of(place);
            let name = place.look(cx).0;
            if shown.as_ref() == Some(&key) {
                if let Some(snapshot) = self.snapshot.as_ref() {
                    sessions.push(SessionAgents { key, name, shown: true, snapshot, since: &self.since });
                }
            } else if let Some(w) = self.watchers.get(&key).filter(|w| w.status != Status::Stopped)
                && let Some(snapshot) = w.snapshot.as_ref()
            {
                sessions.push(SessionAgents { key, name, shown: false, snapshot, since: &w.since });
            }
        }
        sessions
    }

    /// The tree's elements, and whether it lists sessions (then the header leaves out the shown one's name).
    pub(super) fn tree_rows(&self, theme: &Theme, snapshot: &Option<Snapshot>, shown_ws: &Option<String>, active: bool, cx: &mut Context<Self>) -> (Vec<gpui::AnyElement>, bool) {
        let (rows, shown) = self.tree_model(cx);
        let listed = rows.iter().any(|r| matches!(r, TreeRow::Session { .. }));
        // held Ctrl+Shift (Ctrl on macOS): each of the first nine visible workspaces' chord
        let hints: Option<Chords> = self.hints_of(hints::HintMode::Workspaces).map(|opacity| {
            let order = self.numbered_workspaces(&rows, &shown);
            (opacity, order.into_iter().zip(WORKSPACE_ACTIONS).filter_map(|((k, w), a)| Some((k, w, chord_of(cx, a)?))).collect())
        });
        let mut out = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            // a divider and 8 px or more between session groups
            if divider_above(&rows, i) {
                out.push(div().flex_none().mx(px(6.)).mt(px(6.)).mb(px(4.)).h(px(1.)).bg(theme.border.opacity(0.6)).into_any_element());
            }
            match row.clone() {
                TreeRow::Machine { id, label, status, enabled } => out.push(self.machine_row(theme, id, label, status, enabled, cx)),
                TreeRow::Session { key, collapsed, waiting, status } => {
                    if let Some(place) = self.places.iter().find(|p| SessionKey::of(p) == key).cloned() {
                        out.push(self.session_row(theme, place, key, collapsed, waiting, status, cx));
                    }
                }
                TreeRow::Notice { key, text, action } => out.push(self.notice_row(theme, key, text, action, cx)),
                TreeRow::Workspaces { key } => {
                    let ws = if key == shown {
                        self.workspace_rows(theme, snapshot.as_ref(), shown_ws, RowMode::Live, hints.as_ref(), active, cx)
                    } else {
                        let other = self.watchers.snapshot(&key);
                        self.workspace_rows(theme, other, &None, RowMode::Other(key), hints.as_ref(), active, cx)
                    };
                    if listed {
                        // indented under their session's header, a faint guide down their left
                        out.push(
                            div()
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .ml(px(15.))
                                .pl(px(5.))
                                .border_l_1()
                                .border_color(theme.border.opacity(0.6))
                                .children(ws)
                                .into_any_element(),
                        );
                    } else {
                        out.extend(ws);
                    }
                }
            }
        }
        (out, listed)
    }

    /// A machine's header: its icon, its name in small caps, a dot for whether it answers, and ⋯ on hover.
    fn machine_row(&self, theme: &Theme, id: String, label: String, status: Status, enabled: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hover_bg = theme.hover;
        let menu_id = id.clone();
        let group = SharedString::from(format!("tree-machine-group-{id}"));
        let open = self.tree.machine_menu.as_ref().is_some_and(|m| m.is_open() && m.value.0 == id);
        let ink = if enabled { theme.text_muted } else { theme.text_faint.opacity(0.6) };
        let dot = match status {
            _ if !enabled => theme.text_faint.opacity(0.4),
            Status::Unreachable => theme.danger,
            Status::Stopped => theme.text_faint.opacity(0.6),
            _ => theme.success,
        };
        let icon = if id == LOCAL { "icons/monitor.svg" } else { "icons/remote-server.svg" };
        div()
            .id(SharedString::from(format!("tree-machine-{id}")))
            .group(group.clone())
            .h(px(26.))
            .px(px(8.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .text_size(px(10.5))
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(ink)
            .child(gpui::svg().path(icon).size(px(13.)).flex_none().text_color(ink))
            .child(div().flex_1().min_w_0().truncate().child(if enabled { label.to_uppercase() } else { format!("{} · OFF", label.to_uppercase()) }))
            .child(div().size(px(6.)).flex_none().rounded_full().bg(dot))
            .child(
                div()
                    .id(SharedString::from(format!("tree-machine-menu-{id}")))
                    .size(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .when(!open, |el| el.opacity(0.).group_hover(group, |s| s.opacity(1.)))
                    .child(gpui::svg().path("icons/more-horizontal.svg").size(px(12.)).text_color(theme.text_faint))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.tree.machine_menu = Some(crate::ui::Popup::new((menu_id.clone(), e.position)));
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn session_row(&self, theme: &Theme, place: Place, key: SessionKey, collapsed: bool, waiting: usize, status: Status, cx: &mut Context<Self>) -> gpui::AnyElement {
        let idx = self.watchers.index_of(&key).unwrap_or(0);
        let hover_bg = theme.hover;
        let stale = self.watchers.get(&key).is_some_and(|w| w.stale(Instant::now(), self.watchers.activity()));
        let dot = match status {
            Status::Shown | Status::Live if !stale => theme.success,
            Status::Shown | Status::Live | Status::Connecting => theme.warning,
            Status::Stopped => theme.text_faint.opacity(0.6),
            Status::Unreachable => theme.danger,
        };
        let shown = status == Status::Shown;
        let (name, icon) = place.look(cx);
        let ink = if shown { theme.text } else { theme.text_muted };
        // the icon set in Edit…, else a terminal for this computer's sessions and a server for an SSH machine's
        let mark = match (icon, &place) {
            (Some(icon), _) => sessions::icon_mark(&icon, 14., ink),
            (None, Place::Machine(_)) => gpui::svg().path("icons/remote-server.svg").size(px(14.)).flex_none().text_color(ink).into_any_element(),
            (None, Place::Session(_)) => gpui::svg().path("icons/terminal.svg").size(px(14.)).flex_none().text_color(ink).into_any_element(),
        };
        let badge = (waiting > 0).then(|| {
            let goal_key = key.clone();
            div()
                .id(("tree-waiting", idx))
                .flex_none()
                .px(px(5.))
                .h(px(16.))
                .flex()
                .items_center()
                .gap(px(3.))
                .rounded(px(8.))
                .bg(theme.warning.opacity(0.15))
                .text_size(px(10.))
                .text_color(theme.warning)
                .cursor_pointer()
                .tooltip(tip("Go to the agent that has waited longest", None))
                .child("●")
                .child(waiting.to_string())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    let found = if this.shown_key().as_ref() == Some(&goal_key) {
                        this.snapshot.as_ref().and_then(|s| longest_waiting(s, &this.since))
                    } else {
                        this.watchers.get(&goal_key).and_then(|w| longest_waiting(w.snapshot.as_ref()?, &w.since))
                    };
                    if let Some((tab, pane)) = found {
                        this.go_to(goal_key.clone(), Goal::Pane { tab, pane }, cx);
                    }
                }))
        });
        let toggle_key = key.clone();
        let chevron = fold_chevron(theme, &format!("session-{idx}"), !collapsed, 1., None);
        let group = SharedString::from(format!("tree-session-group-{idx}"));
        let open = self.place_menu.as_ref().is_some_and(|m| m.is_open() && m.value.0.key() == place.key());
        let (menu_place, right_place) = (place.clone(), place);
        let more = div()
            .id(("tree-session-menu", idx))
            .size(px(18.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .when(!open, |el| el.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.)))
            .child(gpui::svg().path("icons/more-horizontal.svg").size(px(12.)).text_color(theme.text_faint))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.open_place_menu(menu_place.clone(), e.position, cx);
            }));
        // a header: the chevron, the session's icon and its name in bold; a click anywhere folds its workspaces
        row(theme, false, ("tree-session", idx))
            .group(group)
            .h(px(28.))
            .pl(px(2.))
            .pr(px(4.))
            .gap(px(6.))
            .text_size(px(12.5))
            .text_color(ink)
            .font_weight(if shown { gpui::FontWeight::BOLD } else { gpui::FontWeight::SEMIBOLD })
            .child(chevron)
            .child(mark)
            .child(div().flex_1().min_w_0().truncate().child(name))
            .children(badge)
            .child(div().size(px(6.)).flex_none().rounded_full().bg(dot))
            .child(more)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_collapsed(&toggle_key, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| this.open_place_menu(right_place.clone(), e.position, cx)))
            .into_any_element()
    }

    fn notice_row(&self, theme: &Theme, key: SessionKey, text: String, action: Option<NoticeAction>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let idx = self.watchers.index_of(&key).unwrap_or(0);
        let error = self.watchers.get(&key).and_then(|w| w.error.clone());
        let accent = theme.accent;
        let button = action.map(|action| {
            let key = key.clone();
            div()
                .id(("tree-notice-action", idx))
                .cursor_pointer()
                .text_color(accent)
                .child(match action {
                    NoticeAction::Start => "Start",
                    NoticeAction::Retry => "Retry",
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| match action {
                    NoticeAction::Start => {
                        if let Some(Place::Session(s)) = this.places.iter().find(|p| SessionKey::of(p) == key).cloned() {
                            this.start_session_here(s, cx);
                        }
                    }
                    NoticeAction::Retry => {
                        this.watchers.poll_now(&key, Instant::now());
                        cx.notify();
                    }
                }))
        });
        div()
            .id(("tree-notice", idx))
            .h(px(22.))
            .pl(px(28.))
            .pr(px(8.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .text_size(px(11.))
            .text_color(theme.text_faint)
            .when_some(error, |el, why| el.tooltip(tip(why, None)))
            .child(text)
            .when(button.is_some(), |el| el.child("·"))
            .children(button)
            .into_any_element()
    }

    /// Starts a stopped session's server (from its menu or its notice), then reads herdr's list; its watcher shows it
    /// connecting and asks it every second meanwhile. The session shown is the sync loop's to start, and a start
    /// already under way ignores the click: one server per session.
    fn start_session_here(&mut self, session: SessionInfo, cx: &mut Context<Self>) {
        let key = SessionKey(session.socket_path.clone());
        if self.places_shown(&Place::Session(session.clone())) || !self.watchers.start(&key, Instant::now()) {
            return;
        }
        let task = self.rt.spawn(async move { crate::herdr::lifecycle::start_session(&session.name, session.default) });
        self.after_lifecycle(task, "start", cx);
        cx.notify();
    }

    /// When a start, stop or restart finished (each runs on the runtime, with its deadline): its error as a toast, and
    /// herdr's list read again.
    fn after_lifecycle(&mut self, task: tokio::task::JoinHandle<anyhow::Result<()>>, what: &'static str, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let done = task.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |s, cx| {
                if let Err(e) = done {
                    crate::ui::toast::error(cx, format!("Couldn't {what} the session: {e:#}"));
                }
                s.refresh_places(cx);
            });
        })
        .detach();
    }

    /// Stops the session. The one shown leaves for another running session first: the sync loop would start it
    /// again at once.
    fn stop_session_here(&mut self, session: SessionInfo, cx: &mut Context<Self>) {
        let place = Place::Session(session.clone());
        if self.places_shown(&place) {
            let other = self.places.iter().find(|p| matches!(p, Place::Session(s) if s.running && s.name != session.name)).cloned();
            match other {
                Some(other) => self.show_place_from_tree(other, cx),
                None => return crate::ui::toast::error(cx, "This is the only session running: start another one before stopping it."),
            }
        }
        let task = self.rt.spawn(crate::herdr::lifecycle::stop_session(session.name));
        self.after_lifecycle(task, "stop", cx);
    }

    /// Restarts the session; the one shown is only stopped, and the sync loop starts it again as it does at launch.
    fn restart_session_here(&mut self, session: SessionInfo, cx: &mut Context<Self>) {
        let key = SessionKey(session.socket_path.clone());
        let task = if self.places_shown(&Place::Session(session.clone())) {
            self.rt.spawn(crate::herdr::lifecycle::stop_session(session.name))
        } else {
            // its start counts from now: connecting, not stopped, while it goes down and comes back
            if !self.watchers.start(&key, Instant::now()) {
                return;
            }
            self.rt.spawn(crate::herdr::lifecycle::restart_session(session))
        };
        self.after_lifecycle(task, "restart", cx);
        cx.notify();
    }

    /// The session menu's rows after Edit: Start, or Stop and Restart; New workspace here.
    pub(super) fn place_menu_extras(&self, place: &Place, theme: &Theme, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let item = |id: &'static str, label: &'static str| row(theme, false, id).h(px(28.)).rounded(px(6.)).child(label);
        let close = |this: &mut Shell| {
            if let Some(m) = this.place_menu.as_mut() {
                m.close()
            }
        };
        let mut items = Vec::new();
        // a click on the session's row folds it: switching there is here (or a click on one of its workspaces)
        if !self.places_shown(place) {
            let to = place.clone();
            items.push(item("place-menu-switch", "Switch to this session").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                close(this);
                if !this.places_shown(&to) {
                    this.show_place_from_tree(to.clone(), cx);
                }
            })));
        }
        if let Place::Session(s) = place {
            let key = SessionKey::of(place);
            let stopped = !s.running || self.watchers.get(&key).is_some_and(|w| w.status == Status::Stopped);
            // the session shown is started by the sync loop, and one starting already needs no second start
            let starting = self.watchers.starting(&key, Instant::now());
            if !starting && stopped && !self.places_shown(place) {
                let s = s.clone();
                items.push(item("place-menu-start", "Start").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    close(this);
                    this.start_session_here(s.clone(), cx);
                })));
            } else if !starting && !stopped {
                let (stop, restart) = (s.clone(), s.clone());
                items.push(item("place-menu-stop", "Stop").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    close(this);
                    this.stop_session_here(stop.clone(), cx);
                })));
                items.push(item("place-menu-restart", "Restart").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    close(this);
                    this.restart_session_here(restart.clone(), cx);
                })));
            }
        }
        let here = place.clone();
        items.push(item("place-menu-new-workspace", "New workspace here").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            close(this);
            if !this.places_shown(&here) {
                this.show_place_from_tree(here.clone(), cx);
            }
            if this.places_shown(&here) {
                this.new_workspace(cx);
            }
        })));
        items.into_iter().map(IntoElement::into_any_element).collect()
    }

    /// A machine's saved sessions, enabled or not.
    fn machine_records(&self, target: &str) -> Vec<Machine> {
        let enabled = self.places.iter().filter_map(|p| match p {
            Place::Machine(m) if m.target == target => Some(m.clone()),
            _ => None,
        });
        enabled.chain(self.watchers.disabled.iter().filter(|m| m.target == target).cloned()).collect()
    }

    /// Turns a machine's saved sessions on or off in herdr; one shown that goes off leaves for the default session.
    fn set_machine_enabled(&mut self, target: &str, enabled: bool, cx: &mut Context<Self>) {
        let records = self.machine_records(target);
        if !enabled && records.iter().any(|m| self.places_shown(&Place::Machine(m.clone())))
            && let Some(home) = self.places.iter().find(|p| matches!(p, Place::Session(s) if s.default)).cloned()
        {
            self.show_place_from_tree(home, cx);
        }
        let ids: Vec<String> = records.into_iter().map(|m| m.id).collect();
        let task = self.rt.spawn(async move {
            for id in ids {
                crate::herdr::lifecycle::set_machine_enabled(id, enabled).await?;
            }
            anyhow::Ok(())
        });
        cx.spawn(async move |this, cx| {
            let done = task.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |s, cx| {
                if let Err(e) = done {
                    crate::ui::toast::error(cx, format!("{e:#}"));
                }
                s.refresh_places(cx);
            });
        })
        .detach();
    }

    /// The footer's + opens its menu, or closes it when it is open.
    pub(super) fn toggle_add_menu(&mut self, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        match self.tree.add_menu.as_mut() {
            Some(menu) if menu.is_open() => menu.close(),
            _ => self.tree.add_menu = Some(crate::ui::Popup::new(at)),
        }
        cx.notify();
    }

    /// The tree's menus while they are up: a machine row's, and the footer's +.
    pub(super) fn tree_popups(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        self.machine_popup(theme, cx).into_iter().chain(self.add_popup(theme, cx)).collect()
    }

    /// The footer's + menu, opening up from the button.
    fn add_popup(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let menu = self.tree.add_menu.as_ref()?;
        let at = menu.value;
        let item = |id: &'static str, label: &'static str| row(theme, false, id).h(px(28.)).rounded(px(6.)).child(label);
        let close = |this: &mut Shell| {
            if let Some(m) = this.tree.add_menu.as_mut() {
                m.close()
            }
        };
        let card = menu_card(theme, "add-menu", 170.)
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                close(this);
                cx.notify();
            }))
            .child(item("add-menu-session", "New session…").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                close(this);
                this.open_session_dialog(window, cx);
            })))
            .child(item("add-menu-machine", "Add SSH machine…").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                close(this);
                this.open_session_dialog(window, cx);
                this.toggle_session_remote(window, cx);
            })));
        Some(
            gpui::deferred(
                gpui::anchored().position(at).anchor(gpui::Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(crate::ui::menu_motion("add-menu-in", menu, card)),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The machine row's menu, while it is up.
    fn machine_popup(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let menu = self.tree.machine_menu.as_ref()?;
        let (target, at) = menu.value.clone();
        let item = |id: &'static str, label: &'static str| row(theme, false, id).h(px(28.)).rounded(px(6.)).child(label);
        let close = |this: &mut Shell| {
            if let Some(m) = this.tree.machine_menu.as_mut() {
                m.close()
            }
        };
        let mut card = menu_card(theme, "machine-menu", 170.).on_mouse_down_out(cx.listener(move |this, _, _, cx| {
            close(this);
            cx.notify();
        }));
        if target == LOCAL {
            card = card.child(item("machine-menu-new-session", "New session…").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                close(this);
                this.open_session_dialog(window, cx);
            })));
        } else {
            let records = self.machine_records(&target);
            let enabled = records.iter().any(|m| m.enabled);
            let one = (records.len() == 1).then(|| records[0].clone());
            if let Some(m) = one.clone().filter(|m| m.enabled) {
                card = card.child(item("machine-menu-rename", "Rename…").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    close(this);
                    this.open_place_edit(Place::Machine(m.clone()), window, cx);
                })));
            }
            if let Some(first) = records.first().cloned() {
                card = card.child(item("machine-menu-reconnect", "Reconnect…").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    close(this);
                    let command = crate::herdr::lifecycle::reconnect_command(&first.id);
                    crate::ui::toast::info(cx, format!("Shepherd's SSH never asks for a password: run `{command}` in a terminal, then Retry."), None);
                })));
            }
            let flip = target.clone();
            card = card.child(item("machine-menu-enable", if enabled { "Disable" } else { "Enable" }).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                close(this);
                this.set_machine_enabled(&flip, !enabled, cx);
            })));
            if let Some(m) = one {
                card = card.child(div().mx(px(-4.)).my(px(4.)).h(px(1.)).bg(theme.border)).child(
                    item("machine-menu-forget", "Forget…").text_color(theme.danger).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        close(this);
                        this.confirm_delete(Place::Machine(m.clone()), cx);
                    })),
                );
            }
        }
        Some(
            gpui::deferred(gpui::anchored().position(at).snap_to_window_with_margin(px(8.)).child(crate::ui::menu_motion("machine-menu-in", menu, card)))
                .with_priority(1)
                .into_any_element(),
        )
    }

    pub(super) fn reap_tree_popups(&mut self, window: &mut Window) {
        crate::ui::reap_popup(&mut self.tree.machine_menu, window);
        crate::ui::reap_popup(&mut self.tree.add_menu, window);
    }

    /// Where a workspace row of `mode` keeps its folder: the shown session's as today (git and gh ask it); another
    /// session's here as it is (its icon and HEAD only, see `head_branch`); none for another machine's.
    pub(super) fn row_dir(&self, mode: &RowMode, path: &str) -> Option<String> {
        match mode {
            RowMode::Other(key) => match self.watchers.get(key).map(|w| w.kind()) {
                Some(Kind::Local) => Some(crate::herdr::locate_on(None, path)),
                _ => None,
            },
            _ => Some(crate::herdr::locate(path)),
        }
    }

    /// The branch of another session's workspace row: its checkout's HEAD, read from the file (no git, no status,
    /// no ahead or behind) on a background thread, at most once a minute.
    pub(super) fn other_branch(dir: &str, cx: &mut App) -> Option<String> {
        crate::ui::statusbar::cached(dir, HEAD_TTL, read_head, cx).map(|Head(b)| b)
    }

    /// The fold key and element id prefix of a session's workspace rows.
    pub(super) fn row_keys(&self, mode: &RowMode) -> (String, usize) {
        let key = match mode {
            RowMode::Other(key) => key.clone(),
            _ => self.shown_key().unwrap_or_default(),
        };
        let idx = self.watchers.index_of(&key).map_or(0, |i| i + 1);
        (key.0, idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, running: bool) -> SessionInfo {
        SessionInfo { name: name.into(), running, socket_path: format!("/tmp/{name}.sock"), default: name == "default" }
    }

    fn machine(id: &str, target: &str, enabled: bool) -> Machine {
        Machine { id: id.into(), label: id.into(), target: target.into(), session: "default".into(), enabled }
    }

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    fn snapshot(workspaces: &[(&str, Option<(&str, bool)>)]) -> Snapshot {
        let ws: Vec<_> = workspaces
            .iter()
            .enumerate()
            .map(|(i, (id, tree))| {
                let worktree = tree.map(|(repo, linked)| json!({"repo_key": repo, "is_linked_worktree": linked, "checkout_path": format!("/r/{id}")}));
                json!({"workspace_id": id, "label": id, "number": i + 1, "active_tab_id": null, "agent_status": "idle", "worktree": worktree})
            })
            .collect();
        let panes = json!([{"pane_id": "p1", "tab_id": "t1", "workspace_id": "w1", "terminal_id": "x", "agent_status": "blocked", "agent": "claude"}]);
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": ws, "tabs": [], "panes": panes, "layouts": []}})).unwrap()
    }

    fn watched(places: &[Place], shown: &str) -> Watchers {
        let mut w = Watchers::new();
        w.reconcile(places, Some(&key(shown)), Instant::now());
        w
    }

    fn kinds(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TreeRow::Machine { label, .. } => format!("M {label}"),
                TreeRow::Session { key, .. } => format!("S {}", key.0),
                TreeRow::Workspaces { key } => format!("W {}", key.0),
                TreeRow::Notice { text, .. } => format!("N {text}"),
            })
            .collect()
    }

    #[test]
    fn the_default_session_leads_and_machines_group_by_target() {
        let (places, off) = tree_order(
            vec![info("menqal", true), info("default", true)],
            vec![machine("a", "you@box", true), machine("b", "you@other", true), machine("c", "you@box", true), machine("d", "you@gone", false)],
        );
        let keys: Vec<String> = places.iter().map(|p| p.key()).collect();
        assert_eq!(keys, ["/tmp/default.sock", "/tmp/menqal.sock", "machine:a", "machine:c", "machine:b"]);
        assert_eq!(off.len(), 1);
    }

    #[test]
    fn one_machine_with_one_session_looks_as_before() {
        let places = [Place::Session(info("default", true))];
        let rows = tree(&places, &watched(&places, "default"), &HashSet::new(), None);
        assert_eq!(kinds(&rows), ["W /tmp/default.sock"]);
    }

    #[test]
    fn one_machine_lists_its_sessions_without_a_machine_row() {
        let places = [Place::Session(info("default", true)), Place::Session(info("menqal", true)), Place::Session(info("old", false))];
        let shown = snapshot(&[("w1", None)]);
        let rows = tree(&places, &watched(&places, "default"), &HashSet::new(), Some(&shown));
        assert_eq!(kinds(&rows), ["S /tmp/default.sock", "W /tmp/default.sock", "S /tmp/menqal.sock", "S /tmp/old.sock", "N Stopped"]);
        assert!(matches!(rows[0], TreeRow::Session { waiting: 1, status: Status::Shown, .. }));
        assert!(matches!(rows.last(), Some(TreeRow::Notice { action: Some(NoticeAction::Start), .. })));
    }

    #[test]
    fn machines_get_rows_and_collapsed_sessions_hide_their_workspaces() {
        let (places, off) = tree_order(vec![info("default", true), info("menqal", true)], vec![machine("a", "you@box", true), machine("d", "you@gone", false)]);
        let mut w = watched(&places, "default");
        w.disabled = off;
        let collapsed: HashSet<String> = ["/tmp/default.sock".to_string()].into();
        let rows = tree(&places, &w, &collapsed, Some(&snapshot(&[("w1", None)])));
        assert_eq!(kinds(&rows), ["M This computer", "S /tmp/default.sock", "S /tmp/menqal.sock", "M you@box", "S machine:a", "M you@gone"]);
        assert!(matches!(rows[1], TreeRow::Session { collapsed: true, .. }));
        assert!(matches!(rows.last(), Some(TreeRow::Machine { enabled: false, .. })));
    }

    #[test]
    fn an_unreachable_machine_says_so_and_keeps_its_last_workspaces() {
        let (places, _) = tree_order(vec![info("default", true)], vec![machine("a", "you@box", true)]);
        let mut w = watched(&places, "default");
        let k = SessionKey("machine:a".into());
        let now = Instant::now();
        w.set_client(&k, Client::at("/tmp/x".into()));
        w.due(now, super::super::watchers::Activity::Active);
        w.apply(&k, Ok(snapshot(&[("w1", None)])), now);
        w.apply(&k, Err(anyhow::anyhow!("timed out")), now);
        w.apply(&k, Err(anyhow::anyhow!("timed out")), now);
        let rows = tree(&places, &w, &HashSet::new(), None);
        assert_eq!(&kinds(&rows)[4..], ["S machine:a", "N Can't reach host", "W machine:a"]);
    }

    #[test]
    fn numbering_follows_the_visible_rows() {
        let shown = snapshot(&[("w1", Some(("repo", false))), ("w2", Some(("repo", true))), ("w3", None)]);
        let other = snapshot(&[("w1", None), ("w2", None)]);
        let rows = vec![
            TreeRow::Workspaces { key: key("default") },
            TreeRow::Session { key: key("old"), collapsed: false, waiting: 0, status: Status::Stopped },
            TreeRow::Workspaces { key: key("menqal") },
        ];
        let snaps = |k: &SessionKey| if *k == key("default") { Some(&shown) } else if *k == key("menqal") { Some(&other) } else { None };
        let order = workspace_order(&rows, snaps, &HashSet::new());
        let ids: Vec<String> = order.iter().map(|(k, w)| format!("{}:{w}", &k.0[5..k.0.len() - 5])).collect();
        assert_eq!(ids, ["default:w1", "default:w2", "default:w3", "menqal:w1", "menqal:w2"]);
        // a folded repo's worktrees are skipped
        let folded: HashSet<String> = [root_key("/tmp/default.sock", "w1")].into();
        assert_eq!(workspace_order(&rows, snaps, &folded).len(), 4);
        // never past nine
        let many = snapshot(&[("a", None), ("b", None), ("c", None), ("d", None), ("e", None), ("f", None)]);
        let rows = vec![TreeRow::Workspaces { key: key("x") }, TreeRow::Workspaces { key: key("y") }];
        assert_eq!(workspace_order(&rows, |_| Some(&many), &HashSet::new()).len(), 9);
    }

    #[test]
    fn bare_folds_move_under_the_shown_session() {
        let mut folded: HashSet<String> = ["w1".to_string(), root_key("/tmp/menqal.sock", "w2")].into();
        assert!(migrate_folds(&mut folded, "/tmp/default.sock"));
        assert!(folded.contains(&root_key("/tmp/default.sock", "w1")) && folded.contains(&root_key("/tmp/menqal.sock", "w2")));
        assert_eq!(folded.len(), 2);
        assert!(!migrate_folds(&mut folded, "/tmp/default.sock"), "once");
    }

    #[test]
    fn a_goal_is_dropped_when_another_session_is_switched_to() {
        let mut t = TreeState { pending_goal: Some((key("menqal"), Goal::Workspace("w1".into()), Instant::now())), ..Default::default() };
        t.switching_to(&key("menqal"));
        assert!(t.pending_goal.is_some(), "its own switch keeps it");
        t.switching_to(&key("namazee"));
        assert!(t.pending_goal.is_none());
    }

    #[test]
    fn the_branch_comes_from_head() {
        let root = std::env::temp_dir().join(format!("shepherd-head-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo/.git/worktrees/fix")).unwrap();
        std::fs::create_dir_all(root.join("repo/src")).unwrap();
        std::fs::create_dir_all(root.join("fix")).unwrap();
        std::fs::write(root.join("repo/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join("repo/.git/worktrees/fix/HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        std::fs::write(root.join("fix/.git"), format!("gitdir: {}\n", root.join("repo/.git/worktrees/fix").display())).unwrap();
        assert_eq!(head_branch(&root.join("repo/src")).as_deref(), Some("main"), "from a folder inside");
        assert_eq!(head_branch(&root.join("fix")).as_deref(), Some("0123456"), "a linked worktree, detached");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_longest_waiting_agent_is_picked() {
        let s = snapshot(&[("w1", None)]);
        let mut since = crate::model::Since::new();
        assert_eq!(longest_waiting(&s, &since), Some(("t1".into(), "p1".into())));
        since.insert("p1".into(), (crate::model::AgentStatus::Blocked, Instant::now()));
        assert_eq!(longest_waiting(&s, &since), Some(("t1".into(), "p1".into())));
        assert_eq!(waiting(&s), 1);
    }

    #[test]
    fn a_click_folds_an_open_session_and_opens_a_folded_one() {
        let mut collapsed = vec!["/tmp/a.sock".to_string()];
        toggle_in(&mut collapsed, "/tmp/b.sock");
        assert_eq!(collapsed, ["/tmp/a.sock", "/tmp/b.sock"]);
        toggle_in(&mut collapsed, "/tmp/a.sock");
        assert_eq!(collapsed, ["/tmp/b.sock"]);
        toggle_in(&mut collapsed, "/tmp/b.sock");
        assert!(collapsed.is_empty());
    }

    #[test]
    fn dividers_part_the_session_groups() {
        let session = |n: &str| TreeRow::Session { key: key(n), collapsed: false, waiting: 0, status: Status::Live };
        let machine = TreeRow::Machine { id: "you@box".into(), label: "you@box".into(), status: Status::Live, enabled: true };
        let rows = vec![session("a"), TreeRow::Workspaces { key: key("a") }, session("b"), machine, session("c")];
        let at: Vec<usize> = (0..rows.len()).filter(|i| divider_above(&rows, *i)).collect();
        assert_eq!(at, [2, 3], "not over the first, not under a machine's header");
    }

    fn agents_snapshot(panes: &[(&str, &str)]) -> Snapshot {
        let panes: Vec<_> = panes
            .iter()
            .map(|(id, status)| json!({"pane_id": id, "tab_id": format!("t-{id}"), "workspace_id": "w1", "terminal_id": id, "agent_status": status, "agent": "claude"}))
            .collect();
        let ws = json!([{"workspace_id": "w1", "label": "web", "number": 1, "active_tab_id": null, "agent_status": "idle", "worktree": null}]);
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": ws, "tabs": [], "panes": panes, "layouts": []}})).unwrap()
    }

    #[test]
    fn the_agents_of_every_session_are_listed_in_one_order() {
        let shown = agents_snapshot(&[("p1", "working"), ("p2", "done")]);
        let other = agents_snapshot(&[("q1", "blocked"), ("q2", "idle")]);
        let t0 = Instant::now();
        let mut since_shown = crate::model::Since::new();
        since_shown.insert("p2".into(), (AgentStatus::Done, t0 + Duration::from_secs(240)));
        let mut since_other = crate::model::Since::new();
        since_other.insert("q1".into(), (AgentStatus::Blocked, t0));
        let now = t0 + Duration::from_secs(300);
        let sessions = [
            SessionAgents { key: key("default"), name: "default".into(), shown: true, snapshot: &shown, since: &since_shown },
            SessionAgents { key: key("menqal"), name: "menqal".into(), shown: false, snapshot: &other, since: &since_other },
        ];
        let ids = |e: &[AgentEntry]| e.iter().map(|e| e.pane.pane_id.clone()).collect::<Vec<_>>();

        let (all, any) = cross_agents(&sessions, AgentSort::Status, false, now);
        assert!(any);
        assert_eq!(ids(&all), ["q1", "p2", "p1", "q2"], "what needs you first, whichever session");
        assert_eq!(all[0].workspace, "menqal · web");
        assert_eq!((all[0].key.clone(), all[0].shown, all[0].n), (key("menqal"), false, 1));

        let (attention, any) = cross_agents(&sessions, AgentSort::Status, true, now);
        assert!(any);
        assert_eq!(ids(&attention), ["q1", "p2"]);

        // each session's wait is timed by its own `since`
        let (waiting, _) = cross_agents(&sessions, AgentSort::Waiting, false, now);
        assert_eq!(ids(&waiting)[..2], ["q1", "p2"]);
        assert_eq!(waiting[0].waited.as_deref(), Some("5m"));
        assert_eq!(waiting[1].waited.as_deref(), Some("1m"));

        let (grouped, _) = cross_agents(&sessions, AgentSort::Grouped, false, now);
        assert!(grouped[..2].iter().all(|e| e.key == key("default")), "each session's together, in the tree's order");

        // one session: its workspaces alone name the rows
        let (one, _) = cross_agents(&sessions[..1], AgentSort::Status, false, now);
        assert_eq!(one[0].workspace, "web");
        let (none, any) = cross_agents(&[], AgentSort::Status, true, now);
        assert!(none.is_empty() && !any);
    }
}
