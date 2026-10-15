//! File tabs: each workspace's own, after its herdr tabs in the tab bar (`files::tabs`), local to Shepherd and never
//! told to herdr. While one is shown its view (`ui::viewer`) is drawn in the grid's place: the panes are not drawn,
//! so they keep their size, and the view has the keyboard they would. The herdr tab Shepherd shows (`selected_tab`)
//! stays what the terminals come back to; every way of showing a herdr tab goes through `show_herdr_tab`.
//!
//! Views are made when a tab is first shown (its file read then, not when it is restored), and the most recently
//! shown `MAX_VIEWS` are kept; the pinned tabs are kept for the next run in `open-files.json`
//! (`files::open_files`), written a second after the last change and only when it changed.
//!
//! A change in the Changes tab opens its diff (`ui::diff_view`) as a file tab of its own kind (`TabKind::Diff`),
//! by the same preview rules, never kept for the next run; the shown one reads its diff again on every Changes
//! poll and whenever it comes into view.
use super::*;
use crate::files::open_files::{self, Remembered};
use crate::files::roots::{self, root_key};
use crate::files::status::{Change, Group};
use crate::files::tabs::{self, FileTab, StripEntry, TabId, TabKind, WorkspaceTabs};
use crate::files::diff::Entries;
use crate::ui::diff_view::{DiffEvent, DiffSpec, DiffView};
use crate::ui::viewer::{FileViewer, ViewerEvent};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// Views kept, each up to `content::MAX_TEXT` of text: showing a tab past this many reads its file again.
const MAX_VIEWS: usize = 12;
/// How long after the last change the kept tabs are written.
const SAVE_AFTER: Duration = Duration::from_secs(1);

/// Paths resolved (`real_path`) kept for the run; past this many the cache starts over.
const MAX_REAL_PATHS: usize = 4096;
/// How long resolving a path may take: a file on a hung mount never answers, and is then taken as written.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);

/// `path` with its symlinks resolved, on the runtime's blocking pool; as written when it cannot be (gone, a hung mount).
fn real_path(rt: &tokio::runtime::Handle, path: PathBuf) -> impl std::future::Future<Output = PathBuf> + use<> {
    let fallback = path.clone();
    let job = rt.spawn(async move {
        let asked = path.clone();
        let real = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::task::spawn_blocking(move || std::fs::canonicalize(&asked))).await;
        match real {
            Ok(Ok(Ok(real))) => real,
            _ => path,
        }
    });
    async move { job.await.unwrap_or(fallback) }
}

/// A file tab's view: a file's viewer, or a change's diff.
#[derive(Clone)]
pub(super) enum TabView {
    File(Entity<FileViewer>),
    Diff(Entity<DiffView>),
}

impl TabView {
    pub(super) fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        match self {
            TabView::File(v) => v.read(cx).focus_handle(cx),
            TabView::Diff(v) => v.read(cx).focus_handle(cx),
        }
    }

    /// Markdown's preview is on screen: its selection is `ui::markdown`'s, which must not be forgotten.
    pub(super) fn shows_markdown(&self, cx: &App) -> bool {
        matches!(self, TabView::File(v) if v.read(cx).shows_markdown())
    }

    pub(super) fn element(&self) -> gpui::AnyElement {
        match self {
            TabView::File(v) => v.clone().into_any_element(),
            TabView::Diff(v) => v.clone().into_any_element(),
        }
    }

    /// Out of sight: an image leaves gpui's cache, a large diff goes (read again when shown), a menu closes.
    fn hide(&self, window: Option<&mut Window>, cx: &mut App) {
        match self {
            TabView::File(v) => v.update(cx, |v, cx| v.evict_image(window, cx)),
            TabView::Diff(v) => v.update(cx, |v, _| v.hide()),
        }
    }

    /// Leaving for good: its image, highlight or read let go.
    fn release(&self, window: Option<&mut Window>, cx: &mut App) {
        match self {
            TabView::File(v) => v.update(cx, |v, cx| v.release(window, cx)),
            TabView::Diff(v) => v.update(cx, |v, _| v.release()),
        }
    }
}

