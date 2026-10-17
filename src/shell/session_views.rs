//! What each session showed when it was left, so coming back shows it as it was: its tab and workspace, herdr's
//! focus then, and its panes' terminals, parked (no stream: their sessions detach, their grids stay and paint at
//! once, and attach again when shown). Generic over the pane handle, so the bookkeeping is tested without gpui. The
//! keyboard's pane per tab is kept by `KeyboardMemory`, for every session, across restarts.
use super::watchers::SessionKey;
use crate::model::Snapshot;
use gpui::Focusable;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

/// Panes kept parked per session, the shown tab's first (the `recent_panes` order): each keeps a grid.
pub(super) const PARKED_PER_SESSION: usize = 8;
/// Panes kept parked in all: past it, the session left longest ago loses its panes first (its view stays). A grid
/// of 200×60 measured about 1 MB, a history copy of herdr's 1000 lines about 2.5 MB: some 35 MB at the caps.
pub(super) const PARKED_TOTAL: usize = 24;
/// Parked panes that keep their local copy of herdr's history (scrolled back when left); the rest go back to the
/// live grid, which herdr's own scroll offset repaints on return.
pub(super) const HISTORY_COPIES: usize = 4;
/// How long a view's keyboard pane waits to be shown before it is let go (as a goal is).
pub(super) const RESTORE_EXPIRY: Duration = Duration::from_secs(15);

/// A parked pane: its id, its terminal (`""` when the session sent no snapshot before it was left) and its handle.
pub(super) struct Parked<P> {
    pub pane_id: String,
    pub terminal_id: String,
    pub pane: P,
}

/// A session as it was left.
pub(super) struct SessionView<P> {
    pub selected_tab: Option<String>,
    pub shown_workspace: Option<String>,
    /// herdr's focused tab then: a different one on return means herdr's focus moved while you were away.
    pub herdr_focus: Option<String>,
    /// Most recently shown first.
    pub panes: Vec<Parked<P>>,
    pub left_at: Instant,
}

/// Every session's view but the shown one's.
pub(super) struct SessionViews<P> {
    map: HashMap<SessionKey, SessionView<P>>,
    /// A session was just left: the keyboard must leave its panes, parked now and out of the window's.
    refocus: bool,
}

impl<P> Default for SessionViews<P> {
    fn default() -> Self {
        Self { map: HashMap::new(), refocus: false }
    }
}

impl<P> SessionViews<P> {
    /// Keeps `view` for `key` (replacing an older one); returns the panes the caps let go: past
    /// [`PARKED_PER_SESSION`] of its own, then, past [`PARKED_TOTAL`], the least recent ones of the session left
    /// longest ago.
    pub(super) fn park(&mut self, key: SessionKey, mut view: SessionView<P>) -> Vec<P> {
        self.refocus = true;
        let mut evicted: Vec<P> = view.panes.drain(PARKED_PER_SESSION.min(view.panes.len())..).map(|p| p.pane).collect();
        if let Some(old) = self.map.insert(key, view) {
            evicted.extend(old.panes.into_iter().map(|p| p.pane));
        }
        while self.map.values().map(|v| v.panes.len()).sum::<usize>() > PARKED_TOTAL {
            let Some(oldest) = self.map.values_mut().filter(|v| !v.panes.is_empty()).min_by_key(|v| v.left_at) else { break };
            evicted.extend(oldest.panes.pop().map(|p| p.pane));
        }
        evicted
    }

    /// Once after a park: the render gives the keyboard to the window, before herdr's focused pane takes it, so a
    /// parked pane never keeps it (nor types into another session).
    pub(super) fn take_refocus(&mut self) -> bool {
        std::mem::take(&mut self.refocus)
    }

    /// The view of the session switched to, which leaves the store.
    pub(super) fn unpark(&mut self, key: &SessionKey) -> Option<SessionView<P>> {
        self.map.remove(key)
    }

