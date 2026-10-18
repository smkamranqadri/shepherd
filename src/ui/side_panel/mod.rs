//! The right panel's Files, Changes and Search tabs: one entity beside the Shell, which keeps the panel's frame (its
//! tab strip, edge and width) and the PR tab. It holds what those tabs show and the work that fills them, so a
//! keystroke in Search re-renders the panel, not the window.
mod changes;
mod commit;
mod files;
mod search;

pub use search::UnsavedBuffers;

use crate::files::watch::{self, Batch, Message};
use crate::settings::PanelTab;
use crate::theme::Theme;
use futures::StreamExt;
use gpui::{Context, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, prelude::*, px};
use std::path::PathBuf;
use tokio::runtime::Handle;

/// What the Shell knows that the panel needs: where it is and what the shown workspace's panes look at. Handed over
/// on every render; only a change does anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelContext {
    /// The shown place's key (`Place::key`: a session's socket, `machine:<id>`).
    pub place: String,
    pub workspace_id: Option<String>,
    /// Folders a root could be guessed from, best first: the workspace's worktree, then its first tab's pane.
    pub guess_dirs: Vec<String>,
    /// The focused pane's folder, located (`herdr::locate`).
    pub active_dir: Option<String>,
    /// The workspace is on a herdr machine: its files are not this computer's to list.
    pub remote: bool,
    /// The workspace's root (`files::roots`), chosen or guessed; `None` until one is.
    pub root: Option<String>,
    /// The root is being guessed, or cannot be yet (the places or the snapshot still loading): "No folder" waits for
    /// the answer rather than flash before it.
    pub guessing: bool,
}

pub enum SidePanelEvent {
    /// Esc: keyboard focus goes back to the pane.
    Dismiss,
    /// "Choose folder…" under "No folder": the Shell opens the folder chooser, as `ChooseRoot` does.
    ChooseRoot,
    /// "Use focused pane's folder" under "No folder".
    UsePaneFolder,
    /// A file clicked in the tree, Enter on it, or its menu's "Open": the Shell shows it in a file tab, at `line`
    /// when given. A single click opens it as a preview (replaced by the next one, the keyboard left here); a
    /// double-click, Enter or the menu pins it and gives it the keyboard.
    OpenFile { path: std::path::PathBuf, line: Option<u32>, preview: bool },
    /// Cmd+Enter (Ctrl+Enter on Linux) on a file, or its menu's "Open in editor": the Shell opens it in the editor,
    /// beside the focused pane.
    OpenInEditor(std::path::PathBuf),
    /// A change clicked in the Changes tab, or its menu's "Open file": `path` is its file in the root. The Shell
    /// opens its diff in a tab (a conflict's file with its markers; nothing for a folder). `preview` as
    /// `OpenFile`'s: a single click's tab is replaced by the next.
    OpenChange { path: std::path::PathBuf, change: crate::files::status::Change, preview: bool },
    /// A match clicked in the Search tab, or Enter on it: its file in a tab at `line` (1-based), the bytes `cols` of
    /// the line selected. `preview` as `OpenFile`'s: a single click's leaves the keyboard in the results.
    OpenMatch { path: std::path::PathBuf, line: u32, cols: std::ops::Range<usize>, preview: bool },
    /// The Changes tab's count (its badge, which the Shell draws in the tab strip) changed: the Shell redraws.
    ChangesCounted,
    /// git status answered (changed or not): the shown diff tab reads its diff again.
    ChangesPolled,
    /// A folder opened or folded, or a workspace's tree left: what the Files tab remembers changed, for the next
    /// run.
    FilesRemembered,
}

/// What live updates are doing (`files::watch`); the polls run at their own pace unless `Live`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WatchState {
    /// No watch (the panel closed, no local root), or one starting.
    Off,
    Live,
    /// Watching could not start or stopped: "Live updates off: <why>" under the tab.
    Failed(String),
}

/// The shown root's watch: its batches carry `generation`, so a watch replaced (A to B and back to A) is not heard.
struct Watching {
    root: PathBuf,
    generation: u64,
    _watch: watch::Watch,
}

