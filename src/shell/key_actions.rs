//! The actions herdr's keymap has that Shepherd runs through herdr's API: the ones a chord of `[keys]` reaches (a
//! pane's prefix keys, `herdr/keys.rs`) and the Shortcuts page lists. Each is the same thing herdr's own screen does.
use super::*;

/// The panes with an agent in `snapshot` as (tab, pane), in the sidebar's order: the workspaces as its tree lists
/// them, each one's tabs, each tab's panes.
pub(super) fn agent_panes(snapshot: &Snapshot) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (workspace, _) in snapshot.workspace_tree() {
        for tab in snapshot.tabs_of(&workspace.workspace_id) {
            out.extend(snapshot.panes.iter().filter(|p| p.tab_id == tab.tab_id && p.agent.is_some()).map(|p| (p.tab_id.clone(), p.pane_id.clone())));
        }
    }
    out
}

/// The item after (or before) `current` in `list`, round again at the end; the first (the last) when `current` is
/// none or not in it.
pub(super) fn step_round<T: Clone + PartialEq>(list: &[T], current: Option<&T>, forward: bool) -> Option<T> {
    let n = list.len();
    let at = current.and_then(|c| list.iter().position(|x| x == c));
    let to = match (at, forward) {
        (Some(i), true) => (i + 1) % n.max(1),
        (Some(i), false) => (i + n.max(1) - 1) % n.max(1),
        (None, true) => 0,
        (None, false) => n.saturating_sub(1),
    };
    list.get(to).cloned()
}

/// Where `tab_id` goes to move one place toward the front (or back) among `tabs`, as `tab.move` takes it: a slot in
/// the list before the move. None when it is already there.
pub(super) fn tab_slot(tabs: &[String], tab_id: &str, forward: bool) -> Option<usize> {
    let i = tabs.iter().position(|t| t == tab_id)?;
    match forward {
        true if i + 1 < tabs.len() => Some(i + 2),
        false if i > 0 => Some(i - 1),
        _ => None,
    }
}

impl Shell {
    /// herdr's `reload_config`.
    pub(super) fn reload_herdr_config(&mut self, cx: &mut Context<Self>) {
        self.send(Command::ReloadConfig, cx, |_, _, cx| crate::ui::toast::info(cx, "herdr reloaded its config", None));
    }