    /// Only the sessions `keep` names stay (herdr's list, and the shown one); returns the panes of the others.
    pub(super) fn keep_sessions(&mut self, keep: impl Fn(&SessionKey) -> bool) -> Vec<P> {
        let gone: Vec<SessionKey> = self.map.keys().filter(|k| !keep(k)).cloned().collect();
        gone.into_iter().filter_map(|k| self.map.remove(&k)).flat_map(|v| v.panes.into_iter().map(|p| p.pane)).collect()
    }

    /// The session's server stopped: its terminals are gone, its view (tab, workspace) stays.
    pub(super) fn drop_panes(&mut self, key: &SessionKey) -> Vec<P> {
        self.map.get_mut(key).map(|v| v.panes.drain(..).map(|p| p.pane).collect()).unwrap_or_default()
    }

    /// The session's watcher heard a new snapshot: its parked panes that left it, or whose id now names another
    /// terminal, go.
    pub(super) fn prune_against(&mut self, key: &SessionKey, snapshot: &Snapshot) -> Vec<P> {
        let Some(view) = self.map.get_mut(key) else { return Vec::new() };
        let (stale, kept): (Vec<_>, Vec<_>) = view.panes.drain(..).partition(|p| stale(&p.pane_id, &p.terminal_id, snapshot));
        view.panes = kept;
        stale.into_iter().map(|p| p.pane).collect()
    }

    /// The parked panes holding a history copy past [`HISTORY_COPIES`] (`has_history`), the sessions left last
    /// keeping theirs: those go back to their live grid.
    pub(super) fn beyond_history(&self, has_history: impl Fn(&P) -> bool) -> Vec<&P> {
        let mut views: Vec<&SessionView<P>> = self.map.values().collect();
        views.sort_by_key(|v| std::cmp::Reverse(v.left_at));
        views.into_iter().flat_map(|v| v.panes.iter().map(|p| &p.pane)).filter(|p| has_history(p)).skip(HISTORY_COPIES).collect()
    }

    #[cfg(test)]
    fn parked(&self, key: &SessionKey) -> Vec<&P> {
        self.map.get(key).map(|v| v.panes.iter().map(|p| &p.pane).collect()).unwrap_or_default()
    }
}

/// A parked pane no longer fits `snapshot`: herdr lists no such pane, or the id now names another terminal (herdr
/// gives a closed pane's id to a new one). An unknown terminal (`""`) is taken on the id alone.
pub(super) fn stale(pane_id: &str, terminal_id: &str, snapshot: &Snapshot) -> bool {
    match snapshot.panes.iter().find(|p| p.pane_id == pane_id) {
        None => true,
        Some(p) => !terminal_id.is_empty() && p.terminal_id != terminal_id,
    }
}

/// A view checked against the session's snapshot on return (its watcher's, or none yet).
pub(super) struct Restored<P> {
    pub selected_tab: Option<String>,
    pub shown_workspace: Option<String>,
    pub panes: Vec<Parked<P>>,
    /// Parked panes that no longer fit: their grids go.
    pub dropped: Vec<P>,
}

/// `view` against `snapshot`: the tab herdr's focus calls for ([`tab_on_return`]) while it lives, the workspace while
/// it lives, the panes that still fit. Without a snapshot everything is kept, to be checked on the session's first.
pub(super) fn validate<P>(view: SessionView<P>, snapshot: Option<&Snapshot>) -> Restored<P> {
    let focus = snapshot.and_then(|s| s.focused_tab_id.as_deref());
    let tab = tab_on_return(view.selected_tab.as_deref(), view.herdr_focus.as_deref(), focus).map(str::to_string);
    let Some(snapshot) = snapshot else {
        return Restored { selected_tab: tab, shown_workspace: view.shown_workspace, panes: view.panes, dropped: Vec::new() };
    };
    let tab = tab.filter(|t| snapshot.tabs.iter().any(|x| &x.tab_id == t));
    // herdr's focus moved to another workspace's tab: that workspace, not the one left
    let workspace = match tab.as_deref().and_then(|t| snapshot.tabs.iter().find(|x| x.tab_id == t)) {
        Some(t) => Some(t.workspace_id.clone()),
        None => view.shown_workspace.filter(|w| snapshot.workspaces.iter().any(|x| &x.workspace_id == w)),
    };
    let (dropped, panes): (Vec<_>, Vec<_>) = view.panes.into_iter().partition(|p| stale(&p.pane_id, &p.terminal_id, snapshot));
    Restored { selected_tab: tab, shown_workspace: workspace, panes, dropped: dropped.into_iter().map(|p| p.pane).collect() }
}

