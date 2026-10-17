//! Dragging in the sidebar's tree to reorder it: a workspace with its linked worktrees (one block, herdr's
//! `workspace.move_block`), a worktree among its siblings, a session within its machine, or an SSH machine's whole
//! group. herdr keeps no order of sessions or machines: theirs is Shepherd's (`session_order`, `machine_order`),
//! with this computer first. A drop lands above or below the row under the pointer, by the half it is in, and a
//! 2 px accent line shows where; a drop that would move nothing, into another session, or a worktree out of its
//! repo's group, has no line and does nothing.
use super::sessions::Place;
use super::sidebar_tree::{LOCAL, machine_of};
use super::watchers::{Kind, SessionKey, Status};
use super::*;
use gpui::{DragMoveEvent, Point};

/// The half of a row a drop lands in: before the row, or after it (after a group's last row, for a group).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    Above,
    Below,
}

/// The half of `bounds` the pointer is in, or `None` when it is outside: every row hears every drag move, so only
/// the row under the pointer may answer.
pub(super) fn hovered(bounds: Bounds<Pixels>, at: Point<Pixels>) -> Option<Edge> {
    let mid = bounds.origin.y + bounds.size.height / 2.;
    bounds.contains(&at).then_some(if at.y < mid { Edge::Above } else { Edge::Below })
}

/// What is dragged.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Item {
    /// A top-level workspace, with its linked worktrees (folded or not), moved as one block.
    Group(String),
    /// A linked worktree, which only moves among its siblings.
    Kid(String),
    Session(SessionKey),
    /// An SSH target's group of sessions.
    Machine(String),
}

/// The row under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Over {
    Workspace(SessionKey, String),
    Session(SessionKey),
    Machine(String),
}

/// Where the line is drawn: over or under a row; under a group's or a machine's last row for `Below`.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Line {
    Group(String, Edge),
    Kid(String, Edge),
    Session(SessionKey, Edge),
    Machine(String, Edge),
}

/// A drop's `workspace.move_block`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Move {
    pub ids: Vec<String>,
    pub before: Option<String>,
}

/// What a drop does.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Plan {
    Workspaces(SessionKey, Move),
    /// Every place's key in its new order.
    Sessions(Vec<String>),
    /// The SSH targets in their new order.
    Machines(Vec<String>),
}

/// A session's workspaces as the tree groups them (each top-level one's id and its worktrees'), and herdr's list.
pub(super) struct Workspaces {
    pub groups: Vec<(String, Vec<String>)>,
    pub list: Vec<String>,
}

impl Workspaces {
    pub(super) fn of(s: &Snapshot) -> Self {
        let groups = crate::model::groups(s.workspace_tree())
            .into_iter()
            .map(|(w, kids)| (w.workspace_id.clone(), kids.iter().map(|k| k.workspace_id.clone()).collect()))
            .collect();
        Workspaces { groups, list: s.workspaces.iter().map(|w| w.workspace_id.clone()).collect() }
    }
}

/// `list` after herdr's `move_block`: `ids` taken out and put back, in their order, before `before` (at the end
/// when it is `None`).
pub(super) fn moved(list: &[String], ids: &[String], before: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = list.iter().filter(|w| !ids.contains(w)).cloned().collect();
    let at = before.and_then(|b| out.iter().position(|w| w == b)).unwrap_or(out.len());
    out.splice(at..at, ids.iter().cloned());
    out
}

/// The groups as the tree shows them for `list`: by where each root is in it, the worktrees by where they are.
fn regroup(groups: &[(String, Vec<String>)], list: &[String]) -> Vec<(String, Vec<String>)> {
    let at = |id: &String| list.iter().position(|w| w == id).unwrap_or(usize::MAX);
    let mut out: Vec<(String, Vec<String>)> = groups.to_vec();
    for (_, kids) in &mut out {
        kids.sort_by_key(at);
    }
    out.sort_by_key(|(root, _)| at(root));
    out
}

