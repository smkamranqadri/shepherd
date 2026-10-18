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
            let mut order: Vec<String> = kids.iter().filter(|k| *k != id).cloned().collect();
            let (at, line) = if target == root {
                if edge == Edge::Above {
                    return None;
                }
                (0, Line::Kid(kids[0].clone(), Edge::Above))
            } else {
                let t = order.iter().position(|k| k == target)?;
                (if edge == Edge::Above { t } else { t + 1 }, Line::Kid(target.to_string(), edge))
            };
            order.insert(at, id.clone());
            // the whole group goes over, its checkout first, where its checkout was: herdr's own list (its TUI's
            // order) keeps the group together rather than a worktree landing among other workspaces
            let ids: Vec<String> = std::iter::once(root.clone()).chain(order).collect();
            let before = list.iter().skip_while(|w| *w != root).skip(1).find(|w| !ids.contains(w)).cloned();
            (ids, before, line)
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

/// The order to save: `now` (everything listed, in its new order) in the slots the listed ones had, the names
/// saved before that are not listed but `keep` keeps left in theirs (a machine turned off, or every machine while
/// herdr's list of them could not be read, comes back where it was), and the listed ones saved nowhere after. The
/// rest were deleted.
pub(super) fn merged(now: Vec<String>, saved: &[String], keep: impl Fn(&str) -> bool) -> Vec<String> {
    let mut next = now.iter();
    let mut out: Vec<String> = saved
        .iter()
        .filter_map(|s| if now.contains(s) { next.next().cloned() } else { keep(s).then(|| s.clone()) })
        .collect();
    out.extend(next.cloned());
    out
}

/// Whether a saved name not listed now is kept: a machine turned off (`off`, by key or target), or any machine
/// while herdr's list of them could not be read (`TreeState::machines_known`, the rule `still_known` keeps views
/// by). A session not listed was deleted.
pub(super) fn kept_unlisted(name: &str, off: &[String], machines_known: bool, is_machine: bool) -> bool {
    off.iter().any(|o| o == name) || (!machines_known && is_machine)
}

/// How long a drop's order shows with nothing heard from herdr: past the slowest call's deadline (10 s over SSH).
pub(super) const PENDING_FOR: Duration = Duration::from_secs(15);

/// A session's drops herdr hasn't carried out yet. Each call has its own connection, so they go one at a time,
/// in order: a later drop never overtakes an earlier one. Meanwhile the tree shows the last drop's order.
#[derive(Clone, Debug)]
pub(super) struct Pending {
    /// What herdr listed at each drop: lists on the way to the last order.
    seen: Vec<Vec<String>>,
    /// Each drop's order, the last shown.
    orders: Vec<Vec<String>>,
    /// Drops waiting for the one being sent.
    queued: std::collections::VecDeque<Move>,
    sending: bool,
    /// When a drop was last made or answered.
    at: Instant,
}

impl Pending {
    pub(super) fn new(now: Instant) -> Self {
        Pending { seen: Vec::new(), orders: Vec::new(), queued: Default::default(), sending: false, at: now }
    }

    /// A drop made while herdr lists `list`, which leads to `order`: the move to send now, or `None` when it waits
    /// for the one being sent.
    pub(super) fn push(&mut self, mv: Move, list: Vec<String>, order: Vec<String>, now: Instant) -> Option<Move> {
        if !self.seen.contains(&list) {
            self.seen.push(list);
        }
        self.orders.push(order);
        self.at = now;
        if self.sending {
            self.queued.push_back(mv);
            return None;
        }
        self.sending = true;
        Some(mv)
    }

    /// herdr carried out the move sent: the next to send.
    pub(super) fn sent(&mut self, now: Instant) -> Option<Move> {
        self.at = now;
        let next = self.queued.pop_front();
        self.sending = next.is_some();
        next
    }

    /// A move is out, or waits to go.
    pub(super) fn busy(&self) -> bool {
        self.sending
    }

    /// The order to show while herdr lists `list`: the last drop's, while herdr lists what it had at a drop or a
    /// drop's order on the way; not once it lists the last one (done), something no drop leads through (herdr's
    /// word wins: the TUI moved one), or nothing has been heard for `PENDING_FOR`.
    pub(super) fn order(&self, list: &[String], now: Instant) -> Option<&[String]> {
        let last = self.orders.last()?;
        let on_the_way = self.seen.iter().chain(&self.orders).any(|l| l.as_slice() == list);
        (list != last.as_slice() && on_the_way && now.saturating_duration_since(self.at) < PENDING_FOR).then_some(last.as_slice())
    }
}

