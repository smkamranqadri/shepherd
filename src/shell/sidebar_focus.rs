//! The sidebar's focus mode (`sidebar_focus`): the Workspaces tree narrowed to the session shown, without its header;
//! the panels go on listing every session.
use super::sessions::Place;
use super::sidebar_tree::{TreeRow, model_rows};
use super::watchers::{SessionKey, Watchers};
use super::*;
use std::collections::HashSet;

/// The Workspaces tree as the sidebar shows it, and whether its rows drag.
#[derive(Debug)]
pub(super) struct TreeView {
    pub rows: Vec<TreeRow>,
    pub drags: bool,
}

/// The Workspaces tree for `places`: [`model_rows`]'s, in focus mode the shown session's alone without a header
/// (machines and sessions turned off left out too), its folds shown open (`collapsed` is left as saved).
pub(super) fn tree_view(places: &[Place], w: &Watchers, collapsed: &HashSet<String>, snapshot: Option<&Snapshot>, shown: &SessionKey, focus: bool) -> TreeView {
    let open = HashSet::new();
    let collapsed = if focus { &open } else { collapsed };
    let rows = if focus {
        let one: Vec<Place> = places.iter().filter(|p| SessionKey::of(p) == *shown).cloned().collect();
        let mut rows = model_rows(&one, w, collapsed, snapshot, shown);
        rows.retain(|r| !matches!(r, TreeRow::Machine { .. } | TreeRow::Session { .. }));
        rows
    } else {
        model_rows(places, w, collapsed, snapshot, shown)
    };
    TreeView { rows, drags: true }
}

impl Shell {
    /// The Workspaces tree shown now (focus mode applied), and the shown session's key.
    pub(super) fn shown_tree(&self, cx: &mut App) -> (TreeView, SessionKey) {
        let settings = &SettingsGlobal::of(cx).settings;
        let collapsed = sidebar_tree::saved_folds(settings);
        let focus = settings.focus_mode();
        let shown = self.shown_key().unwrap_or_default();
        let view = tree_view(&self.places, &self.watchers, &collapsed, self.snapshot.as_ref(), &shown, focus);
        (view, shown)
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
    use super::super::sidebar_tree::workspace_order;
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
    fn the_tree_drags_with_focus_mode_on_or_off() {
        let (places, w, shown) = sessions();
        let open = HashSet::new();
        assert!(tree_view(&places, &w, &open, Some(&shown), &key("default"), false).drags);
        assert!(tree_view(&places, &w, &open, Some(&shown), &key("default"), true).drags, "focus mode drags");
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
        let rows = tree_view(&places, &w, &sidebar_tree::saved_folds(&settings), Some(&shown), &key("default"), false).rows;
        assert_eq!(kinds(&rows), ["S default", "W default", "S notes folded", "S old", "N Stopped"]);
        // what `workspace_rows` draws: each of `shown_groups`, its worktrees under it
        let drawn: Vec<String> = sidebar_tree::shown_groups(&shown).into_iter().flat_map(|(w, kids)| std::iter::once(w).chain(kids)).map(|w| w.workspace_id.clone()).collect();
        assert_eq!(drawn, ["w1", "w2", "w3", "w4"], "the folded checkout's worktrees drawn");
        let snaps = |k: &SessionKey| (*k == key("default")).then_some(&shown);
        let numbered: Vec<String> = workspace_order(&rows, snaps).into_iter().map(|(_, w)| w).collect();
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
    fn focus_mode_shows_the_shown_session_alone_without_headers() {
        let (places, w, shown) = sessions();
        let collapsed: HashSet<String> = [key("default").0].into();
        let focus = tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), true);
        assert_eq!(kinds(&focus.rows), ["W default"], "no machine or session rows, its fold aside");
        let notes = w.snapshot(&key("notes")).unwrap().clone();
        let snaps = |k: &SessionKey| if *k == key("default") { Some(&shown) } else if *k == key("notes") { Some(&notes) } else { None };
        let order: Vec<String> = workspace_order(&focus.rows, snaps).into_iter().map(|(_, w)| w).collect();
        assert_eq!(order, ["w1", "w2", "w3", "w4"], "Workspace1–9 number the shown session's");
        let plain = tree_view(&places, &w, &collapsed, Some(&shown), &key("default"), false);
        assert_eq!(kinds(&plain.rows)[..2], ["S default folded", "S notes"], "off, every session, its saved fold kept");
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
