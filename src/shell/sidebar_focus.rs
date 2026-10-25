//! The sidebar's focus mode (`sidebar_focus`): the Workspaces tree keeps the workspaces with an agent, in any
//! status, under every session's header; a session with none goes. With the sessions bar, which lists the shown
//! session alone, it keeps that session's. Nothing in the tree drags meanwhile; the panels go on listing everything.
use super::sessions::Place;
use super::sidebar_tree::{TreeRow, machine_of, model_rows, tree_snapshot};
use super::watchers::{SessionKey, Watchers};
use super::*;
use std::collections::HashSet;

/// What focus mode keeps of each session it keeps: its workspaces by id ([`with_agents`]).
pub(super) type Kept = HashMap<SessionKey, HashSet<String>>;

/// The Workspaces tree as the sidebar shows it: its rows, whether they drag, what focus mode keeps of each session
/// (`None` with it off), and the line it shows when it keeps nothing.
#[derive(Debug)]
pub(super) struct TreeView {
    pub rows: Vec<TreeRow>,
    pub drags: bool,
    pub kept: Option<Kept>,
    pub empty: Option<&'static str>,
}

/// The workspaces of `snapshot` focus mode keeps: each with an agent in any status (working, waiting, done, idle),
/// and the checkout over a worktree with one, as its group's header.
pub(super) fn with_agents(snapshot: &Snapshot) -> HashSet<String> {
    let has = |id: &str| snapshot.panes.iter().any(|p| p.agent.is_some() && p.workspace_id == id);
    let mut ids = HashSet::new();
    for (root, kids) in crate::model::groups(snapshot.workspace_tree()) {
        let kids: Vec<String> = kids.iter().filter(|k| has(&k.workspace_id)).map(|k| k.workspace_id.clone()).collect();
        if has(&root.workspace_id) || !kids.is_empty() {
            ids.insert(root.workspace_id.clone());
            ids.extend(kids);
        }
    }
    ids
}

/// Whether the tree's rows drag and take drops: not in focus mode (`focus`), whose rows are a selection, so a drop
/// among them would land somewhere unseen.
pub(super) fn drags_while(focus: bool) -> bool {
    !focus
}

/// The Workspaces tree for `places`, the one selector its rows and Workspace1–9 go by: [`model_rows`]'s; with the
/// sessions bar (`one`) the shown session's alone without a header, its folds shown open (`collapsed` is left as
/// saved); in focus mode (`focus`) every session headed, a lone one too, and only those with a workspace
/// [`with_agents`] keeps, with their machines.
pub(super) fn tree_view(places: &[Place], w: &Watchers, collapsed: &HashSet<String>, snapshot: Option<&Snapshot>, shown: &SessionKey, one: bool, focus: bool) -> TreeView {
    let open = HashSet::new();
    let mut rows = if one {
        let one: Vec<Place> = places.iter().filter(|p| SessionKey::of(p) == *shown).cloned().collect();
        let mut rows = model_rows(&one, w, &open, snapshot, shown, false);
        rows.retain(|r| !matches!(r, TreeRow::Machine { .. } | TreeRow::Session { .. }));
        rows
    } else {
        model_rows(places, w, collapsed, snapshot, shown, focus)
    };
    let kept = focus.then(|| {
        let snap = |k: &SessionKey| if k == shown { snapshot } else { tree_snapshot(w, k, snapshot) };
        let kept_of = |k: &SessionKey| Some(with_agents(snap(k)?)).filter(|ids| !ids.is_empty());
        let keys = places.iter().map(SessionKey::of).chain([shown.clone()]);
        let kept: Kept = keys.filter_map(|k| Some((k.clone(), kept_of(&k)?))).collect();
        // a machine stays while a session of it is kept, folded or not
        let machine_kept = |id: &str| places.iter().any(|p| machine_of(p) == id && kept.contains_key(&SessionKey::of(p)));
        rows.retain(|r| match r {
            TreeRow::Machine { id, .. } => machine_kept(id),
            TreeRow::Session { key, .. } | TreeRow::Workspaces { key } | TreeRow::Notice { key, .. } => kept.contains_key(key),
        });
        kept
    });
    let empty = (focus && rows.is_empty()).then_some(if one { "No agents in this session" } else { "No agents in any session" });
    TreeView { rows, drags: drags_while(focus), kept, empty }
}