/// How fast the tree scrolls by itself, in px a frame, with the pointer `at` near its view's top (up) or bottom
/// (down) edge during a drag, fastest once past it (above or below the tree, over its width); 0 elsewhere.
pub(super) fn edge_scroll(view: Bounds<Pixels>, at: Point<Pixels>) -> f32 {
    const BAND: f32 = 28.;
    const FASTEST: f32 = 10.;
    if at.x < view.origin.x || at.x >= view.origin.x + view.size.width {
        return 0.;
    }
    // past an edge counts as on it
    let (top, bottom) = (f32::from(at.y - view.origin.y).max(0.), f32::from(view.origin.y + view.size.height - at.y).max(0.));
    if top < BAND {
        -FASTEST * (1. - top / BAND)
    } else if bottom < BAND {
        FASTEST * (1. - bottom / BAND)
    } else {
        0.
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

/// The row under the pointer while a row is dragged, the half it is in, the line its drop draws, and the row's
/// bounds: off them (a gap, a divider, a notice) the drop lands nowhere.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TreeDrop {
    pub over: Over,
    pub edge: Edge,
    pub line: Line,
    pub bounds: Bounds<Pixels>,
    /// The tree's scroll offset when the row's bounds were taken: a scroll since moves the row from them.
    pub offset: Pixels,
}

impl TreeDrop {
    /// The pointer `at` is still on its row.
    pub(super) fn holds(&self, at: Point<Pixels>) -> bool {
        hovered(self.bounds, at).is_some()
    }

    /// The tree scrolled to `offset` (by the drag at its edge, or a wheel): the row moved with it, and the drop holds
    /// while the row is still under the pointer (`at`, where it last moved), so a pointer resting at the edge keeps
    /// its row until the row scrolls away from it. The half of the row the pointer is in now, if it still is.
    pub(super) fn scrolled(&mut self, offset: Pixels, at: Option<Point<Pixels>>) -> Option<Edge> {
        self.bounds.origin.y += offset - self.offset;
        self.offset = offset;
        hovered(self.bounds, at?)
    }
}

/// What a scroll to `offset` since `drop`'s bounds were taken does to it: nothing, or the drop goes (its row left the
/// pointer `at`), or the pointer is in its row's other half now (`Moved`), to be planned again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Followed {
    Same,
    Gone,
    Moved(Edge),
}

pub(super) fn follow(drop: &mut Option<TreeDrop>, offset: Pixels, at: Option<Point<Pixels>>) -> Followed {
    let Some(d) = drop.as_mut().filter(|d| d.offset != offset) else { return Followed::Same };
    match d.scrolled(offset, at) {
        None => {
            *drop = None;
            Followed::Gone
        }
        Some(edge) if edge != d.edge => Followed::Moved(edge),
        Some(_) => Followed::Same,
    }
}

/// The drop a release at `at` carries out: `drop` after the scroll since its bounds were taken, if its row is still
/// under the pointer; none off it.
pub(super) fn released(mut drop: Option<TreeDrop>, offset: Pixels, at: Point<Pixels>) -> Option<TreeDrop> {
    follow(&mut drop, offset, Some(at));
    drop.filter(|d| d.holds(at))
}

/// The tree's scroll ticker while a drag rests at its edge: one at a time.
#[derive(Default)]
pub(super) struct Ticker {
    running: bool,
}

impl Ticker {
    /// Whether to start a ticker for `speed`: only when it should scroll and none runs, so a pointer jittering at the
    /// band's edge never starts a second.
    pub(super) fn start(&mut self, speed: f32) -> bool {
        let go = speed != 0. && !self.running;
        self.running |= go;
        go
    }

    /// A frame of the running ticker: whether it goes on (`speed` and a drag still on); stopped, the next rest at
    /// the edge starts one again.
    pub(super) fn tick(&mut self, speed: f32, dragging: bool) -> bool {
        let go = speed != 0. && dragging;
        self.running &= go;
        go
    }

    /// The ticker went with its window.
    pub(super) fn gone(&mut self) {
        self.running = false;
    }
}

/// Whom a workspace move goes through: the session shown's own client, or another's watcher's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Via {
    Shown,
    Watcher,
}