/// The tab to show on return: herdr's focused tab now (`focus`) when it moved since the session was left (its TUI,
/// another window), else the tab left (`left`).
pub(super) fn tab_on_return<'a>(left: Option<&'a str>, focus_then: Option<&str>, focus: Option<&'a str>) -> Option<&'a str> {
    match focus {
        Some(now) if focus_then != Some(now) => Some(now),
        _ => left,
    }
}

/// The keyboard's pane to give back once its tab shows: after a switch of session, or of tab or workspace.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Restore {
    pub tab: String,
    pub pane: String,
    pub at: Instant,
}

/// What a render does with a [`Restore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Gives the pane the keyboard.
    Apply,
    /// Not yet: its tab or pane is not shown, or something covers the panes.
    Wait,
    /// Let go: too old, or a goal (an agent's pane, a workspace asked for) came after it and wins.
    Drop,
}

/// `restore` this render: `shown_tab` is the tab shown, `pane_shown` whether its pane is among the tab's,
/// `goal` whether a goal waits (it wins), `covered` whether Settings or a file tab covers the panes.
pub(super) fn restore_step(restore: &Restore, shown_tab: Option<&str>, pane_shown: bool, goal: bool, covered: bool, now: Instant) -> Step {
    if goal || now.duration_since(restore.at) > RESTORE_EXPIRY {
        return Step::Drop;
    }
    if shown_tab != Some(restore.tab.as_str()) || !pane_shown || covered {
        return Step::Wait;
    }
    Step::Apply
}

/// The pane a tab's keyboard was in, by its terminal too: herdr gives a closed pane's id to a new one, and a
/// restart starts the ids again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct KeyPane {
    pub pane: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub terminal: String,
}

/// Each session's keyboard pane per tab (by [`SessionKey`], then tab id).
pub(super) type KeyboardMemory = BTreeMap<String, BTreeMap<String, KeyPane>>;

impl super::Shell {
    /// Leaving the shown session `left` (before its snapshot goes to its watcher): its tab, workspace and herdr's
    /// focus are kept, and its panes parked in `recent_panes` order as far as the caps allow; the rest go. The
    /// keyboard leaves them on the next render.
    pub(super) fn park_view(&mut self, left: &SessionKey, cx: &mut gpui::Context<Self>) {
        let snapshot = self.snapshot.as_ref();
        let terminal = |id: &str| snapshot.and_then(|s| s.panes.iter().find(|p| p.pane_id == id)).map(|p| p.terminal_id.clone()).unwrap_or_default();
        let mut panes = Vec::new();
        for id in &self.recent_panes {
            let Some(pane) = self.panes.remove(id) else { continue };
            pane.update(cx, |p, cx| p.park(cx));
            panes.push(Parked { pane_id: id.clone(), terminal_id: terminal(id), pane });
        }
        let view = SessionView {
            selected_tab: self.shown_tab().map(|t| t.tab_id),
            shown_workspace: self.shown_workspace.clone(),
            herdr_focus: snapshot.and_then(|s| s.focused_tab_id.clone()),
            panes,
            left_at: Instant::now(),
        };
        let rest: Vec<_> = self.panes.drain().map(|(_, p)| p).collect();
        let evicted = self.views.park(left.clone(), view);
        self.retire_panes(evicted.into_iter().chain(rest), cx);
        let beyond: Vec<_> = self.views.beyond_history(|p| p.read(cx).has_history()).into_iter().cloned().collect();
        for pane in beyond {
            pane.update(cx, |p, cx| p.drop_history(cx));
        }
    }

