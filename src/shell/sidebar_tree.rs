//! The sidebar's tree: every machine, its sessions, and each session's workspaces, all at once. A level with one
//! entry hides (one machine: no machine row; one machine with one session: no session rows either, as before the
//! tree). The shown session's workspace rows are its live ones; another session's come from its watcher's last
//! snapshot, and a click there switches to it (`go_to`). A session row is a header over its indented workspaces: a
//! click on it folds them, and its menu switches there. The session and machine menus start, stop and restart
//! sessions and turn machines on and off.
use super::sessions::Place;
use super::watchers::{Kind, SessionKey, Status, Watchers};
use super::*;
use crate::herdr::{Machine, SessionInfo};
use gpui::DragMoveEvent;
use std::collections::HashSet;
use tree_drag::{DraggedTreeItem, Item, Over};
use super::sidebar_focus::{Kept, TreeView};

/// What a session's workspace rows show and do: in focus mode the workspaces it keeps (`None` without it), and
/// whether they drag (not in focus mode, nor another place's preview).
#[derive(Clone, Copy)]
pub(super) struct RowFilter<'a> {
    pub keep: Option<&'a HashSet<String>>,
    pub drags: bool,
}

impl RowFilter<'_> {
    /// Whether a row may drag or take a drop that it otherwise would (`ok`).
    pub(super) fn allows(self, ok: bool) -> bool {
        self.drags && ok
    }
}

/// A faint line where the tree has nothing to list.
fn no_match(theme: &Theme, text: &'static str) -> gpui::AnyElement {
    div().flex_none().px(px(8.)).py(px(6.)).text_size(px(11.)).text_color(theme.text_faint).child(text).into_any_element()
}

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
pub(super) const LOCAL: &str = "local";

/// How long a goal waits for the session switched to (a stopped one being started) to list its workspace.
const GOAL_EXPIRY: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TreeRow {
    /// A machine: this computer, or an SSH target with its saved sessions. Disabled machines are listed greyed. A
    /// folded one (`collapsed`) hides its sessions and counts their agents waiting or done (`waiting`).
    Machine { id: String, label: String, status: Status, enabled: bool, collapsed: bool, waiting: usize },
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

/// Where a click in the tree (or a row of the agents and workspaces panels) goes once its session is shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Goal {
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
    /// Sessions on this host…: an SSH host's sessions not saved as machines, to add one.
    pub(super) discover: Option<crate::ui::Popup<super::discover::Discover>>,
    /// The sessions being added as machines from that popup, kept while it closes and opens again.
    pub(super) adds: super::discover::Adds,
    /// Where a row being dragged would land.
    pub(super) drop: Option<tree_drag::TreeDrop>,
    /// The tree's view while a row is dragged: rows scrolled out of it don't take a drop.
    pub(super) view: Option<Bounds<Pixels>>,
    /// Each session's drops that herdr hasn't carried out yet, sent one at a time and shown meanwhile.
    pub(super) pending: HashMap<SessionKey, tree_drag::Pending>,
    /// The tree's scroll, which a drag near its edge moves.
    pub(super) scroll: gpui::ScrollHandle,
    /// How fast that drag scrolls it, in px a frame; 0 when it doesn't.
    pub(super) scroll_speed: f32,
    /// The scroll ticker (`scroll_while_dragging`): one at a time.
    pub(super) ticker: tree_drag::Ticker,
    /// The tree's scroll offset its rows were last painted at: a drag move's row bounds are from that paint.
    pub(super) painted_offset: Pixels,
    /// What is being dragged, for a drop planned again after a scroll.
    pub(super) dragged: Option<DraggedTreeItem>,
    /// Where the drag's pointer last moved over the tree: a scroll since must leave the drop's row under it.
    pub(super) pointer: Option<gpui::Point<Pixels>>,
    /// A drag was under way at the last frame: its end lets a peek it held out go.
    pub(super) was_dragging: bool,
    /// herdr's list of machines was read the last time: when not, a drop keeps every machine's saved place.
    pub(super) machines_known: bool,
}

/// A row's dot in a box as wide as a workspace row's status, so the dots of machine, session and workspace rows, all
/// at their row's right padding, make one column.
fn status_column(color: Hsla, selector: &'static str) -> Div {
    div().debug_selector(move || selector.into()).size(px(12.)).flex_none().flex().items_center().justify_center().child(div().size(px(6.)).rounded_full().bg(color))
}