/// How a move to the session `shown` (or not) is sent, and its deadline: the shown one's own client; another's
/// watcher's (`watcher`: its status and whether it has a client) unless stopped or without one. Over SSH
/// (`remote`) the deadline is longer, but every move has one.
pub(super) fn move_via(shown: bool, watcher: Option<(Status, bool)>, remote: bool) -> Option<(Via, Duration)> {
    let via = match watcher {
        _ if shown => Via::Shown,
        Some((status, true)) if status != Status::Stopped => Via::Watcher,
        _ => return None,
    };
    Some((via, move_timeout(remote)))
}

/// What the end of a drag does to the peek card: nothing (`None`, no drag ended); keeps it out (`Some(true)`, the
/// pointer `at` is on it, which then counts as hovering it, as gpui forgets hover during a drag); or lets it go. The
/// card is `width` wide in a window `height` tall.
pub(super) fn peek_after_drag(ended: bool, hovered: bool, at: Point<Pixels>, width: f32, height: f32) -> Option<bool> {
    ended.then(|| hovered || on_peek(at, width, height))
}

/// How long a workspace move may wait for herdr's answer, the session shown or another: on this machine 2 s, over
/// SSH 10 s. With none, a herdr that hangs would hold every drop after it.
pub(super) fn move_timeout(remote: bool) -> Duration {
    Duration::from_secs(if remote { 10 } else { 2 })
}

/// `call` given up on after `timeout`, as an error that names the wait.
pub(super) async fn bounded<T>(timeout: Duration, call: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    match tokio::time::timeout(timeout, call).await {
        Ok(done) => done,
        Err(_) => Err(anyhow::anyhow!("no answer in {} s", timeout.as_secs())),
    }
}