/// The move that drops `item` on the `edge` half of the workspace `target`, and the line that shows it. A group
/// dropped anywhere in another group lands before or after that whole group (its worktrees' rows count as its
/// lower half); a worktree lands among its siblings only. `None` when the drop moves nothing in the tree, or names
/// a workspace herdr no longer lists.
pub(super) fn plan_workspaces(ws: &Workspaces, item: &Item, target: &str, edge: Edge) -> Option<(Move, Line)> {
    let Workspaces { groups, list } = ws;
    let group_of = |id: &str| groups.iter().position(|(root, kids)| root == id || kids.iter().any(|k| k == id));
    let at = group_of(target)?;
    let (root, kids) = &groups[at];
    let (ids, before, line) = match item {
        Item::Group(id) => {
            let own = group_of(id).filter(|g| groups[*g].0 == *id)?;
            if own == at {
                return None;
            }
            let edge = if target == root { edge } else { Edge::Below };
            let before = match edge {
                Edge::Above => Some(root.clone()),
                Edge::Below => groups[at + 1..].iter().map(|(r, _)| r).find(|r| *r != id).cloned(),
            };
            let (own_root, own_kids) = &groups[own];
            let ids: Vec<String> = std::iter::once(own_root).chain(own_kids).cloned().collect();
            (ids, before, Line::Group(root.clone(), edge))
        }
        Item::Kid(id) => {
            // a worktree stays under its repo's checkout
            if !kids.contains(id) || target == id {
                return None;
            }
            let next_sibling = |from: usize| kids[from..].iter().find(|k| *k != id).cloned();
            let (before, line) = if target == root {
                if edge == Edge::Above {
                    return None;
                }
                (next_sibling(0), Line::Kid(kids[0].clone(), Edge::Above))
            } else {
                let t = kids.iter().position(|k| k == target)?;
                let before = match edge {
                    Edge::Above => Some(target.to_string()),
                    // after the last sibling: before whatever herdr lists after it
                    Edge::Below => next_sibling(t + 1).or_else(|| {
                        let last = kids.iter().rev().find(|k| *k != id)?;
                        let from = list.iter().position(|w| w == last)?;
                        list[from + 1..].iter().find(|w| *w != id).cloned()
                    }),
                };
                (before, Line::Kid(target.to_string(), edge))
            };
            (vec![id.clone()], before, line)
        }
        Item::Session(_) | Item::Machine(_) => return None,
    };
    if !ids.iter().chain(&before).all(|id| list.contains(id)) {
        return None;
    }
    let next = regroup(groups, &moved(list, &ids, before.as_deref()));
    (next != regroup(groups, list)).then_some((Move { ids, before }, line))
}

/// The places' new order when the session `key` drops on the `edge` half of the session `target`: both on one
/// machine, the order changed. `places` are (machine, key) in the tree's order.
pub(super) fn plan_sessions(places: &[(String, String)], key: &str, target: &str, edge: Edge) -> Option<Vec<String>> {
    let machine = |k: &str| places.iter().find(|(_, p)| p == k).map(|(m, _)| m.clone());
    if key == target || machine(key)? != machine(target)? {
        return None;
    }
    let keys: Vec<String> = places.iter().map(|(_, k)| k.clone()).collect();
    let before = match edge {
        Edge::Above => Some(target.to_string()),
        Edge::Below => keys.iter().skip_while(|k| *k != target).skip(1).find(|k| *k != key).cloned(),
    };
    let next = moved(&keys, &[key.to_string()], before.as_deref());
    (next != keys).then_some(next)
}

/// The SSH targets' new order when the machine `dragged` drops on the `edge` half of the machine `target`'s group:
/// this computer stays first, so only its lower half takes a drop. `targets` are the SSH targets in the tree's order.
pub(super) fn plan_machines(targets: &[String], dragged: &str, target: &str, edge: Edge) -> Option<Vec<String>> {
    if dragged == target || dragged == LOCAL || !targets.iter().any(|t| t == dragged) {
        return None;
    }
    let before = match (target == LOCAL, edge) {
        (true, Edge::Above) => return None,
        (true, Edge::Below) => targets.iter().find(|t| *t != dragged).cloned(),
        (false, Edge::Above) => Some(target.to_string()),
        (false, Edge::Below) => targets.iter().skip_while(|t| *t != target).skip(1).find(|t| *t != dragged).cloned(),
    };
    if target != LOCAL && !targets.iter().any(|t| t == target) {
        return None;
    }
    let next = moved(targets, &[dragged.to_string()], before.as_deref());
    (next != targets).then_some(next)
}