/// The Focus chip's width in the titlebar with its word, and as the bare icon when the sidebar is too narrow for the word.
const CHIP_FULL: f32 = 66.;
const CHIP_ICON: f32 = 24.;
/// The chip's height, and the gap between it and the ⋯.
const CHIP_H: f32 = 18.;
const CHIP_GAP: f32 = 6.;

/// Where the titlebar's Sidebar view ⋯ sits: at the sidebar's right edge, hidden once the sidebar is too narrow to
/// hold it beside the toggle.
pub(super) fn view_button_left(sidebar_w: f32) -> Option<f32> {
    (sidebar_w >= TITLEBAR_CLUSTER + 32.).then_some(sidebar_w - 8. - 24.)
}

/// The Focus chip's left edge and width in the titlebar, right before the ⋯: its word and icon while they clear the
/// toggle, the icon alone when only that does, and gone with the ⋯.
pub(super) fn chip_box(sidebar_w: f32) -> Option<(f32, f32)> {
    let right = view_button_left(sidebar_w)? - CHIP_GAP;
    [CHIP_FULL, CHIP_ICON].into_iter().map(|w| (right - w, w)).find(|(left, _)| *left >= TITLEBAR_CLUSTER)
}

/// The Focus chip as the titlebar draws it, absolute at its place before the ⋯ (the caller adds its click).
pub(super) fn titlebar_chip(theme: &Theme, sidebar_w: f32) -> Option<gpui::Stateful<Div>> {
    let (left, width) = chip_box(sidebar_w)?;
    let accent = theme.accent;
    let word = width == CHIP_FULL;
    Some(
        div()
            .id("sidebar-focus-chip")
            .debug_selector(|| "sidebar-focus-chip".into())
            .absolute()
            .top(px(TRAFFIC_LIGHTS_CENTER - CHIP_H / 2.))
            .left(px(left))
            .w(px(width))
            .h(px(CHIP_H))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(3.))
            .rounded(px(9.))
            .bg(accent.opacity(0.14))
            .text_size(px(11.))
            .text_color(accent)
            .cursor_pointer()
            .occlude()
            .hover(move |s| s.bg(accent.opacity(0.22)))
            .tooltip(tip("Show every session again", Some("ToggleFocusMode")))
            .child(gpui::svg().path("icons/solar/target.svg").size(px(10.)).flex_none().text_color(accent))
            .when(word, |el| el.child("Focus").child(gpui::svg().path("icons/close.svg").size(px(9.)).flex_none().text_color(accent))),
    )
}

impl Shell {
    /// The Workspaces tree shown now (the sessions bar and focus mode applied), and the shown session's key.
    pub(super) fn shown_tree(&self, cx: &mut App) -> (TreeView, SessionKey) {
        let settings = &SettingsGlobal::of(cx).settings;
        let collapsed = sidebar_tree::saved_folds(settings);
        let (one, focus) = (settings.tree_one_session(), settings.tree_agents_only());
        let shown = self.shown_key().unwrap_or_default();
        let view = tree_view(&self.places, &self.watchers, &collapsed, self.snapshot.as_ref(), &shown, one, focus);
        (view, shown)
    }

    /// Whether the tree takes a drag now: not in focus mode, a drag begun before it included.
    pub(super) fn tree_drags(&self, cx: &App) -> bool {
        drags_while(SettingsGlobal::of(cx).settings.tree_agents_only())
    }