/// Whether the peek card still has the pointer `at` when a drag ends: gpui forgets hover during a drag, so the card
/// is measured, `width` wide, 6 px in from the window's left, top and bottom (`height` tall); the edge strip that
/// brings it out counts too.
pub(super) fn on_peek(at: Point<Pixels>, width: f32, height: f32) -> bool {
    let (x, y) = (f32::from(at.x), f32::from(at.y));
    (0. ..=6. + width).contains(&x) && (6. ..=height - 6.).contains(&y)
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

    /// `s` (the session `key`'s) in the order of drops herdr hasn't carried out yet; `None` when there are none.
    pub(super) fn pending_order(&self, key: &SessionKey, s: &Snapshot) -> Option<Snapshot> {
        let order = self.tree.pending.get(key)?.order(&ids_of(s), Instant::now())?;
        Some(with_order(s, order))
    }

    /// The shown session's snapshot in the tree's order: what the keys' steps and the command panel walk.
    pub(super) fn shown_in_order(&self) -> Option<std::borrow::Cow<'_, Snapshot>> {
        let s = self.snapshot.as_ref()?;
        Some(match self.shown_key().and_then(|k| self.pending_order(&k, s)) {
            Some(ordered) => std::borrow::Cow::Owned(ordered),
            None => std::borrow::Cow::Borrowed(s),
        })
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
        // a drag begun before a search: no drop among the rows it hides
        if !super::sidebar_search::drags_while(self.search_query(cx).is_some()) {
            return;
        }
        if self.tree.view.is_some_and(|v| !v.contains(&e.event.position)) {
            return;
        }
        let Some(edge) = hovered(e.bounds, e.event.position) else { return };
        let d = e.drag(cx).clone();
        // the bounds are the last paint's, at the offset it was painted with: a scroll since moves the row from them
        let offset = self.tree.painted_offset;
        let next = self.plan_drop(&d, &over, edge).map(|(_, line)| TreeDrop { over, edge, line, bounds: e.bounds, offset });
        self.tree.dragged = Some(d);
        if self.tree.drop != next {
            self.tree.drop = next;
            cx.notify();
        }
    }

    /// The tree's drag listener, heard before its rows': where its view is; off the row it was over (a gap, a
    /// divider, a notice, off the tree) a drop lands nowhere until another row takes it; near the view's top or
    /// bottom edge the tree scrolls.
    pub(super) fn tree_drag_left(&mut self, e: &DragMoveEvent<DraggedTreeItem>, cx: &mut Context<Self>) {
        self.tree.view = Some(e.bounds);
        self.tree.pointer = Some(e.event.position);
        self.follow_scroll(cx);
        if self.tree.drop.as_ref().is_some_and(|d| !d.holds(e.event.position)) {
            self.tree.drop = None;
            cx.notify();
        }
        let speed = edge_scroll(e.bounds, e.event.position);
        self.tree.scroll_speed = speed;
        if self.tree.ticker.start(speed) {
            self.scroll_while_dragging(cx);
        }
    }

    /// The drop's row follows a scroll of the tree since its bounds were taken: it goes once it leaves the pointer,
    /// and is planned again when the pointer is in its other half now.
    fn follow_scroll(&mut self, cx: &mut Context<Self>) {
        let offset = self.tree.scroll.offset().y;
        match follow(&mut self.tree.drop, offset, self.tree.pointer) {
            Followed::Same => {}
            Followed::Gone => cx.notify(),
            Followed::Moved(edge) => {
                let (Some(d), Some(drop)) = (self.tree.dragged.clone(), self.tree.drop.clone()) else { return };
                self.tree.drop = self.plan_drop(&d, &drop.over, edge).map(|(_, line)| TreeDrop { edge, line, ..drop });
                cx.notify();
            }
        }
    }

    /// Scrolls the tree a frame's worth at a time while a drag rests near its edge, one ticker at a time. Rows move
    /// under a still pointer: the drop's row moves with them and goes once it leaves the pointer; the pointer's next
    /// move finds the row under it.
    fn scroll_while_dragging(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                let go_on = this.update(cx, |this, cx| {
                    let speed = this.tree.scroll_speed;
                    if !this.tree.ticker.tick(speed, cx.has_active_drag()) {
                        this.tree.scroll_speed = 0.;
                        return false;
                    }
                    let (offset, max) = (this.tree.scroll.offset(), this.tree.scroll.max_offset());
                    let y = (offset.y - px(speed)).clamp(-max.y, px(0.));
                    if y != offset.y {
                        this.tree.scroll.set_offset(gpui::point(offset.x, y));
                        this.follow_scroll(cx);
                        cx.notify();
                    }
                    true
                });
                if !matches!(go_on, Ok(true)) {
                    // the window went: no ticker runs
                    let _ = this.update(cx, |this, _| this.tree.ticker.gone());
                    break;
                }
            }
        })
        .detach();
    }

    /// Where a drag's line is drawn now.
    pub(super) fn tree_line(&self) -> Option<&Line> {
        self.tree.drop.as_ref().map(|d| &d.line)
    }

    /// A release over the tree, `at` the pointer: on the row the line is at, planned again against what is shown
    /// now, then carried out.
    pub(super) fn drop_tree_item(&mut self, d: &DraggedTreeItem, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !super::sidebar_search::drags_while(self.search_query(cx).is_some()) {
            self.tree.drop = None;
            return;
        }
        // a wheel scroll since the last move moved the rows under the pointer
        let Some(TreeDrop { over, edge, .. }) = released(self.tree.drop.take(), self.tree.scroll.offset().y, at) else { return };
        cx.notify();
        let plan = self.plan_drop(d, &over, edge).map(|(plan, _)| plan);
        if matches!(plan, Some(Plan::Sessions(_) | Plan::Machines(_))) && SettingsGlobal::of(cx).error.is_some() {
            return crate::ui::toast::error(cx, "Couldn't keep the new order: the settings file has an error to fix first");
        }
        let known = self.tree.machines_known;
        match plan {
            Some(Plan::Workspaces(key, mv)) => self.move_workspaces(key, mv, cx),
            Some(Plan::Sessions(order)) => {
                let off: Vec<String> = self.watchers.disabled.iter().map(|m| format!("machine:{}", m.id)).collect();
                let saved = SettingsGlobal::of(cx).settings.session_order.clone();
                let order = merged(order, &saved, |k| kept_unlisted(k, &off, known, k.starts_with("machine:")));
                save_from_sidebar(cx, |s| s.session_order = order);
                self.reorder_places(cx);
            }
            Some(Plan::Machines(order)) => {
                let off: Vec<String> = self.watchers.disabled.iter().map(|m| m.target.clone()).collect();
                let saved = SettingsGlobal::of(cx).settings.machine_order.clone();
                let order = merged(order, &saved, |t| kept_unlisted(t, &off, known, true));
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

    /// A drop's `move_block` for the session `key`: the tree shows its order at once, built on the drops before it
    /// that herdr hasn't carried out, and it is sent once they are.
    fn move_workspaces(&mut self, key: SessionKey, mv: Move, cx: &mut Context<Self>) {
        let Some(s) = self.session_snapshot(&key) else { return };
        let list = ids_of(s);
        let shown = self.pending_order(&key, s).map(|s| ids_of(&s)).unwrap_or_else(|| list.clone());
        let order = moved(&shown, &mv.ids, mv.before.as_deref());
        let now = Instant::now();
        let pending = self.tree.pending.entry(key.clone()).or_insert_with(|| Pending::new(now));
        if let Some(mv) = pending.push(mv, list, order, now) {
            self.send_move(key, mv, cx);
        }
        self.notify_when_stale(cx);
    }

    /// Sends a move through the session's own client when shown, another's watcher's (2 s here, 10 s over SSH);
    /// once herdr answers, the next drop waiting goes. A refusal puts herdr's order back and drops the drops
    /// waiting, which built on it, with a toast.
    fn send_move(&mut self, key: SessionKey, mv: Move, cx: &mut Context<Self>) {
        let command = Command::MoveWorkspaces { workspace_ids: mv.ids, before: mv.before };
        let watcher = self.watchers.get(&key);
        let remote = watcher.is_some_and(|w| w.kind() == Kind::Remote);
        // the shown session's call has its deadline too: a herdr that hangs fails it, and the drops after it go
        let via = move_via(self.shown_key().as_ref() == Some(&key), watcher.map(|w| (w.status, w.client.is_some())), remote);
        let (client, timeout) = match via {
            Some((Via::Shown, timeout)) => (self.client.clone(), timeout),
            Some((Via::Watcher, timeout)) if let Some(client) = watcher.and_then(|w| w.client.clone()) => (client, timeout),
            _ => {
                self.tree.pending.remove(&key);
                return crate::ui::toast::error(cx, "Couldn't move the workspace: not connected to its session");
            }
        };
        let (method, params) = command.request();
        let call = self.rt.spawn(async move { bounded(timeout, client.call(method, params)).await });
        cx.spawn(async move |this, cx| {
            let result = call.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        // another session's watcher asks it now, rather than at its next poll
                        if this.shown_key().as_ref() != Some(&key) {
                            this.watchers.poll_now(&key, Instant::now());
                        }
                        if let Some(next) = this.tree.pending.get_mut(&key).and_then(|p| p.sent(Instant::now())) {
                            this.send_move(key.clone(), next, cx);
                        }
                        this.notify_when_stale(cx);
                    }
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

    /// Draws again once a drop's order would go stale, so it never outstays `PENDING_FOR` waiting for a frame.
    fn notify_when_stale(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PENDING_FOR + Duration::from_millis(50)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// Once no drag is under way (released anywhere): its line goes, and a peek it held out hides unless the
    /// pointer is on it. Drops herdr has carried out, with none out, go too.
    pub(super) fn tidy_tree_drag(&mut self, window: &Window, cx: &mut Context<Self>) {
        let dragging = cx.has_active_drag();
        if !dragging {
            self.tree.drop = None;
            self.tree.view = None;
            self.tree.pointer = None;
            self.tree.dragged = None;
            // gpui forgets hover during a drag: the pointer itself says whether it is still on the card
            let height = f32::from(window.viewport_size().height);
            let ended = std::mem::take(&mut self.tree.was_dragging);
            match peek_after_drag(ended, self.peek_hovered, window.mouse_position(), self.sidebar_width, height) {
                // on it: a drag shorter than the linger would otherwise hide it under the pointer
                Some(true) => self.peek_hovered = true,
                Some(false) => self.hover_peek(false, cx),
                None => {}
            }
        } else {
            // a wheel scroll mid drag: the drop's row moves with the rows
            self.follow_scroll(cx);
        }
        // the offset this frame's rows are painted at, which their bounds in the next drag moves are taken at
        self.tree.painted_offset = self.tree.scroll.offset().y;
        self.tree.was_dragging |= dragging;
        let now = Instant::now();
        let lists: Vec<(SessionKey, Vec<String>)> =
            self.tree.pending.keys().map(|k| (k.clone(), self.session_snapshot(k).map(ids_of).unwrap_or_default())).collect();
        for (key, list) in lists {
            if self.tree.pending.get(&key).is_some_and(|p| !p.busy() && p.order(&list, now).is_none()) {
                self.tree.pending.remove(&key);
            }
        }
    }
}

/// herdr's list of the snapshot's workspaces.
fn ids_of(s: &Snapshot) -> Vec<String> {
    s.workspaces.iter().map(|w| w.workspace_id.clone()).collect()
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
        // the group goes as one block, its checkout first, before what followed its checkout: herdr's list keeps
        // the group together where it was
        let reordered = Some((ids(&["m", "m-y", "m-x"]), Some("b".into())));
        assert_eq!(mv(kid("m-y"), "m-x", Edge::Above), reordered);
        assert_eq!(mv(kid("m-y"), "m", Edge::Below), reordered);
        assert_eq!(mv(kid("m-x"), "m-y", Edge::Below), reordered, "after the last sibling");
        let (m, _) = plan_workspaces(&ws(), &kid("m-x"), "m-y", Edge::Below).unwrap();
        assert_eq!(moved(&ws().list, &m.ids, m.before.as_deref()), ids(&["a", "m", "m-y", "m-x", "b", "c"]));
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
    fn a_worktree_never_crosses_sessions() {
        let both = |_: &SessionKey| Some(ws());
        let drop = |over: Over| plan(&key("/s/default"), &kid("m-y"), &over, Edge::Above, both, &places());
        assert!(matches!(drop(Over::Workspace(key("/s/default"), "m-x".into())), Some((Plan::Workspaces(..), _))));
        assert_eq!(drop(Over::Workspace(key("/s/menqal"), "m-x".into())), None);
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
        assert_eq!(order, ids(&["machine:off", "/s/menqal", "/s/default"]));
    }

    #[test]
    fn a_machine_turned_off_comes_back_where_it_was() {
        // a, off, b on one target; b dragged above a while off is turned off
        let saved = ids(&["machine:a", "machine:off", "machine:b"]);
        let order = merged(ids(&["machine:b", "machine:a"]), &saved, |k| k == "machine:off");
        assert_eq!(order, ids(&["machine:b", "machine:off", "machine:a"]));
        let back = apply_order(ids(&["machine:a", "machine:b", "machine:off"]), &order, String::clone);
        assert_eq!(back, ids(&["machine:b", "machine:off", "machine:a"]), "not last");
    }

    #[test]
    fn unlisted_machines_are_kept_while_herdr_cannot_list_them() {
        let off = ids(&["machine:off"]);
        assert!(kept_unlisted("machine:off", &off, true, true));
        assert!(!kept_unlisted("machine:gone", &off, true, true), "deleted");
        assert!(kept_unlisted("machine:x", &off, false, true), "machines unknown");
        assert!(!kept_unlisted("/s/old", &off, false, false), "a session not listed was deleted");
        // a session dragged while the machines are unknown leaves their saved order alone
        let saved = ids(&["machine:b", "/s/default", "machine:a", "/s/menqal"]);
        let order = merged(ids(&["/s/menqal", "/s/default"]), &saved, |k| kept_unlisted(k, &off, false, k.starts_with("machine:")));
        assert_eq!(order, ids(&["machine:b", "/s/menqal", "machine:a", "/s/default"]));
    }

    fn one(id: &str) -> Move {
        Move { ids: ids(&[id]), before: None }
    }

    #[test]
    fn a_drops_order_shows_until_herdrs_list_changes() {
        let t0 = Instant::now();
        let mut p = Pending::new(t0);
        assert_eq!(p.push(one("c"), ids(&["a", "b", "c"]), ids(&["c", "a", "b"]), t0), Some(one("c")));
        assert_eq!(p.order(&ids(&["a", "b", "c"]), t0), Some(&ids(&["c", "a", "b"])[..]));
        assert_eq!(p.order(&ids(&["c", "a", "b"]), t0), None, "herdr carried it out");
        assert_eq!(p.order(&ids(&["b", "a", "c"]), t0), None, "something else moved it: herdr's word wins");
        assert_eq!(p.order(&ids(&["a", "b", "c"]), t0 + PENDING_FOR), None, "nothing heard for too long");
        assert!(PENDING_FOR > Duration::from_secs(10), "outlasts the slowest call");
    }

    #[test]
    fn two_drops_in_a_row_go_one_at_a_time_and_show_the_last() {
        let t0 = Instant::now();
        let mut p = Pending::new(t0);
        let base = ids(&["a", "b", "c"]);
        let first = moved(&base, &ids(&["c"]), Some("a"));
        assert_eq!(p.push(one("c"), base.clone(), first.clone(), t0), Some(one("c")));
        // the second drop builds on the first's order, which herdr hasn't listed yet
        let shown = p.order(&base, t0).unwrap().to_vec();
        let second = moved(&shown, &ids(&["b"]), Some("c"));
        assert_eq!(second, ids(&["b", "c", "a"]));
        assert_eq!(p.push(one("b"), base.clone(), second.clone(), t0), None, "waits for the first");
        assert!(p.busy());
        // the first carried out: the tree keeps the last order, not the one on the way
        assert_eq!(p.order(&first, t0), Some(&second[..]));
        assert_eq!(p.sent(t0), Some(one("b")));
        assert!(p.busy());
        assert_eq!(p.order(&second, t0), None);
        assert_eq!(p.sent(t0), None);
        assert!(!p.busy());
    }

    #[test]
    fn the_tree_scrolls_near_its_edges_while_dragging() {
        let view = Bounds::new(gpui::point(px(0.), px(100.)), gpui::size(px(200.), px(300.)));
        let at = |y: f32| edge_scroll(view, gpui::point(px(10.), px(y)));
        assert!(at(101.) < 0. && at(101.) < at(120.), "up, faster at the edge");
        assert!(at(399.) > 0.);
        assert_eq!(at(250.), 0.);
        // past an edge, over the tree's width: the fastest, so the scroll goes on
        assert_eq!(at(90.), -10., "above the tree");
        assert_eq!(at(450.), 10., "below it");
        assert_eq!(edge_scroll(view, gpui::point(px(250.), px(90.))), 0., "beside it");
        assert_eq!(edge_scroll(view, gpui::point(px(-5.), px(390.))), 0.);
    }

    #[test]
    fn one_scroll_ticker_runs_at_a_time_and_a_stopped_one_starts_again() {
        let mut t = Ticker::default();
        assert!(!t.start(0.), "nothing to scroll");
        assert!(t.start(-4.));
        assert!(!t.start(-4.), "one runs already: a jitter at the band's edge starts no second");
        assert!(t.tick(-4., true), "goes on while resting at the edge");
        assert!(!t.tick(0., true), "left the band: stops");
        assert!(t.start(6.), "back at the edge: scrolls again");
        assert!(!t.tick(6., false), "the drag ended");
        assert!(t.start(6.), "the next drag's rest scrolls");
        t.gone();
        assert!(t.start(6.), "its window went: a new one may start");
    }

    #[test]
    fn a_move_goes_through_its_session_s_client_always_with_a_deadline() {
        let s = |status| Some((status, true));
        assert_eq!(move_via(true, s(Status::Shown), false), Some((Via::Shown, Duration::from_secs(2))), "the shown one has a deadline too");
        assert_eq!(move_via(true, None, true), Some((Via::Shown, Duration::from_secs(10))), "over SSH, a socket of one's own or not");
        assert_eq!(move_via(false, s(Status::Live), true), Some((Via::Watcher, Duration::from_secs(10))));
        assert_eq!(move_via(false, s(Status::Unreachable), false), Some((Via::Watcher, Duration::from_secs(2))));
        assert_eq!(move_via(false, s(Status::Stopped), false), None, "no server");
        assert_eq!(move_via(false, Some((Status::Live, false)), false), None, "no client");
        assert_eq!(move_via(false, None, false), None);
    }

    #[test]
    fn a_drag_ending_on_the_peek_card_keeps_it_out() {
        let at = |x: f32, y: f32| gpui::point(px(x), px(y));
        assert_eq!(peek_after_drag(false, false, at(500., 300.), 240., 800.), None, "no drag ended");
        assert_eq!(peek_after_drag(true, false, at(100., 300.), 240., 800.), Some(true), "on it though gpui forgot the hover");
        assert_eq!(peek_after_drag(true, false, at(245., 300.), 240., 800.), Some(true), "its right end, 6 px in plus its width");
        assert_eq!(peek_after_drag(true, false, at(247., 300.), 240., 800.), Some(false), "past it");
        assert_eq!(peek_after_drag(true, false, at(100., 4.), 240., 800.), Some(false), "over its top inset");
        assert_eq!(peek_after_drag(true, false, at(100., 795.), 240., 800.), Some(false), "under its bottom inset");
        assert_eq!(peek_after_drag(true, true, at(500., 300.), 240., 800.), Some(true), "still hovered");
    }

    fn aimed(edge: Edge) -> TreeDrop {
        let bounds = Bounds::new(gpui::point(px(0.), px(100.)), gpui::size(px(200.), px(30.)));
        TreeDrop { over: Over::Machine("you@box".into()), edge, line: Line::Machine("you@box".into(), edge), bounds, offset: px(0.) }
    }

    #[test]
    fn a_scroll_moves_the_drop_s_row_and_a_release_follows_it() {
        let at = |y: f32| gpui::point(px(10.), px(y));
        // no scroll since: as aimed
        let mut drop = Some(aimed(Edge::Below));
        assert_eq!(follow(&mut drop, px(0.), Some(at(120.))), Followed::Same);
        // scrolled 10 px: the row at 90..120, the pointer at 118 still in its lower half
        assert_eq!(follow(&mut drop, px(-10.), Some(at(118.))), Followed::Same);
        // 10 more: at 80..110, its middle at 95: a pointer at 92 is in its upper half
        assert_eq!(follow(&mut drop, px(-20.), Some(at(92.))), Followed::Moved(Edge::Above), "the pointer in its upper half now");
        assert_eq!(follow(&mut drop, px(-40.), Some(at(92.))), Followed::Gone, "scrolled away from the pointer");
        assert!(drop.is_none());
        // a wheel scroll between the last move and the release
        assert!(released(Some(aimed(Edge::Above)), px(-20.), at(85.)).is_some(), "the row moved under the pointer");
        assert!(released(Some(aimed(Edge::Above)), px(-20.), at(115.)).is_none(), "the row moved away from it");
        assert!(released(Some(aimed(Edge::Above)), px(0.), at(115.)).is_some());
        assert!(released(Some(aimed(Edge::Above)), px(0.), at(140.)).is_none(), "no scroll, released off its row");
        assert!(released(None, px(0.), at(115.)).is_none());
    }

    #[test]
    fn a_drop_s_row_moves_with_a_scroll_and_holds_while_under_the_pointer() {
        let bounds = Bounds::new(gpui::point(px(0.), px(100.)), gpui::size(px(200.), px(30.)));
        let mut d = TreeDrop { over: Over::Machine("you@box".into()), edge: Edge::Above, line: Line::Machine("you@box".into(), Edge::Above), bounds, offset: px(0.) };
        // scrolled down 10 px: the row is at 90..120, still under a pointer resting at 115, in its lower half
        assert_eq!(d.scrolled(px(-10.), Some(gpui::point(px(10.), px(115.)))), Some(Edge::Below));
        assert_eq!(d.bounds.origin.y, px(90.));
        // 20 px more: at 70..100, no longer under it
        assert_eq!(d.scrolled(px(-30.), Some(gpui::point(px(10.), px(115.)))), None);
        assert_eq!(d.scrolled(px(-30.), None), None, "no pointer known: nowhere");
    }

    #[test]
    fn a_move_herdr_never_answers_fails_after_its_deadline() {
        assert_eq!(move_timeout(false), Duration::from_secs(2));
        assert_eq!(move_timeout(true), Duration::from_secs(10));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let hung = std::future::pending::<anyhow::Result<()>>();
        // the outer deadline keeps a bounded call that never gives up from hanging the test
        let done = rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), bounded(Duration::from_millis(20), hung)).await });
        let err = done.expect("given up on, not hung").unwrap_err();
        assert_eq!(format!("{err}"), "no answer in 0 s");
        let answered = rt.block_on(async { bounded(Duration::from_millis(20), async { anyhow::Ok(7) }).await });
        assert_eq!(answered.unwrap(), 7);
    }

    #[test]
    fn the_peek_card_keeps_a_pointer_still_on_it_after_a_drag() {
        assert!(on_peek(gpui::point(px(100.), px(300.)), 240., 800.), "on the card");
        assert!(on_peek(gpui::point(px(2.), px(300.)), 240., 800.), "on the edge strip");
        assert!(!on_peek(gpui::point(px(260.), px(300.)), 240., 800.), "over the panes");
        assert!(!on_peek(gpui::point(px(100.), px(797.)), 240., 800.), "under it");
    }

    #[test]
    fn a_drop_off_its_row_lands_nowhere() {
        let bounds = Bounds::new(gpui::point(px(0.), px(100.)), gpui::size(px(200.), px(30.)));
        let d = TreeDrop { over: Over::Machine("you@box".into()), edge: Edge::Above, line: Line::Machine("you@box".into(), Edge::Above), bounds, offset: px(0.) };
        assert!(d.holds(gpui::point(px(10.), px(110.))));
        assert!(!d.holds(gpui::point(px(10.), px(131.))), "the gap under it");
    }
}