/// What dropping `item`, picked up in the session `from`, on the `edge` half of `over` does, and its line. Workspaces
/// never cross sessions: herdr has no move between them. `workspaces` reads a session's workspaces as shown.
pub(super) fn plan(
    from: &SessionKey,
    item: &Item,
    over: &Over,
    edge: Edge,
    workspaces: impl Fn(&SessionKey) -> Option<Workspaces>,
    places: &[(String, String)],
) -> Option<(Plan, Line)> {
    match (item, over) {
        (Item::Group(_) | Item::Kid(_), Over::Workspace(key, target)) => {
            if key != from {
                return None;
            }
            let (mv, line) = plan_workspaces(&workspaces(key)?, item, target, edge)?;
            Some((Plan::Workspaces(key.clone(), mv), line))
        }
        (Item::Session(key), Over::Session(target)) => {
            let order = plan_sessions(places, &key.0, &target.0, edge)?;
            Some((Plan::Sessions(order), Line::Session(target.clone(), edge)))
        }
        (Item::Machine(m), Over::Machine(_) | Over::Session(_)) => {
            // a session's row is part of its machine's group: its lower half
            let (target, edge) = match over {
                Over::Machine(t) => (t.clone(), edge),
                Over::Session(k) => (places.iter().find(|(_, p)| *p == k.0)?.0.clone(), Edge::Below),
                Over::Workspace(..) => return None,
            };
            let mut targets: Vec<String> = Vec::new();
            for (machine, _) in places {
                if machine != LOCAL && !targets.contains(machine) {
                    targets.push(machine.clone());
                }
            }
            let order = plan_machines(&targets, m, &target, edge)?;
            Some((Plan::Machines(order), Line::Machine(target, edge)))
        }
        _ => None,
    }
}

/// `items` with those `order` names first, in its order, then the rest as they came.
pub(super) fn apply_order<T>(items: Vec<T>, order: &[String], key: impl Fn(&T) -> String) -> Vec<T> {
    let mut items: Vec<(usize, T)> = items.into_iter().map(|t| (order.iter().position(|o| *o == key(&t)).unwrap_or(usize::MAX), t)).collect();
    items.sort_by_key(|(at, _)| *at); // stable: the rest keep their order
    items.into_iter().map(|(_, t)| t).collect()
}

/// `places` (in herdr's order, as `tree_order` groups them) in the order dragged to: this computer's sessions first,
/// then each SSH target's group in `machine_order`, each group's sessions in `session_order`; whatever neither
/// names comes after, as herdr listed it.
pub(super) fn order_places(places: Vec<Place>, session_order: &[String], machine_order: &[String]) -> Vec<Place> {
    let mut places = apply_order(places, session_order, Place::key);
    let mut targets: Vec<String> = Vec::new();
    for p in &places {
        if let Place::Machine(m) = p
            && !targets.contains(&m.target)
        {
            targets.push(m.target.clone());
        }
    }
    let targets = apply_order(targets, machine_order, String::clone);
    places.sort_by_key(|p| match p {
        Place::Session(_) => 0,
        Place::Machine(m) => 1 + targets.iter().position(|t| *t == m.target).unwrap_or(usize::MAX - 1),
    });
    places
}

/// The order to save: `now` (everything listed, as dropped), then the names saved before that are not listed but
/// `keep` keeps (a machine turned off, which comes back where it was). The rest were deleted.
pub(super) fn merged(now: Vec<String>, saved: &[String], keep: impl Fn(&str) -> bool) -> Vec<String> {
    let gone: Vec<String> = saved.iter().filter(|s| !now.contains(s) && keep(s)).cloned().collect();
    now.into_iter().chain(gone).collect()
}

/// How long a drop's order is shown while herdr's list has not changed.
const PENDING_FOR: Duration = Duration::from_secs(5);

/// A drop's order shown before herdr's answer: what herdr listed when it was dropped, and the order it should list.
#[derive(Clone, Debug)]
pub(super) struct Pending {
    pub base: Vec<String>,
    pub order: Vec<String>,
    pub at: Instant,
}

impl Pending {
    /// The order to show while herdr lists `list`: the drop's, until herdr's list changes at all (to it, or to
    /// anything else: herdr's word wins) or `PENDING_FOR` passes.
    pub(super) fn order(&self, list: &[String], now: Instant) -> Option<&[String]> {
        (list == self.base.as_slice() && now.saturating_duration_since(self.at) < PENDING_FOR).then_some(self.order.as_slice())
    }
}