    /// ToggleFocusMode, the titlebar's Focus chip and the sidebar view menu: the Workspaces tree keeps the workspaces with an
    /// agent, or shows every one again.
    pub(super) fn toggle_focus_mode(&mut self, cx: &mut Context<Self>) {
        // a drop being aimed: its rows come and go
        self.tree.drop = None;
        save_from_sidebar(cx, |s| s.sidebar_focus = !s.sidebar_focus);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::super::sidebar_tree::{RowFilter, shown_groups, workspace_order};
    use super::super::watchers::Activity;
    use super::*;
    use crate::files::roots::root_key;
    use crate::herdr::SessionInfo;

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    fn info(name: &str) -> SessionInfo {
        SessionInfo { name: name.into(), running: name != "old", socket_path: format!("/tmp/{name}.sock"), default: name == "default" }
    }

    /// Workspaces by (id, label, worktree: (repo, linked)), each checked out at `/src/<id>`; one pane per entry of
    /// `panes`: (pane, workspace, its agent's status, or `None` for a plain shell).
    fn snap(workspaces: &[(&str, &str, Option<(&str, bool)>)], panes: &[(&str, &str, Option<&str>)]) -> Snapshot {
        let ws: Vec<_> = workspaces
            .iter()
            .enumerate()
            .map(|(i, (id, label, tree))| {
                let worktree = tree.map(|(repo, linked)| json!({"repo_key": repo, "is_linked_worktree": linked, "checkout_path": format!("/src/{id}")}));
                json!({"workspace_id": id, "label": label, "number": i + 1, "active_tab_id": null, "agent_status": "idle", "worktree": worktree})
            })
            .collect();
        let panes: Vec<_> = panes
            .iter()
            .map(|(pane, ws, status)| {
                let agent = status.map(|_| "claude");
                json!({"pane_id": pane, "tab_id": format!("t-{pane}"), "workspace_id": ws, "terminal_id": pane, "agent_status": status.unwrap_or("unknown"), "agent": agent})
            })
            .collect();
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": ws, "tabs": [], "panes": panes, "layouts": []}})).unwrap()
    }

    const SHOP: [(&str, &str, Option<(&str, bool)>); 4] =
        [("w1", "app", Some(("/src/shop", false))), ("w2", "login", Some(("/src/shop", true))), ("w3", "billing", Some(("/src/shop", true))), ("w4", "docs", None)];

    /// `web` with a checkout `app` of the repo `shop` and its worktrees `login` (an agent at work) and `billing` (a
    /// plain shell), and a lone `docs` (an idle agent).
    fn web() -> Snapshot {
        snap(&SHOP, &[("p1", "w2", Some("working")), ("p3", "w3", None), ("p2", "w4", Some("idle"))])
    }

    /// `default` shown with `shown`, `notes` another session with its snapshot `notes`, `old` stopped.
    fn sessions_with(shown: Snapshot, notes: Snapshot) -> (Vec<Place>, Watchers, Snapshot) {
        let places: Vec<Place> = ["default", "notes", "old"].into_iter().map(|n| Place::Session(info(n))).collect();
        let mut w = Watchers::new();
        let now = Instant::now();
        w.reconcile(&places, Some(&key("default")), now);
        w.due(now, Activity::Active);
        w.apply(&key("notes"), Ok(notes), now);
        (places, w, shown)
    }

    /// `default` with `web`, `notes` without an agent.
    fn sessions() -> (Vec<Place>, Watchers, Snapshot) {
        sessions_with(web(), snap(&[("n1", "notes", None)], &[]))
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

    fn ids(set: &HashSet<String>) -> Vec<String> {
        let mut v: Vec<String> = set.iter().cloned().collect();
        v.sort();
        v
    }

    /// Workspace1–9's order for `view`, each session's snapshot by `snaps`.
    fn order<'a>(view: &TreeView, snaps: impl Fn(&SessionKey) -> Option<&'a Snapshot>) -> Vec<String> {
        workspace_order(&view.rows, snaps, view.kept.as_ref()).into_iter().map(|(k, w)| format!("{}:{w}", &k.0[5..k.0.len() - 5])).collect()
    }

    #[test]
    fn each_agent_status_keeps_its_workspace_and_a_plain_shell_none() {
        for status in ["working", "blocked", "done", "idle", "unknown"] {
            let s = snap(&[("w1", "solo", None), ("w2", "shell", None)], &[("p1", "w1", Some(status)), ("p2", "w2", None)]);
            assert_eq!(ids(&with_agents(&s)), ["w1"], "{status}");
            let (places, w, shown) = sessions_with(s, snap(&[("n1", "notes", None)], &[]));
            let view = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, true);
            assert_eq!(kinds(&view.rows), ["S default", "W default"], "{status}");
            assert_eq!(ids(&view.kept.unwrap()[&key("default")]), ["w1"], "{status}");
        }
    }

