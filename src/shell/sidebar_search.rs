//! The sidebar's search and focus mode. A search (the header's magnifier, FilterSidebar or the command panel) keeps
//! the workspaces of every session that match it, fuzzily as the command panel matches (`command_panel::score`), on
//! their name, branch, repo or checkout folder; a session whose name matches keeps them all. While it is on, every
//! fold shows open without its saved state changing, Workspace1–9 number what it shows, and nothing drags. In the
//! Agents view it keeps the agents whose session, workspace, agent or title match. Focus mode (`sidebar_focus`)
//! narrows the Workspaces tree to the session shown, without its header; a search still looks in every session, and
//! the Agents section and view, Next agent that needs you and the panels go on listing every session.
use super::sessions::Place;
use super::sidebar_tree::{SessionAgents, TreeRow, model_rows};
use super::watchers::{SessionKey, Watchers};
use super::*;
use crate::model::Workspace;
use std::collections::HashSet;

/// What a search keeps of one session: everything (its name matched), or the workspaces (agents, in the Agents
/// view) by id.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Keep {
    All,
    Only(HashSet<String>),
}

impl Keep {
    pub(super) fn has(&self, id: &str) -> bool {
        match self {
            Keep::All => true,
            Keep::Only(ids) => ids.contains(id),
        }
    }

    fn any(&self) -> bool {
        match self {
            Keep::All => true,
            Keep::Only(ids) => !ids.is_empty(),
        }
    }
}

/// A search's [`Keep`] for each session it keeps anything of.
pub(super) type Kept = HashMap<SessionKey, Keep>;

/// Whether `query` matches `text` as the command panel matches.
fn hit(query: &str, text: &str) -> bool {
    crate::ui::command_panel::score(query, text).is_some()
}

/// The last part of a path, `/` or `\` apart.
fn base(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path)
}

/// One session as the search reads it: its name, and the snapshot the tree shows it by (none for a stopped one).
pub(super) struct Searched<'a> {
    pub key: SessionKey,
    pub name: String,
    pub snapshot: Option<&'a Snapshot>,
}

/// The workspaces the search `query` keeps in each of `sessions`: one that matches on its name, its branch
/// (`branch`), its repo or its checkout folder, with its worktrees when it is a repo's checkout, and a matching
/// worktree with the checkout it sits under; all of a session whose name matches.
pub(super) fn workspace_hits(query: &str, sessions: &[Searched], mut branch: impl FnMut(&SessionKey, &Snapshot, &Workspace) -> Option<String>) -> Kept {
    let mut kept = Kept::new();
    for s in sessions {
        if hit(query, &s.name) {
            kept.insert(s.key.clone(), Keep::All);
            continue;
        }
        let Some(snapshot) = s.snapshot else { continue };
        let mut matches = |w: &Workspace| {
            hit(query, &w.label)
                || w.worktree.as_ref().is_some_and(|t| hit(query, base(&t.repo_key)) || (!t.checkout_path.is_empty() && hit(query, base(&t.checkout_path))))
                || branch(&s.key, snapshot, w).is_some_and(|b| hit(query, &b))
        };
        let mut ids = HashSet::new();
        for (root, kids) in crate::model::groups(snapshot.workspace_tree()) {
            let root_hit = matches(root);
            let kid_hits: Vec<&Workspace> = kids.iter().copied().filter(|k| root_hit || matches(k)).collect();
            if root_hit || !kid_hits.is_empty() {
                ids.insert(root.workspace_id.clone());
                ids.extend(kid_hits.iter().map(|k| k.workspace_id.clone()));
            }
        }
        if !ids.is_empty() {
            kept.insert(s.key.clone(), Keep::Only(ids));
        }
    }
    kept
}

