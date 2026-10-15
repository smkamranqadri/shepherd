//! The right panel's frame: a tab strip (Files, Changes, Search, and the PR while the shown repo has a remote), the
//! shown tab's body and the edge that resizes it. One toggle and one width for all of them; the PR tab's body is
//! `review.rs`'s, the others are `ui::side_panel`'s entity.
use super::*;
use super::review::ShownReview;
use crate::settings::{PANEL_MAX, PANEL_MIN, PANEL_WIDTH, PanelTab};
use crate::ui::side_panel::PanelContext;
use gpui::AnyElement;

impl Shell {
    /// `ToggleReview`: shows or hides the panel on the tab it had.
    pub(super) fn toggle_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel_open {
            self.close_panel(window, cx);
        } else {
            self.panel_open = true;
            save_from_sidebar(cx, |s| s.panel_open = true);
            cx.notify();
        }
    }

    /// Hides the panel; keyboard focus inside it goes back to the pane.
    fn close_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel_open = false;
        save_from_sidebar(cx, |s| s.panel_open = false);
        if self.side_panel_focused(window, cx) {
            self.focus_pane(window, cx);
        }
        cx.notify();
    }

    fn side_panel_focused(&self, window: &Window, cx: &App) -> bool {
        self.side_panel.read(cx).focus_handle(cx).contains_focused(window, cx)
    }

    /// Keyboard focus back to herdr's focused pane of the shown tab (the side panel's Esc, its closing), else to
    /// the window, as when no pane is up.
    pub(super) fn focus_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane(window, cx).and_then(|id| Some((self.panes.get(&id)?.read(cx).focus_handle(cx), id)));
        match pane {
            Some((handle, id)) => {
                window.focus(&handle, cx);
                self.focused = Some(id);
            }
            None => {
                window.focus(&self.focus, cx);
                self.focused = None;
            }
        }
        cx.notify();
    }

    /// The tab the panel shows: the one picked, but Files for the PR tab while the shown repo has no remote (the
    /// pick is kept, so the PR tab comes back with one).
    pub(super) fn shown_panel_tab(&self) -> PanelTab {
        if self.panel_tab == PanelTab::Pr && !self.panel_has_pr { PanelTab::Files } else { self.panel_tab }
    }

    /// `ShowFiles`, `ShowChanges`, …, as VS Code's: the panel on `tab` with keyboard focus; the chord again, focus
    /// still in it, hides it. The PR tab has nothing to type into: its chord shows and hides it, and does nothing
    /// while the shown repo has no remote.
    pub(super) fn show_panel_tab(&mut self, tab: PanelTab, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.panel_open && self.shown_panel_tab() == tab;
        match tab {
            PanelTab::Pr if !self.panel_has_pr => {}
            PanelTab::Pr if shown => self.close_panel(window, cx),
            _ if shown && self.side_panel_focused(window, cx) => self.close_panel(window, cx),
            _ => self.open_panel_tab(tab, window, cx),
        }
    }

    /// The panel open on `tab`, with keyboard focus in it (the PR tab has nothing to type into, so the pane keeps it).
    pub(super) fn open_panel_tab(&mut self, tab: PanelTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panel_open {
            self.panel_open = true;
            save_from_sidebar(cx, |s| s.panel_open = true);
        }
        self.pick_panel_tab(tab, cx);
        if tab != PanelTab::Pr && !self.side_panel_focused(window, cx) {
            // herdr's focused pane counts as focused already, so the next render does not take the keyboard back
            // for it (as after the command panel, which ran this, closes)
            self.focused = self.active_pane(window, cx);
            window.focus(&self.side_panel.read(cx).focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// A click on a tab: it shows, and is saved for the next launch. Focus left in the side panel when the PR tab
    /// takes its place goes back to the pane on the next render.
    fn pick_panel_tab(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        if self.panel_tab != tab {
            self.panel_tab = tab;
            save_from_sidebar(cx, |s| s.panel_tab = tab);
        }
        cx.notify();
    }

    pub(super) fn drag_panel(&mut self, x: Pixels, window: &Window, cx: &mut Context<Self>) {
        if self.panel_drag {
            self.panel_width = f32::from(window.viewport_size().width - x).clamp(PANEL_MIN, PANEL_MAX);
            cx.notify();
        }
    }

    pub(super) fn end_panel_drag(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.panel_drag) {
            cx.set_global(GridHold(false));
            let width = self.panel_width;
            save_from_sidebar(cx, |s| s.panel_width = width);
            cx.refresh_windows();
        }
    }

    /// What the side panel's tabs need to know of the shown place and workspace; handed over every render.
    pub(super) fn panel_context(&self, window: &Window, cx: &App) -> PanelContext {
        let place = self.places.iter().find(|p| self.places_shown(p)).map(|p| p.key()).unwrap_or_else(|| self.client.path().to_string_lossy().into_owned());
        let active_dir = crate::ui::statusbar::pane_dir(self.snapshot.as_ref(), self.active_pane(window, cx).as_deref());
        let machine = SettingsGlobal::of(cx).settings.herdr_machine.is_some();
        let remote = machine || active_dir.as_deref().is_some_and(|d| crate::herdr::split(d).0.is_some());
        // ponytail: the root guess (M2) fills `guess_dirs`
        PanelContext { place, workspace_id: self.shown_workspace.clone(), guess_dirs: Vec::new(), active_dir, remote }
    }

    /// The panel, and its edge: drag to resize, double-click for the default width.
    pub(super) fn render_right_panel(&mut self, theme: &Theme, shown: &ShownReview, window: &mut Window, cx: &mut Context<Self>) -> [AnyElement; 2] {
        let tab = self.shown_panel_tab();
        let context = self.panel_context(window, cx);
        self.side_panel.update(cx, |p, cx| {
            p.set_context(context, cx);
            p.set_tab(tab, cx);
        });

        let has_pr = self.panel_has_pr;
        let tabs: Vec<_> = PanelTab::ALL.into_iter().filter(|t| *t != PanelTab::Pr || has_pr).map(|t| {
            let on = t == tab;
            let hover = theme.hover;
            div()
                .id(SharedString::from(format!("panel-tab-{}", t.label())))
                .size(px(26.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .occlude()
                .when(on, |el| el.bg(theme.selected))
                .when(!on, |el| el.hover(move |s| s.bg(hover)))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .child(gpui::svg().path(t.icon()).size(px(15.)).text_color(if on { theme.text } else { theme.text_muted }))
                .tooltip(tip(t.label(), Some(t.action())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_panel_tab(t, cx)))
        }).collect();
        let (title, actions): (SharedString, Vec<AnyElement>) = match tab {
            PanelTab::Pr => self.review_header(theme, shown, window, cx),
            other => (other.label().into(), Vec::new()),
        };
        let header = div()
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .pl(px(8.))
            // clear of the window's buttons, which sit over its right end off macOS
            .pr(px(12. + window_controls_w(cx)))
            .flex()
            .items_center()
            .gap(px(2.))
            .border_b_1()
            .border_color(theme.border)
            .children(tabs)
            .child(div().flex_1().min_w_0().pl(px(6.)).truncate().font_weight(gpui::FontWeight::MEDIUM).child(title))
            .children(actions)
            .child(
                titlebar_button(theme, "panel-close", "icons/close.svg")
                    .tooltip(tip("Close", Some("ToggleReview")))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_panel(window, cx))),
            );

        let (body, footer) = match tab {
            PanelTab::Pr => {
                let (body, footer) = self.review_body(theme, shown, window, cx);
                let body = div()
                    .id("review-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    // one reset for the description's and the logs' selectable text, before either paints
                    .child(crate::ui::markdown::frame_reset())
                    .children(body);
                (body.into_any_element(), footer)
            }
            _ => (div().flex_1().min_h_0().child(self.side_panel.clone()).into_any_element(), None),
        };

        let width = self.panel_width;
        let panel = div()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.wash(0.05))
            .border_l_1()
            .border_color(theme.border)
            .child(header)
            .child(body)
            .children(footer);
        let edge = div()
            .id("panel-resize")
            .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover_seam("panel-resize", *on, cx)))
            .absolute()
            .top(px(TITLEBAR_HEIGHT))
            .bottom_0()
            .right(px(width - 3.))
            .w(px(6.))
            .cursor_col_resize()
            // the panel's 1 px border sits at x = 3 of the strip
            .child(resize_seam("panel-resize".into(), true, 3., theme.accent, self.panel_drag, self.seam_hover.get("panel-resize").copied()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.panel_drag = true;
                    cx.set_global(GridHold(true));
                    cx.notify();
                }),
            )
            .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                if e.click_count() == 2 {
                    this.panel_width = PANEL_WIDTH;
                    save_from_sidebar(cx, |s| s.panel_width = PANEL_WIDTH);
                    cx.notify();
                }
            }));
        [panel.into_any_element(), edge.into_any_element()]
    }
}