/// A machine's header as drawn, before its clicks, drag and menu: the chevron (a machine turned off has none), its
/// icon, its name in small caps (OFF when disabled), the ●N `badge` when folded, then the status dot.
fn machine_header(theme: &Theme, id: &str, label: &str, enabled: bool, dot: Hsla, chevron: Option<gpui::AnyElement>, badge: Option<gpui::AnyElement>) -> Stateful<Div> {
    let ink = if enabled { theme.text_muted } else { theme.text_faint.opacity(0.6) };
    let icon = if id == LOCAL { "icons/monitor.svg" } else { "icons/remote-server.svg" };
    div()
        .id(SharedString::from(format!("tree-machine-{id}")))
        .h(px(26.))
        .pl(px(if chevron.is_some() { 2. } else { 8. }))
        .pr(px(8.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(ink)
        .children(chevron)
        .child(gpui::svg().path(icon).size(px(13.)).flex_none().text_color(ink))
        .child(div().flex_1().min_w_0().truncate().child(if enabled { label.to_uppercase() } else { format!("{} · OFF", label.to_uppercase()) }))
        .children(badge)
        .child(status_column(dot, "tree-machine-dot"))
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

    /// A tree menu or the sessions popup is up.
    pub(super) fn menu_open(&self) -> bool {
        self.machine_menu.as_ref().is_some_and(|m| m.is_open()) || self.add_menu_open() || self.discover_open()
    }

    pub(super) fn discover_open(&self) -> bool {
        self.discover.as_ref().is_some_and(|d| d.is_open())
    }

    pub(super) fn add_menu_open(&self) -> bool {
        self.add_menu.as_ref().is_some_and(|m| m.is_open())
    }
}

/// The machine a place runs on: this computer, or its SSH target.
pub(super) fn machine_of(place: &Place) -> &str {
    match place {
        Place::Session(_) => LOCAL,
        Place::Machine(m) => &m.target,
    }
}

/// herdr's sessions and machines in the tree's order: this computer's sessions first, its default one leading, then
/// each SSH target's enabled machines together, in the order herdr lists them, unless dragged elsewhere
/// (`session_order`, `machine_order`). The disabled machines go apart.
pub(super) fn tree_order(mut sessions: Vec<SessionInfo>, machines: Vec<Machine>, session_order: &[String], machine_order: &[String]) -> (Vec<Place>, Vec<Machine>) {
    sessions.sort_by_key(|s| !s.default); // stable: herdr's order otherwise
    let (enabled, disabled): (Vec<Machine>, Vec<Machine>) = machines.into_iter().partition(|m| m.enabled);
    let mut targets: Vec<&str> = Vec::new();
    for m in &enabled {
        if !targets.contains(&m.target.as_str()) {
            targets.push(&m.target);
        }
    }
    let grouped: Vec<Place> = targets.iter().flat_map(|t| enabled.iter().filter(move |m| m.target == *t).cloned().map(Place::Machine)).collect();
    let places = sessions.into_iter().map(Place::Session).chain(grouped).collect();
    (tree_drag::order_places(places, session_order, machine_order), disabled)
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

/// The snapshot the tree reads the session `key` by: the shown one's live (`shown`), another's its watcher's last,
/// one out of reach included (a stopped one's watcher holds none).
pub(super) fn tree_snapshot<'a>(w: &'a Watchers, key: &SessionKey, shown: Option<&'a Snapshot>) -> Option<&'a Snapshot> {
    match w.get(key) {
        Some(watcher) if watcher.status == Status::Shown => shown,
        Some(watcher) => watcher.snapshot.as_ref(),
        None => None,
    }
}

/// A session's ●N: its agents waiting or done in the snapshot the tree reads it by. Its row in the tree and the
/// session switcher count with this one rule.
pub(super) fn session_waiting(w: &Watchers, key: &SessionKey, shown: Option<&Snapshot>) -> usize {
    tree_snapshot(w, key, shown).map_or(0, waiting)
}

/// Every agent waiting or done that the Dock counts: the shown session's live
/// snapshot and every other's watcher's, but for those out of reach, whose last word may be long over (the tree's ●N
/// keeps them).
pub(super) fn waiting_everywhere(shown: Option<&Snapshot>, w: &Watchers) -> usize {
    super::attention::badge(shown.into_iter().chain(w.snapshots()))
}

/// Whether the session `key` is out of reach: what it shows is the last snapshot it sent.
pub(super) fn out_of_reach(w: &Watchers, key: &SessionKey) -> bool {
    w.get(key).is_some_and(|w| w.status == Status::Unreachable)
}

/// The workspaces panel's sessions: each one's snapshot in `ordered`'s order where a drop herdr hasn't carried out
/// gives one (as the tree shows it), one out of reach marked so.
pub(super) fn panel_workspaces<'a>(sessions: &'a [SessionAgents], ordered: &'a [Option<Snapshot>], w: &Watchers) -> Vec<crate::ui::command_panel::SessionWorkspaces<'a>> {
    sessions
        .iter()
        .zip(ordered)
        .map(|(s, ordered)| crate::ui::command_panel::SessionWorkspaces {
            key: s.key.0.clone(),
            name: s.name.clone(),
            snapshot: ordered.as_ref().unwrap_or(s.snapshot),
            out_of_reach: out_of_reach(w, &s.key),
        })
        .collect()
}

/// The tree's rows as the sidebar draws them: [`tree`]'s (every session with its header when `headed`, a lone one
/// too: focus mode), and a socket of one's own (no session of herdr's list), or the session shown before the list
/// came, shows its workspaces first, without a header.
pub(super) fn model_rows(places: &[Place], w: &Watchers, collapsed: &HashSet<String>, snapshot: Option<&Snapshot>, shown: &SessionKey, headed: bool) -> Vec<TreeRow> {
    let mut rows = tree(places, w, collapsed, snapshot, headed);
    if !places.iter().any(|p| SessionKey::of(p) == *shown) {
        rows.insert(0, TreeRow::Workspaces { key: shown.clone() });
    }
    rows
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

/// Whether Fold or unfold the session has a session to fold: only one the tree gives a header row. One session
/// alone, or a socket of one's own outside herdr's list, has none, and folding it would hide its workspaces for good.
pub(super) fn foldable(rows: &[TreeRow], shown: &SessionKey) -> bool {
    rows.iter().any(|r| matches!(r, TreeRow::Session { key, .. } if key == shown))
}

/// Whether a header (a session or machine row) gets a divider over it: every one but the first, and none right
/// under its machine's row, which heads the group already.
pub(super) fn divider_above(rows: &[TreeRow], i: usize) -> bool {
    i > 0 && matches!(rows[i], TreeRow::Session { .. } | TreeRow::Machine { .. }) && !matches!(rows[i - 1], TreeRow::Machine { .. })
}

/// The sessions whose agents are read, in order: the shown one first when herdr's list lacks it (a socket of one's
/// own leads, as its workspaces do), then the list's, in the tree's order.
pub(super) fn agent_order(shown: Option<&SessionKey>, listed: &[SessionKey]) -> Vec<SessionKey> {
    let own = shown.filter(|k| !listed.contains(k)).cloned();
    own.into_iter().chain(listed.iter().cloned()).collect()
}

/// One session, as the workspaces panel reads it: the shown session's live snapshot, another's from its watcher.
pub(super) struct SessionAgents<'a> {
    pub key: SessionKey,
    pub name: String,
    pub snapshot: &'a Snapshot,
}

/// The key a machine's group is folded by among the tree's folds, beside the sessions' keys (a socket,
/// `machine:<id>`), which it never matches.
pub(super) fn group_key(id: &str) -> String {
    format!("group:{id}")
}

/// The tree's folds as [`tree`] reads them: the sessions' by key, and the machines' groups by [`group_key`].
pub(super) fn saved_folds(s: &crate::settings::Settings) -> HashSet<String> {
    s.collapsed_sessions.iter().cloned().chain(s.collapsed_machines.iter().map(|id| group_key(id))).collect()
}

/// A click on a machine's row: its group folds when it is open, opens when it is folded, in the settings.
pub(super) fn fold_machine(s: &mut crate::settings::Settings, id: &str) {
    toggle_in(&mut s.collapsed_machines, id);
}

/// The machine `id`'s group opens, if it is folded: a session of it is shown now. Whether it was.
pub(super) fn unfold_machine(s: &mut crate::settings::Settings, id: &str) -> bool {
    let before = s.collapsed_machines.len();
    s.collapsed_machines.retain(|m| m != id);
    s.collapsed_machines.len() != before
}

/// What Fold or unfold the session does: the shown session's machine folded (`machine`) opens first, so the session
/// shows; else the session folds or opens, when the tree gives it a header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ShownFold {
    UnfoldMachine(String),
    FoldSession,
    Nothing,
}

pub(super) fn shown_fold(rows: &[TreeRow], shown: &SessionKey, machine: Option<&str>) -> ShownFold {
    match machine {
        Some(m) if rows.iter().any(|r| matches!(r, TreeRow::Machine { id, collapsed: true, .. } if id == m)) => ShownFold::UnfoldMachine(m.to_string()),
        _ if foldable(rows, shown) => ShownFold::FoldSession,
        _ => ShownFold::Nothing,
    }
}

/// A folded machine's ●N is faded while none of its sessions answers: the count is their last word.
pub(super) fn pill_faded(status: Status) -> bool {
    status == Status::Unreachable
}

/// The agent that has waited longest among `sessions` (each its key, snapshot and clocks), else the first that
/// waits: its session, tab and pane. A folded machine's ●N goes there.
pub(super) fn longest_waiting_among<'a>(sessions: impl IntoIterator<Item = (SessionKey, &'a Snapshot, &'a crate::model::Since)>) -> Option<(SessionKey, String, String)> {
    sessions
        .into_iter()
        .flat_map(|(key, s, since)| {
            s.panes
                .iter()
                .filter(|p| p.agent.is_some() && crate::notify::wants_attention(p.agent_status))
                .map(move |p| (key.clone(), crate::model::waiting_since(since, p), p))
        })
        .min_by_key(|(_, at, _)| (at.is_none(), *at))
        .map(|(key, _, p)| (key, p.tab_id.clone(), p.pane_id.clone()))
}

/// The tree's rows for `places` (in [`tree_order`]), their watchers and the folds (`saved_folds`: sessions, and
/// machines' groups); `shown` is the shown session's snapshot, which its watcher does not hold. Every session gets
/// its header when `headed` (focus mode), a lone one too.
pub(super) fn tree(places: &[Place], w: &Watchers, collapsed: &HashSet<String>, shown: Option<&Snapshot>, headed: bool) -> Vec<TreeRow> {
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
    let show_sessions = headed || show_machines || machines.first().is_some_and(|(_, list)| list.len() > 1);
    let mut rows = Vec::new();
    for (id, list) in &machines {
        let statuses: Vec<Status> = list.iter().map(|p| w.get(&SessionKey::of(p)).map_or(Status::Connecting, |w| w.status)).collect();
        let group_folded = show_machines && collapsed.contains(&group_key(id));
        if show_machines {
            let status = if statuses.iter().all(|s| *s == Status::Unreachable) { Status::Unreachable } else { Status::Live };
            let label = if *id == LOCAL { "This computer".to_string() } else { id.to_string() };
            // folded, the group's count stands in for its sessions' own
            let waiting = if group_folded { list.iter().map(|p| session_waiting(w, &SessionKey::of(p), shown)).sum() } else { 0 };
            rows.push(TreeRow::Machine { id: id.to_string(), label, status, enabled: true, collapsed: group_folded, waiting });
        }
        if group_folded {
            continue;
        }
        for (place, status) in list.iter().zip(statuses) {
            let key = SessionKey::of(place);
            let snapshot = tree_snapshot(w, &key, shown);
            let folded = show_sessions && collapsed.contains(&key.0);
            if show_sessions {
                rows.push(TreeRow::Session { key: key.clone(), collapsed: folded, waiting: session_waiting(w, &key, shown), status });
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
        rows.push(TreeRow::Machine { id: target.to_string(), label: target.to_string(), status: Status::Stopped, enabled: false, collapsed: false, waiting: 0 });
    }
    rows
}

/// A session's worktree groups as its rows draw them and Workspace1–9 count them: herdr's, and in focus mode
/// (`keep`) only the workspaces it keeps (a worktree's checkout whenever it keeps the worktree).
pub(super) fn shown_groups<'s>(snapshot: &'s Snapshot, keep: Option<&HashSet<String>>) -> Vec<(&'s crate::model::Workspace, Vec<&'s crate::model::Workspace>)> {
    let shows = |id: &str| keep.is_none_or(|k| k.contains(id));
    crate::model::groups(snapshot.workspace_tree())
        .into_iter()
        .filter(|(w, _)| shows(&w.workspace_id))
        .map(|(w, kids)| (w, kids.into_iter().filter(|k| shows(&k.workspace_id)).collect()))
        .collect()
}

/// The workspaces Workspace1–9 go to: the tree's visible rows from the top, each group's worktrees under it (a
/// group always shows them); in focus mode (`kept`) only those it keeps.
pub(super) fn workspace_order<'a>(rows: &[TreeRow], snaps: impl Fn(&SessionKey) -> Option<&'a Snapshot>, kept: Option<&Kept>) -> Vec<(SessionKey, String)> {
    let mut order = Vec::new();
    for row in rows {
        let TreeRow::Workspaces { key } = row else { continue };
        let Some(snapshot) = snaps(key) else { continue };
        // focus mode keeps nothing of the session: its rows are gone
        let keep = match kept.map(|k| k.get(key)) {
            Some(None) => continue,
            keep => keep.flatten(),
        };
        for (w, kids) in shown_groups(snapshot, keep) {
            order.push((key.clone(), w.workspace_id.clone()));
            order.extend(kids.iter().map(|k| (key.clone(), k.workspace_id.clone())));
        }
    }
    order.truncate(9);
    order
}

/// The row a session or machine drag's line is drawn on, and its side: over the row the line names, or under the
/// last row of its group (a session's workspaces and notice, a machine's sessions).
pub(super) fn line_row(rows: &[TreeRow], line: &tree_drag::Line) -> Option<(usize, tree_drag::Edge)> {
    use tree_drag::{Edge, Line};
    let session_of = |r: &TreeRow| match r {
        TreeRow::Session { key, .. } | TreeRow::Workspaces { key } | TreeRow::Notice { key, .. } => Some(key.clone()),
        TreeRow::Machine { .. } => None,
    };
    let (head, edge) = match line {
        Line::Session(key, edge) => (rows.iter().position(|r| matches!(r, TreeRow::Session { key: k, .. } if k == key))?, *edge),
        Line::Machine(id, edge) => (rows.iter().position(|r| matches!(r, TreeRow::Machine { id: m, .. } if m == id))?, *edge),
        Line::Group(..) | Line::Kid(..) => return None,
    };
    if edge == Edge::Above {
        return Some((head, edge));
    }
    let belongs = |r: &TreeRow| match line {
        Line::Session(key, _) => session_of(r).as_ref() == Some(key),
        _ => !matches!(r, TreeRow::Machine { .. }),
    };
    let last = head + rows[head + 1..].iter().take_while(|r| belongs(r)).count();
    Some((last, edge))
}

/// [`workspace_order`] as the tree shows it: with drops herdr hasn't carried out yet (`pending`) already counted.
pub(super) fn numbered<'a>(
    rows: &[TreeRow],
    snaps: impl Fn(&SessionKey) -> Option<&'a Snapshot>,
    pending: &HashMap<SessionKey, tree_drag::Pending>,
    now: Instant,
    kept: Option<&Kept>,
) -> Vec<(SessionKey, String)> {
    let ordered: HashMap<SessionKey, Snapshot> = pending
        .iter()
        .filter_map(|(k, p)| {
            let s = snaps(k)?;
            let list: Vec<String> = s.workspaces.iter().map(|w| w.workspace_id.clone()).collect();
            Some((k.clone(), tree_drag::with_order(s, p.order(&list, now)?)))
        })
        .collect();
    workspace_order(rows, |k| ordered.get(k).or_else(|| snaps(k)), kept)
}