/// The snapshot `s` with its workspaces in `order`.
pub(super) fn with_order(s: &Snapshot, order: &[String]) -> Snapshot {
    let mut s = s.clone();
    s.workspaces = apply_order(s.workspaces, order, |w| w.workspace_id.clone());
    s
}

/// A row being dragged, drawn under the pointer as a pill with its name.
#[derive(Clone)]
pub(super) struct DraggedTreeItem {
    /// The session a workspace was picked up in.
    pub from: SessionKey,
    pub item: Item,
    pub label: String,
    pub theme: Theme,
}

impl Render for DraggedTreeItem {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let icon = match self.item {
            Item::Group(_) => "icons/folder.svg",
            Item::Kid(_) => "icons/git-branch.svg",
            Item::Session(_) => "icons/terminal.svg",
            Item::Machine(_) => "icons/remote-server.svg",
        };
        div()
            .h(px(28.))
            .max_w(px(240.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(8.))
            .bg(theme.card)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .opacity(0.9)
            .font_family(theme.font_ui.clone())
            .text_size(px(13.))
            .text_color(theme.text)
            .child(gpui::svg().path(icon).size(px(14.)).flex_none().text_color(theme.text_muted))
            .child(div().truncate().child(self.label.clone()))
    }
}

/// The row under the pointer while a row is dragged, the half it is in, and the line its drop draws.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TreeDrop {
    pub over: Over,
    pub edge: Edge,
    pub line: Line,
}

/// The 2 px line along a row's top or bottom, inside it: a worktree group's body clips what is outside.
pub(super) fn drop_line(theme: &Theme, edge: Edge) -> Div {
    let bar = div().absolute().left(px(4.)).right(px(4.)).h(px(2.)).rounded_full().bg(theme.accent);
    match edge {
        Edge::Above => bar.top_0(),
        Edge::Below => bar.bottom_0(),
    }
}

/// `el` with `line`'s bar when it is `edge`'s.
pub(super) fn with_line(el: impl IntoElement, theme: &Theme, edge: Option<Edge>) -> gpui::AnyElement {
    match edge {
        Some(edge) => div().relative().flex_none().flex().flex_col().child(el).child(drop_line(theme, edge)).into_any_element(),
        None => el.into_any_element(),
    }
}

impl Shell {
    /// The places as (machine, key) in the tree's order, for a drop's plan.
    fn place_keys(&self) -> Vec<(String, String)> {
        self.places.iter().map(|p| (machine_of(p).to_string(), p.key())).collect()
    }

    /// The session `key`'s snapshot as herdr last sent it: the shown one's live, another's from its watcher.
    pub(super) fn session_snapshot(&self, key: &SessionKey) -> Option<&Snapshot> {
        if self.shown_key().as_ref() == Some(key) { self.snapshot.as_ref() } else { self.watchers.snapshot(key) }
    }

    /// `s` (the session `key`'s) in the order of a drop herdr hasn't answered yet; `None` when there is none.
    pub(super) fn pending_order(&self, key: &SessionKey, s: &Snapshot) -> Option<Snapshot> {
        let list: Vec<String> = s.workspaces.iter().map(|w| w.workspace_id.clone()).collect();
        let order = self.tree.pending.get(key)?.order(&list, Instant::now())?;
        Some(with_order(s, order))
    }

    /// Whether the session `key`'s workspaces can be dragged: the shown one, or another that answers and has a
    /// client to send the move through; never while a name is being edited.
    pub(super) fn can_drag_workspaces(&self, key: &SessionKey) -> bool {
        if self.renaming.is_some() {
            return false;
        }
        self.shown_key().as_ref() == Some(key) || self.watchers.get(key).is_some_and(|w| w.status == Status::Live && w.client.is_some())
    }

    /// What dropping `d` on the `edge` half of `over` does now, in the order shown.
    fn plan_drop(&self, d: &DraggedTreeItem, over: &Over, edge: Edge) -> Option<(Plan, Line)> {
        let workspaces = |k: &SessionKey| {
            let s = self.session_snapshot(k)?;
            Some(Workspaces::of(&self.pending_order(k, s).unwrap_or_else(|| s.clone())))
        };
        plan(&d.from, &d.item, over, edge, workspaces, &self.place_keys())
    }

