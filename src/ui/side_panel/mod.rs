//! The right panel's Files, Changes and Search tabs: one entity beside the Shell, which keeps the panel's frame (its
//! tab strip, edge and width) and the PR tab. It holds what those tabs show and the work that fills them, so a
//! keystroke in Search re-renders the panel, not the window.
mod files;

use crate::settings::PanelTab;
use crate::theme::Theme;
use gpui::{Context, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, prelude::*, px};
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
}

pub struct SidePanel {
    focus: FocusHandle,
    /// Where processes (`files::runner`) and folder reads run.
    rt: Handle,
    tab: PanelTab,
    context: PanelContext,
    /// The Files tab's tree of the context's root, once the tab has shown it.
    files: Option<files::FileTree>,
    file_menu: Option<crate::ui::Popup<files::FileMenu>>,
    /// The Files tab is on screen in the active window: its open folders are polled.
    files_live: bool,
}

impl SidePanel {
    pub fn new(rt: Handle, tab: PanelTab, cx: &mut Context<Self>) -> SidePanel {
        // the poll's clock: a tick does nothing unless the Files tab is live
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(files::POLL).await;
            if this.update(cx, |p, cx| p.poll_files(cx)).is_err() {
                return;
            }
        })
        .detach();
        SidePanel { focus: cx.focus_handle(), rt, tab, context: PanelContext::default(), files: None, file_menu: None, files_live: false }
    }

    /// Takes the Shell's view of things; called every render, so it notifies only on a change.
    pub fn set_context(&mut self, context: PanelContext, cx: &mut Context<Self>) {
        if self.context != context {
            self.context = context;
            self.sync_files(cx);
            cx.notify();
        }
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
        } else {
            let (title, hint) = match self.tab {
                PanelTab::Files => ("Files", ""),
                PanelTab::Changes => ("Changes", "The workspace's git changes will be listed here."),
                PanelTab::Search => ("Search", "Search across the workspace's folder will be here."),
                // the Shell draws the PR tab itself
                PanelTab::Pr => ("Pull request", ""),
            };
            notice(theme, title, hint).into_any_element()
        };
        div()
            .id("side-panel")
            .track_focus(&self.focus)
            .key_context("SidePanel")
            .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, _, cx| {
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
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .child(body)
    }
}
