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

/// Whether what is kept for the session `key` stays on a read of herdr's list: it is `listed` or `shown`, a machine
/// turned `off`, or a machine while the machines could not be read (`machines_known` false: a timeout must not
/// forget them all).
pub(super) fn still_known(key: &str, listed: &[SessionKey], shown: Option<&SessionKey>, off: &[String], machines_known: bool) -> bool {
    listed.iter().any(|l| l.0 == key) || shown.is_some_and(|s| s.0 == key) || off.iter().any(|o| o == key) || (!machines_known && key.starts_with("machine:"))
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

/// The view of a session being left, read from its last snapshot (so before it goes to its watcher): herdr's focus
/// then, and each parked pane's terminal, which its return is checked against.
pub(super) fn leaving_view<P>(snapshot: Option<&Snapshot>, selected_tab: Option<String>, shown_workspace: Option<String>, mut panes: Vec<Parked<P>>, now: Instant) -> SessionView<P> {
    for p in &mut panes {
        p.terminal_id = snapshot.and_then(|s| s.panes.iter().find(|x| x.pane_id == p.pane_id)).map(|x| x.terminal_id.clone()).unwrap_or_default();
    }
    SessionView { selected_tab, shown_workspace, herdr_focus: snapshot.and_then(|s| s.focused_tab_id.clone()), panes, left_at: now }
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
    /// The pane with the keyboard when the restore was first weighed uncovered (`None` until then): the keyboard
    /// moving elsewhere since (herdr's focus, a click) was not the restore's doing, and lets it go.
    pub focus: Option<Option<String>>,
}

impl Restore {
    pub(super) fn new(tab: String, pane: String, at: Instant) -> Restore {
        Restore { tab, pane, at, focus: None }
    }
}

/// What a render knows when it weighs a [`Restore`].
pub(super) struct Moment<'a> {
    pub shown_tab: Option<&'a str>,
    /// The restore's pane is among the shown tab's (not, say, a pane hidden by another's zoom).
    pub pane_shown: bool,
    /// A goal waits (an agent's pane, a workspace asked for): it wins.
    pub goal: bool,
    /// Settings, a panel, herdr's popup or a file tab covers the panes.
    pub covered: bool,
    /// The last key or click in the window.
    pub touched: Instant,
    /// The shown pane with the keyboard now, if one has it.
    pub focus: Option<&'a str>,
    pub now: Instant,
}

/// What a render does with a [`Restore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Gives the pane the keyboard.
    Apply,
    /// Not yet: its tab or pane is not shown, or something covers the panes.
    Wait,
    /// Let go: too old, a goal came after it, you typed or clicked since, the keyboard moved without it, or its tab
    /// shows without its pane: given later, the keyboard would jump mid-command.
    Drop,
}

/// `restore` at moment `m`. The first uncovered look notes where the keyboard is (`Restore::focus`); covered, the
/// look starts again once uncovered, since the cover held the keyboard.
pub(super) fn restore_step(restore: &mut Restore, m: &Moment) -> Step {
    if m.goal || m.touched > restore.at || m.now.duration_since(restore.at) > RESTORE_EXPIRY {
        return Step::Drop;
    }
    if m.covered {
        restore.focus = None;
        return Step::Wait;
    }
    match &restore.focus {
        None => restore.focus = Some(m.focus.map(str::to_string)),
        Some(then) if then.as_deref() != m.focus => return Step::Drop,
        Some(_) => {}
    }
    if m.shown_tab != Some(restore.tab.as_str()) {
        return Step::Wait;
    }
    if !m.pane_shown {
        return Step::Drop;
    }
    Step::Apply
}

/// The entry a render notes for the shown tab's keyboard (`focused`, the shown pane with it), or none: not before the
/// kept file is read (the last run's entry would be written over by whichever pane herdr focused first), not while a
/// remembered pane waits for the keyboard (`waiting`), and not when it is noted already (`noted`).
pub(super) fn keyboard_note(loaded: bool, waiting: Option<&Restore>, tab: &str, focused: Option<KeyPane>, noted: Option<&KeyPane>) -> Option<KeyPane> {
    if !loaded || waiting.is_some_and(|r| r.tab == tab) {
        return None;
    }
    focused.filter(|f| noted != Some(f))
}