    /// A row's drag listener: the row under the pointer sets where the drop lands, or that it lands nowhere. A row
    /// scrolled out of the tree's view keeps its bounds, so the pointer must be in the view too.
    pub(super) fn tree_drag_over(&mut self, over: Over, e: &DragMoveEvent<DraggedTreeItem>, cx: &mut Context<Self>) {
        if self.tree.view.is_some_and(|v| !v.contains(&e.event.position)) {
            return;
        }
        let Some(edge) = hovered(e.bounds, e.event.position) else { return };
        let d = e.drag(cx).clone();
        let next = self.plan_drop(&d, &over, edge).map(|(_, line)| TreeDrop { over, edge, line });
        if self.tree.drop != next {
            self.tree.drop = next;
            cx.notify();
        }
    }

    /// The tree's drag listener, heard before its rows': where its view is, and off it a drop lands nowhere.
    pub(super) fn tree_drag_left(&mut self, e: &DragMoveEvent<DraggedTreeItem>, cx: &mut Context<Self>) {
        self.tree.view = Some(e.bounds);
        if !e.bounds.contains(&e.event.position) && self.tree.drop.take().is_some() {
            cx.notify();
        }
    }

    /// Where a drag's line is drawn now.
    pub(super) fn tree_line(&self) -> Option<&Line> {
        self.tree.drop.as_ref().map(|d| &d.line)
    }

    /// A release over the tree: planned again against what is shown now, then carried out.
    pub(super) fn drop_tree_item(&mut self, d: &DraggedTreeItem, cx: &mut Context<Self>) {
        let Some(TreeDrop { over, edge, .. }) = self.tree.drop.take() else { return };
        cx.notify();
        match self.plan_drop(d, &over, edge).map(|(plan, _)| plan) {
            Some(Plan::Workspaces(key, mv)) => self.move_workspaces(key, mv, cx),
            Some(Plan::Sessions(order)) => {
                let off: Vec<String> = self.watchers.disabled.iter().map(|m| format!("machine:{}", m.id)).collect();
                let saved = SettingsGlobal::of(cx).settings.session_order.clone();
                let order = merged(order, &saved, |k| off.iter().any(|o| o == k));
                save_from_sidebar(cx, |s| s.session_order = order);
                self.reorder_places(cx);
            }
            Some(Plan::Machines(order)) => {
                let off: Vec<String> = self.watchers.disabled.iter().map(|m| m.target.clone()).collect();
                let saved = SettingsGlobal::of(cx).settings.machine_order.clone();
                let order = merged(order, &saved, |t| off.iter().any(|o| o == t));
                save_from_sidebar(cx, |s| s.machine_order = order);
                self.reorder_places(cx);
            }
            None => {}
        }
    }

    /// The places in the order saved, at once: the tree, the keys' steps and the numbering follow them.
    fn reorder_places(&mut self, cx: &mut Context<Self>) {
        let settings = &SettingsGlobal::of(cx).settings;
        self.places = order_places(std::mem::take(&mut self.places), &settings.session_order, &settings.machine_order);
    }

