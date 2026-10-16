//! File tabs: each workspace's own, after its herdr tabs in the tab bar (`files::tabs`), local to Shepherd and never
//! told to herdr. While one is shown its view (`ui::viewer`) is drawn in the grid's place: the panes are not drawn,
//! so they keep their size, and the view has the keyboard they would. The herdr tab Shepherd shows (`selected_tab`)
//! stays what the terminals come back to; every way of showing a herdr tab goes through `show_herdr_tab`.
//!
//! Views are made when a tab is first shown (its file read then, not when it is restored), and the most recently
//! shown `MAX_VIEWS` are kept; the pinned tabs are kept for the next run in `open-files.json`
//! (`files::open_files`), written a second after the last change and only when it changed.
//!
//! A file's view is its editor (`ui::editor`), which shows what it cannot edit in the read-only viewer. A tab with
//! unsaved changes has a dot, is pinned on its first edit, is never closed to make room nor replaced as a preview,
//! and asks Save / Don't Save / Cancel before it closes; quitting or closing the window with any asks Save All /
//! Don't Save / Cancel. The cursor and scroll of a view dropped to save memory are put back when it is made again.
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
use crate::files::save::SaveMode;
use crate::ui::editor::{EditorEvent, FileEditor, Saving};
use gpui::PromptLevel;
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