    #[test]
    fn every_session_with_an_agent_is_listed_under_its_header_and_one_without_goes() {
        let notes = snap(&[("n1", "notes", None), ("n2", "drafts", None)], &[("q1", "n2", Some("blocked"))]);
        let (places, w, shown) = sessions_with(web(), notes.clone());
        let view = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, true);
        assert_eq!(kinds(&view.rows), ["S default", "W default", "S notes", "W notes"], "old, stopped, has none");
        let kept = view.kept.as_ref().unwrap();
        assert_eq!(ids(&kept[&key("default")]), ["w1", "w2", "w4"], "billing's plain shell and nothing else: gone");
        assert_eq!(ids(&kept[&key("notes")]), ["n2"], "notes' workspace without an agent: gone");
        // a folded session keeps its header, folded
        let folded: HashSet<String> = [key("notes").0].into();
        let view = tree_view(&places, &w, &folded, Some(&shown), &key("default"), false, true);
        assert_eq!(kinds(&view.rows), ["S default", "W default", "S notes folded"]);
        // notes without an agent: absent
        let (places, w, shown) = sessions();
        let view = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, true);
        assert_eq!(kinds(&view.rows), ["S default", "W default"]);
        assert_eq!(view.empty, None);
    }

    #[test]
    fn a_worktree_s_agent_keeps_its_checkout_and_a_checkout_s_agent_keeps_it_alone() {
        let groups = |s: &Snapshot| {
            let keep = with_agents(s);
            shown_groups(s, Some(&keep)).into_iter().map(|(w, kids)| (w.workspace_id.clone(), kids.iter().map(|k| k.workspace_id.clone()).collect::<Vec<_>>())).collect::<Vec<_>>()
        };
        let kid_only = snap(&SHOP, &[("p1", "w3", Some("done"))]);
        assert_eq!(groups(&kid_only), [("w1".to_string(), vec!["w3".to_string()])], "the checkout heads its worktree");
        let parent_only = snap(&SHOP, &[("p1", "w1", Some("working"))]);
        assert_eq!(groups(&parent_only), [("w1".to_string(), Vec::<String>::new())], "the checkout alone");
        assert_eq!(groups(&web()), [("w1".to_string(), vec!["w2".to_string()]), ("w4".to_string(), vec![])]);
    }

    #[test]
    fn no_agent_anywhere_says_so() {
        let none = snap(&SHOP, &[("p3", "w3", None)]);
        let (places, w, shown) = sessions_with(none, snap(&[("n1", "notes", None)], &[]));
        let view = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, true);
        assert!(view.rows.is_empty());
        assert_eq!(view.empty, Some("No agents in any session"));
        assert_eq!(tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, false).empty, None, "off: no such line");
        let bar = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), true, true);
        assert!(bar.rows.is_empty());
        assert_eq!(bar.empty, Some("No agents in this session"));
    }

    #[test]
    fn a_lone_session_keeps_its_header_in_focus_mode() {
        let places = vec![Place::Session(info("default"))];
        let mut w = Watchers::new();
        w.reconcile(&places, Some(&key("default")), Instant::now());
        let shown = web();
        assert_eq!(kinds(&tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, true).rows), ["S default", "W default"]);
        assert_eq!(kinds(&tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, false).rows), ["W default"], "off, as before");
    }

    #[test]
    fn the_sessions_bar_lists_the_shown_session_alone_and_focus_filters_it() {
        let notes = snap(&[("n1", "notes", None)], &[("q1", "n1", Some("blocked"))]);
        let (places, w, shown) = sessions_with(web(), notes.clone());
        let snaps = |k: &SessionKey| if *k == key("default") { Some(&shown) } else if *k == key("notes") { Some(&notes) } else { None };
        let collapsed: HashSet<String> = [key("default").0].into();
        let bar = tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), true, false);
        assert_eq!(kinds(&bar.rows), ["W default"], "no machine or session rows, its fold aside");
        assert_eq!((bar.kept.is_none(), bar.drags), (true, true));
        assert_eq!(order(&bar, snaps), ["default:w1", "default:w2", "default:w3", "default:w4"]);
        let focused = tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), true, true);
        assert_eq!(kinds(&focused.rows), ["W default"], "notes has an agent, but the bar shows the shown session alone");
        assert_eq!(order(&focused, snaps), ["default:w1", "default:w2", "default:w4"]);
        assert!(!focused.drags);
    }

    #[test]
    fn focus_off_is_the_tree_as_before() {
        let notes = snap(&[("n1", "notes", None)], &[("q1", "n1", Some("blocked"))]);
        let (places, w, shown) = sessions_with(web(), notes);
        for collapsed in [HashSet::new(), [key("notes").0].into()] {
            let view = tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), false, false);
            assert_eq!(view.rows, model_rows(&places, &w, &collapsed, Some(&shown), &key("default"), false));
            assert_eq!(view.rows, sidebar_tree::tree(&places, &w, &collapsed, Some(&shown), false));
            assert_eq!((view.kept.is_none(), view.empty, view.drags), (true, None, true));
        }
        let plain = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, false);
        assert_eq!(kinds(&plain.rows), ["S default", "W default", "S notes", "W notes", "S old", "N Stopped"]);
    }

    #[test]
    fn numbering_is_the_rows_focus_mode_draws_in_their_order() {
        let notes = snap(&[("n1", "notes", None), ("n2", "drafts", None)], &[("q1", "n2", Some("blocked"))]);
        let (places, w, shown) = sessions_with(web(), notes.clone());
        let snaps = |k: &SessionKey| if *k == key("default") { Some(&shown) } else if *k == key("notes") { Some(&notes) } else { None };
        let view = tree_view(&places, &w, &HashSet::new(), Some(&shown), &key("default"), false, true);
        // what `workspace_rows` draws for each session row: `shown_groups` with what focus mode keeps of it
        let drawn: Vec<String> = view
            .rows
            .iter()
            .filter_map(|r| if let TreeRow::Workspaces { key } = r { Some(key) } else { None })
            .flat_map(|k| {
                let keep = view.kept.as_ref().and_then(|kept| kept.get(k));
                shown_groups(snaps(k).unwrap(), keep).into_iter().flat_map(|(w, kids)| std::iter::once(w).chain(kids)).map(|w| format!("{}:{}", &k.0[5..k.0.len() - 5], w.workspace_id)).collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(drawn, ["default:w1", "default:w2", "default:w4", "notes:n2"]);
        assert_eq!(order(&view, snaps), drawn);
    }

    #[test]
    fn nothing_drags_in_focus_mode() {
        let (places, w, shown) = sessions();
        let open = HashSet::new();
        assert!(tree_view(&places, &w, &open, Some(&shown), &key("default"), false, false).drags);
        assert!(tree_view(&places, &w, &open, Some(&shown), &key("default"), true, false).drags, "the sessions bar alone drags");
        assert!(!tree_view(&places, &w, &open, Some(&shown), &key("default"), false, true).drags);
        assert!(!tree_view(&places, &w, &open, Some(&shown), &key("default"), true, true).drags);
        let focused = RowFilter { keep: None, drags: drags_while(true) };
        assert!(!focused.allows(true), "a row that would drag, or take a drop");
        assert!(RowFilter { keep: None, drags: drags_while(false) }.allows(true));
    }

    /// A settings file from before groups always showed their worktrees: its saved folds (a bare id, as the first
    /// builds kept them, and one by session) load and hide nothing; the rows drawn, each of `shown_groups` with its
    /// worktrees under it, are what Workspace1–9 number, in order. Its session fold still folds and opens.
    #[test]
    fn legacy_worktree_folds_hide_nothing_and_numbering_is_the_rows_drawn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let file = json!({"folded": ["w1", root_key(&key("default").0, "w1")], "collapsed_sessions": [key("notes").0]});
        std::fs::write(&path, file.to_string()).unwrap();
        let (settings, err) = crate::settings::Settings::load(&path);
        assert!(err.is_none(), "{err:?}");
        let (places, w, shown) = sessions();
        let rows = tree_view(&places, &w, &sidebar_tree::saved_folds(&settings), Some(&shown), &key("default"), false, false).rows;
        assert_eq!(kinds(&rows), ["S default", "W default", "S notes folded", "S old", "N Stopped"]);
        // what `workspace_rows` draws: each of `shown_groups`, its worktrees under it
        let drawn: Vec<String> = sidebar_tree::shown_groups(&shown, None).into_iter().flat_map(|(w, kids)| std::iter::once(w).chain(kids)).map(|w| w.workspace_id.clone()).collect();
        assert_eq!(drawn, ["w1", "w2", "w3", "w4"], "the folded checkout's worktrees drawn");
        let snaps = |k: &SessionKey| (*k == key("default")).then_some(&shown);
        let numbered: Vec<String> = workspace_order(&rows, snaps, None).into_iter().map(|(_, w)| w).collect();
        assert_eq!(numbered, drawn);
        // the session's fold still opens, and folds again
        let mut collapsed = settings.collapsed_sessions.clone();
        sidebar_tree::toggle_in(&mut collapsed, &key("notes").0);
        assert!(collapsed.is_empty());
        sidebar_tree::toggle_in(&mut collapsed, &key("notes").0);
        assert_eq!(collapsed, [key("notes").0]);
        // saved again, the old folds are gone
        settings.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("\"folded\""));
    }

    #[test]
    fn focus_mode_is_off_until_turned_on() {
        assert!(!crate::settings::Settings::default().sidebar_focus);
        let old: crate::settings::Settings = serde_json::from_str(r#"{"sidebar_peek": false}"#).unwrap();
        assert!(!old.sidebar_focus, "a file from before focus mode");
        let on: crate::settings::Settings = serde_json::from_str(r#"{"sidebar_focus": true}"#).unwrap();
        assert!(on.sidebar_focus && on.tree_agents_only() && !on.tree_one_session());
    }

    /// A titlebar as the shell draws the sidebar's part of it: the toggle's place, the chip when focus mode is on, the
    /// ⋯, all at `w` of sidebar.
    struct Bar {
        w: f32,
        focus: bool,
    }

    impl gpui::Render for Bar {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            let (w, focus) = (self.w, self.focus);
            div()
                .relative()
                .w(px(w))
                .h(px(TITLEBAR_HEIGHT))
                .child(div().debug_selector(|| "toggle".into()).absolute().top(px(TRAFFIC_LIGHTS_CENTER - 12.)).left(px(TOGGLE_LEFT)).size(px(24.)))
                .when_some(focus.then(|| titlebar_chip(&theme, w)).flatten(), |el, chip| el.child(chip))
                .when_some(view_button_left(w), |el, left| {
                    el.child(titlebar_button(&theme, "sidebar-view", "icons/more-horizontal.svg").debug_selector(|| "sidebar-view".into()).absolute().top(px(TRAFFIC_LIGHTS_CENTER - 12.)).left(px(left)))
                })
        }
    }

    fn bar_at(cx: &mut gpui::TestAppContext, w: f32, focus: bool) -> &'static mut gpui::VisualTestContext {
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let (_, cx) = cx.add_window_view(|_, _| Bar { w, focus });
        let cx: &'static mut gpui::VisualTestContext = Box::leak(Box::new(cx.clone()));
        cx.run_until_parked();
        cx
    }

    /// Focus on, the chip is in the titlebar before the ⋯: clear of the toggle and of the ⋯ at every width the ⋯ shows,
    /// from the narrowest the sidebar goes to the widest, and where the word will not fit the icon alone.
    #[gpui::test]
    fn the_focus_chip_sits_in_the_titlebar_before_the_menu_button(cx: &mut gpui::TestAppContext) {
        // the dev badge heads the sidebar's row under the titlebar, not the titlebar
        assert!(is_dev_build(), "a test build is a dev build, with its badge");
        for w in [crate::settings::SIDEBAR_MIN, 256., crate::settings::SIDEBAR_MAX, TITLEBAR_CLUSTER + 32., TITLEBAR_CLUSTER + 60., TITLEBAR_CLUSTER + 100., TITLEBAR_CLUSTER + 200.] {
            let cx = bar_at(cx, w, true);
            let toggle = cx.debug_bounds("toggle").unwrap();
            let more = cx.debug_bounds("sidebar-view").unwrap_or_else(|| panic!("the ⋯ drawn at {w}"));
            let chip = cx.debug_bounds("sidebar-focus-chip");
            let Some(chip) = chip else {
                assert!(w < TITLEBAR_CLUSTER + 100., "no chip at {w}, where the icon fits");
                continue;
            };
            assert!(chip.left() >= toggle.right(), "{w}: chip {:?} over the toggle {:?}", chip, toggle);
            assert!(chip.right() <= more.left(), "{w}: chip {:?} over the ⋯ {:?}", chip, more);
            assert!(chip.right() <= px(w), "{w}: chip inside the sidebar");
            assert_eq!(more.left() - chip.right(), px(6.), "{w}: the chip beside the ⋯");
            assert!(chip.top() >= px(0.) && chip.bottom() <= px(TITLEBAR_HEIGHT), "{w}: chip inside the titlebar");
        }
        // wide: the word; narrow: the icon alone; where the ⋯ hides: neither
        let wide = bar_at(cx, 256., true).debug_bounds("sidebar-focus-chip").unwrap();
        assert_eq!(wide.size.width, px(if cfg!(target_os = "macos") { 24. } else { 66. }));
        let icon = bar_at(cx, TITLEBAR_CLUSTER + 100., true).debug_bounds("sidebar-focus-chip").expect("the icon chip where only it fits");
        assert_eq!(icon.size.width, px(24.));
        let narrow = bar_at(cx, TITLEBAR_CLUSTER + 60., true);
        assert!(narrow.debug_bounds("sidebar-view").is_some() && narrow.debug_bounds("sidebar-focus-chip").is_none(), "the ⋯ shows, the chip clears the toggle nowhere");
        let hidden = bar_at(cx, TITLEBAR_CLUSTER + 31., true);
        assert!(hidden.debug_bounds("sidebar-view").is_none() && hidden.debug_bounds("sidebar-focus-chip").is_none(), "chip hides with the ⋯");
    }

    #[gpui::test]
    fn focus_off_shows_no_chip(cx: &mut gpui::TestAppContext) {
        let cx = bar_at(cx, 256., false);
        assert!(cx.debug_bounds("sidebar-focus-chip").is_none());
        assert!(cx.debug_bounds("sidebar-view").is_some(), "the ⋯ stays");
    }

    /// The sidebar's head row has no chip: it is the same row with focus mode on or off, as one built from the place's
    /// label alone.
    #[gpui::test]
    fn the_sidebar_head_has_no_chip_row(cx: &mut gpui::TestAppContext) {
        let cx = head_with(cx, false, false);
        let head = cx.debug_bounds("sidebar-head").expect("the head drawn");
        assert!(cx.debug_bounds("sidebar-focus-chip").is_none(), "no chip in the head");
        // 8 px over and 6 under the dev badge, the tallest thing in it: the same with focus mode on or off
        assert_eq!(head.size.height, px(8. + 18. + 6.));
    }

    /// The head as `sidebar_head` draws it for a tree that names its sessions (`listed`) or not, with a spinner or not.
    struct Head {
        listed: bool,
        spinning: bool,
    }

    impl gpui::Render for Head {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            let spinner = self.spinning.then(|| div().debug_selector(|| "head-spinner".into()).size(px(6.)));
            div().w(px(256.)).children(super::super::sidebar::sidebar_head(&theme, self.listed, || "default".to_string(), spinner))
        }
    }

    fn head_with(cx: &mut gpui::TestAppContext, listed: bool, spinning: bool) -> &'static mut gpui::VisualTestContext {
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let (_, cx) = cx.add_window_view(|_, _| Head { listed, spinning });
        let cx: &'static mut gpui::VisualTestContext = Box::leak(Box::new(cx.clone()));
        cx.run_until_parked();
        cx
    }

    /// The head row is drawn only with the place's label (the tree names no session) or the connecting spinner in it;
    /// with neither it takes no room under the titlebar.
    #[gpui::test]
    fn the_sidebar_head_is_drawn_only_with_a_label_or_the_spinner(cx: &mut gpui::TestAppContext) {
        // (the tree names its sessions, spinner): label only, spinner only, both, neither
        for (listed, spinning, label, spinner) in [(false, false, true, false), (true, true, false, true), (false, true, true, true), (true, false, false, false)] {
            let cx = head_with(cx, listed, spinning);
            let case = format!("listed {listed}, spinning {spinning}");
            assert_eq!(cx.debug_bounds("sidebar-head").is_some(), label || spinner, "{case}: the row");
            assert_eq!(cx.debug_bounds("sidebar-head-label").is_some(), label, "{case}: the label");
            assert_eq!(cx.debug_bounds("head-spinner").is_some(), spinner, "{case}: the spinner");
        }
    }
}
