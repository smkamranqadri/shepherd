//! The right panel's Files, Changes and Search tabs: one entity beside the Shell, which keeps the panel's frame (its
//! tab strip, edge and width) and the PR tab. It holds what those tabs show and the work that fills them, so a
//! keystroke in Search re-renders the panel, not the window.
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
}

pub enum SidePanelEvent {
    /// Esc: keyboard focus goes back to the pane.
    Dismiss,
}

pub struct SidePanel {
    focus: FocusHandle,
    /// Where processes (`files::runner`) run.
    #[allow(dead_code)] // the tabs' work (M2 on) runs there
    rt: Handle,
    tab: PanelTab,
    context: PanelContext,
}

impl SidePanel {
    pub fn new(rt: Handle, tab: PanelTab, cx: &mut Context<Self>) -> SidePanel {
        SidePanel { focus: cx.focus_handle(), rt, tab, context: PanelContext::default() }
    }

    /// Takes the Shell's view of things; called every render, so it notifies only on a change.
    pub fn set_context(&mut self, context: PanelContext, cx: &mut Context<Self>) {
        if self.context != context {
            self.context = context;
            cx.notify();
        }
    }

    pub fn set_tab(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let body = if self.context.remote {
            notice(theme, "Files on herdr machines aren't supported yet", "Local only for now.").into_any_element()
        } else {
            let (title, hint) = match self.tab {
                PanelTab::Files => ("Files", "The workspace's folder will be listed here."),
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
            .on_key_down(cx.listener(|_, e: &gpui::KeyDownEvent, _, cx| {
                let m = &e.keystroke.modifiers;
                if e.keystroke.key == "escape" && !(m.platform || m.control || m.alt || m.shift) {
                    cx.emit(SidePanelEvent::Dismiss);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .child(body)
    }
}