impl Shell {
    /// The workspaces Workspace1–9 go to and their hint chips sit on, in the order of the tree shown (`shown_tree`).
    pub(super) fn numbered_workspaces(&self, rows: &[TreeRow], shown: &SessionKey, kept: Option<&Kept>) -> Vec<(SessionKey, String)> {
        let snap = |k: &SessionKey| if k == shown { self.snapshot.as_ref() } else { self.watchers.snapshot(k) };
        numbered(rows, snap, &self.tree.pending, Instant::now(), kept)
    }

    /// Workspace1–9: the `index`th workspace the tree shows, in whichever session.
    pub(super) fn go_to_tree_workspace(&mut self, index: usize, cx: &mut Context<Self>) {
        let (view, shown) = self.shown_tree(cx);
        if let Some((key, ws)) = self.numbered_workspaces(&view.rows, &shown, view.kept.as_ref()).into_iter().nth(index) {
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

    /// The shown session sent a snapshot of its own: a goal waiting for it is shown.
    pub(super) fn after_snapshot(&mut self, cx: &mut Context<Self>) {
        // before a goal: the panes a view brought back that this snapshot no longer backs go first
        self.check_parked(cx);
        let shown = self.shown_key();
        let Some((key, goal, at)) = self.tree.pending_goal.clone() else { return };
        // given up on after a while, else shown once the session lists it
        if at.elapsed() > GOAL_EXPIRY || (shown.as_ref() == Some(&key) && self.apply_goal(&goal, cx)) {
            self.tree.pending_goal = None;
        }
    }

    fn toggle_collapsed(&mut self, key: &SessionKey, cx: &mut Context<Self>) {
        let mut collapsed = SettingsGlobal::of(cx).settings.collapsed_sessions.clone();
        toggle_in(&mut collapsed, &key.0);
        // a drop aimed mid drag: the rows under it fold away or come in
        self.tree.drop = None;
        save_from_sidebar(cx, move |s| s.collapsed_sessions = collapsed);
        cx.notify();
    }

    /// A click on a machine's row folds or unfolds its group, as a session's row folds its workspaces.
    fn toggle_machine_fold(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut settings = SettingsGlobal::of(cx).settings.clone();
        fold_machine(&mut settings, id);
        // a drop aimed mid drag: the rows under it fold away or come in
        self.tree.drop = None;
        let folded = settings.collapsed_machines;
        save_from_sidebar(cx, move |s| s.collapsed_machines = folded);
        cx.notify();
    }

    /// A session of the machine `id` is shown: its group opens if it was folded, so the session is not hidden.
    pub(super) fn reveal_machine(&mut self, id: &str, cx: &mut Context<Self>) {
        if SettingsGlobal::of(cx).settings.collapsed_machines.iter().any(|m| m == id) {
            let id = id.to_string();
            save_from_sidebar(cx, move |s| {
                unfold_machine(s, &id);
            });
            cx.notify();
        }
    }

    /// Fold or unfold the session: the shown one's header, as a click on it does, whether the sidebar shows, peeks
    /// or is hidden (it is not brought out); nothing when the tree gives it no header.
    pub(super) fn toggle_shown_session(&mut self, cx: &mut Context<Self>) {
        let (view, shown) = self.shown_tree(cx);
        let machine = self.places.iter().find(|p| SessionKey::of(p) == shown).map(|p| machine_of(p).to_string());
        match shown_fold(&view.rows, &shown, machine.as_deref()) {
            ShownFold::UnfoldMachine(m) => self.reveal_machine(&m, cx),
            ShownFold::FoldSession => self.toggle_collapsed(&shown, cx),
            ShownFold::Nothing => {}
        }
    }

    /// [`waiting_everywhere`] now: the Dock's count.
    pub(super) fn waiting_everywhere(&self) -> usize {
        waiting_everywhere(self.snapshot.as_ref(), &self.watchers)
    }


    /// Every session's snapshot as the workspaces panel reads them, in the tree's order: a socket of one's own first,
    /// the shown session's live snapshot, the others' from their watchers (a stopped one's left out).
    pub(super) fn session_agents(&self, cx: &App) -> Vec<SessionAgents<'_>> {
        let shown = self.shown_key();
        let listed: Vec<SessionKey> = self.places.iter().map(SessionKey::of).collect();
        let mut sessions: Vec<SessionAgents> = Vec::new();
        for key in agent_order(shown.as_ref(), &listed) {
            let place = self.places.iter().find(|p| SessionKey::of(p) == key);
            if shown.as_ref() == Some(&key) {
                if let Some(snapshot) = self.snapshot.as_ref() {
                    let name = place.map_or_else(|| self.place_label(cx), |p| p.look(cx).0);
                    sessions.push(SessionAgents { key, name, snapshot });
                }
            } else if let Some(place) = place
                && let Some(w) = self.watchers.get(&key).filter(|w| w.status != Status::Stopped)
                && let Some(snapshot) = w.snapshot.as_ref()
            {
                sessions.push(SessionAgents { key, name: place.look(cx).0, snapshot });
            }
        }
        sessions
    }

    /// The tree's elements, and whether it lists sessions (then the header leaves out the shown one's name).
    pub(super) fn tree_rows(&self, theme: &Theme, snapshot: &Option<Snapshot>, shown_ws: &Option<String>, active: bool, cx: &mut Context<Self>) -> (Vec<gpui::AnyElement>, bool) {
        let (TreeView { rows, drags, kept, empty }, shown) = self.shown_tree(cx);
        let listed = rows.iter().any(|r| matches!(r, TreeRow::Session { .. }));
        // held Ctrl+Shift (Ctrl on macOS): each of the first nine visible workspaces' chord
        let hints: Option<Chords> = self.hints_of(hints::HintMode::Workspaces).map(|opacity| {
            let order = self.numbered_workspaces(&rows, &shown, kept.as_ref());
            (opacity, order.into_iter().zip(WORKSPACE_ACTIONS).filter_map(|((k, w), a)| Some((k, w, chord_of(cx, a)?))).collect())
        });
        let mut out = Vec::new();
        // a session or machine drag's line: over its row, or under the last row of its group
        let line = self.tree_line().and_then(|l| line_row(&rows, l));
        for (i, row) in rows.iter().enumerate() {
            // a divider and 8 px or more between session groups
            if divider_above(&rows, i) {
                out.push(div().flex_none().mx(px(6.)).mt(px(6.)).mb(px(4.)).h(px(1.)).bg(theme.border.opacity(0.6)).into_any_element());
            }
            let edge = line.filter(|(at, _)| *at == i).map(|(_, edge)| edge);
            let start = out.len();
            match row.clone() {
                TreeRow::Machine { id, label, status, enabled, collapsed, waiting } => out.push(self.machine_row(theme, id, label, status, enabled, collapsed, waiting, drags, cx)),
                TreeRow::Session { key, collapsed, waiting, status } => {
                    if let Some(place) = self.places.iter().find(|p| SessionKey::of(p) == key).cloned() {
                        out.push(self.session_row(theme, place, key, collapsed, waiting, status, drags, cx));
                    }
                }
                TreeRow::Notice { key, text, action } => out.push(self.notice_row(theme, key, text, action, cx)),
                TreeRow::Workspaces { key } => {
                    // focus mode: only what it keeps of the session
                    let filter = RowFilter { keep: kept.as_ref().and_then(|k| k.get(&key)), drags };
                    let ws = if key == shown {
                        self.workspace_rows(theme, snapshot.as_ref(), shown_ws, RowMode::Live, hints.as_ref(), active, filter, cx)
                    } else {
                        let other = self.watchers.snapshot(&key);
                        self.workspace_rows(theme, other, &None, RowMode::Other(key), hints.as_ref(), active, filter, cx)
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
            if let Some(edge) = edge.filter(|_| out.len() == start + 1) {
                let el = out.pop().expect("one row pushed");
                out.push(tree_drag::with_line(el, theme, Some(edge)));
            }
        }
        out.extend(empty.map(|text| no_match(theme, text)));
        (out, listed)
    }

    /// A machine's header: a chevron, its icon, its name in small caps, folded its ●N, a dot for whether it answers;
    /// a click anywhere folds its sessions, a right-click opens its menu. `drags`: it is dragged and dropped on.
    #[allow(clippy::too_many_arguments)]
    fn machine_row(&self, theme: &Theme, id: String, label: String, status: Status, enabled: bool, collapsed: bool, waiting: usize, drags: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hover_bg = theme.hover;
        let menu_id = id.clone();
        let dot = match status {
            _ if !enabled => theme.text_faint.opacity(0.4),
            Status::Unreachable => theme.danger,
            Status::Stopped => theme.text_faint.opacity(0.6),
            _ => theme.success,
        };
        // an SSH machine's group moves among the others; this computer's stays first
        let mut targets: Vec<&str> = self.places.iter().map(machine_of).filter(|m| *m != LOCAL).collect();
        targets.dedup();
        let movable = drags && enabled && id != LOCAL && targets.len() > 1;
        let dragged = DraggedTreeItem { from: SessionKey::default(), item: Item::Machine(id.clone()), label: label.clone(), theme: theme.clone() };
        let over = id.clone();
        // a machine turned off lists no sessions to fold
        let folds = enabled.then(|| id.clone());
        let chevron = folds.is_some().then(|| fold_chevron(theme, &format!("machine-{id}"), !collapsed, 1., None).into_any_element());
        // folded: its sessions' agents waiting or done; a click goes to the one that has waited longest
        let badge = (waiting > 0).then(|| {
            let target = id.clone();
            waiting_pill(theme, waiting)
                .when(pill_faded(status), |el| el.opacity(0.5))
                .id(SharedString::from(format!("tree-machine-waiting-{id}")))
                .cursor_pointer()
                .tooltip(tip("Go to the agent that has waited longest", None))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.go_to_longest_waiting_on(&target, cx);
                }))
                .into_any_element()
        });
        machine_header(theme, &id, &label, enabled, dot, chevron, badge)
            .when_some(folds, |el, id| {
                el.cursor_pointer().hover(move |s| s.bg(hover_bg)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_machine_fold(&id, cx)))
            })
            .when(movable, |el| el.on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone())))
            .when(drags, |el| el.on_drag_move(cx.listener(move |this, e: &DragMoveEvent<DraggedTreeItem>, _, cx| this.tree_drag_over(Over::Machine(over.clone()), e, cx))))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                this.tree.machine_menu = Some(crate::ui::Popup::new((menu_id.clone(), e.position)));
                cx.notify();
            }))
            .into_any_element()
    }

    /// A folded machine's ●N: the agent that has waited longest in any of its sessions.
    fn go_to_longest_waiting_on(&mut self, machine: &str, cx: &mut Context<Self>) {
        let shown = self.shown_key();
        let sessions = self.places.iter().filter(|p| machine_of(p) == machine).filter_map(|p| {
            let key = SessionKey::of(p);
            if shown.as_ref() == Some(&key) {
                Some((key, self.snapshot.as_ref()?, &self.since))
            } else {
                let w = self.watchers.get(&key)?;
                Some((key, w.snapshot.as_ref()?, &w.since))
            }
        });
        if let Some((key, tab, pane)) = longest_waiting_among(sessions) {
            self.go_to(key, Goal::Pane { tab, pane }, cx);
        }
    }

    /// A session's header; `drags` as [`Self::machine_row`]'s.
    #[allow(clippy::too_many_arguments)]
    fn session_row(&self, theme: &Theme, place: Place, key: SessionKey, collapsed: bool, waiting: usize, status: Status, drags: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let idx = self.watchers.index_of(&key).unwrap_or(0);
        let stale = self.watchers.get(&key).is_some_and(|w| w.stale(Instant::now()));
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
            waiting_pill(theme, waiting)
                .id(("tree-waiting", idx))
                .cursor_pointer()
                .tooltip(tip("Go to the agent that has waited longest", None))
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
        // a session moves among its machine's others
        let movable = drags && self.places.iter().filter(|p| machine_of(p) == machine_of(&place)).count() > 1;
        let dragged = DraggedTreeItem { from: key.clone(), item: Item::Session(key.clone()), label: name.clone(), theme: theme.clone() };
        let over = key.clone();
        let chevron = fold_chevron(theme, &format!("session-{idx}"), !collapsed, 1., None);
        // the dot in a box as wide as a workspace row's status, at the row's right padding as theirs are: one column
        let dot = status_column(dot, "tree-session-dot");
        // a header: the chevron, the session's icon and its name in bold; a click anywhere folds its workspaces, a
        // right-click opens its menu
        row(theme, false, ("tree-session", idx))
            .h(px(28.))
            .pl(px(2.))
            .gap(px(6.))
            .text_size(px(12.5))
            .text_color(ink)
            .font_weight(if shown { gpui::FontWeight::BOLD } else { gpui::FontWeight::SEMIBOLD })
            .child(chevron)
            .child(mark)
            .child(div().flex_1().min_w_0().truncate().child(name))
            .children(badge)
            .child(dot)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_collapsed(&toggle_key, cx)))
            .when(movable, |el| el.on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone())))
            .when(drags, |el| el.on_drag_move(cx.listener(move |this, e: &DragMoveEvent<DraggedTreeItem>, _, cx| this.tree_drag_over(Over::Session(over.clone()), e, cx))))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| this.open_place_menu(place.clone(), e.position, cx)))
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
    pub(super) fn machine_records(&self, target: &str) -> Vec<Machine> {
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
                this.open_add_machine(None, false, window, cx);
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
            let host = target.clone();
            card = card.child(item("machine-menu-sessions", "Sessions on this host…").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                close(this);
                this.open_discover(host.clone(), cx);
            })));
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
        self.reap_discover(window);
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
        crate::ui::statusbar::cached(dir, crate::power::stretch(HEAD_TTL), read_head, cx).map(|Head(b)| b)
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
            &[],
            &[],
        );
        let keys: Vec<String> = places.iter().map(|p| p.key()).collect();
        assert_eq!(keys, ["/tmp/default.sock", "/tmp/menqal.sock", "machine:a", "machine:c", "machine:b"]);
        assert_eq!(off.len(), 1);
    }

    #[test]
    fn dragged_orders_lead_with_this_computer_first() {
        let machines = vec![machine("a", "you@box", true), machine("b", "you@other", true), machine("c", "you@box", true)];
        let (places, _) = tree_order(vec![info("default", true), info("menqal", true)], machines, &["/tmp/menqal.sock".into(), "machine:c".into()], &["you@other".into()]);
        let keys: Vec<String> = places.iter().map(|p| p.key()).collect();
        assert_eq!(keys, ["/tmp/menqal.sock", "/tmp/default.sock", "machine:b", "machine:c", "machine:a"]);
        // the machine rows, and the numbering across them, follow
        let mut w = watched(&places, "menqal");
        let now = Instant::now();
        for k in ["machine:a", "machine:b", "machine:c"] {
            w.set_client(&SessionKey(k.into()), Client::at("/tmp/x".into()));
        }
        w.due(now, super::super::watchers::Activity::Active);
        for k in ["machine:a", "machine:b", "machine:c"] {
            w.apply(&SessionKey(k.into()), Ok(snapshot(&[("w1", None)])), now);
        }
        let shown = snapshot(&[("w1", None)]);
        let rows = tree(&places, &w, &HashSet::new(), Some(&shown), false);
        let machine_rows: Vec<String> = kinds(&rows).into_iter().filter(|r| r.starts_with("M ")).collect();
        assert_eq!(machine_rows, ["M This computer", "M you@other", "M you@box"]);
        let snaps = |k: &SessionKey| if *k == key("menqal") { Some(&shown) } else { w.snapshot(k) };
        let order: Vec<String> = workspace_order(&rows, snaps, None).into_iter().map(|(k, _)| k.0).collect();
        assert_eq!(order, ["/tmp/menqal.sock", "machine:b", "machine:c", "machine:a"]);
    }

    #[test]
    fn a_drag_line_goes_over_its_row_or_under_its_group() {
        use super::super::tree_drag::{Edge, Line};
        let session = |n: &str| TreeRow::Session { key: key(n), collapsed: false, waiting: 0, status: Status::Live };
        let machine = |id: &str| TreeRow::Machine { id: id.into(), label: id.into(), status: Status::Live, enabled: true, collapsed: false, waiting: 0 };
        let notice = TreeRow::Notice { key: key("b"), text: "Stopped".into(), action: None };
        let rows = vec![machine(LOCAL), session("a"), TreeRow::Workspaces { key: key("a") }, session("b"), notice, machine("you@box"), session("c")];
        assert_eq!(line_row(&rows, &Line::Session(key("a"), Edge::Above)), Some((1, Edge::Above)));
        assert_eq!(line_row(&rows, &Line::Session(key("a"), Edge::Below)), Some((2, Edge::Below)), "under its workspaces");
        assert_eq!(line_row(&rows, &Line::Session(key("b"), Edge::Below)), Some((4, Edge::Below)), "under its notice");
        assert_eq!(line_row(&rows, &Line::Machine(LOCAL.into(), Edge::Below)), Some((4, Edge::Below)), "under its last session");
        assert_eq!(line_row(&rows, &Line::Machine("you@box".into(), Edge::Below)), Some((6, Edge::Below)));
        assert_eq!(line_row(&rows, &Line::Machine("you@box".into(), Edge::Above)), Some((5, Edge::Above)));
        assert_eq!(line_row(&rows, &Line::Session(key("gone"), Edge::Above)), None);
        assert_eq!(line_row(&rows, &Line::Group("w1".into(), Edge::Above)), None, "drawn by the workspace rows");
    }

    #[test]
    fn one_machine_with_one_session_looks_as_before() {
        let places = [Place::Session(info("default", true))];
        let rows = tree(&places, &watched(&places, "default"), &HashSet::new(), None, false);
        assert_eq!(kinds(&rows), ["W /tmp/default.sock"]);
    }

    #[test]
    fn one_machine_lists_its_sessions_without_a_machine_row() {
        let places = [Place::Session(info("default", true)), Place::Session(info("menqal", true)), Place::Session(info("old", false))];
        let shown = snapshot(&[("w1", None)]);
        let rows = tree(&places, &watched(&places, "default"), &HashSet::new(), Some(&shown), false);
        assert_eq!(kinds(&rows), ["S /tmp/default.sock", "W /tmp/default.sock", "S /tmp/menqal.sock", "S /tmp/old.sock", "N Stopped"]);
        assert!(matches!(rows[0], TreeRow::Session { waiting: 1, status: Status::Shown, .. }));
        assert!(matches!(rows.last(), Some(TreeRow::Notice { action: Some(NoticeAction::Start), .. })));
    }

    #[test]
    fn machines_get_rows_and_collapsed_sessions_hide_their_workspaces() {
        let (places, off) = tree_order(vec![info("default", true), info("menqal", true)], vec![machine("a", "you@box", true), machine("d", "you@gone", false)], &[], &[]);
        let mut w = watched(&places, "default");
        w.disabled = off;
        let collapsed: HashSet<String> = ["/tmp/default.sock".to_string()].into();
        let rows = tree(&places, &w, &collapsed, Some(&snapshot(&[("w1", None)])), false);
        assert_eq!(kinds(&rows), ["M This computer", "S /tmp/default.sock", "S /tmp/menqal.sock", "M you@box", "S machine:a", "M you@gone"]);
        assert!(matches!(rows[1], TreeRow::Session { collapsed: true, .. }));
        assert!(matches!(rows.last(), Some(TreeRow::Machine { enabled: false, .. })));
    }

    #[test]
    fn an_unreachable_machine_says_so_and_keeps_its_last_workspaces() {
        let (places, _) = tree_order(vec![info("default", true)], vec![machine("a", "you@box", true)], &[], &[]);
        let mut w = watched(&places, "default");
        let k = SessionKey("machine:a".into());
        let now = Instant::now();
        w.set_client(&k, Client::at("/tmp/x".into()));
        w.due(now, super::super::watchers::Activity::Active);
        w.apply(&k, Ok(snapshot(&[("w1", None)])), now);
        w.apply(&k, Err(anyhow::anyhow!("timed out")), now);
        w.apply(&k, Err(anyhow::anyhow!("timed out")), now);
        let rows = tree(&places, &w, &HashSet::new(), None, false);
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
        let order = workspace_order(&rows, snaps, None);
        let ids: Vec<String> = order.iter().map(|(k, w)| format!("{}:{w}", &k.0[5..k.0.len() - 5])).collect();
        assert_eq!(ids, ["default:w1", "default:w2", "default:w3", "menqal:w1", "menqal:w2"], "a repo's worktree under its checkout");
        // never past nine
        let many = snapshot(&[("a", None), ("b", None), ("c", None), ("d", None), ("e", None), ("f", None)]);
        let rows = vec![TreeRow::Workspaces { key: key("x") }, TreeRow::Workspaces { key: key("y") }];
        assert_eq!(workspace_order(&rows, |_| Some(&many), None).len(), 9);
    }

    #[test]
    fn numbering_and_its_hints_follow_a_drop_herdr_has_not_carried_out() {
        use super::super::tree_drag::{Move, Pending};
        let shown = snapshot(&[("w1", None), ("w2", None), ("w3", None)]);
        let rows = vec![TreeRow::Workspaces { key: key("default") }];
        let snaps = |k: &SessionKey| (*k == key("default")).then_some(&shown);
        let now = Instant::now();
        let mut pending = HashMap::new();
        let mut p = Pending::new(now);
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        p.push(Move { ids: ids(&["w3"]), before: Some("w1".into()) }, ids(&["w1", "w2", "w3"]), ids(&["w3", "w1", "w2"]), now);
        pending.insert(key("default"), p);
        let order: Vec<String> = numbered(&rows, snaps, &pending, now, None).into_iter().map(|(_, w)| w).collect();
        // the chips sit on the same order: `tree_rows` zips it with Workspace1–9's chords
        assert_eq!(order, ["w3", "w1", "w2"]);
        let none: Vec<String> = numbered(&rows, snaps, &HashMap::new(), now, None).into_iter().map(|(_, w)| w).collect();
        assert_eq!(none, ["w1", "w2", "w3"]);
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
    fn only_a_session_with_a_header_folds() {
        let places = [Place::Session(info("default", true)), Place::Session(info("menqal", true))];
        let w = watched(&places, "default");
        let shown = snapshot(&[("w1", None)]);
        let rows = tree(&places, &w, &HashSet::new(), Some(&shown), false);
        assert!(foldable(&rows, &key("default")), "the shown session's header");
        // folded, its header stays, so the key opens it again
        let folded: HashSet<String> = ["/tmp/default.sock".to_string()].into();
        assert!(foldable(&tree(&places, &w, &folded, Some(&shown), false), &key("default")));
        // one session alone: no header to fold
        let one = [Place::Session(info("default", true))];
        assert!(!foldable(&tree(&one, &watched(&one, "default"), &HashSet::new(), Some(&shown), false), &key("default")));
        // a socket of one's own, outside herdr's list: its workspaces lead without a header, as the sidebar draws them
        let own = model_rows(&places, &w, &HashSet::new(), Some(&shown), &key("mine"), false);
        assert_eq!(kinds(&own)[..2], ["W /tmp/mine.sock", "S /tmp/default.sock"]);
        assert!(!foldable(&own, &key("mine")));
        // a listed session shown gets no second row
        let listed = model_rows(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false);
        assert_eq!(listed, rows);
        assert!(foldable(&listed, &key("default")));
    }

    #[test]
    fn dividers_part_the_session_groups() {
        let session = |n: &str| TreeRow::Session { key: key(n), collapsed: false, waiting: 0, status: Status::Live };
        let machine = TreeRow::Machine { id: "you@box".into(), label: "you@box".into(), status: Status::Live, enabled: true, collapsed: false, waiting: 0 };
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

    /// Three sessions, `default` shown: `menqal` with two agents waiting and one at work, the machine `a` out of reach
    /// with its last snapshot's one waiting agent.
    fn counted() -> (Vec<Place>, Watchers) {
        let (places, _) = tree_order(vec![info("default", true), info("menqal", true)], vec![machine("a", "you@box", true)], &[], &[]);
        let mut w = watched(&places, "default");
        let now = Instant::now();
        let m = SessionKey("machine:a".into());
        w.set_client(&m, Client::at("/tmp/x".into()));
        w.due(now, super::super::watchers::Activity::Active);
        w.apply(&key("menqal"), Ok(agents_snapshot(&[("q1", "blocked"), ("q2", "done"), ("q3", "working")])), now);
        w.apply(&m, Ok(snapshot(&[("w1", None)])), now);
        w.apply(&m, Err(anyhow::anyhow!("timed out")), now);
        w.apply(&m, Err(anyhow::anyhow!("timed out")), now);
        assert_eq!(w.get(&m).unwrap().status, Status::Unreachable);
        (places, w)
    }

    #[test]
    fn a_session_s_count_is_its_tree_row_s_wherever_it_is_read() {
        let (places, w) = counted();
        let shown = agents_snapshot(&[("p1", "blocked")]);
        let m = SessionKey("machine:a".into());
        assert_eq!(session_waiting(&w, &key("default"), Some(&shown)), 1, "the shown one's live snapshot");
        assert_eq!(session_waiting(&w, &key("default"), None), 0, "not its watcher's");
        assert_eq!(session_waiting(&w, &key("menqal"), Some(&shown)), 2, "waiting and done, not working");
        assert_eq!(session_waiting(&w, &m, Some(&shown)), 1, "out of reach: its last snapshot");
        assert_eq!(session_waiting(&w, &key("gone"), Some(&shown)), 0);
        // the tree's rows count the same
        let rows = tree(&places, &w, &HashSet::new(), Some(&shown), false);
        let counts: Vec<(String, usize)> = rows
            .iter()
            .filter_map(|r| match r {
                TreeRow::Session { key, waiting, .. } => Some((key.0.clone(), *waiting)),
                _ => None,
            })
            .collect();
        assert_eq!(counts, [("/tmp/default.sock".to_string(), 1), ("/tmp/menqal.sock".to_string(), 2), ("machine:a".to_string(), 1)]);
    }

    #[test]
    fn the_panels_mark_a_session_out_of_reach() {
        let (_, w) = counted();
        let m = SessionKey("machine:a".into());
        assert!(out_of_reach(&w, &m));
        assert!(!out_of_reach(&w, &key("menqal")) && !out_of_reach(&w, &key("default")) && !out_of_reach(&w, &key("gone")));
        let shown = agents_snapshot(&[("p1", "blocked")]);
        let sessions = [
            SessionAgents { key: key("default"), name: "default".into(), snapshot: &shown },
            SessionAgents { key: m.clone(), name: "a".into(), snapshot: w.snapshot(&m).unwrap() },
        ];
        let ordered = vec![None, Some(snapshot(&[("w9", None)]))];
        let ws = panel_workspaces(&sessions, &ordered, &w);
        let marks: Vec<(&str, bool, &str)> = ws.iter().map(|s| (s.key.as_str(), s.out_of_reach, s.snapshot.workspaces[0].workspace_id.as_str())).collect();
        assert_eq!(marks, [("/tmp/default.sock", false, "w1"), ("machine:a", true, "w9")], "a drop's order where one waits");
    }

    #[test]
    fn the_dock_counts_every_session_but_those_out_of_reach() {
        let (_, w) = counted();
        let shown = agents_snapshot(&[("p1", "blocked"), ("p2", "working")]);
        assert_eq!(waiting_everywhere(Some(&shown), &w), 3, "the shown one's 1 and menqal's 2, not the machine's last 1");
        assert_eq!(waiting_everywhere(None, &w), 2);
        // the tree's ●N still counts the one out of reach
        assert_eq!(session_waiting(&w, &SessionKey("machine:a".into()), Some(&shown)), 1);
    }

    /// This computer with `default` shown and `menqal`; you@box with machines a and c, each a snapshot with one agent
    /// waiting.
    fn two_machines() -> (Vec<Place>, Watchers, Snapshot) {
        let (places, _) = tree_order(vec![info("default", true), info("menqal", true)], vec![machine("a", "you@box", true), machine("c", "you@box", true)], &[], &[]);
        let mut w = watched(&places, "default");
        let now = Instant::now();
        for k in ["machine:a", "machine:c"] {
            w.set_client(&SessionKey(k.into()), Client::at("/tmp/x".into()));
        }
        w.due(now, super::super::watchers::Activity::Active);
        for k in ["machine:a", "machine:c"] {
            w.apply(&SessionKey(k.into()), Ok(snapshot(&[("w1", None)])), now);
        }
        (places, w, snapshot(&[("w1", None)]))
    }

    #[test]
    fn a_folded_machine_hides_its_sessions_and_counts_their_agents() {
        let (places, w, shown) = two_machines();
        let rows = tree(&places, &w, &[group_key("you@box")].into(), Some(&shown), false);
        assert_eq!(kinds(&rows), ["M This computer", "S /tmp/default.sock", "W /tmp/default.sock", "S /tmp/menqal.sock", "M you@box"]);
        assert_eq!(
            rows[4],
            TreeRow::Machine { id: "you@box".into(), label: "you@box".into(), status: Status::Live, enabled: true, collapsed: true, waiting: 2 },
            "a and c, one each"
        );
        assert!(matches!(rows[0], TreeRow::Machine { collapsed: false, waiting: 0, .. }), "open, its sessions show their own");
        // this computer folds the same way, the shown session's live count in it (menqal has no snapshot)
        let rows = tree(&places, &w, &[group_key(LOCAL)].into(), Some(&shown), false);
        assert_eq!(kinds(&rows), ["M This computer", "M you@box", "S machine:a", "W machine:a", "S machine:c", "W machine:c"]);
        assert!(matches!(rows[0], TreeRow::Machine { collapsed: true, waiting: 1, .. }));
        // a session's fold folds no machine, and no machine's fold any session
        let rows = tree(&places, &w, &["you@box".to_string(), LOCAL.to_string()].into(), Some(&shown), false);
        assert!(rows.iter().all(|r| !matches!(r, TreeRow::Machine { collapsed: true, .. } | TreeRow::Session { collapsed: true, .. })));
        // a folded machine's group still moves as one: the drag line goes under its row
        use super::super::tree_drag::{Edge, Line};
        let rows = tree(&places, &w, &[group_key("you@box")].into(), Some(&shown), false);
        assert_eq!(line_row(&rows, &Line::Machine("you@box".into(), Edge::Below)), Some((4, Edge::Below)));
        assert_eq!(line_row(&rows, &Line::Machine(LOCAL.into(), Edge::Below)), Some((3, Edge::Below)));
    }

    #[test]
    fn a_lone_machine_has_no_row_to_fold() {
        let places = [Place::Session(info("default", true)), Place::Session(info("menqal", true))];
        let rows = tree(&places, &watched(&places, "default"), &[group_key(LOCAL)].into(), None, false);
        assert_eq!(kinds(&rows), ["S /tmp/default.sock", "W /tmp/default.sock", "S /tmp/menqal.sock"], "no machine row, so nothing folded out of reach");
    }

    #[test]
    fn a_machine_fold_is_saved_and_read_back() {
        let old: crate::settings::Settings = serde_json::from_str(r#"{"collapsed_sessions": ["/tmp/a.sock"]}"#).unwrap();
        assert!(old.collapsed_machines.is_empty(), "a file from before machine folds");
        let mut s = crate::settings::Settings::default();
        assert!(s.collapsed_machines.is_empty());
        fold_machine(&mut s, "you@box");
        fold_machine(&mut s, LOCAL);
        let back: crate::settings::Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.collapsed_machines, ["you@box", LOCAL]);
        assert!(back.collapsed_sessions.is_empty());
        assert_eq!(saved_folds(&back), HashSet::from([group_key("you@box"), group_key(LOCAL)]));
        fold_machine(&mut s, "you@box");
        assert_eq!(s.collapsed_machines, [LOCAL], "a second click opens it");
        let both = crate::settings::Settings { collapsed_sessions: vec!["/tmp/a.sock".into()], ..back };
        assert_eq!(saved_folds(&both).len(), 3);
    }

    #[test]
    fn the_sessions_bar_shows_a_folded_machine_s_session_open_and_focus_keeps_the_fold() {
        use super::super::sidebar_focus::tree_view;
        let (places, w, shown) = two_machines();
        let folds: HashSet<String> = [group_key("you@box")].into();
        // the sessions bar on a machine's session shows its workspaces whatever its machine's fold
        let bar = tree_view(&places, &w, &folds, Some(&shown), &SessionKey("machine:a".into()), true, false);
        assert_eq!(kinds(&bar.rows), ["W machine:a"]);
        // focus mode keeps the folded machine's header, its sessions having agents, and drops menqal, which has none
        let focused = tree_view(&places, &w, &folds, Some(&shown), &key("default"), false, true);
        assert_eq!(kinds(&focused.rows), ["M This computer", "S /tmp/default.sock", "W /tmp/default.sock", "M you@box"]);
        assert!(matches!(focused.rows[3], TreeRow::Machine { collapsed: true, waiting: 2, .. }));
        // its sessions' agents gone: the machine goes too, folded or open
        let mut w = w;
        for k in ["machine:a", "machine:c"] {
            w.apply(&SessionKey(k.into()), Ok(agents_snapshot(&[])), Instant::now());
        }
        for folds in [folds, HashSet::new()] {
            let focused = tree_view(&places, &w, &folds, Some(&shown), &key("default"), false, true);
            assert_eq!(kinds(&focused.rows), ["M This computer", "S /tmp/default.sock", "W /tmp/default.sock"]);
        }
    }

    #[test]
    fn a_session_switched_to_opens_its_machine() {
        let mut s = crate::settings::Settings { collapsed_machines: vec![LOCAL.into(), "you@box".into()], ..Default::default() };
        assert!(unfold_machine(&mut s, "you@box"));
        assert_eq!(s.collapsed_machines, [LOCAL]);
        assert!(!unfold_machine(&mut s, "you@box"), "open already");
        assert!(unfold_machine(&mut s, LOCAL));
        assert!(s.collapsed_machines.is_empty());
    }

    #[test]
    fn fold_or_unfold_the_session_opens_its_folded_machine_first() {
        let (places, w, shown) = two_machines();
        let folded = tree(&places, &w, &[group_key(LOCAL)].into(), Some(&shown), false);
        assert_eq!(shown_fold(&folded, &key("default"), Some(LOCAL)), ShownFold::UnfoldMachine(LOCAL.into()));
        let open = tree(&places, &w, &HashSet::new(), Some(&shown), false);
        assert_eq!(shown_fold(&open, &key("default"), Some(LOCAL)), ShownFold::FoldSession);
        // another machine folded: only the shown session folds
        let other = tree(&places, &w, &[group_key("you@box")].into(), Some(&shown), false);
        assert_eq!(shown_fold(&other, &key("default"), Some(LOCAL)), ShownFold::FoldSession);
        // a socket of one's own: no header, nothing
        assert_eq!(shown_fold(&open, &key("own"), None), ShownFold::Nothing);
    }

    #[test]
    fn a_folded_machine_out_of_reach_fades_its_count() {
        let (places, mut w, shown) = two_machines();
        let now = Instant::now();
        for k in ["machine:a", "machine:c"] {
            for _ in 0..2 {
                w.apply(&SessionKey(k.into()), Err(anyhow::anyhow!("timed out")), now);
            }
        }
        let rows = tree(&places, &w, &[group_key("you@box")].into(), Some(&shown), false);
        assert!(matches!(rows[4], TreeRow::Machine { status: Status::Unreachable, collapsed: true, waiting: 2, .. }), "{:?}", rows[4]);
        assert!(pill_faded(Status::Unreachable));
        assert!(!pill_faded(Status::Live));
        // one session still answering: the machine is live, its count not faded
        w.apply(&SessionKey("machine:a".into()), Ok(snapshot(&[("w1", None)])), now);
        let rows = tree(&places, &w, &[group_key("you@box")].into(), Some(&shown), false);
        assert!(matches!(rows[4], TreeRow::Machine { status: Status::Live, .. }));
    }

    #[test]
    fn a_folded_machine_s_count_goes_to_its_longest_wait() {
        let s = snapshot(&[("w1", None)]);
        let (mut early, mut late) = (crate::model::Since::new(), crate::model::Since::new());
        let t0 = Instant::now();
        late.insert("p1".into(), (crate::model::AgentStatus::Blocked, t0 + Duration::from_secs(60)));
        early.insert("p1".into(), (crate::model::AgentStatus::Blocked, t0));
        let (a, c) = (SessionKey("machine:a".into()), SessionKey("machine:c".into()));
        let found = longest_waiting_among([(a.clone(), &s, &late), (c.clone(), &s, &early)]);
        assert_eq!(found, Some((c.clone(), "t1".to_string(), "p1".to_string())), "not the first session's");
        let none = crate::model::Since::new();
        assert_eq!(longest_waiting_among([(a.clone(), &s, &none), (c.clone(), &s, &early)]).map(|f| f.0), Some(c), "a known wait before an unknown one");
        assert_eq!(longest_waiting_among([(a.clone(), &s, &none)]).map(|f| f.0), Some(a), "else the first that waits");
        assert_eq!(longest_waiting_among(std::iter::empty()), None);
    }

    /// A machine's row ends in its status dot and nothing after it (no hover ⋯), and the dot sits where a session
    /// row's does: one column down the tree.
    #[gpui::test]
    fn a_machine_row_ends_in_its_dot_in_the_session_rows_column(cx: &mut gpui::TestAppContext) {
        struct Rows;
        impl gpui::Render for Rows {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = Theme::of(cx).clone();
                let machine = machine_header(&theme, "you@box", "box", true, theme.success, None, None);
                // a session row as `session_row` builds it, to its dot
                let session = row(&theme, false, ("tree-session", 0usize)).h(px(28.)).pl(px(2.)).child(div().flex_1()).child(status_column(theme.success, "tree-session-dot"));
                div().w(px(256.)).px(px(8.)).flex().flex_col().child(machine.debug_selector(|| "machine-row".into())).child(session.debug_selector(|| "session-row".into()))
            }
        }
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let (_, cx) = cx.add_window_view(|_, _| Rows);
        cx.run_until_parked();
        let (machine, dot) = (cx.debug_bounds("machine-row").unwrap(), cx.debug_bounds("tree-machine-dot").unwrap());
        let session_dot = cx.debug_bounds("tree-session-dot").unwrap();
        assert_eq!(dot.right(), machine.right() - px(8.), "the dot is last, at the row's padding");
        assert_eq!((dot.left(), dot.size.width), (session_dot.left(), session_dot.size.width), "the session row's column");
    }
}