/// The file tabs kept for the next run, as last read and changed since.
#[derive(Default)]
pub(super) struct KeptTabs {
    map: BTreeMap<String, Remembered>,
    /// The file has been read: until then nothing is restored or written (it would be written without the rest).
    loaded: bool,
    /// Workspaces whose kept tabs were put back this run (or had none): only their changes are recorded.
    restored: HashSet<String>,
    /// Workspaces of the shown place seen in a snapshot this run, whose kept entry was checked (`open_files::reconcile`).
    seen: HashSet<String>,
    writes: open_files::Writes,
    /// The timer of the next write; the write itself runs detached, so a later timer replacing this one never
    /// cancels it.
    save: Option<gpui::Task<()>>,
}

impl Shell {
    /// A workspace's key among the file tabs: its place and id (`roots::root_key`); none until the places are listed
    /// (as `root_slot`), so no tab is kept under a key `prune_file_tabs` would never see.
    pub(super) fn tabs_key(&self, workspace: &str) -> Option<String> {
        Some(root_key(&self.shown_place()?, workspace))
    }

    /// The shown workspace's key among the file tabs.
    pub(super) fn shown_tabs_key(&self) -> Option<String> {
        self.tabs_key(self.shown_workspace.as_deref()?)
    }

    /// The file tab shown in the grid's place: the shown workspace's active one, off the settings page.
    pub(super) fn shown_file_tab(&self) -> Option<(String, &FileTab)> {
        if self.settings.is_some() {
            return None;
        }
        let key = self.shown_tabs_key()?;
        let tab = self.file_tabs.get(&key)?.shown()?;
        Some((key, tab))
    }

    /// The shown file tab's view, if it has been made.
    pub(super) fn shown_file_view(&self) -> Option<TabView> {
        let (_, tab) = self.shown_file_tab()?;
        self.view_of(tab.id)
    }

    fn view_of(&self, id: TabId) -> Option<TabView> {
        self.tab_views.iter().find(|(t, _)| *t == id).map(|(_, v)| v.clone())
    }

    /// The shown workspace's strip (its herdr tabs, then its file tabs) and the entry shown in it.
    fn shown_strip(&self) -> Option<(String, Vec<StripEntry>, StripEntry)> {
        let (snapshot, tab) = (self.snapshot.as_ref()?, self.shown_tab()?);
        let key = self.tabs_key(&tab.workspace_id)?;
        let herdr: Vec<String> = snapshot.tabs_of(&tab.workspace_id).into_iter().map(|t| t.tab_id.clone()).collect();
        let files = self.file_tabs.get(&key).map_or(&[][..], |t| &t.files[..]);
        let at = match self.shown_file_tab() {
            Some((_, f)) => StripEntry::File(f.id),
            None => StripEntry::Herdr(tab.tab_id),
        };
        Some((key, tabs::strip(&herdr, files), at))
    }

    /// Shows herdr's tab `tab` (`None`: the workspace's own pick). With `terminals`, its workspace's terminals show
    /// (a file tab shown there steps aside, and herdr's focused pane gets the keyboard back); without, the workspace
    /// shows what it showed last, a file tab included (a switch of workspace). The one way `selected_tab` is set.
    pub(super) fn show_herdr_tab(&mut self, tab: Option<String>, terminals: bool, cx: &mut Context<Self>) {
        if terminals {
            let of_tab = tab.as_deref().and_then(|id| self.snapshot.as_ref()?.tabs.iter().find(|t| t.tab_id == id)).map(|t| t.workspace_id.clone());
            if let Some(key) = of_tab.or_else(|| self.shown_workspace.clone()).and_then(|ws| self.tabs_key(&ws))
                && self.file_tabs.get_mut(&key).is_some_and(|t| t.active.take().is_some())
            {
                self.focused = None;
                self.remember_tabs(&key, cx);
            }
        }
        self.selected_tab = tab;
        cx.notify();
    }