pub struct SidePanel {
    focus: FocusHandle,
    /// Where processes (`files::runner`) and folder reads run.
    rt: Handle,
    tab: PanelTab,
    context: PanelContext,
    /// The Files tab's tree of the context's root, once the tab has shown it.
    files: Option<files::FileTree>,
    /// What each workspace's tree had open when it was left.
    files_memory: files::FilesMemories,
    file_menu: Option<crate::ui::Popup<files::FileMenu>>,
    /// The Files tab is on screen in the active window: its open folders are polled.
    files_live: bool,
    /// The Changes tab's list of the context's root, while it has a local one.
    changes: Option<changes::ChangeList>,
    change_menu: Option<crate::ui::Popup<changes::ChangeMenu>>,
    /// The panel is open in the active window: git is asked every `POLL`, for the list and the tab's badge.
    changes_live: bool,
    /// The Changes tab itself is in view (not just its badge): the list is asked at the tab's pace, and its coming
    /// into view lifts a halt.
    changes_tab_shown: bool,
    /// The Changes tab's folded groups, kept across roots as VS Code keeps them across repositories.
    changes_folded: std::collections::HashSet<crate::files::status::Group>,
    /// The Changes tab's commit message and the stage, discard or commit running.
    commit: commit::CommitBox,
    /// The panel is open (on any tab, the window active or not): its root is watched.
    panel_shown: bool,
    watch: Option<Watching>,
    watch_state: WatchState,
    next_watch: u64,
    /// The Search tab's query, results and the search out.
    search: search::SearchState,
    /// Where the buffers with unsaved changes are (the Shell's file tabs): a search looks in them, not on disk.
    unsaved: Option<UnsavedBuffers>,
}