/// The agents (by pane id) the search `query` keeps in the Agents view: each whose workspace, agent or title
/// matches; all of a session whose name matches.
pub(super) fn agent_hits(query: &str, sessions: &[SessionAgents]) -> Kept {
    let mut kept = Kept::new();
    for s in sessions {
        if hit(query, &s.name) {
            kept.insert(s.key.clone(), Keep::All);
            continue;
        }
        let label = |id: &str| s.snapshot.workspaces.iter().find(|w| w.workspace_id == id).map(|w| w.label.as_str());
        let ids: HashSet<String> = s
            .snapshot
            .panes
            .iter()
            .filter(|p| p.agent.is_some())
            .filter(|p| {
                label(&p.workspace_id).is_some_and(|l| hit(query, l))
                    || p.agent.as_deref().is_some_and(|a| hit(query, a))
                    || p.terminal_title_stripped.as_deref().is_some_and(|t| hit(query, t))
            })
            .map(|p| p.pane_id.clone())
            .collect();
        if !ids.is_empty() {
            kept.insert(s.key.clone(), Keep::Only(ids));
        }
    }
    kept
}

/// The tree's `rows` with only the sessions `kept` keeps something of: the others' headers, notices and
/// workspaces go, and a machine's header with no session left under it.
pub(super) fn filter_rows(rows: Vec<TreeRow>, kept: &Kept) -> Vec<TreeRow> {
    let keeps = |key: &SessionKey| kept.get(key).is_some_and(Keep::any);
    let rows: Vec<TreeRow> = rows
        .into_iter()
        .filter(|r| match r {
            TreeRow::Machine { .. } => true,
            TreeRow::Session { key, .. } | TreeRow::Workspaces { key } | TreeRow::Notice { key, .. } => keeps(key),
        })
        .collect();
    let lone = |i: usize| matches!(rows[i], TreeRow::Machine { .. }) && rows.get(i + 1).is_none_or(|r| matches!(r, TreeRow::Machine { .. }));
    let lone: Vec<bool> = (0..rows.len()).map(lone).collect();
    rows.into_iter().zip(lone).filter(|(_, lone)| !lone).map(|(r, _)| r).collect()
}

/// The Workspaces tree as the sidebar shows it, and whether its rows drag.
#[derive(Debug)]
pub(super) struct TreeView {
    pub rows: Vec<TreeRow>,
    pub drags: bool,
}

/// The Workspaces tree for `places`: [`model_rows`]'s, in focus mode the shown session's alone without a header
/// (machines and sessions turned off left out too), and with a search (`kept`) only what it keeps, in every session
/// whatever focus mode says. A search shows every session open, as it shows every worktree group (`collapsed` is
/// left as saved), and stops the drags: a drop among rows it hides would land somewhere unseen.
pub(super) fn tree_view(places: &[Place], w: &Watchers, collapsed: &HashSet<String>, snapshot: Option<&Snapshot>, shown: &SessionKey, focus: bool, kept: Option<&Kept>) -> TreeView {
    let filtering = kept.is_some();
    let focused = focus && !filtering;
    let open = HashSet::new();
    let collapsed = if filtering || focused { &open } else { collapsed };
    let mut rows = if focused {
        let one: Vec<Place> = places.iter().filter(|p| SessionKey::of(p) == *shown).cloned().collect();
        let mut rows = model_rows(&one, w, collapsed, snapshot, shown);
        rows.retain(|r| !matches!(r, TreeRow::Machine { .. } | TreeRow::Session { .. }));
        rows
    } else {
        model_rows(places, w, collapsed, snapshot, shown)
    };
    if let Some(kept) = kept {
        rows = filter_rows(rows, kept);
    }
    TreeView { rows, drags: !filtering }
}

/// The tree's rows the Agents view lists its sessions' agents under: every session's, whatever focus mode says (it
/// narrows the Workspaces tree alone), and with a search (`kept`) those it keeps an agent of.
pub(super) fn agents_rows(places: &[Place], w: &Watchers, collapsed: &HashSet<String>, snapshot: Option<&Snapshot>, shown: &SessionKey, kept: Option<&Kept>) -> Vec<TreeRow> {
    tree_view(places, w, collapsed, snapshot, shown, false, kept).rows
}

/// Folds or unfolds the session `key` in the saved list, and whether it changed: never while a search is on
/// (`filtering`), which shows every session open; a click then must not change what it gives back.
pub(super) fn fold_saved(saved: &mut Vec<String>, key: &str, filtering: bool) -> bool {
    if filtering {
        return false;
    }
    sidebar_tree::toggle_in(saved, key);
    true
}