/// The pane a tab's keyboard was in, by its terminal too: herdr gives a closed pane's id to a new one, and a
/// restart starts the ids again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct KeyPane {
    pub pane: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub terminal: String,
}

/// Each session's keyboard pane per tab (by [`SessionKey`], then tab id), kept for the next run.
pub(super) type KeyboardMemory = BTreeMap<String, BTreeMap<String, KeyPane>>;

/// The tabs of `memory` (one session's) that `snapshot` no longer backs: a tab gone, its pane gone or moved to
/// another tab, or another terminal behind its id. True when any went.
pub(super) fn prune_keyboard(memory: &mut BTreeMap<String, KeyPane>, snapshot: &Snapshot) -> bool {
    // a server still starting (or restarted empty) lists nothing: no reason to forget every tab
    if snapshot.workspaces.is_empty() {
        return false;
    }
    let before = memory.len();
    memory.retain(|tab, k| {
        snapshot.tabs.iter().any(|t| &t.tab_id == tab)
            && snapshot.panes.iter().any(|p| p.pane_id == k.pane && &p.tab_id == tab && (k.terminal.is_empty() || p.terminal_id == k.terminal))
    });
    memory.len() != before
}

impl super::Shell {
    /// Leaving the shown session `left` (before its snapshot goes to its watcher): its tab, workspace and herdr's
    /// focus are kept, and its panes parked in `recent_panes` order as far as the caps allow; the rest go. The
    /// keyboard leaves them on the next render.
    pub(super) fn park_view(&mut self, left: &SessionKey, cx: &mut gpui::Context<Self>) {
        let mut panes = Vec::new();
        for id in &self.recent_panes {
            let Some(pane) = self.panes.remove(id) else { continue };
            pane.update(cx, |p, cx| p.park(cx));
            panes.push(Parked { pane_id: id.clone(), terminal_id: String::new(), pane });
        }
        let view = leaving_view(self.snapshot.as_ref(), self.shown_tab().map(|t| t.tab_id), self.shown_workspace.clone(), panes, Instant::now());
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
        self.restore = remembered.map(|k| Restore::new(tab.to_string(), k.pane.clone(), now));
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
        let Some(mut restore) = self.restore.take() else { return };
        let handle = self.panes.get(&restore.pane).filter(|_| shown.iter().any(|f| f.pane_id == restore.pane)).map(|p| p.read(cx).focus_handle(cx));
        let focus = self.keyboard_pane(shown, window, cx);
        let moment = Moment { shown_tab: tab, pane_shown: handle.is_some(), goal, covered, touched: self.touched, focus: focus.as_deref(), now };
        match restore_step(&mut restore, &moment) {
            Step::Apply => {
                if let Some(handle) = handle {
                    window.focus(&handle, cx);
                }
            }
            Step::Drop => {}
            Step::Wait => self.restore = Some(restore),
        }
    }

    /// The shown pane with the keyboard.
    fn keyboard_pane(&self, shown: &[crate::model::PaneFrame], window: &gpui::Window, cx: &gpui::App) -> Option<String> {
        shown.iter().find(|f| self.panes.get(&f.pane_id).is_some_and(|p| p.read(cx).focus_handle(cx).is_focused(window))).map(|f| f.pane_id.clone())
    }