    /// herdr's `rename_tab`: the shown tab's name, in the strip.
    pub(super) fn rename_shown_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.shown_tab() else { return };
        self.start_rename(RenameTarget::Tab(tab.tab_id), tab.label, window, cx);
    }

    /// herdr's `rename_workspace`: the shown workspace's name, in the sidebar (opened for it).
    pub(super) fn rename_shown_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.shown_tab() else { return };
        let label = self.snapshot.as_ref().and_then(|s| s.workspaces.iter().find(|w| w.workspace_id == tab.workspace_id)).map(|w| w.label.clone());
        if !self.sidebar_open {
            self.toggle_sidebar(cx);
        }
        self.start_rename(RenameTarget::Workspace(tab.workspace_id), label.unwrap_or_default(), window, cx);
    }

    /// herdr's `close_workspace`, as the sidebar's menu does it.
    pub(super) fn close_shown_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.shown_tab() else { return };
        self.send(Command::CloseWorkspace { workspace_id: tab.workspace_id }, cx, |_, _, _| {});
    }

    /// herdr's `new_worktree`, `open_worktree` and `remove_worktree` for the shown workspace: its dialog, its list, its
    /// confirmation. A linked worktree has no worktrees of its own to make or open, and only it can be removed.
    pub(super) fn worktree_key(&mut self, what: WorktreeKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.shown_tab() else { return };
        let linked = self
            .snapshot
            .as_ref()
            .and_then(|s| s.workspaces.iter().find(|w| w.workspace_id == tab.workspace_id))
            .and_then(|w| w.worktree.as_ref())
            .is_some_and(|w| w.is_linked_worktree);
        let id = tab.workspace_id;
        match (what, linked) {
            (WorktreeKey::Remove, true) => self.confirm_remove_worktree(id, false, cx),
            (WorktreeKey::Remove, false) => crate::ui::toast::info(cx, "Only a linked worktree can be removed", None),
            (WorktreeKey::New, _) => self.open_worktree_dialog(id, window, cx),
            (WorktreeKey::Open, _) => self.open_worktrees_panel(id, window, cx),
        }
    }

    /// herdr's `next_agent` and `previous_agent`: the agent after (before) the one you are in, round the sidebar's
    /// order in this session; the first (last) when you are not in one.
    pub(super) fn step_agent(&mut self, forward: bool, window: &Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.as_ref() else { return };
        let list = agent_panes(snapshot);
        let here = self.active_pane(window, cx).and_then(|pane| list.iter().find(|(_, p)| *p == pane).cloned());
        match step_round(&list, here.as_ref(), forward) {
            Some((tab, pane)) => self.show_agent(tab, pane, cx),
            None => crate::ui::toast::info(cx, "No agents in this session", None),
        }
    }

    /// herdr's `focus_agent`: the `index`th agent of this session, in the sidebar's order.
    pub(super) fn go_to_agent(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.as_ref() else { return };
        match agent_panes(snapshot).get(index).cloned() {
            Some((tab, pane)) => self.show_agent(tab, pane, cx),
            None => crate::ui::toast::info(cx, format!("No agent {}", index + 1), None),
        }
    }

    /// herdr's `move_tab_previous` and `move_tab_next`: the shown tab one place toward the front or the back.
    pub(super) fn move_shown_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (Some(snapshot), Some(tab)) = (self.snapshot.as_ref(), self.shown_tab()) else { return };
        let tabs: Vec<String> = snapshot.tabs_of(&tab.workspace_id).iter().map(|t| t.tab_id.clone()).collect();
        if let Some(insert_index) = tab_slot(&tabs, &tab.tab_id, forward) {
            self.send(Command::MoveTab { tab_id: tab.tab_id, insert_index }, cx, |_, _, _| {});
        }
    }

    /// herdr's `cycle_pane_next` and `cycle_pane_previous`: the next (previous) pane of the shown tab, in its layout's
    /// order, round again.
    pub(super) fn cycle_pane(&mut self, forward: bool, window: &Window, cx: &mut Context<Self>) {
        let (Some(snapshot), Some(tab)) = (self.snapshot.as_ref(), self.shown_tab()) else { return };
        let Some(layout) = snapshot.layouts.iter().find(|l| l.tab_id == tab.tab_id) else { return };
        let panes: Vec<String> = layout.panes.iter().map(|p| p.pane_id.clone()).collect();
        let here = self.active_pane(window, cx);
        if let Some(pane_id) = step_round(&panes, here.as_ref(), forward) {
            self.show_terminals(cx);
            self.focus_next = Some(pane_id.clone());
            self.send(Command::FocusPaneId { pane_id }, cx, |_, _, _| {});
        }
    }

    /// herdr's `rename_pane`: asks for the name of the pane you are in.
    pub(super) fn rename_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_terminals(cx);
        if let Some(pane_id) = self.active_pane(window, cx) {
            self.open_pane_name_dialog(pane_id, window, cx);
        }
    }

    /// A chord of a herdr action Shepherd has no equivalent of, caught instead of typed into the pane.
    pub(super) fn herdr_only(&mut self, only: &crate::herdr::keys::HerdrOnly, cx: &mut Context<Self>) {
        crate::ui::toast::info(cx, herdr_only_message(only), None);
    }

    /// A key after the prefix that herdr binds to nothing: swallowed, as herdr's prefix mode does.
    pub(super) fn herdr_unbound(&mut self, unbound: &crate::herdr::keys::HerdrUnbound, cx: &mut Context<Self>) {
        if !unbound.chord.is_empty() {
            crate::ui::toast::info(cx, format!("herdr binds nothing to {}", spaced_chord(&unbound.chord)), None);
        }
    }
}

/// The three worktree actions of herdr's keys that need the shown workspace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum WorktreeKey {
    New,
    Open,
    Remove,
}