/// The search field in the sidebar's header, while it is up.
pub(super) struct SidebarSearch {
    pub input: Entity<InputState>,
    _events: Vec<Subscription>,
}

impl Shell {
    /// What the sidebar's search keeps to, while it is up and typed in.
    pub(super) fn search_query(&self, cx: &App) -> Option<String> {
        let query = self.sidebar_search.as_ref()?.input.read(cx).value().trim().to_string();
        (!query.is_empty()).then_some(query)
    }

    /// FilterSidebar: the search field in the sidebar's header, with the keyboard; the sidebar comes out first when
    /// it is hidden.
    pub(super) fn open_sidebar_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // floating out already (the peek): it stays so
        if !self.sidebar_open && self.peek_now() == 0. {
            self.toggle_sidebar(cx);
        }
        if let Some(search) = &self.sidebar_search {
            search.input.update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        let theme = Theme::of(cx).clone();
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Filter workspaces");
            input.set_editor_style(crate::ui::input_style(&theme));
            input
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let events = vec![cx.subscribe_in(&input, window, |shell, _, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => shell.go_to_first_match(cx),
            InputEvent::Change => {
                // a drop being aimed: the rows under it change, and a search drags nothing
                shell.tree.drop = None;
                cx.notify();
            }
            _ => {}
        })];
        self.tree.drop = None;
        self.sidebar_search = Some(SidebarSearch { input, _events: events });
        cx.notify();
    }

    /// Escape or ×: the search goes, the tree shows as before it, and the keyboard goes back to herdr's focused pane.
    pub(super) fn close_sidebar_search(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_search.take().is_some() {
            self.focused = None;
            cx.notify();
        }
    }

    /// Enter in the search: the first workspace it shows (the first agent in the Agents view), as a click on it goes,
    /// and the search closes.
    fn go_to_first_match(&mut self, cx: &mut Context<Self>) {
        if self.search_query(cx).is_none() {
            return;
        }
        let agents = SettingsGlobal::of(cx).settings.sidebar_view == SidebarView::Agents && SettingsGlobal::of(cx).settings.show_agents;
        let goal = if agents {
            self.first_agent_shown(cx)
        } else {
            let (view, shown, kept) = self.shown_tree(cx);
            self.numbered_workspaces(&view.rows, &shown, kept.as_ref()).into_iter().next().map(|(k, ws)| (k, sidebar_tree::Goal::Workspace(ws)))
        };
        self.close_sidebar_search(cx);
        if let Some((key, goal)) = goal {
            self.go_to(key, goal, cx);
        }
    }

    /// The first agent the Agents view shows, as filtered.
    fn first_agent_shown(&self, cx: &mut App) -> Option<(SessionKey, sidebar_tree::Goal)> {
        let sessions = self.session_agents(cx);
        let (rows, kept) = self.agents_tree(&sessions, cx);
        let attention = SettingsGlobal::of(cx).settings.attention_agents;
        sidebar_tree::agents_view(rows, &sessions, self.agent_sort, attention, Instant::now(), kept.as_ref()).into_iter().find_map(|r| match r {
            sidebar_tree::ViewRow::Agents(entries) => entries.into_iter().next().map(|e| (e.key, sidebar_tree::Goal::Pane { tab: e.pane.tab_id, pane: e.pane.pane_id })),
            _ => None,
        })
    }

    /// The Workspaces tree shown now (focus mode and the search applied), the shown session's key, and what the
    /// search keeps.
    pub(super) fn shown_tree(&self, cx: &mut App) -> (TreeView, SessionKey, Option<Kept>) {
        let settings = &SettingsGlobal::of(cx).settings;
        let collapsed: HashSet<String> = settings.collapsed_sessions.iter().cloned().collect();
        let focus = settings.sidebar_focus;
        let shown = self.shown_key().unwrap_or_default();
        let kept = self.search_query(cx).map(|query| {
            let sessions = self.searched(&shown, cx);
            workspace_hits(&query, &sessions, |key, snapshot, w| self.workspace_branch(key, &shown, snapshot, w, cx))
        });
        let view = tree_view(&self.places, &self.watchers, &collapsed, self.snapshot.as_ref(), &shown, focus, kept.as_ref());
        (view, shown, kept)
    }

    /// The tree's rows for the Agents view, every session's whatever focus mode says, and with the search the
    /// sessions with an agent it keeps; and what it keeps.
    pub(super) fn agents_tree(&self, sessions: &[SessionAgents], cx: &App) -> (Vec<TreeRow>, Option<Kept>) {
        let collapsed: HashSet<String> = SettingsGlobal::of(cx).settings.collapsed_sessions.iter().cloned().collect();
        let shown = self.shown_key().unwrap_or_default();
        let kept = self.search_query(cx).map(|query| agent_hits(&query, sessions));
        (agents_rows(&self.places, &self.watchers, &collapsed, self.snapshot.as_ref(), &shown, kept.as_ref()), kept)
    }

    /// Every session the search reads, in the tree's order, by the snapshot the tree shows it by.
    fn searched(&self, shown: &SessionKey, cx: &App) -> Vec<Searched<'_>> {
        let listed: Vec<SessionKey> = self.places.iter().map(SessionKey::of).collect();
        sidebar_tree::agent_order(Some(shown), &listed)
            .into_iter()
            .map(|key| {
                let place = self.places.iter().find(|p| SessionKey::of(p) == key);
                let name = place.map_or_else(|| self.place_label(cx), |p| p.look(cx).0);
                let snapshot = sidebar_tree::tree_snapshot(&self.watchers, &key, self.snapshot.as_ref());
                Searched { key, name, snapshot }
            })
            .collect()
    }

    /// A workspace's branch for the search: its checkout's HEAD, read as another session's rows read it (a
    /// machine's not at all).
    fn workspace_branch(&self, key: &SessionKey, shown: &SessionKey, snapshot: &Snapshot, w: &Workspace, cx: &mut App) -> Option<String> {
        let path = w.worktree.as_ref().map(|t| t.checkout_path.as_str()).filter(|d| !d.is_empty()).or_else(|| snapshot.panes.iter().find(|p| p.workspace_id == w.workspace_id)?.cwd.as_deref())?;
        let mode = if key == shown { sidebar_tree::RowMode::Live } else { sidebar_tree::RowMode::Other(key.clone()) };
        let dir = self.row_dir(&mode, path)?;
        // a machine's shown session's folder is on the machine: HEAD can't be read from here
        if crate::herdr::split(&dir).0.is_some() {
            return None;
        }
        Shell::other_branch(&dir, cx)
    }

    /// ToggleFocusMode, the header's menu and its Focus chip: the Workspaces tree shows the session shown alone, or
    /// every session again.
    pub(super) fn toggle_focus_mode(&mut self, cx: &mut Context<Self>) {
        // a drop being aimed: its rows come and go
        self.tree.drop = None;
        save_from_sidebar(cx, |s| s.sidebar_focus = !s.sidebar_focus);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::super::sidebar_tree::{AgentEntry, ViewRow, agents_view, workspace_order};
    use super::super::watchers::{Activity, Status};
    use super::*;
    use crate::files::roots::root_key;
    use crate::herdr::SessionInfo;
    use crate::model::{AgentSort, Since};

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    fn info(name: &str) -> SessionInfo {
        SessionInfo { name: name.into(), running: name != "old", socket_path: format!("/tmp/{name}.sock"), default: name == "default" }
    }

    /// Workspaces by (id, label, worktree: (repo, linked)), each checked out at `/src/<id>`; one agent per entry of
    /// `agents`: (pane, workspace, title).
    fn snap(workspaces: &[(&str, &str, Option<(&str, bool)>)], agents: &[(&str, &str, &str)]) -> Snapshot {
        let ws: Vec<_> = workspaces
            .iter()
            .enumerate()
            .map(|(i, (id, label, tree))| {
                let worktree = tree.map(|(repo, linked)| json!({"repo_key": repo, "is_linked_worktree": linked, "checkout_path": format!("/src/{id}")}));
                json!({"workspace_id": id, "label": label, "number": i + 1, "active_tab_id": null, "agent_status": "idle", "worktree": worktree})
            })
            .collect();
        let panes: Vec<_> = agents
            .iter()
            .map(|(pane, ws, title)| json!({"pane_id": pane, "tab_id": format!("t-{pane}"), "workspace_id": ws, "terminal_id": pane, "agent_status": "blocked", "agent": "claude", "terminal_title_stripped": title}))
            .collect();
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": ws, "tabs": [], "panes": panes, "layouts": []}})).unwrap()
    }

    /// `web` with a checkout `app` of the repo `shop` and its worktrees `login` and `billing`, and a lone `docs`.
    fn web() -> Snapshot {
        snap(
            &[("w1", "app", Some(("/src/shop", false))), ("w2", "login", Some(("/src/shop", true))), ("w3", "billing", Some(("/src/shop", true))), ("w4", "docs", None)],
            &[("p1", "w2", "fixing the sign-in form"), ("p2", "w4", "writing the guide")],
        )
    }

    fn only(ids: &[&str]) -> Keep {
        Keep::Only(ids.iter().map(|s| s.to_string()).collect())
    }

    fn no_branch(_: &SessionKey, _: &Snapshot, _: &Workspace) -> Option<String> {
        None
    }

    fn hits(query: &str, s: &Snapshot) -> Option<Keep> {
        let sessions = [Searched { key: key("web"), name: "web".into(), snapshot: Some(s) }];
        workspace_hits(query, &sessions, no_branch).remove(&key("web"))
    }

    #[test]
    fn a_worktree_keeps_its_checkout_and_a_checkout_its_worktrees() {
        let s = web();
        assert_eq!(hits("login", &s), Some(only(&["w1", "w2"])), "the worktree, under its checkout; not its sibling");
        assert_eq!(hits("app", &s), Some(only(&["w1", "w2", "w3"])), "the checkout, with every worktree");
        assert_eq!(hits("shop", &s), Some(only(&["w1", "w2", "w3"])), "all of the repo");
        assert_eq!(hits("docs", &s), Some(only(&["w4"])));
        assert_eq!(hits("bllng", &s), Some(only(&["w1", "w3"])), "fuzzy, as the command panel matches");
        assert_eq!(hits("zzz", &s), None, "nothing kept, the session left out");
    }

    #[test]
    fn a_session_s_name_keeps_all_of_it() {
        let (s, other) = (web(), snap(&[("w1", "notes", None)], &[]));
        let sessions = [
            Searched { key: key("web"), name: "web".into(), snapshot: Some(&s) },
            Searched { key: key("notes"), name: "notes".into(), snapshot: Some(&other) },
            Searched { key: key("old"), name: "old".into(), snapshot: None },
        ];
        let kept = workspace_hits("web", &sessions, no_branch);
        assert_eq!(kept.get(&key("web")), Some(&Keep::All));
        assert_eq!(kept.len(), 1);
        assert_eq!(workspace_hits("old", &sessions, no_branch).get(&key("old")), Some(&Keep::All), "a stopped one, by its name alone");
    }

    #[test]
    fn a_branch_or_a_checkout_folder_matches_too() {
        let s = web();
        let sessions = [Searched { key: key("web"), name: "web".into(), snapshot: Some(&s) }];
        let branch = |_: &SessionKey, _: &Snapshot, w: &Workspace| (w.workspace_id == "w4").then(|| "feature/handbook".to_string());
        assert_eq!(workspace_hits("handbook", &sessions, branch).remove(&key("web")), Some(only(&["w4"])));
        let folders = snap(&[("w1", "first", Some(("/src/app", false))), ("w9", "second", Some(("/src/app", true)))], &[]);
        assert_eq!(hits("w9", &folders), Some(only(&["w1", "w9"])), "the checkout's folder, /src/w9");
    }

    /// `default` shown with `web`, `notes` another session with its snapshot, `old` stopped.
    fn sessions() -> (Vec<Place>, Watchers, Snapshot) {
        let places: Vec<Place> = ["default", "notes", "old"].into_iter().map(|n| Place::Session(info(n))).collect();
        let mut w = Watchers::new();
        let now = Instant::now();
        w.reconcile(&places, Some(&key("default")), now);
        w.due(now, Activity::Active);
        w.apply(&key("notes"), Ok(snap(&[("n1", "notes", None)], &[])), now);
        (places, w, web())
    }

    fn kinds(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TreeRow::Machine { label, .. } => format!("M {label}"),
                TreeRow::Session { key, collapsed, .. } => format!("S {}{}", &key.0[5..key.0.len() - 5], if *collapsed { " folded" } else { "" }),
                TreeRow::Workspaces { key } => format!("W {}", &key.0[5..key.0.len() - 5]),
                TreeRow::Notice { text, .. } => format!("N {text}"),
            })
            .collect()
    }

    #[test]
    fn sessions_with_nothing_kept_go_with_their_notices_and_machines() {
        let rows = vec![
            TreeRow::Machine { id: "local".into(), label: "This computer".into(), status: Status::Live, enabled: true },
            TreeRow::Session { key: key("default"), collapsed: false, waiting: 0, status: Status::Shown },
            TreeRow::Workspaces { key: key("default") },
            TreeRow::Session { key: key("old"), collapsed: false, waiting: 0, status: Status::Stopped },
            TreeRow::Notice { key: key("old"), text: "Stopped".into(), action: None },
            TreeRow::Machine { id: "you@workbox".into(), label: "you@workbox".into(), status: Status::Live, enabled: true },
            TreeRow::Session { key: SessionKey("machine:m1".into()), collapsed: false, waiting: 0, status: Status::Live },
            TreeRow::Workspaces { key: SessionKey("machine:m1".into()) },
            TreeRow::Machine { id: "you@otherbox".into(), label: "you@otherbox".into(), status: Status::Stopped, enabled: false },
        ];
        let kept: Kept = [(key("default"), only(&["w1"]))].into();
        assert_eq!(kinds(&filter_rows(rows.clone(), &kept)), ["M This computer", "S default", "W default"]);
        let kept: Kept = [(key("old"), Keep::All), (key("default"), only(&[]))].into();
        assert_eq!(kinds(&filter_rows(rows.clone(), &kept)), ["M This computer", "S old", "N Stopped"], "kept by name, notice and all");
        assert!(filter_rows(rows, &Kept::new()).is_empty(), "nothing matches: the sidebar says so");
    }

    #[test]
    fn a_search_opens_every_fold_without_saving_it() {
        let (places, w, shown) = sessions();
        let collapsed: HashSet<String> = [key("default").0].into();
        let saved = collapsed.clone();
        let plain = tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), false, None);
        assert_eq!(kinds(&plain.rows)[..2], ["S default folded", "S notes"]);
        let kept: Kept = [(key("default"), only(&["w4"])), (key("notes"), Keep::All)].into();
        let found = tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), false, Some(&kept));
        assert_eq!(kinds(&found.rows), ["S default", "W default", "S notes", "W notes"]);
        assert_eq!(collapsed, saved);
        // a click on a header meanwhile changes nothing saved
        let mut list = vec![key("default").0];
        assert!(!fold_saved(&mut list, &key("default").0, true));
        assert_eq!(list, [key("default").0]);
        assert!(fold_saved(&mut list, &key("default").0, false));
        assert!(list.is_empty());
    }

    #[test]
    fn nothing_drags_while_searching() {
        let (places, w, shown) = sessions();
        let open = HashSet::new();
        assert!(tree_view(&places, &w, &open, Some(&shown), &key("default"), false, None).drags);
        assert!(tree_view(&places, &w, &open, Some(&shown), &key("default"), true, None).drags, "focus mode drags");
        assert!(!tree_view(&places, &w, &open, Some(&shown), &key("default"), false, Some(&Kept::new())).drags);
    }

    #[test]
    fn numbering_counts_only_what_the_search_shows() {
        let (places, w, shown) = sessions();
        let notes = w.snapshot(&key("notes")).unwrap().clone();
        let snaps = |k: &SessionKey| if *k == key("default") { Some(&shown) } else if *k == key("notes") { Some(&notes) } else { None };
        // the checkout's worktrees are folded, which a search opens
        let folded: HashSet<String> = [root_key(&key("default").0, "w1")].into();
        let kept: Kept = [(key("default"), only(&["w1", "w3"])), (key("notes"), Keep::All)].into();
        let rows = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, Some(&kept)).rows;
        let ids = |order: Vec<(SessionKey, String)>| order.into_iter().map(|(_, w)| w).collect::<Vec<_>>();
        assert_eq!(ids(workspace_order(&rows, snaps, &folded, Some(&kept))), ["w1", "w3", "n1"]);
        let all = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, None).rows;
        assert_eq!(ids(workspace_order(&all, snaps, &folded, None)), ["w1", "w4", "n1"], "folded, unsearched");
    }

    #[test]
    fn focus_mode_shows_the_shown_session_alone_without_headers() {
        let (places, w, shown) = sessions();
        let collapsed: HashSet<String> = [key("default").0].into();
        let focus = tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), true, None);
        assert_eq!(kinds(&focus.rows), ["W default"], "no machine or session rows, its fold aside");
        let notes = w.snapshot(&key("notes")).unwrap().clone();
        let snaps = |k: &SessionKey| if *k == key("default") { Some(&shown) } else if *k == key("notes") { Some(&notes) } else { None };
        let order: Vec<String> = workspace_order(&focus.rows, snaps, &HashSet::new(), None).into_iter().map(|(_, w)| w).collect();
        assert_eq!(order, ["w1", "w2", "w3", "w4"], "Workspace1–9 number the shown session's");
        // a search looks in every session all the same
        let kept: Kept = [(key("notes"), Keep::All)].into();
        assert_eq!(kinds(&tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), true, Some(&kept)).rows), ["S notes", "W notes"]);
    }

    fn entries(rows: &[ViewRow]) -> Vec<String> {
        rows.iter()
            .flat_map(|r| match r {
                ViewRow::Agents(list) => list.iter().map(|e: &AgentEntry| e.pane.pane_id.clone()).collect(),
                ViewRow::Tree(TreeRow::Session { key, .. }) => vec![format!("S {}", &key.0[5..key.0.len() - 5])],
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn focus_mode_leaves_the_agents_view_every_session_and_a_search_filters_its_agents() {
        let (places, w, _) = sessions();
        let shown = web();
        let notes = snap(&[("n1", "notes", None)], &[("q1", "n1", "summing up the week")]);
        let since = Since::default();
        let listed = [
            SessionAgents { key: key("default"), name: "default".into(), shown: true, snapshot: &shown, since: &since },
            SessionAgents { key: key("notes"), name: "notes".into(), shown: false, snapshot: &notes, since: &since },
        ];
        let now = Instant::now();
        // with focus mode on or off: the Agents view never reads it
        let rows = agents_rows(&places, &w, &HashSet::new(), Some(&shown), &key("default"), None);
        assert_eq!(entries(&agents_view(rows, &listed, AgentSort::Grouped, false, now, None)), ["S default", "p1", "p2", "S notes", "q1", "S old"]);
        let kept = agent_hits("sign-in", &listed);
        assert_eq!(kept, [(key("default"), only(&["p1"]))].into());
        let rows = agents_rows(&places, &w, &HashSet::new(), Some(&shown), &key("default"), Some(&kept));
        assert_eq!(entries(&agents_view(rows, &listed, AgentSort::Grouped, false, now, Some(&kept))), ["S default", "p1"]);
        assert_eq!(agent_hits("docs", &listed), [(key("default"), only(&["p2"]))].into(), "by its workspace");
        assert_eq!(agent_hits("notes", &listed), [(key("notes"), Keep::All)].into(), "by its session");
    }

    #[test]
    fn focus_mode_is_off_until_turned_on() {
        assert!(!crate::settings::Settings::default().sidebar_focus);
        let old: crate::settings::Settings = serde_json::from_str(r#"{"sidebar_peek": false}"#).unwrap();
        assert!(!old.sidebar_focus, "a file from before focus mode");
        let on: crate::settings::Settings = serde_json::from_str(r#"{"sidebar_focus": true}"#).unwrap();
        assert!(on.sidebar_focus);
    }
}
