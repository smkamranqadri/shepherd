//! The file viewer over the pane grid (`ui::viewer`): one per workspace, kept while Shepherd runs, so going back to
//! a workspace finds the file it showed. Only the shown workspace's is drawn, never on the settings page; while it
//! is up it has the keyboard the panes would (they are under it), and Esc gives it back to the pane.
use super::*;
use crate::files::roots::root_key;
use crate::ui::viewer::{FileViewer, ViewerEvent};
use std::path::PathBuf;

/// Viewers kept, each up to `content::MAX_TEXT` of text: opening a file in a ninth workspace drops the viewer
/// opened (or opened into) longest ago.
const MAX_VIEWERS: usize = 8;

impl Shell {
    /// The shown workspace's key among the viewers: its place and id (`roots::root_key`).
    fn viewer_key(&self) -> Option<String> {
        let place = self.shown_place().unwrap_or_else(|| self.client.path().to_string_lossy().into_owned());
        Some(root_key(&place, self.shown_workspace.as_deref()?))
    }

    /// The viewer drawn over the grid: the shown workspace's, off the settings page.
    pub(super) fn shown_viewer(&self) -> Option<Entity<FileViewer>> {
        if self.settings.is_some() {
            return None;
        }
        let key = self.viewer_key()?;
        self.viewers.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
    }

    /// Shows `path` (at `line`) in the shown workspace's viewer, made for it if it has none, with the keyboard.
    pub(super) fn open_file(&mut self, path: PathBuf, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.viewer_key() else { return };
        let root = self.panel_context(window, cx).root.map(PathBuf::from);
        let viewer = match self.viewers.iter().position(|(k, _)| *k == key) {
            Some(at) => self.viewers.remove(at).1,
            None => {
                let rt = self.rt.clone();
                let viewer = cx.new(|cx| FileViewer::new(rt, cx));
                let closes = key.clone();
                cx.subscribe_in(&viewer, window, move |s: &mut Shell, _, event: &ViewerEvent, window, cx| match event {
                    ViewerEvent::Dismiss => s.close_viewer(&closes, window, cx),
                    ViewerEvent::OpenInEditor { path, line } => {
                        let pane = s.active_pane(window, cx);
                        s.close_viewer(&closes, window, cx);
                        crate::terminal::pane::open_in_editor(path.clone(), *line, None, pane, cx);
                    }
                })
                .detach();
                viewer
            }
        };
        viewer.update(cx, |v, cx| v.open(path, line, root, window, cx));
        window.focus(&viewer.read(cx).focus_handle(cx), cx);
        self.viewers.insert(0, (key, viewer));
        for (_, gone) in self.viewers.split_off(self.viewers.len().min(MAX_VIEWERS)) {
            gone.update(cx, |v, cx| v.release(Some(window), cx));
        }
        cx.notify();
    }

    /// The tree's "Open in editor" (its menu, Cmd/Ctrl+Enter): the editor's pane (nvim's split) must not open under
    /// the shown viewer, which would also take the keyboard back from it, so the viewer closes first.
    pub(super) fn open_in_editor_from_panel(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane(window, cx);
        if let Some(key) = self.shown_viewer().and(self.viewer_key()) {
            self.close_viewer(&key, window, cx);
        }
        crate::terminal::pane::open_in_editor(path, None, None, pane, cx);
    }

    /// Closes the viewer kept under `key`; the keyboard goes back to the pane when it had it.
    fn close_viewer(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.viewers.iter().position(|(k, _)| k == key) else { return };
        let (_, viewer) = self.viewers.remove(at);
        self.retire_viewer(viewer, window, cx);
        cx.notify();
    }

    /// A viewer leaving for good: its image and highlight let go, and the keyboard back to the pane if it had it.
    fn retire_viewer(&mut self, viewer: Entity<FileViewer>, window: &mut Window, cx: &mut Context<Self>) {
        let focused = viewer.read(cx).focus_handle(cx).contains_focused(window, cx);
        viewer.update(cx, |v, cx| v.release(Some(window), cx));
        if focused {
            self.focus_pane(window, cx);
        }
    }

    /// On each snapshot of the shown place (as `prune_roots`): the viewers of its workspaces that closed go. They
    /// are retired on the next render, which has the window to see whether one had the keyboard.
    pub(super) fn prune_viewers(&mut self, _: &mut Context<Self>) {
        let (Some(place), Some(snapshot)) = (self.shown_place(), self.snapshot.as_ref()) else { return };
        if snapshot.workspaces.is_empty() {
            return;
        }
        let prefix = format!("{place}\n");
        let live = |k: &str| k.strip_prefix(&prefix).is_none_or(|ws| snapshot.workspaces.iter().any(|w| w.workspace_id == ws));
        if self.viewers.iter().all(|(k, _)| live(k)) {
            return;
        }
        let (keep, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.viewers).into_iter().partition(|(k, _)| live(k));
        self.viewers = keep;
        self.viewers_gone.extend(gone.into_iter().map(|(_, v)| v));
    }

    /// Each render, before the panes take focus: retires the pruned viewers; a viewer no longer drawn (another
    /// workspace shown, the settings page) lets go of its image and gives the pane back the keyboard it had.
    /// Returns the viewer drawn, and whether it has the keyboard.
    pub(super) fn sync_viewers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> (Option<Entity<FileViewer>>, bool) {
        for viewer in std::mem::take(&mut self.viewers_gone) {
            self.retire_viewer(viewer, window, cx);
        }
        let shown = self.shown_viewer();
        let drawn = shown.as_ref().map(|v| v.entity_id());
        if self.viewer_drawn != drawn {
            if let Some(hidden) = self.viewers.iter().find(|(_, v)| Some(v.entity_id()) == self.viewer_drawn).map(|(_, v)| v.clone()) {
                hidden.update(cx, |v, cx| v.evict_image(Some(window), cx));
            }
            self.viewer_drawn = drawn;
        }
        let hidden_focused = self.viewers.iter().any(|(_, v)| Some(v.entity_id()) != drawn && v.read(cx).focus_handle(cx).contains_focused(window, cx));
        if hidden_focused && self.settings.is_none() {
            self.focus_pane(window, cx);
        }
        let focused = shown.as_ref().is_some_and(|v| v.read(cx).focus_handle(cx).contains_focused(window, cx));
        (shown, focused)
    }

    /// After the panes took focus: a pane under the viewer (or the window, with none) that has the keyboard hands it
    /// to the viewer, which covers them. A popup's terminal, the side panel or the command panel keep theirs.
    pub(super) fn viewer_takes_keys(&mut self, viewer: &Entity<FileViewer>, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.panes.values().any(|p| p.read(cx).focus_handle(cx).is_focused(window));
        if pane || self.focus.is_focused(window) {
            window.focus(&viewer.read(cx).focus_handle(cx), cx);
        }
    }

    /// The viewer over the grid, as the grid's last child but herdr's popup.
    pub(super) fn viewer_overlay(viewer: Entity<FileViewer>) -> impl IntoElement {
        div().absolute().inset_0().occlude().child(viewer)
    }
}