    /// The shown tab's pane with the keyboard is noted for the tab (and the next run), unless a remembered one
    /// waits to be given it back.
    pub(super) fn note_keyboard(&mut self, key: Option<&SessionKey>, tab: Option<&str>, shown: &[crate::model::PaneFrame], window: &gpui::Window, cx: &mut gpui::Context<Self>) {
        let (Some(key), Some(tab)) = (key, tab) else { return };
        let focused = self.keyboard_pane(shown, window, cx).map(|pane| {
            let terminal = self.snapshot.as_ref().and_then(|s| s.panes.iter().find(|p| p.pane_id == pane)).map(|p| p.terminal_id.clone()).unwrap_or_default();
            KeyPane { pane, terminal }
        });
        let noted = self.keyboard.get(&key.0).and_then(|m| m.get(tab));
        if let Some(entry) = keyboard_note(self.kept_views.loaded, self.restore.as_ref(), tab, focused, noted) {
            self.keyboard.entry(key.0.clone()).or_default().insert(tab.to_string(), entry);
            self.save_kept_views(cx);
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
    fn an_unread_machine_list_forgets_no_machine() {
        let listed = [key("a"), SessionKey("machine:m1".into())];
        let off = ["machine:m2".to_string()];
        let known = |k: &str, machines_known: bool| still_known(k, &listed, Some(&key("shown")), &off, machines_known);
        assert!(known("/tmp/a.sock", true) && known("/tmp/shown.sock", true) && known("machine:m1", true));
        assert!(known("machine:m2", true), "turned off: kept for when it is back");
        assert!(!known("machine:m3", true), "forgotten by herdr");
        assert!(!known("/tmp/gone.sock", true));
        // the machines could not be read: none is forgotten, a session still is
        assert!(known("machine:m3", false));
        assert!(!known("/tmp/gone.sock", false));
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

    /// A moment uncovered, with `w1:t1` and its pane shown, no goal, nothing touched since `t0 - 1 s`, the keyboard in
    /// herdr's focused pane `w1:p1`.
    fn moment(t0: Instant) -> Moment<'static> {
        Moment { shown_tab: Some("w1:t1"), pane_shown: true, goal: false, covered: false, touched: t0 - Duration::from_secs(1), focus: Some("w1:p1"), now: t0 }
    }

    fn restore(t0: Instant) -> Restore {
        Restore::new("w1:t1".into(), "w1:p2".into(), t0)
    }

    #[test]
    fn a_restore_waits_for_its_tab_and_yields_to_a_goal() {
        let t0 = Instant::now();
        let m = moment(t0);
        assert_eq!(restore_step(&mut restore(t0), &m), Step::Apply);
        assert_eq!(restore_step(&mut restore(t0), &Moment { shown_tab: Some("w1:t2"), ..moment(t0) }), Step::Wait, "another tab shown");
        assert_eq!(restore_step(&mut restore(t0), &Moment { shown_tab: None, focus: None, ..moment(t0) }), Step::Wait, "no snapshot yet");
        assert_eq!(restore_step(&mut restore(t0), &Moment { covered: true, ..moment(t0) }), Step::Wait, "Settings or a file tab over the panes");
        assert_eq!(restore_step(&mut restore(t0), &Moment { goal: true, ..moment(t0) }), Step::Drop, "a goal wins");
        let late = |d: Duration| restore_step(&mut restore(t0), &Moment { now: t0 + d, ..moment(t0) });
        assert_eq!(late(RESTORE_EXPIRY + Duration::from_millis(1)), Step::Drop);
        assert_eq!(late(RESTORE_EXPIRY), Step::Apply);
    }

    #[test]
    fn a_restore_never_takes_the_keyboard_later() {
        let t0 = Instant::now();
        // its tab shows without its pane (another pane zoomed): let go, not given when the zoom ends mid-command
        assert_eq!(restore_step(&mut restore(t0), &Moment { pane_shown: false, ..moment(t0) }), Step::Drop);
        // your own key or click after it began
        assert_eq!(restore_step(&mut restore(t0), &Moment { touched: t0 + Duration::from_millis(5), ..moment(t0) }), Step::Drop);
        // the key or click that switched came before it: it stands
        assert_eq!(restore_step(&mut restore(t0), &Moment { touched: t0, ..moment(t0) }), Step::Apply);
        // waiting for its tab, the keyboard moved without it (herdr's focus, a click elsewhere): let go
        let mut r = restore(t0);
        assert_eq!(restore_step(&mut r, &Moment { shown_tab: Some("w1:t2"), ..moment(t0) }), Step::Wait);
        assert_eq!(r.focus, Some(Some("w1:p1".to_string())));
        assert_eq!(restore_step(&mut r, &Moment { focus: Some("w1:p3"), ..moment(t0) }), Step::Drop);
        // a cover held the keyboard: once uncovered, where the keyboard went then is the start
        let mut r = restore(t0);
        assert_eq!(restore_step(&mut r, &Moment { shown_tab: Some("w1:t2"), ..moment(t0) }), Step::Wait);
        assert_eq!(restore_step(&mut r, &Moment { covered: true, focus: None, ..moment(t0) }), Step::Wait);
        assert_eq!(r.focus, None);
        assert_eq!(restore_step(&mut r, &Moment { focus: Some("w1:p3"), ..moment(t0) }), Step::Apply);
    }

    #[test]
    fn the_keyboard_is_noted_once_the_kept_file_is_read_and_no_restore_waits() {
        let k = |pane: &str| KeyPane { pane: pane.into(), terminal: format!("term-{pane}") };
        let t0 = Instant::now();
        assert_eq!(keyboard_note(true, None, "w1:t1", Some(k("w1:p2")), None), Some(k("w1:p2")));
        assert_eq!(keyboard_note(false, None, "w1:t1", Some(k("w1:p2")), None), None, "the last run's entry not read yet");
        assert_eq!(keyboard_note(true, Some(&restore(t0)), "w1:t1", Some(k("w1:p1")), Some(&k("w1:p2"))), None, "the remembered pane waits");
        assert_eq!(keyboard_note(true, Some(&restore(t0)), "w1:t9", Some(k("w9:p1")), None), Some(k("w9:p1")), "another tab's restore");
        assert_eq!(keyboard_note(true, None, "w1:t1", Some(k("w1:p2")), Some(&k("w1:p2"))), None, "noted already");
        assert_eq!(keyboard_note(true, None, "w1:t1", None, Some(&k("w1:p2"))), None, "no pane has it");
    }

    #[test]
    fn leaving_reads_the_view_from_the_snapshot_before_it_goes() {
        let snap = snapshot(&[("w1:p1", "w1:t1", "term-1")], Some("w1:t1"));
        let t0 = Instant::now();
        let v = leaving_view(Some(&snap), Some("w1:t1".into()), Some("w1".into()), vec![Parked { pane_id: "w1:p1".into(), terminal_id: String::new(), pane: 1 }], t0);
        assert_eq!(v.herdr_focus.as_deref(), Some("w1:t1"));
        assert_eq!(v.panes[0].terminal_id, "term-1", "its terminal, for the check on return");
        // the snapshot already handed to the watcher: nothing to check against later
        let v = leaving_view(None, Some("w1:t1".into()), None, vec![Parked { pane_id: "w1:p1".into(), terminal_id: String::new(), pane: 1 }], t0);
        assert_eq!((v.herdr_focus, v.panes[0].terminal_id.as_str()), (None, ""));
    }

    #[test]
    fn keyboard_memory_follows_the_snapshot() {
        let k = |pane: &str, terminal: &str| KeyPane { pane: pane.into(), terminal: terminal.into() };
        let mut memory: BTreeMap<String, KeyPane> = [
            ("w1:t1".to_string(), k("w1:p2", "term-2")),
            ("w1:t2".to_string(), k("w1:p3", "term-3")),
            ("w1:t3".to_string(), k("w1:p4", "")),
            ("w1:t4".to_string(), k("w1:p5", "term-5")),
            ("w9:t1".to_string(), k("w9:p1", "term-9")),
        ]
        .into();
        // t2's pane is another terminal now (a restart), t4's pane moved to t1, w9 closed
        let snap = snapshot(&[("w1:p2", "w1:t1", "term-2"), ("w1:p5", "w1:t1", "term-5"), ("w1:p3", "w1:t2", "term-new"), ("w1:p4", "w1:t3", "x"), ("w1:p6", "w1:t4", "y")], None);
        assert!(prune_keyboard(&mut memory, &snap));
        assert_eq!(memory.keys().collect::<Vec<_>>(), ["w1:t1", "w1:t3"]);
        assert!(!prune_keyboard(&mut memory, &snap));
        // a server starting lists nothing: nothing is forgotten
        assert!(!prune_keyboard(&mut memory, &snapshot(&[], None)));
        assert_eq!(memory.len(), 2);
        // kept for the next run as written
        let text = serde_json::to_string(&memory).unwrap();
        assert_eq!(serde_json::from_str::<BTreeMap<String, KeyPane>>(&text).unwrap(), memory);
    }
}