    /// Back in the session `key` (its watcher's snapshot in hand): its view as it was left, checked against that
    /// snapshot; its panes are attached again as they show. No view (never shown this run): herdr's pick.
    pub(super) fn restore_view(&mut self, key: &SessionKey, cx: &mut gpui::Context<Self>) {
        let Some(view) = self.views.unpark(key) else {
            self.show_herdr_tab(None, false, cx);
            return;
        };
        let back = validate(view, self.snapshot.as_ref());
        self.retire_panes(back.dropped, cx);
        self.parked_check = Some(back.panes.iter().map(|p| (p.pane_id.clone(), p.terminal_id.clone())).collect());
        for p in back.panes {
            self.recent_panes.push(p.pane_id.clone());
            self.panes.insert(p.pane_id, p.pane);
        }
        // the workspace shows what it showed, a file tab included
        self.show_herdr_tab(back.selected_tab, false, cx);
        self.shown_workspace = back.shown_workspace;
    }

    /// The session's first snapshot of its own: the panes its view brought back that it no longer backs (closed,
    /// their id given to another terminal, ended) go, and are made afresh if shown.
    pub(super) fn check_parked(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(snapshot) = self.snapshot.as_ref() else { return };
        let Some(checks) = self.parked_check.take() else { return };
        let gone: Vec<String> = checks
            .into_iter()
            .filter(|(id, terminal)| stale(id, terminal, snapshot) || self.panes.get(id).is_some_and(|p| p.read(cx).ended().is_some()))
            .map(|(id, _)| id)
            .collect();
        for id in gone {
            self.recent_panes.retain(|r| *r != id);
            if self.focused.as_deref() == Some(id.as_str()) {
                self.focused = None;
            }
            if let Some(pane) = self.panes.remove(&id) {
                self.retire_panes([pane], cx);
            }
        }
    }

    /// Panes let go: their images leave the window's atlas on the next render.
    pub(super) fn retire_panes(&mut self, panes: impl IntoIterator<Item = gpui::Entity<crate::terminal::PaneTerminal>>, cx: &mut gpui::Context<Self>) {
        for pane in panes {
            self.retired_images.extend(pane.update(cx, |p, cx| p.set_images(Vec::new(), cx)));
        }
    }

    /// A render with `tab` shown: another tab than the last (a switch of tab, workspace or session) gives back the
    /// pane its keyboard was in, while that pane is still the terminal it was.
    pub(super) fn note_tab_shown(&mut self, key: Option<&SessionKey>, tab: Option<&str>, now: Instant) {
        let Some(tab) = tab.filter(|t| self.keyboard_tab.as_deref() != Some(*t)) else { return };
        self.keyboard_tab = Some(tab.to_string());
        let snapshot = self.snapshot.as_ref();
        let fits = |k: &&KeyPane| {
            snapshot.is_some_and(|s| s.panes.iter().any(|p| p.pane_id == k.pane && p.tab_id == tab && (k.terminal.is_empty() || p.terminal_id == k.terminal)))
        };
        let remembered = key.and_then(|k| self.keyboard.get(&k.0)?.get(tab)).filter(fits);
        self.restore = remembered.map(|k| Restore { tab: tab.to_string(), pane: k.pane.clone(), at: now });
    }