    /// A pane command (split, close, focus, zoom) or a herdr command that opens a pane or popup, while a file tab
    /// shows: the terminals it acts on come into view first.
    pub(super) fn show_terminals(&mut self, cx: &mut Context<Self>) {
        if self.shown_file_tab().is_some() {
            self.show_herdr_tab(self.selected_tab.clone(), true, cx);
        }
    }

    /// Tab1–9: the strip's `index`th entry, herdr tabs first.
    pub(super) fn go_to_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((key, strip, _)) = self.shown_strip() else { return };
        match strip.get(index) {
            Some(StripEntry::Herdr(id)) => self.show_herdr_tab(Some(id.clone()), true, cx),
            Some(StripEntry::File(id)) => self.show_file_tab(&key, *id, cx),
            None => {}
        }
    }

    /// Next and previous tab: round the strip, across herdr's tabs and the file tabs.
    pub(super) fn step_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some((key, strip, at)) = self.shown_strip() else { return };
        match tabs::step(&strip, &at, forward) {
            Some(StripEntry::Herdr(id)) => self.show_herdr_tab(Some(id), true, cx),
            Some(StripEntry::File(id)) => self.show_file_tab(&key, id, cx),
            None => {}
        }
    }

    /// Shows the file tab `id` of the workspace `key` (the shown one). Its view, made on the next render if it has
    /// none, takes the keyboard from a pane then.
    pub(super) fn show_file_tab(&mut self, key: &str, id: TabId, cx: &mut Context<Self>) {
        let Some(tabs) = self.file_tabs.get_mut(key) else { return };
        if tabs.active != Some(id) && tabs.get(id).is_some() {
            tabs.active = Some(id);
            self.remember_tabs(key, cx);
        }
        cx.notify();
    }

    /// A double-click on a file tab: it stays past the next preview.
    pub(super) fn pin_file_tab(&mut self, key: &str, id: TabId, cx: &mut Context<Self>) {
        if let Some(tabs) = self.file_tabs.get_mut(key).filter(|t| t.get(id).is_some_and(|t| t.preview)) {
            tabs.pin(id);
            self.remember_tabs(key, cx);
            cx.notify();
        }
    }

    /// Opens `path` (at `line`) in a file tab of the shown workspace and shows it: as a preview (a single click in
    /// the Files tab), which leaves the keyboard where it is, or pinned (a double-click, Enter, the menu's Open),
    /// which gives the tab the keyboard. Tabs are kept by the file's real path (`real_path`), so a file reached
    /// through a symlinked root and through its git top level is one tab.
    pub(super) fn open_file(&mut self, path: PathBuf, line: Option<u32>, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_at(path, line, None, preview, window, cx);
    }

    /// `open_file`, going to `fragment` (a markdown link's `#heading`) once the file is in.
    fn open_file_at(&mut self, path: PathBuf, line: Option<u32>, fragment: Option<String>, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.shown_tabs_key() else { return };
        if let Some(real) = self.real_paths.get(&path).cloned() {
            return self.open_real(key, real, line, fragment, preview, window, cx);
        }
        let real = real_path(&self.rt, path.clone());
        cx.spawn_in(window, async move |this, cx| {
            let real = real.await;
            let _ = this.update_in(cx, |s, window, cx| {
                if s.real_paths.len() >= MAX_REAL_PATHS {
                    s.real_paths.clear();
                }
                s.real_paths.insert(path, real.clone());
                // another workspace or place shown meanwhile: the file is not opened into that one
                if s.shown_tabs_key().as_deref() == Some(key.as_str()) {
                    s.open_real(key, real, line, fragment, preview, window, cx);
                }
            });
        })
        .detach();
    }

    /// `open_file` with the file's real path in hand.
    #[allow(clippy::too_many_arguments)]
    fn open_real(&mut self, key: String, path: PathBuf, line: Option<u32>, fragment: Option<String>, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_tabs(&key, cx);
        let tabs = self.file_tabs.entry(key.clone()).or_default();
        // ponytail: nothing is dirty until tabs edit (E1)
        let id = tabs.open(path.clone(), TabKind::File, preview, |_| false, &mut self.next_tab_id);
        tabs.trim(id, |_| false);
        self.drop_dead_views(cx);
        let view = match self.view_of(id) {
            Some(TabView::File(view)) => {
                // a replaced preview's view shows the new file; the same file again reads it afresh only to go to `line`
                if view.read(cx).path() != path || line.is_some() {
                    let root = self.panel_context(window, cx).root.map(PathBuf::from);
                    view.update(cx, |v, cx| v.open(path, line, root, window, cx));
                }
                view
            }
            // a diff's preview replaced by a file: a viewer in its place
            Some(diff) => {
                self.drop_view(id, diff, window, cx);
                self.make_view(id, path, line, window, cx)
            }
            None => self.make_view(id, path, line, window, cx),
        };
        if let Some(fragment) = fragment {
            view.update(cx, |v, cx| v.go_to(fragment, cx));
        }
        if !preview {
            window.focus(&view.read(cx).focus_handle(cx), cx);
        }
        self.remember_tabs(&key, cx);
        cx.notify();
    }

    /// A change clicked in the Changes tab (or its menu's Open): its diff in a tab, as a preview or pinned as
    /// `open_file`'s files; a conflict's file itself, its markers in it; nothing for a folder.
    pub(super) fn open_change(&mut self, path: PathBuf, change: Change, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        if change.group == Group::Conflicts {
            if change.openable() {
                self.open_file(path, None, preview, window, cx);
            }
            return;
        }
        if change.is_folder() {
            return;
        }
        let Some(key) = self.shown_tabs_key() else { return };
        self.restore_tabs(&key, cx);
        let root = self.panel_context(window, cx).root.map(PathBuf::from).or_else(|| path.parent().map(PathBuf::from)).unwrap_or_default();
        let entries = self.side_panel.read(cx).entries(&change.path);
        let spec = DiffSpec { root, path: path.clone(), change };
        let tabs = self.file_tabs.entry(key.clone()).or_default();
        // ponytail: nothing is dirty until tabs edit (E1)
        let id = tabs.open(path, TabKind::Diff(spec.change.group), preview, |_| false, &mut self.next_tab_id);
        tabs.trim(id, |_| false);
        self.diff_specs.insert(id, spec.clone());
        self.drop_dead_views(cx);
        let view = match self.view_of(id) {
            // the same change again reads it afresh; a replaced preview's view shows the new one
            Some(TabView::Diff(view)) => {
                view.update(cx, |v, cx| v.open(spec, entries, cx));
                TabView::Diff(view)
            }
            Some(file) => {
                self.drop_view(id, file, window, cx);
                self.make_diff_view(id, spec, entries, window, cx)
            }
            None => self.make_diff_view(id, spec, entries, window, cx),
        };
        if !preview {
            window.focus(&view.focus_handle(cx), cx);
        }
        self.remember_tabs(&key, cx);
        cx.notify();
    }

    /// The view in tab `id`'s slot goes (the tab shows another kind now).
    fn drop_view(&mut self, id: TabId, view: TabView, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_views.retain(|(t, _)| *t != id);
        self.retire_view(view, window, cx);
    }

    /// A diff view for the tab `id`, kept first among the views.
    fn make_diff_view(&mut self, id: TabId, spec: DiffSpec, entries: Entries, window: &mut Window, cx: &mut Context<Self>) -> TabView {
        let rt = self.rt.clone();
        let view = cx.new(|cx| DiffView::new(rt, spec, entries, cx));
        cx.subscribe_in(&view, window, |s: &mut Shell, _, event: &DiffEvent, window, cx| match event {
            // "Open file": a pinned tab of the file, at its first change, which takes the keyboard
            DiffEvent::OpenFile { path, line } => s.open_file(path.clone(), *line, false, window, cx),
            DiffEvent::OpenInEditor { path, line } => {
                let pane = s.active_pane(window, cx);
                s.show_terminals(cx);
                crate::terminal::pane::open_in_editor(path.clone(), *line, None, pane, cx);
            }
        })
        .detach();
        let view = TabView::Diff(view);
        self.keep_view(id, view.clone(), cx);
        view
    }

    /// The shown diff tab reads its diff again (a Changes poll answered, `poll`, or it came into view), with its
    /// change as the list has it now.
    pub(super) fn reload_shown_diff(&mut self, poll: bool, cx: &mut Context<Self>) {
        let Some((_, tab)) = self.shown_file_tab() else { return };
        let (id, TabKind::Diff(group)) = (tab.id, tab.kind) else { return };
        let (Some(TabView::Diff(view)), Some(spec)) = (self.view_of(id), self.diff_specs.get_mut(&id)) else { return };
        let panel = self.side_panel.read(cx);
        let now = panel.change_now(group, &spec.change.path);
        let entries = panel.entries(&spec.change.path);
        if let Some(change) = &now {
            spec.change = change.clone();
        }
        view.update(cx, |v, cx| v.reload(now, entries, poll, cx));
    }

    /// A view for the tab `id`, reading `path`, kept first among the views.
    fn make_view(&mut self, id: TabId, path: PathBuf, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) -> Entity<FileViewer> {
        let root = self.panel_context(window, cx).root.map(PathBuf::from);
        let rt = self.rt.clone();
        let view = cx.new(|cx| FileViewer::new(rt, cx));
        cx.subscribe_in(&view, window, move |s: &mut Shell, _, event: &ViewerEvent, window, cx| match event {
            // the editor's pane opens beside the focused one: the terminals show, the file tab stays
            ViewerEvent::OpenInEditor { path, line } => {
                let pane = s.active_pane(window, cx);
                s.show_terminals(cx);
                crate::terminal::pane::open_in_editor(path.clone(), *line, None, pane, cx);
            }
            // a markdown link to another file of the root: a tab of its own, which takes the keyboard
            ViewerEvent::OpenFile { path, line, fragment } => s.open_file_at(path.clone(), *line, fragment.clone(), false, window, cx),
        })
        .detach();
        view.update(cx, |v, cx| v.open(path, line, root, window, cx));
        self.keep_view(id, TabView::File(view.clone()), cx);
        view
    }

    /// Keeps `view` first among the views; the least recently shown past `MAX_VIEWS` go, but not one with unsaved
    /// changes.
    fn keep_view(&mut self, id: TabId, view: TabView, cx: &App) {
        self.tab_views.insert(0, (id, view));
        while self.tab_views.len() > MAX_VIEWS {
            let Some(at) = self.tab_views.iter().rposition(|(t, v)| *t != id && !Self::view_dirty(v, cx)) else { break };
            let (_, gone) = self.tab_views.remove(at);
            self.views_gone.push(gone);
        }
    }

    /// The view has unsaved changes: it is never dropped to save memory.
    // ponytail: always false until tabs edit (E1)
    fn view_dirty(_view: &TabView, _cx: &App) -> bool {
        false
    }

    /// The tree's "Open in editor" (its menu, Cmd/Ctrl+Enter): the editor's pane (nvim's split) opens beside the
    /// focused one, so the terminals show; the file tab stays.
    pub(super) fn open_in_editor_from_panel(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane(window, cx);
        self.show_terminals(cx);
        crate::terminal::pane::open_in_editor(path, None, None, pane, cx);
    }

    /// Ctrl/Cmd+W and Ctrl/Cmd+Shift+W while a file tab shows, its `×`, a middle-click: closes it, showing its
    /// right neighbour, else its left one, else the terminals. True when there was a file tab shown to close.
    pub(super) fn close_shown_file_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((key, id)) = self.shown_file_tab().map(|(k, t)| (k, t.id)) else { return false };
        self.close_file_tab(&key, id, window, cx);
        true
    }

    pub(super) fn close_file_tab(&mut self, key: &str, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        // ponytail: a dirty tab asks Save / Don't Save / Cancel first once tabs edit (E1)
        let Some(tabs) = self.file_tabs.get_mut(key) else { return };
        if tabs.close(id).is_none() {
            // back to the terminals: herdr's focused pane takes the keyboard on the next render
            self.focused = None;
        }
        if let Some(at) = self.tab_views.iter().position(|(t, _)| *t == id) {
            let (_, view) = self.tab_views.remove(at);
            self.retire_view(view, window, cx);
        }
        self.diff_specs.remove(&id);
        self.remember_tabs(key, cx);
        cx.notify();
    }

    /// Views of tabs no workspace has any more (replaced, trimmed, pruned) go, retired on the next render; one with
    /// unsaved changes stays (E1 asks before its tab goes).
    fn drop_dead_views(&mut self, cx: &App) {
        let live: HashSet<TabId> = self.file_tabs.values().flat_map(|t| t.files.iter().map(|f| f.id)).collect();
        // a diff tab's change goes with it (or with its slot, now a file's)
        let diffs: HashSet<TabId> = self.file_tabs.values().flat_map(|t| t.files.iter().filter(|f| matches!(f.kind, TabKind::Diff(_))).map(|f| f.id)).collect();
        self.diff_specs.retain(|id, _| diffs.contains(id));
        let (keep, gone): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.tab_views).into_iter().partition(|(id, v)| live.contains(id) || Self::view_dirty(v, cx));
        self.tab_views = keep;
        self.views_gone.extend(gone.into_iter().map(|(_, v)| v));
    }

    /// A view leaving for good: its image and highlight let go, and the keyboard back to the pane if it had it.
    fn retire_view(&mut self, view: TabView, window: &mut Window, cx: &mut Context<Self>) {
        let focused = view.focus_handle(cx).contains_focused(window, cx);
        view.release(Some(window), cx);
        if focused {
            self.focus_pane(window, cx);
        }
    }

    /// On each snapshot of the shown place (as `prune_roots`): the file tabs of its workspaces that closed go, kept
    /// ones too. Their views are retired on the next render, which has the window to see whether one had the keyboard.
    pub(super) fn prune_file_tabs(&mut self, cx: &mut Context<Self>) {
        let (Some(place), Some(snapshot)) = (self.shown_place(), self.snapshot.as_ref()) else { return };
        if snapshot.workspaces.is_empty() {
            return;
        }
        let live: Vec<(&str, String)> =
            snapshot.workspaces.iter().map(|w| (w.workspace_id.as_str(), roots::fingerprint(&w.label, w.worktree.as_ref().map(|t| t.checkout_path.as_str())))).collect();
        let prefix = format!("{place}\n");
        let is_live = |k: &str| k.strip_prefix(&prefix).is_none_or(|ws| live.iter().any(|(id, _)| *id == ws));
        let before = self.file_tabs.len();
        self.file_tabs.retain(|k, _| is_live(k));
        self.tab_roots.retain(|k, _| is_live(k));
        // a workspace id herdr gives again later starts with nothing kept
        self.kept_tabs.restored.retain(|k| is_live(k));
        // only once the file is read: a workspace is checked against what was kept for it
        let kept = self.kept_tabs.loaded && open_files::reconcile(&mut self.kept_tabs.map, &place, &live, &mut self.kept_tabs.seen);
        if self.file_tabs.len() != before {
            self.drop_dead_views(cx);
        }
        if kept {
            self.save_kept_tabs(cx);
        }
    }

    /// Each render, before the panes take focus: retires the views that went; puts back the shown workspace's kept
    /// tabs the first time it shows; makes the shown tab's view if it has none; a view no longer drawn lets go of
    /// its image and gives the pane back the keyboard it had. Returns the view drawn, and whether it has the keyboard.
    pub(super) fn sync_file_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> (Option<TabView>, bool) {
        for view in std::mem::take(&mut self.views_gone) {
            self.retire_view(view, window, cx);
        }
        if let Some(key) = self.shown_tabs_key().filter(|_| self.settings.is_none()) {
            self.restore_tabs(&key, cx);
            self.follow_root(&key, cx);
        }
        let shown = self.shown_file_tab().map(|(_, t)| (t.id, t.path.clone(), t.kind));
        let drawn = shown.as_ref().map(|(id, ..)| *id);
        let view = shown.map(|(id, path, kind)| match self.tab_views.iter().position(|(t, _)| *t == id) {
            Some(0) => self.tab_views[0].1.clone(),
            Some(at) => {
                let kept = self.tab_views.remove(at);
                self.tab_views.insert(0, kept.clone());
                kept.1
            }
            None => match (kind, self.diff_specs.get(&id).cloned()) {
                (TabKind::Diff(_), Some(spec)) => {
                    let entries = self.side_panel.read(cx).entries(&spec.change.path);
                    self.make_diff_view(id, spec, entries, window, cx)
                }
                _ => TabView::File(self.make_view(id, path, None, window, cx)),
            },
        });
        if self.view_drawn != drawn {
            if let Some(hidden) = self.view_drawn.and_then(|id| self.view_of(id)) {
                hidden.hide(Some(window), cx);
            }
            self.view_drawn = drawn;
            // a diff coming into view reads its diff again: it may have changed while out of sight
            if matches!(view, Some(TabView::Diff(_))) {
                self.reload_shown_diff(false, cx);
            }
        }
        let hidden_focused = self.tab_views.iter().any(|(id, v)| Some(*id) != drawn && v.focus_handle(cx).contains_focused(window, cx));
        if hidden_focused && self.settings.is_none() {
            self.focus_pane(window, cx);
        }
        let focused = view.as_ref().is_some_and(|v| v.focus_handle(cx).contains_focused(window, cx));
        (view, focused)
    }

    /// The shown workspace's root, as the settings keep it, moved since it was last seen (chosen, or the pane's
    /// folder taken): its clean tabs of files outside the new root close (`close_outside`), once the root's real path
    /// is in (its files' tabs are by real path). The first root seen this run closes nothing.
    fn follow_root(&mut self, key: &str, cx: &mut Context<Self>) {
        let roots = &SettingsGlobal::of(cx).settings.workspace_roots;
        let Some(root) = roots.get(key).map(|r| r.path.as_str()) else { return };
        match self.tab_roots.get(key) {
            Some(seen) if seen == root => return,
            Some(_) => {}
            None => {
                self.tab_roots.insert(key.to_string(), root.to_string());
                return;
            }
        }
        let root = root.to_string();
        self.tab_roots.insert(key.to_string(), root.clone());
        let real = real_path(&self.rt, PathBuf::from(&root));
        let key = key.to_string();
        cx.spawn(async move |this, cx| {
            let real = real.await;
            let _ = this.update(cx, |s, cx| {
                // moved again meanwhile: that move closes its own
                if s.tab_roots.get(&key) != Some(&root) {
                    return;
                }
                let Some(tabs) = s.file_tabs.get_mut(&key) else { return };
                let was = tabs.active;
                // ponytail: nothing is dirty until tabs edit (E1)
                if tabs.close_outside(&[PathBuf::from(&root), real], |_| false).is_empty() {
                    return;
                }
                if was.is_some() && tabs.active.is_none() {
                    // back to the terminals: herdr's focused pane takes the keyboard on the next render
                    s.focused = None;
                }
                s.drop_dead_views(cx);
                s.remember_tabs(&key, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// After the panes took focus: a pane (or the window, with none) that has the keyboard hands it to the shown
    /// file tab's view, drawn in their place. A popup's terminal, the side panel or the command panel keep theirs.
    pub(super) fn view_takes_keys(&mut self, view: &TabView, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.panes.values().any(|p| p.read(cx).focus_handle(cx).is_focused(window));
        if pane || self.focus.is_focused(window) {
            window.focus(&view.focus_handle(cx), cx);
        }
    }

    /// `roots::fingerprint` of a workspace of the shown place, as its kept tabs are matched against.
    fn workspace_fingerprint(&self, key: &str) -> Option<String> {
        let (_, ws) = key.split_once('\n')?;
        let w = self.snapshot.as_ref()?.workspaces.iter().find(|w| w.workspace_id == ws)?;
        Some(roots::fingerprint(&w.label, w.worktree.as_ref().map(|t| t.checkout_path.as_str())))
    }

    /// Reads the kept tabs off the main thread at launch; tabs opened meanwhile are joined by them when their
    /// workspace next renders.
    pub(super) fn load_kept_tabs(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let path = open_files::path();
            let map = cx.background_spawn(async move { open_files::load(&path) }).await;
            let _ = this.update(cx, |s, cx| {
                s.kept_tabs.writes.read(open_files::encode(&map));
                s.kept_tabs.map = map;
                s.kept_tabs.loaded = true;
                // checked against the snapshot in hand, so the shown workspace's come back without waiting for another
                s.prune_file_tabs(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Puts back the tabs kept for the workspace `key`, once per workspace and run, when they were kept for it (its
    /// fingerprint fits). Only their paths: each reads its file when first shown.
    fn restore_tabs(&mut self, key: &str, cx: &mut Context<Self>) {
        // checked against the workspace first (`prune_file_tabs`): not known yet, the snapshot will list it
        if !self.kept_tabs.loaded || !self.kept_tabs.seen.contains(key) || self.kept_tabs.restored.contains(key) {
            return;
        }
        let Some(fingerprint) = self.workspace_fingerprint(key) else { return };
        self.kept_tabs.restored.insert(key.to_string());
        if let Some((files, active)) = self.kept_tabs.map.get(key).and_then(|r| r.fitting(&fingerprint)) {
            self.file_tabs.entry(key.to_string()).or_default().restore(&files, active.as_deref(), &mut self.next_tab_id);
        }
        // tabs opened before the file was read join the kept ones; kept ones of another workspace that had the id go
        self.remember_tabs(key, cx);
    }

    /// Records the workspace `key`'s pinned tabs for the next run (once its kept ones were put back, so they are not
    /// written over), and saves a second later.
    fn remember_tabs(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.kept_tabs.restored.contains(key) {
            return;
        }
        let (files, active) = self.file_tabs.get(key).map(WorkspaceTabs::remembered).unwrap_or_default();
        let entry = (!files.is_empty()).then(|| Remembered::new(self.workspace_fingerprint(key).unwrap_or_default(), files, active));
        let changed = match entry {
            Some(entry) => self.kept_tabs.map.insert(key.to_string(), entry.clone()).as_ref() != Some(&entry),
            None => self.kept_tabs.map.remove(key).is_some(),
        };
        if changed {
            self.save_kept_tabs(cx);
        }
    }

    /// Writes the kept tabs `SAVE_AFTER` from now (a later change moves it on).
    fn save_kept_tabs(&mut self, cx: &mut Context<Self>) {
        if !self.kept_tabs.loaded {
            return;
        }
        self.kept_tabs.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let _ = this.update(cx, |s, cx| s.write_kept_tabs(cx).map(gpui::Task::detach));
        }));
    }

    /// Starts the write of the kept tabs on the background executor when they differ from what the file holds, and
    /// returns it (quitting waits for it). A change made while one runs is written when it ends.
    pub(super) fn write_kept_tabs(&mut self, cx: &mut Context<Self>) -> Option<gpui::Task<()>> {
        if !self.kept_tabs.loaded {
            return None;
        }
        let text = open_files::encode(&self.kept_tabs.map);
        if !self.kept_tabs.writes.start(&text) {
            return None;
        }
        let write = cx.background_spawn(async move {
            let saved = open_files::save(&open_files::path(), &text);
            if let Err(e) = &saved {
                eprintln!("shepherd: cannot save open files: {e}");
            }
            (text, saved.is_ok())
        });
        Some(cx.spawn(async move |this, cx| {
            let (text, ok) = write.await;
            let _ = this.update(cx, |s, cx| {
                if s.kept_tabs.writes.finish(text, ok) {
                    s.save_kept_tabs(cx);
                }
            });
        }))
    }

    /// Quitting: what changed since the last write is written now (unless a write is still under way).
    pub(super) fn flush_kept_tabs(&mut self, cx: &mut Context<Self>) -> Option<gpui::Task<()>> {
        // the timer is dropped (cancelled): the write happens here instead
        drop(self.kept_tabs.save.take());
        self.write_kept_tabs(cx)
    }
}