    /// Sends a drop's `move_block` to the session `key`: the shown one's own client, another's watcher's (2 s
    /// here, 10 s over SSH). The tree shows the order at once; a refusal puts herdr's back, with a toast.
    fn move_workspaces(&mut self, key: SessionKey, mv: Move, cx: &mut Context<Self>) {
        let Some(s) = self.session_snapshot(&key) else { return };
        let base: Vec<String> = s.workspaces.iter().map(|w| w.workspace_id.clone()).collect();
        let shown = self.pending_order(&key, s).map(|s| s.workspaces.iter().map(|w| w.workspace_id.clone()).collect()).unwrap_or_else(|| base.clone());
        let order = moved(&shown, &mv.ids, mv.before.as_deref());
        self.tree.pending.insert(key.clone(), Pending { base, order, at: Instant::now() });
        let command = Command::MoveWorkspaces { workspace_ids: mv.ids, before: mv.before };
        if self.shown_key().as_ref() == Some(&key) {
            self.send_then(command, cx, move |this, result, cx| {
                if let Err(e) = result {
                    this.tree.pending.remove(&key);
                    crate::ui::toast::error(cx, format!("Couldn't move the workspace: {e:#}"));
                    cx.notify();
                }
            });
            return;
        }
        let Some(w) = self.watchers.get(&key) else { return };
        let Some(client) = w.client.clone() else {
            self.tree.pending.remove(&key);
            return crate::ui::toast::error(cx, "Couldn't move the workspace: not connected to its session yet");
        };
        let timeout = if w.kind() == Kind::Remote { Duration::from_secs(10) } else { Duration::from_secs(2) };
        let (method, params) = command.request();
        let call = self.rt.spawn(async move {
            match tokio::time::timeout(timeout, client.call(method, params)).await {
                Ok(done) => done,
                Err(_) => Err(anyhow::anyhow!("no answer in {} s", timeout.as_secs())),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = call.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                match result {
                    // its watcher asks it now, rather than at its next poll
                    Ok(_) => this.watchers.poll_now(&key, Instant::now()),
                    Err(e) => {
                        this.tree.pending.remove(&key);
                        crate::ui::toast::error(cx, format!("Couldn't move the workspace: {e:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Drops a drag's line once no drag is under way (released anywhere), and orders herdr has answered.
    pub(super) fn tidy_tree_drag(&mut self, cx: &App) {
        if !cx.has_active_drag() {
            self.tree.drop = None;
            self.tree.view = None;
        }
        let now = Instant::now();
        let lists: Vec<(SessionKey, Vec<String>)> = self
            .tree
            .pending
            .keys()
            .map(|k| (k.clone(), self.session_snapshot(k).map(|s| s.workspaces.iter().map(|w| w.workspace_id.clone()).collect()).unwrap_or_default()))
            .collect();
        for (key, list) in lists {
            if self.tree.pending.get(&key).is_some_and(|p| p.order(&list, now).is_none()) {
                self.tree.pending.remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{Machine, SessionInfo};

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// herdr's list a, m, m-x, b, m-y (m's worktrees x and y, one listed after b), c; tree a, m [m-x, m-y], b, c.
    fn ws() -> Workspaces {
        Workspaces {
            groups: vec![("a".into(), vec![]), ("m".into(), ids(&["m-x", "m-y"])), ("b".into(), vec![]), ("c".into(), vec![])],
            list: ids(&["a", "m", "m-x", "b", "m-y", "c"]),
        }
    }

    fn mv(item: Item, target: &str, edge: Edge) -> Option<(Vec<String>, Option<String>)> {
        plan_workspaces(&ws(), &item, target, edge).map(|(m, _)| (m.ids, m.before))
    }

    fn group(id: &str) -> Item {
        Item::Group(id.into())
    }

    fn kid(id: &str) -> Item {
        Item::Kid(id.into())
    }

    #[test]
    fn herdrs_move_block_takes_the_ids_out_and_puts_them_back_in_order() {
        let list = ids(&["w1", "w2", "w3", "w4", "w5"]);
        // as the scratch session answered: apart, reversed, and at the end for `None`
        assert_eq!(moved(&list, &ids(&["w1", "w2"]), Some("w4")), ids(&["w3", "w1", "w2", "w4", "w5"]));
        assert_eq!(moved(&list, &ids(&["w5", "w3"]), None), ids(&["w1", "w2", "w4", "w5", "w3"]));
        assert_eq!(moved(&list, &ids(&["w4"]), Some("w1")), ids(&["w4", "w1", "w2", "w3", "w5"]));
    }

    #[test]
    fn a_group_lands_before_or_after_the_group_it_is_dropped_on() {
        assert_eq!(mv(group("c"), "a", Edge::Above), Some((ids(&["c"]), Some("a".into()))));
        assert_eq!(mv(group("a"), "b", Edge::Below), Some((ids(&["a"]), Some("c".into()))));
        // after the last group: at the end
        assert_eq!(mv(group("a"), "c", Edge::Below), Some((ids(&["a"]), None)));
        // a worktree's row is its group's lower half
        assert_eq!(mv(group("c"), "m-x", Edge::Above), Some((ids(&["c"]), Some("b".into()))));
        let (_, line) = plan_workspaces(&ws(), &group("c"), "m-x", Edge::Above).unwrap();
        assert_eq!(line, Line::Group("m".into(), Edge::Below));
    }

    #[test]
    fn a_group_moves_with_its_worktrees_in_their_order_folded_or_not() {
        assert_eq!(mv(group("m"), "c", Edge::Below), Some((ids(&["m", "m-x", "m-y"]), None)));
        assert_eq!(mv(group("m"), "a", Edge::Above), Some((ids(&["m", "m-x", "m-y"]), Some("a".into()))));
    }

    #[test]
    fn a_drop_on_its_own_slot_moves_nothing() {
        assert_eq!(mv(group("m"), "m-y", Edge::Below), None, "its own group");
        assert_eq!(mv(group("b"), "m", Edge::Below), None, "already after m");
        assert_eq!(mv(group("b"), "c", Edge::Above), None, "already before c");
        assert_eq!(mv(kid("m-x"), "m-y", Edge::Above), None);
        assert_eq!(mv(kid("m-x"), "m", Edge::Below), None, "already first");
    }

    #[test]
    fn a_worktree_reorders_among_its_siblings() {
        assert_eq!(mv(kid("m-y"), "m-x", Edge::Above), Some((ids(&["m-y"]), Some("m-x".into()))));
        assert_eq!(mv(kid("m-y"), "m", Edge::Below), Some((ids(&["m-y"]), Some("m-x".into()))));
        // after the last sibling: before what herdr lists after it
        assert_eq!(mv(kid("m-x"), "m-y", Edge::Below), Some((ids(&["m-x"]), Some("c".into()))));
        let (_, line) = plan_workspaces(&ws(), &kid("m-y"), "m", Edge::Below).unwrap();
        assert_eq!(line, Line::Kid("m-x".into(), Edge::Above));
    }

    #[test]
    fn a_worktree_never_leaves_its_group() {
        assert_eq!(mv(kid("m-x"), "b", Edge::Above), None);
        assert_eq!(mv(kid("m-x"), "a", Edge::Below), None);
        assert_eq!(mv(kid("m-x"), "m", Edge::Above), None, "over its checkout");
    }

    #[test]
    fn a_workspace_herdr_no_longer_lists_is_refused() {
        assert_eq!(mv(group("gone"), "a", Edge::Above), None);
        assert_eq!(mv(group("a"), "gone", Edge::Above), None);
        assert_eq!(mv(group("m-x"), "a", Edge::Above), None, "a worktree is not a group");
        let mut stale = ws();
        stale.list.retain(|w| w != "c");
        assert_eq!(plan_workspaces(&stale, &group("a"), "b", Edge::Below), None, "the group after left herdr's list");
    }

    fn key(s: &str) -> SessionKey {
        SessionKey(s.into())
    }

    fn places() -> Vec<(String, String)> {
        [("local", "/s/default"), ("local", "/s/menqal"), ("you@box", "machine:a"), ("you@box", "machine:b"), ("you@other", "machine:c")]
            .iter()
            .map(|(m, k)| (m.to_string(), k.to_string()))
            .collect()
    }

    #[test]
    fn workspaces_never_cross_sessions() {
        // both sessions list the same ids: only the session tells them apart
        let both = |_: &SessionKey| Some(ws());
        let item = group("c");
        let drop = |over: Over| plan(&key("/s/default"), &item, &over, Edge::Above, both, &places());
        assert!(matches!(drop(Over::Workspace(key("/s/default"), "a".into())), Some((Plan::Workspaces(..), _))));
        assert_eq!(drop(Over::Workspace(key("/s/menqal"), "a".into())), None);
        assert_eq!(drop(Over::Session(key("/s/menqal"))), None);
    }

    #[test]
    fn sessions_reorder_within_their_machine() {
        assert_eq!(plan_sessions(&places(), "/s/menqal", "/s/default", Edge::Above), Some(ids(&["/s/menqal", "/s/default", "machine:a", "machine:b", "machine:c"])));
        assert_eq!(plan_sessions(&places(), "machine:a", "machine:b", Edge::Below), Some(ids(&["/s/default", "/s/menqal", "machine:b", "machine:a", "machine:c"])));
        assert_eq!(plan_sessions(&places(), "/s/default", "machine:b", Edge::Above), None, "another machine");
        assert_eq!(plan_sessions(&places(), "/s/menqal", "/s/default", Edge::Below), None, "its own slot");
        assert_eq!(plan_sessions(&places(), "/s/gone", "/s/default", Edge::Above), None);
    }

    #[test]
    fn machine_groups_reorder_after_this_computer() {
        let t = ids(&["you@box", "you@other"]);
        assert_eq!(plan_machines(&t, "you@other", "you@box", Edge::Above), Some(ids(&["you@other", "you@box"])));
        assert_eq!(plan_machines(&t, "you@box", "you@other", Edge::Below), Some(ids(&["you@other", "you@box"])));
        assert_eq!(plan_machines(&t, "you@other", LOCAL, Edge::Below), Some(ids(&["you@other", "you@box"])));
        assert_eq!(plan_machines(&t, "you@other", LOCAL, Edge::Above), None, "this computer stays first");
        assert_eq!(plan_machines(&t, LOCAL, "you@other", Edge::Below), None, "this computer stays put");
        assert_eq!(plan_machines(&t, "you@box", "you@other", Edge::Above), None, "its own slot");
        // over one of a machine's sessions: after that machine's group
        let none = |_: &SessionKey| None;
        let drop = plan(&key(""), &Item::Machine("you@box".into()), &Over::Session(key("machine:c")), Edge::Above, none, &places());
        assert_eq!(drop, Some((Plan::Machines(ids(&["you@other", "you@box"])), Line::Machine("you@other".into(), Edge::Below))));
    }

    #[test]
    fn only_the_row_under_the_pointer_answers() {
        let b = Bounds::new(gpui::point(px(0.), px(100.)), gpui::size(px(200.), px(30.)));
        assert_eq!(hovered(b, gpui::point(px(10.), px(110.))), Some(Edge::Above));
        assert_eq!(hovered(b, gpui::point(px(10.), px(120.))), Some(Edge::Below));
        assert_eq!(hovered(b, gpui::point(px(10.), px(140.))), None);
        assert_eq!(hovered(b, gpui::point(px(10.), px(90.))), None);
    }

    #[test]
    fn an_order_puts_what_it_names_first_and_the_rest_after() {
        let items = ids(&["a", "b", "c", "d"]);
        assert_eq!(apply_order(items.clone(), &ids(&["c", "gone", "a"]), String::clone), ids(&["c", "a", "b", "d"]));
        assert_eq!(apply_order(items.clone(), &[], String::clone), items);
    }

    fn session(name: &str, default: bool) -> Place {
        Place::Session(SessionInfo { name: name.into(), default, running: true, socket_path: format!("/s/{name}") })
    }

    fn machine(id: &str, target: &str) -> Place {
        Place::Machine(Machine { id: id.into(), label: id.into(), target: target.into(), session: "default".into(), enabled: true })
    }

    #[test]
    fn places_follow_the_saved_orders_this_computer_first() {
        // as `tree_order` hands them over: each target's machines together
        let herdr = vec![session("default", true), session("menqal", false), machine("a", "you@box"), machine("c", "you@box"), machine("b", "you@other")];
        let keys = |p: &[Place]| p.iter().map(Place::key).collect::<Vec<_>>();
        assert_eq!(keys(&order_places(herdr.clone(), &[], &[])), keys(&herdr));
        let sessions = ids(&["machine:c", "/s/menqal", "machine:a"]);
        let ordered = order_places(herdr.clone(), &sessions, &ids(&["you@other"]));
        assert_eq!(keys(&ordered), ["/s/menqal", "/s/default", "machine:b", "machine:c", "machine:a"]);
        // a machine named nowhere keeps herdr's order, after the named ones
        let ordered = order_places(herdr, &[], &ids(&["you@gone", "you@other"]));
        assert_eq!(keys(&ordered), ["/s/default", "/s/menqal", "machine:b", "machine:a", "machine:c"]);
    }

    #[test]
    fn a_saved_order_keeps_machines_turned_off_and_forgets_the_deleted() {
        let saved = ids(&["/s/old", "machine:off", "/s/default"]);
        let order = merged(ids(&["/s/menqal", "/s/default"]), &saved, |k| k == "machine:off");
        assert_eq!(order, ids(&["/s/menqal", "/s/default", "machine:off"]));
    }

    #[test]
    fn a_drops_order_shows_until_herdrs_list_changes() {
        let t0 = Instant::now();
        let p = Pending { base: ids(&["a", "b", "c"]), order: ids(&["c", "a", "b"]), at: t0 };
        assert_eq!(p.order(&ids(&["a", "b", "c"]), t0), Some(&ids(&["c", "a", "b"])[..]));
        assert_eq!(p.order(&ids(&["c", "a", "b"]), t0), None, "herdr answered");
        assert_eq!(p.order(&ids(&["b", "a", "c"]), t0), None, "something else moved it: herdr's word wins");
        assert_eq!(p.order(&ids(&["a", "b", "c"]), t0 + PENDING_FOR), None, "no answer for too long");
    }
}