    /// After herdr's focused pane (and a goal's) took the keyboard: the remembered pane takes it once shown.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn restore_keyboard(
        &mut self,
        shown: &[crate::model::PaneFrame],
        tab: Option<&str>,
        goal: bool,
        covered: bool,
        now: Instant,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(restore) = self.restore.as_ref() else { return };
        let handle = self.panes.get(&restore.pane).filter(|_| shown.iter().any(|f| f.pane_id == restore.pane)).map(|p| p.read(cx).focus_handle(cx));
        match restore_step(restore, tab, handle.is_some(), goal, covered, now) {
            Step::Apply => {
                if let Some(handle) = handle {
                    window.focus(&handle, cx);
                }
                self.restore = None;
            }
            Step::Drop => self.restore = None,
            Step::Wait => {}
        }
    }

    /// The shown tab's pane with the keyboard is noted for the tab, unless a remembered one
    /// waits to be given it back.
    pub(super) fn note_keyboard(&mut self, key: Option<&SessionKey>, tab: Option<&str>, shown: &[crate::model::PaneFrame], window: &gpui::Window, cx: &mut gpui::Context<Self>) {
        let (Some(key), Some(tab)) = (key, tab) else { return };
        if self.restore.as_ref().is_some_and(|r| r.tab == tab) {
            return;
        }
        let Some(f) = shown.iter().find(|f| self.panes.get(&f.pane_id).is_some_and(|p| p.read(cx).focus_handle(cx).is_focused(window))) else { return };
        let terminal = self.snapshot.as_ref().and_then(|s| s.panes.iter().find(|p| p.pane_id == f.pane_id)).map(|p| p.terminal_id.clone()).unwrap_or_default();
        let entry = KeyPane { pane: f.pane_id.clone(), terminal };
        let tabs = self.keyboard.entry(key.0.clone()).or_default();
        if tabs.get(tab) != Some(&entry) {
            tabs.insert(tab.to_string(), entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    /// Panes as (pane, tab, terminal); tabs and workspaces follow from them (`w1:t1` is in `w1`).
    fn snapshot(panes: &[(&str, &str, &str)], focused_tab: Option<&str>) -> Snapshot {
        let mut tabs: Vec<&str> = panes.iter().map(|p| p.1).collect();
        tabs.dedup();
        let ws = |tab: &str| tab.split(':').next().unwrap().to_string();
        let mut workspaces: Vec<String> = tabs.iter().map(|t| ws(t)).collect();
        workspaces.dedup();
        let workspaces: Vec<_> = workspaces.iter().enumerate().map(|(i, w)| json!({"workspace_id": w, "label": w, "number": i + 1, "active_tab_id": null, "agent_status": "idle"})).collect();
        let tabs: Vec<_> = tabs.iter().enumerate().map(|(i, t)| json!({"tab_id": t, "workspace_id": ws(t), "label": t, "number": i + 1, "agent_status": "idle"})).collect();
        let panes: Vec<_> = panes.iter().map(|(p, t, term)| json!({"pane_id": p, "tab_id": t, "workspace_id": ws(t), "terminal_id": term, "agent_status": "idle"})).collect();
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": focused_tab, "focused_pane_id": null, "workspaces": workspaces, "tabs": tabs, "panes": panes, "layouts": []}})).unwrap()
    }

    fn view<S: AsRef<str>>(panes: &[S], left_at: Instant) -> SessionView<String> {
        let panes = panes.iter().map(|p| p.as_ref()).map(|p| Parked { pane_id: p.to_string(), terminal_id: format!("term-{p}"), pane: p.to_string() }).collect();
        SessionView { selected_tab: Some("w1:t1".into()), shown_workspace: Some("w1".into()), herdr_focus: Some("w1:t1".into()), panes, left_at }
    }

    fn ids(n: usize, prefix: &str) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}:p{i}")).collect()
    }

    #[test]
    fn a_view_parks_and_comes_back_whole() {
        let mut views = SessionViews::default();
        assert!(!views.take_refocus());
        assert!(views.park(key("a"), view(&["w1:p1", "w1:p2"], Instant::now())).is_empty());
        assert!(views.take_refocus(), "the keyboard leaves the panes parked");
        assert!(!views.take_refocus(), "once");
        let back = views.unpark(&key("a")).unwrap();
        assert!(!views.take_refocus());
        assert_eq!((back.selected_tab.as_deref(), back.shown_workspace.as_deref(), back.herdr_focus.as_deref()), (Some("w1:t1"), Some("w1"), Some("w1:t1")));
        assert_eq!(back.panes.iter().map(|p| (p.pane_id.as_str(), p.terminal_id.as_str())).collect::<Vec<_>>(), [("w1:p1", "term-w1:p1"), ("w1:p2", "term-w1:p2")]);
        assert!(views.unpark(&key("a")).is_none(), "it left the store");
    }

    #[test]
    fn a_session_keeps_its_first_panes_and_all_keep_the_newest_sessions() {
        let t0 = Instant::now();
        let mut views = SessionViews::default();
        let a = ids(10, "a");
        // past PARKED_PER_SESSION: the least recent (the shown tab's are first) go
        assert_eq!(views.park(key("a"), view(&a, t0)), a[PARKED_PER_SESSION..].to_vec());
        assert_eq!(views.parked(&key("a")).len(), PARKED_PER_SESSION);
        assert!(views.park(key("b"), view(&ids(8, "b"), t0 + Duration::from_secs(1))).is_empty());
        assert!(views.park(key("c"), view(&ids(8, "c"), t0 + Duration::from_secs(2))).is_empty());
        // 32 parked: the session left longest ago (a) loses its least recent panes, down to PARKED_TOTAL
        let evicted = views.park(key("d"), view(&ids(8, "d"), t0 + Duration::from_secs(3)));
        assert_eq!(evicted, ["a:p7", "a:p6", "a:p5", "a:p4", "a:p3", "a:p2", "a:p1", "a:p0"]);
        assert!(views.parked(&key("a")).is_empty());
        assert_eq!(views.parked(&key("d")).len(), 8);
        // its view (tab, workspace) stays without panes
        assert_eq!(views.unpark(&key("a")).unwrap().selected_tab.as_deref(), Some("w1:t1"));
        // parking a session again replaces its old view, whose panes go
        assert_eq!(views.park(key("b"), view(&["b:new"], t0 + Duration::from_secs(4))).len(), 8);
    }

    #[test]
    fn unlisted_sessions_go_and_a_stopped_one_keeps_its_view() {
        let mut views = SessionViews::default();
        views.park(key("a"), view(&["w1:p1"], Instant::now()));
        views.park(key("b"), view(&["w1:p2"], Instant::now()));
        assert_eq!(views.keep_sessions(|k| *k == key("a")), ["w1:p2"]);
        assert!(views.unpark(&key("b")).is_none());
        assert_eq!(views.drop_panes(&key("a")), ["w1:p1"]);
        assert!(views.drop_panes(&key("gone")).is_empty());
        let a = views.unpark(&key("a")).unwrap();
        assert!(a.panes.is_empty());
        assert_eq!(a.selected_tab.as_deref(), Some("w1:t1"), "Tier 0 kept");
    }

    #[test]
    fn a_watcher_snapshot_drops_panes_that_left_or_changed_terminal() {
        let mut views = SessionViews::default();
        views.park(key("a"), view(&["w1:p1", "w1:p2", "w1:p3"], Instant::now()));
        // p2 closed, p3's id now names another terminal
        let snap = snapshot(&[("w1:p1", "w1:t1", "term-w1:p1"), ("w1:p3", "w1:t1", "term-new")], None);
        assert_eq!(views.prune_against(&key("a"), &snap), ["w1:p2", "w1:p3"]);
        assert_eq!(views.parked(&key("a")), [&"w1:p1".to_string()]);
    }

    #[test]
    fn only_the_newest_sessions_keep_history_copies() {
        let t0 = Instant::now();
        let mut views = SessionViews::default();
        views.park(key("a"), view(&["a1", "a2", "a3"], t0));
        views.park(key("b"), view(&["b1", "b2", "b3"], t0 + Duration::from_secs(1)));
        // every pane but b2 is scrolled back: b1, b3, a1, a2 keep theirs
        let beyond = views.beyond_history(|p| p != "b2");
        assert_eq!(beyond, [&"a3".to_string()]);
    }

    #[test]
    fn a_view_is_checked_against_the_snapshot_on_return() {
        let mut v = view(&["w1:p1", "w1:p2", "w1:p3"], Instant::now());
        v.panes[2].terminal_id.clear(); // parked before the session's first snapshot
        // p2 closed and its id reused by another terminal; p3 taken on its id
        let snap = snapshot(&[("w1:p1", "w1:t1", "term-w1:p1"), ("w1:p2", "w1:t1", "term-other"), ("w1:p3", "w1:t2", "term-x")], Some("w1:t1"));
        let back = validate(v, Some(&snap));
        assert_eq!(back.selected_tab.as_deref(), Some("w1:t1"));
        assert_eq!(back.shown_workspace.as_deref(), Some("w1"));
        assert_eq!(back.panes.iter().map(|p| p.pane_id.as_str()).collect::<Vec<_>>(), ["w1:p1", "w1:p3"]);
        assert_eq!(back.dropped, ["w1:p2"]);
        // the tab and workspace left closed: none, so the snapshot's pick shows
        let mut v = view::<&str>(&[], Instant::now());
        (v.selected_tab, v.shown_workspace, v.herdr_focus) = (Some("w9:t1".into()), Some("w9".into()), None);
        let back = validate(v, Some(&snapshot(&[("w1:p1", "w1:t1", "t")], None)));
        assert_eq!((back.selected_tab, back.shown_workspace), (None, None));
        // no snapshot yet: kept as left, for the first one to check
        let back = validate(view(&["w1:p1"], Instant::now()), None);
        assert_eq!((back.selected_tab.as_deref(), back.panes.len()), (Some("w1:t1"), 1));
    }

    #[test]
    fn herdrs_focus_moved_while_away_wins() {
        // unmoved: the tab left
        assert_eq!(tab_on_return(Some("w1:t2"), Some("w1:t2"), Some("w1:t2")), Some("w1:t2"));
        assert_eq!(tab_on_return(Some("w1:t2"), Some("w1:t1"), Some("w1:t1")), Some("w1:t2"), "herdr's focus where it was");
        // moved (the TUI went to another tab): herdr's
        assert_eq!(tab_on_return(Some("w1:t2"), Some("w1:t2"), Some("w2:t1")), Some("w2:t1"));
        // nothing known of herdr's focus: the tab left
        assert_eq!(tab_on_return(Some("w1:t2"), Some("w1:t2"), None), Some("w1:t2"));
        // the moved focus takes its workspace along
        let mut v = view::<&str>(&[], Instant::now());
        v.selected_tab = Some("w1:t1".into());
        let back = validate(v, Some(&snapshot(&[("w1:p1", "w1:t1", "a"), ("w2:p1", "w2:t1", "b")], Some("w2:t1"))));
        assert_eq!((back.selected_tab.as_deref(), back.shown_workspace.as_deref()), (Some("w2:t1"), Some("w2")));
    }

    #[test]
    fn a_restore_waits_for_its_tab_and_yields_to_a_goal() {
        let t0 = Instant::now();
        let r = Restore { tab: "w1:t1".into(), pane: "w1:p2".into(), at: t0 };
        assert_eq!(restore_step(&r, Some("w1:t1"), true, false, false, t0), Step::Apply);
        assert_eq!(restore_step(&r, Some("w1:t2"), true, false, false, t0), Step::Wait, "another tab shown");
        assert_eq!(restore_step(&r, None, false, false, false, t0), Step::Wait, "no snapshot yet");
        assert_eq!(restore_step(&r, Some("w1:t1"), false, false, false, t0), Step::Wait, "its pane not laid out yet");
        assert_eq!(restore_step(&r, Some("w1:t1"), true, false, true, t0), Step::Wait, "Settings or a file tab over the panes");
        assert_eq!(restore_step(&r, Some("w1:t1"), true, true, false, t0), Step::Drop, "a goal wins");
        assert_eq!(restore_step(&r, Some("w1:t1"), true, false, false, t0 + RESTORE_EXPIRY + Duration::from_millis(1)), Step::Drop);
        assert_eq!(restore_step(&r, Some("w1:t1"), true, false, false, t0 + RESTORE_EXPIRY), Step::Apply);
    }
}