/// What a pane says for a chord of herdr's own action: where it does run.
pub(super) fn herdr_only_message(only: &crate::herdr::keys::HerdrOnly) -> String {
    format!("{} is an action of herdr's own screen; Shepherd has no equivalent. Run it in herdr's terminal UI.", only.name)
}

/// A chord of keystrokes as the settings page writes it: `ctrl-b a` → `Ctrl+B then A`.
pub(super) fn spaced_chord(chord: &str) -> String {
    chord.split_whitespace().map(crate::ui::settings::display_chord).collect::<Vec<_>>().join(" then ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_goes_round_and_starts_at_an_end_when_nowhere() {
        let list = ["a", "b", "c"];
        assert_eq!(step_round(&list, Some(&"a"), true), Some("b"));
        assert_eq!(step_round(&list, Some(&"c"), true), Some("a"));
        assert_eq!(step_round(&list, Some(&"a"), false), Some("c"));
        assert_eq!(step_round(&list, None, true), Some("a"));
        assert_eq!(step_round(&list, None, false), Some("c"));
        assert_eq!(step_round(&list, Some(&"gone"), false), Some("c"));
        assert_eq!(step_round::<&str>(&[], None, true), None);
    }

    #[test]
    fn a_tab_moves_one_place_and_stops_at_the_ends() {
        let tabs: Vec<String> = ["t1", "t2", "t3"].map(String::from).to_vec();
        assert_eq!(tab_slot(&tabs, "t2", false), Some(0));
        assert_eq!(tab_slot(&tabs, "t2", true), Some(3), "a slot before the move: past the last");
        assert_eq!(tab_slot(&tabs, "t1", false), None);
        assert_eq!(tab_slot(&tabs, "t3", true), None);
        assert_eq!(tab_slot(&tabs, "t1", true), Some(2));
        assert_eq!(tab_slot(&tabs, "nope", true), None);
    }

    #[test]
    fn agents_are_listed_in_the_sidebars_order() {
        let panes = serde_json::json!([
            {"pane_id": "p3", "tab_id": "t2", "workspace_id": "w1", "terminal_id": "x", "agent_status": "idle", "agent": "claude"},
            {"pane_id": "p1", "tab_id": "t1", "workspace_id": "w1", "terminal_id": "x", "agent_status": "idle", "agent": "codex"},
            {"pane_id": "p2", "tab_id": "t1", "workspace_id": "w1", "terminal_id": "x", "agent_status": "idle"},
            {"pane_id": "p4", "tab_id": "t3", "workspace_id": "w2", "terminal_id": "x", "agent_status": "idle", "agent": "claude"},
        ]);
        let tab = |id: &str, ws: &str, n: u32| serde_json::json!({"tab_id": id, "workspace_id": ws, "label": id, "number": n, "agent_status": "idle"});
        let ws = |id: &str, n: u32| serde_json::json!({"workspace_id": id, "label": id, "number": n, "active_tab_id": null, "agent_status": "idle", "worktree": null});
        let snapshot = Snapshot::from_result(&serde_json::json!({"snapshot": {
            "focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null,
            "workspaces": [ws("w1", 1), ws("w2", 2)], "tabs": [tab("t1", "w1", 1), tab("t2", "w1", 2), tab("t3", "w2", 3)],
            "panes": panes, "layouts": []}}))
        .unwrap();
        let order: Vec<_> = agent_panes(&snapshot).into_iter().map(|(_, p)| p).collect();
        assert_eq!(order, ["p1", "p3", "p4"], "tab t1's agent, then t2's, then the next workspace's; p2 runs no agent");
    }

    #[test]
    fn a_caught_chord_says_where_the_action_runs() {
        let only = crate::herdr::keys::HerdrOnly { name: "copy_mode".into(), chord: "prefix+[".into() };
        assert!(herdr_only_message(&only).starts_with("copy_mode is an action of herdr's own screen"));
        assert_eq!(spaced_chord("ctrl-b a"), "Ctrl+B then A");
    }
}