/// A file tab's view: a file's editor, or a change's diff.
#[derive(Clone)]
pub(super) enum TabView {
    File(Entity<FileEditor>),
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
        matches!(self, TabView::File(v) if v.read(cx).shows_markdown(cx))
    }

    /// It has unsaved changes.
    pub(super) fn is_dirty(&self, cx: &App) -> bool {
        matches!(self, TabView::File(v) if v.read(cx).is_dirty())
    }

    /// Back in view.
    fn show(&self, cx: &mut App) {
        if let TabView::File(v) = self {
            v.update(cx, |v, cx| v.show(cx));
        }
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
            TabView::File(v) => v.update(cx, |v, cx| v.hide(window, cx)),
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
        let dirty = self.dirty_tabs(cx);
        let tabs = self.file_tabs.entry(key.clone()).or_default();
        // a preview with unsaved changes is not replaced, nor a dirty tab closed to make room
        let id = tabs.open(path.clone(), TabKind::File, preview, |t| dirty.contains(&t), &mut self.next_tab_id);
        tabs.trim(id, |t| dirty.contains(&t));
        self.drop_dead_views(cx);
        self.adopt_orphan(id, &path, window, cx);
        let view = match self.view_of(id) {
            Some(TabView::File(view)) => {
                // a replaced preview's view shows the new file; the same file (its edits kept) only goes to `line`
                if view.read(cx).path() != path || line.is_some() {
                    let root = self.panel_context(window, cx).root.map(PathBuf::from);
                    view.update(cx, |v, cx| v.open(path, line, root, None, window, cx));
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
            view.update(cx, |v, cx| v.go_to(fragment, window, cx));
        }
        if !preview {
            let handle = view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
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
        let dirty = self.dirty_tabs(cx);
        let tabs = self.file_tabs.entry(key.clone()).or_default();
        let id = tabs.open(path, TabKind::Diff(spec.change.group), preview, |t| dirty.contains(&t), &mut self.next_tab_id);
        tabs.trim(id, |t| dirty.contains(&t));
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

    /// A view for the tab `id`, reading `path`, kept first among the views; the cursor and scroll it had when its
    /// last view went to save memory come back.
    fn make_view(&mut self, id: TabId, path: PathBuf, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) -> Entity<FileEditor> {
        let root = self.panel_context(window, cx).root.map(PathBuf::from);
        let rt = self.rt.clone();
        let view = cx.new(|cx| FileEditor::new(rt, cx));
        cx.subscribe_in(&view, window, move |s: &mut Shell, _, event: &EditorEvent, window, cx| match event {
            // the editor's pane opens beside the focused one: the terminals show, the file tab stays
            EditorEvent::OpenInEditor { path, line } => {
                let pane = s.active_pane(window, cx);
                s.show_terminals(cx);
                crate::terminal::pane::open_in_editor(path.clone(), *line, None, pane, cx);
            }
            // a markdown link to another file of the root: a tab of its own, which takes the keyboard
            EditorEvent::OpenFile { path, line, fragment } => s.open_file_at(path.clone(), *line, fragment.clone(), false, window, cx),
            // the dot: a preview edited is kept
            EditorEvent::Dirty(dirty) => {
                let preview = s.file_tabs.iter().find(|(_, t)| t.get(id).is_some_and(|t| t.preview)).map(|(k, _)| k.clone());
                if let Some(key) = preview.filter(|_| *dirty) {
                    s.pin_file_tab(&key, id, cx);
                }
                cx.notify();
            }
        })
        .detach();
        let place = self.tab_places.remove(&id);
        view.update(cx, |v, cx| v.open(path, line, root, place, window, cx));
        self.keep_view(id, TabView::File(view.clone()), cx);
        view
    }

    /// Keeps `view` first among the views; the least recently shown past `MAX_VIEWS` go, but not one with unsaved
    /// changes.
    fn keep_view(&mut self, id: TabId, view: TabView, cx: &App) {
        self.tab_views.insert(0, (id, view));
        while self.tab_views.len() > MAX_VIEWS {
            // one with unsaved changes is never dropped to save memory
            let Some(at) = self.tab_views.iter().rposition(|(t, v)| *t != id && !v.is_dirty(cx)) else { break };
            let (gone_id, gone) = self.tab_views.remove(at);
            // where it was, for the view its tab gets when shown again
            if let TabView::File(v) = &gone
                && let Some(place) = v.read(cx).place(cx)
            {
                self.tab_places.insert(gone_id, place);
            }
            self.views_gone.push(gone);
        }
    }

    /// The tabs whose views have unsaved changes.
    pub(super) fn dirty_tabs(&self, cx: &App) -> HashSet<TabId> {
        self.tab_views.iter().filter(|(_, v)| v.is_dirty(cx)).map(|(id, _)| *id).collect()
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

    /// Closes the tab `id` of the workspace `key`; one with unsaved changes is shown and asks Save / Don't Save /
    /// Cancel first, and closes once saved (a save that fails leaves it open, saying why).
    pub(super) fn close_file_tab(&mut self, key: &str, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let editor = match self.view_of(id) {
            Some(TabView::File(v)) if v.read(cx).is_dirty() => v,
            _ => return self.discard_file_tab(key, id, window, cx),
        };
        if self.asking {
            return;
        }
        self.asking = true;
        // the question is about the tab in view
        self.show_file_tab(key, id, cx);
        let name = editor.read(cx).path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let message = format!("Save changes to {name}?");
        let answer = window.prompt(PromptLevel::Warning, &message, Some(LOST), &CLOSE_ANSWERS, cx);
        let key = key.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let close = match closing(answer.await.ok()) {
                Closing::Keep => false,
                Closing::Discard => true,
                Closing::Save => {
                    let save = this.update_in(cx, |_, window, cx| editor.update(cx, |e, cx| e.save(SaveMode::Safe, window, cx)));
                    match save {
                        Ok(Some(save)) => save.await,
                        _ => false,
                    }
                }
            };
            let _ = this.update_in(cx, |s, window, cx| {
                s.asking = false;
                if close {
                    s.discard_file_tab(&key, id, window, cx);
                }
            });
        })
        .detach();
    }

    /// Closes the tab, unsaved changes or not.
    fn discard_file_tab(&mut self, key: &str, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
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
    /// unsaved changes stays (its workspace closed: Save All on quit still saves it).
    fn drop_dead_views(&mut self, cx: &App) {
        let live: HashSet<TabId> = self.file_tabs.values().flat_map(|t| t.files.iter().map(|f| f.id)).collect();
        self.tab_places.retain(|id, _| live.contains(id));
        // a diff tab's change goes with it (or with its slot, now a file's)
        let diffs: HashSet<TabId> = self.file_tabs.values().flat_map(|t| t.files.iter().filter(|f| matches!(f.kind, TabKind::Diff(_))).map(|f| f.id)).collect();
        self.diff_specs.retain(|id, _| diffs.contains(id));
        let (keep, gone): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.tab_views).into_iter().partition(|(id, v)| live.contains(id) || v.is_dirty(cx));
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
        // a tab restored or reopened for a file whose unsaved buffer outlived its workspace takes that buffer
        if let Some((id, path, TabKind::File)) = &shown
            && self.view_of(*id).is_none()
        {
            self.adopt_orphan(*id, &path.clone(), window, cx);
        }
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
            if let Some(view) = &view {
                view.show(cx);
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
                let dirty = s.dirty_tabs(cx);
                let Some(tabs) = s.file_tabs.get_mut(&key) else { return };
                let was = tabs.active;
                if tabs.close_outside(&[PathBuf::from(&root), real], |t| dirty.contains(&t)).is_empty() {
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

/// A close prompt's answers, in order (`closing` reads the index).
const CLOSE_ANSWERS: [&str; 3] = ["Save", "Don't Save", "Cancel"];
/// Quitting's or closing the window's.
const EXIT_ANSWERS: [&str; 3] = ["Save All", "Don't Save", "Cancel"];
const LOST: &str = "Your changes are lost if you don't save them.";

/// What an answer to Save / Don't Save / Cancel does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closing {
    Save,
    Discard,
    /// Cancel, or the prompt went without an answer.
    Keep,
}

/// A file's name as a prompt or a toast says it.
fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn closing(answer: Option<usize>) -> Closing {
    match answer {
        Some(0) => Closing::Save,
        Some(1) => Closing::Discard,
        _ => Closing::Keep,
    }
}

/// The question before quitting with unsaved files (their names), and its detail.
fn unsaved_message(names: &[String]) -> (String, String) {
    const LISTED: usize = 4;
    let message = match names.len() {
        1 => format!("Save changes to {}?", names[0]),
        n => format!("Save changes to {n} files?"),
    };
    let detail = match names.len() {
        0 | 1 => LOST.to_string(),
        n if n <= LISTED => format!("{}. {LOST}", names.join(", ")),
        n => format!("{} and {} more. {LOST}", names[..LISTED].join(", "), n - LISTED),
    };
    (message, detail)
}

/// What a window with unsaved tabs does once they are dealt with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Exit {
    Quit,
    Close,
}

/// What runs once Shepherd quits, however it quits (the updater's relaunch): kept until a quit goes through.
#[derive(Default)]
pub struct AfterQuit(Vec<Box<dyn FnOnce()>>);

impl gpui::Global for AfterQuit {}

impl AfterQuit {
    /// Runs what was left to run (on the app's quit).
    pub(super) fn run(cx: &mut App) {
        if let Some(after) = cx.try_global::<AfterQuit>().is_some().then(|| std::mem::take(&mut cx.global_mut::<AfterQuit>().0)) {
            for f in after {
                f();
            }
        }
    }
}

impl Shell {
    /// Quit (its chord, the menu): the first window with unsaved file tabs asks first; with none, Shepherd quits.
    pub fn quit(cx: &mut App) {
        for window in cx.windows() {
            let Some(window) = window.downcast::<Shell>() else { continue };
            if window.update(cx, |s, window, cx| s.ask_unsaved(Exit::Quit, window, cx)).unwrap_or(false) {
                return;
            }
        }
        cx.quit();
    }

    /// `quit`, and `after` once it does (the updater relaunches only if the quit goes through; cancelled, it waits
    /// for the next one).
    pub fn quit_and(cx: &mut App, after: impl FnOnce() + 'static) {
        cx.default_global::<AfterQuit>().0.push(Box::new(after));
        Self::quit(cx);
    }

    /// The views with unsaved changes (`saving`: or a save under way), those of tabs that closed with their workspace
    /// too.
    fn editors(&self, saving: bool, cx: &App) -> Vec<Entity<FileEditor>> {
        let mut out: Vec<Entity<FileEditor>> = Vec::new();
        for view in self.tab_views.iter().map(|(_, v)| v).chain(&self.views_gone) {
            if let TabView::File(v) = view
                && (v.read(cx).is_dirty() || saving && v.read(cx).is_saving())
                && !out.contains(v)
            {
                out.push(v.clone());
            }
        }
        out
    }

    /// Unsaved file tabs before quitting or closing the window: asks Save All / Don't Save / Cancel, and quits or
    /// closes once they are saved (all of them) or let go. A save under way is waited for either way, and alone it
    /// is waited for without asking. True when it asked or waits, so the caller does not go yet.
    pub(super) fn ask_unsaved(&mut self, exit: Exit, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.leaving {
            return false;
        }
        let dirty = self.editors(false, cx);
        let saving = self.editors(true, cx);
        if saving.is_empty() {
            return false;
        }
        if self.asking {
            return true;
        }
        self.asking = true;
        let answer = (!dirty.is_empty()).then(|| {
            let names: Vec<String> = dirty.iter().map(|e| file_name(e.read(cx).path())).collect();
            let (message, detail) = unsaved_message(&names);
            window.prompt(PromptLevel::Warning, &message, Some(&detail), &EXIT_ANSWERS, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let answer = match answer {
                Some(answer) => closing(answer.await.ok()),
                // nothing unsaved, a save under way: only waited for
                None => Closing::Discard,
            };
            let go = match answer {
                Closing::Keep => false,
                Closing::Discard => {
                    // what is being written finishes first, whatever was answered
                    let writes = this.update(cx, |s, cx| s.editors(true, cx).iter().filter_map(|e| e.read(cx).saving()).collect::<Vec<_>>()).unwrap_or_default();
                    for write in writes {
                        write.await;
                    }
                    true
                }
                Closing::Save => {
                    let saves = this.update_in(cx, |s, window, cx| s.save_all(window, cx)).unwrap_or_default();
                    Self::all_saved(&this, saves, cx).await
                }
            };
            let _ = this.update_in(cx, |s, window, cx| {
                s.asking = false;
                if go {
                    s.leaving = true;
                    match exit {
                        Exit::Quit => cx.quit(),
                        Exit::Close => window.remove_window(),
                    }
                }
            });
        })
        .detach();
        true
    }

    /// Ctrl/Cmd+Alt+S, and Save All before quitting: saves every view with unsaved changes, and hands back those
    /// under way too (a clean tab still writing), each with whether its file was written.
    pub(super) fn save_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<(Entity<FileEditor>, Saving)> {
        let mut out = Vec::new();
        for e in self.editors(true, cx) {
            let save = if e.read(cx).is_dirty() { e.update(cx, |e, cx| e.save(SaveMode::Safe, window, cx)) } else { e.read(cx).saving() };
            out.push((e, save.unwrap_or_else(|| futures::FutureExt::shared(futures::FutureExt::boxed_local(async { false })))));
        }
        out
    }

    /// Waits for `saves`; true when every one wrote its file, else the first that did not is shown (its banner says
    /// why), or named when its tab is not in this window's shown workspace.
    async fn all_saved(this: &gpui::WeakEntity<Shell>, saves: Vec<(Entity<FileEditor>, Saving)>, cx: &mut gpui::AsyncWindowContext) -> bool {
        let mut failed = None;
        for (editor, save) in saves {
            if !save.await && failed.is_none() {
                failed = Some(editor);
            }
        }
        let Some(editor) = failed else { return true };
        let _ = this.update(cx, |s, cx| s.show_failed(&editor, cx));
        false
    }

    /// A save of Save All failed: its tab shows, if it is one of the shown workspace's; else a toast names the file.
    fn show_failed(&mut self, editor: &Entity<FileEditor>, cx: &mut Context<Self>) {
        let id = self.tab_views.iter().find(|(_, v)| matches!(v, TabView::File(v) if v == editor)).map(|(id, _)| *id);
        let key = self.shown_tabs_key().filter(|k| id.is_some_and(|id| self.file_tabs.get(k).is_some_and(|t| t.get(id).is_some())));
        match (key, id) {
            (Some(key), Some(id)) => self.show_file_tab(&key, id, cx),
            _ => {
                let name = file_name(editor.read(cx).path());
                crate::ui::toast::error(cx, format!("Couldn't save {name}"))
            }
        }
    }

    /// Ctrl/Cmd+Alt+S: Save All, the first tab that failed shown.
    pub(super) fn save_all_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saves = self.save_all(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            Self::all_saved(&this, saves, cx).await;
        })
        .detach();
    }

    /// Quitting without asking (the Dock's Quit, logging out, `leaving` not set): every unsaved view is saved and every
    /// write under way waited for, as long as the app's quit lets them run.
    pub(super) fn flush_editors(&self, cx: &App) -> Vec<futures::future::BoxFuture<'static, ()>> {
        if self.leaving {
            // Don't Save was the answer; what was being written was waited for then
            return Vec::new();
        }
        self.editors(true, cx).iter().filter_map(|e| e.read(cx).flush(cx)).collect()
    }

    /// The view of a tab that went with its workspace, unsaved, for the file a tab now opens: that tab takes it over
    /// (so one file never has two buffers), in place of a view of another file its slot had. True when it did.
    fn adopt_orphan(&mut self, id: TabId, path: &std::path::Path, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let live: HashSet<TabId> = self.file_tabs.values().flat_map(|t| t.files.iter().map(|f| f.id)).collect();
        let orphan = self.tab_views.iter().position(|(t, v)| !live.contains(t) && matches!(v, TabView::File(v) if v.read(cx).path() == path));
        let Some(at) = orphan else { return false };
        if let Some(own) = self.view_of(id) {
            if matches!(&own, TabView::File(v) if v.read(cx).path() == path) {
                return false;
            }
            self.drop_view(id, own, window, cx);
        }
        let at = self.tab_views.iter().position(|(t, v)| !live.contains(t) && matches!(v, TabView::File(v) if v.read(cx).path() == path)).unwrap_or(at);
        self.tab_views[at].0 = id;
        true
    }

    /// git's state may have moved (a Changes poll answered; W1's watch on `.git`): the editors' gutters catch up, the
    /// shown one now and the others when they next show.
    pub(super) fn files_git_changed(&mut self, cx: &mut Context<Self>) {
        let shown = self.view_drawn;
        for (id, view) in &self.tab_views {
            if let TabView::File(v) = view {
                v.update(cx, |v, cx| v.git_changed(Some(*id) == shown, cx));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_close_prompt_routes_its_answer() {
        assert_eq!(CLOSE_ANSWERS[0], "Save");
        assert_eq!(closing(Some(0)), Closing::Save);
        assert_eq!(closing(Some(1)), Closing::Discard);
        assert_eq!(closing(Some(2)), Closing::Keep, "Cancel");
        assert_eq!(closing(None), Closing::Keep, "dismissed without an answer");
        assert_eq!(EXIT_ANSWERS.len(), CLOSE_ANSWERS.len(), "the same routing for both");
    }

    #[test]
    fn the_quit_prompt_names_the_files() {
        let names = |n: usize| (0..n).map(|i| format!("f{i}.rs")).collect::<Vec<_>>();
        assert_eq!(unsaved_message(&names(1)).0, "Save changes to f0.rs?");
        let (message, detail) = unsaved_message(&names(3));
        assert_eq!(message, "Save changes to 3 files?");
        assert!(detail.starts_with("f0.rs, f1.rs, f2.rs. "), "{detail}");
        let (_, detail) = unsaved_message(&names(7));
        assert!(detail.starts_with("f0.rs, f1.rs, f2.rs, f3.rs and 3 more. "), "{detail}");
    }
}