impl SidePanel {
    pub fn new(rt: Handle, tab: PanelTab, cx: &mut Context<Self>) -> SidePanel {
        // the polls' clock: a tick does nothing for a tab that is not live; half the pace on battery
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(files::poll_on(crate::power::on_battery())).await;
            let polled = this.update(cx, |p, cx| {
                p.poll_files(true, cx);
                p.poll_changes(cx);
            });
            if polled.is_err() {
                return;
            }
        })
        .detach();
        SidePanel {
            focus: cx.focus_handle(),
            rt,
            tab,
            context: PanelContext::default(),
            files: None,
            files_memory: Default::default(),
            file_menu: None,
            files_live: false,
            changes: None,
            change_menu: None,
            changes_live: false,
            changes_tab_shown: false,
            changes_folded: Default::default(),
            commit: Default::default(),
            panel_shown: false,
            watch: None,
            watch_state: WatchState::Off,
            next_watch: 0,
            search: Default::default(),
            unsaved: None,
        }
    }

    /// Takes the Shell's view of things; called every render, so it notifies only on a change.
    pub fn set_context(&mut self, context: PanelContext, cx: &mut Context<Self>) {
        if self.context != context {
            self.context = context;
            self.sync_files(cx);
            self.sync_changes(cx);
            self.sync_watch(cx);
            cx.notify();
        }
    }

    /// Watching works: the polls are only a safety net (`watch::SAFETY_POLL`).
    fn watching(&self) -> bool {
        self.watch_state == WatchState::Live
    }

    /// Keeps one watch on the shown root while the panel is open and the root is this computer's; a new root, or
    /// the panel closing, drops the old one (and its task and watcher with it).
    fn sync_watch(&mut self, cx: &mut Context<Self>) {
        let root = self.context.root.as_deref().filter(|_| self.panel_shown && !self.context.remote).map(PathBuf::from);
        if self.watch.as_ref().map(|w| &w.root) == root.as_ref() {
            return;
        }
        self.watch = None;
        self.set_watch_state(WatchState::Off, cx);
        let Some(root) = root else { return };
        self.next_watch += 1;
        let generation = self.next_watch;
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let handle = watch::Watch::start(&self.rt, root.clone(), tx);
        self.watch = Some(Watching { root, generation, _watch: handle });
        cx.spawn(async move |this, cx| {
            // ends when the watch is dropped (its task holds the sender)
            while let Some(message) = rx.next().await {
                if this.update(cx, |p, cx| p.take_watch(generation, message, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn set_watch_state(&mut self, state: WatchState, cx: &mut Context<Self>) {
        if self.watch_state == state {
            return;
        }
        let was_live = self.watching();
        self.watch_state = state;
        if was_live && !self.watching() {
            // back to the polls' own pace, starting now
            if let Some(list) = self.changes.as_mut() {
                list.next_at = None;
            }
            if let Some(tree) = self.files.as_mut() {
                tree.polled_at = None;
            }
        }
        // the note under the tab comes or goes
        cx.notify();
    }

    /// A watch's message: its start (what changed before it is caught up on), its failure, or a batch.
    fn take_watch(&mut self, generation: u64, message: Message, cx: &mut Context<Self>) {
        if self.watch.as_ref().is_none_or(|w| w.generation != generation) {
            return;
        }
        match message {
            Message::Live => {
                self.set_watch_state(WatchState::Live, cx);
                self.take_batch(Batch::everything(), cx);
            }
            Message::Off(why) => self.set_watch_state(WatchState::Failed(why), cx),
            Message::Batch(batch) => self.take_batch(batch, cx),
        }
    }

    /// What changed on disk: the tree's open folders among it read again, git asked again.
    fn take_batch(&mut self, batch: Batch, cx: &mut Context<Self>) {
        if batch.status || batch.rescan {
            self.want_changes(cx);
        }
        self.watch_files(batch, cx);
    }

    pub fn set_tab(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.sync_files(cx);
            cx.notify();
        }
    }
}

impl EventEmitter<SidePanelEvent> for SidePanel {}

impl Focusable for SidePanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A quiet button under a notice: a hairline, a wash on hover (the Shell's buttons are its own module's).
fn notice_button(theme: &Theme, id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .px(px(12.))
        .py(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.))
        .text_color(theme.text)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label)
}

/// A tab's message in the middle of the panel: a title and a line under it.
fn notice(theme: &Theme, title: impl Into<SharedString>, hint: impl Into<SharedString>) -> impl IntoElement {
    div()
        .pt(px(24.))
        .px(px(12.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .text_center()
        .child(div().text_color(theme.text).child(title.into()))
        .child(div().text_size(px(12.)).text_color(theme.text_muted).child(hint.into()))
}

impl Render for SidePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &Theme::of(cx).clone();
        // why the tree and the list are polled rather than watched, faint under them
        let watch_note = match &self.watch_state {
            WatchState::Failed(why) if matches!(self.tab, PanelTab::Files | PanelTab::Changes) && self.context.root.is_some() && !self.context.remote => Some(
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(4.))
                    .text_size(px(11.))
                    .text_color(theme.text_faint)
                    .truncate()
                    .child(format!("Live updates off: {why}")),
            ),
            _ => None,
        };
        let body = if self.context.remote {
            notice(theme, "Files on herdr machines aren't supported yet", "Local only for now.").into_any_element()
        } else if self.context.root.is_none() && (self.context.guessing || self.context.workspace_id.is_none()) {
            div().into_any_element()
        } else if self.context.root.is_none() {
            // no guess took (home, `/`, no pane yet): the folder is the user's to pick
            let buttons = div()
                .pt(px(14.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .child(notice_button(theme, "root-choose", "Choose folder…").on_click(cx.listener(|_, _, _, cx| cx.emit(SidePanelEvent::ChooseRoot))))
                .when(self.context.active_dir.is_some(), |el| {
                    el.child(notice_button(theme, "root-use-pane", "Use focused pane's folder").on_click(cx.listener(|_, _, _, cx| cx.emit(SidePanelEvent::UsePaneFolder))))
                });
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(notice(theme, "No folder", "Pick the folder this workspace's files, changes and search are in."))
                .child(buttons)
                .into_any_element()
        } else if self.tab == PanelTab::Files {
            self.render_files(theme, window, cx)
        } else if self.tab == PanelTab::Changes {
            self.render_changes(theme, window, cx)
        } else if self.tab == PanelTab::Search {
            self.render_search(theme, window, cx)
        } else {
            let (title, hint) = match self.tab {
                PanelTab::Files => ("Files", ""),
                PanelTab::Changes => ("Changes", ""),
                PanelTab::Search => ("Search", ""),
                // the Shell draws the PR tab itself
                PanelTab::Pr => ("Pull request", ""),
            };
            notice(theme, title, hint).into_any_element()
        };
        div()
            .id("side-panel")
            .track_focus(&self.focus)
            .key_context("SidePanel")
            .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
                let m = &e.keystroke.modifiers;
                if e.keystroke.key == "enter" && crate::platform::link_modifier(m) && !m.alt && !m.shift && this.tab == PanelTab::Files && !this.context.remote {
                    if this.open_selected_in_editor(cx) {
                        cx.stop_propagation();
                    }
                    return;
                }
                if m.platform || m.control || m.alt || m.shift {
                    return;
                }
                if e.keystroke.key == "escape" {
                    cx.emit(SidePanelEvent::Dismiss);
                    cx.stop_propagation();
                } else if this.tab == PanelTab::Files && !this.context.remote && this.files_key(&e.keystroke.key, cx) {
                    cx.stop_propagation();
                } else if this.tab == PanelTab::Search && !this.context.remote && this.focus.is_focused(window) && this.search_key(&e.keystroke.key, cx) {
                    // the results' own keys; the inputs keep theirs
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .map(|el| match watch_note {
                Some(note) => el.child(div().flex_1().min_h_0().child(body)).child(note),
                None => el.child(body),
            })
    }
}
