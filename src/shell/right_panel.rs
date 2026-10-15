//! The right panel's frame: a tab strip (Files, Changes, Search, and the PR while the shown repo has a remote), the
//! shown tab's body and the edge that resizes it. One toggle and one width for all of them; the PR tab's body is
//! `review.rs`'s, the others are `ui::side_panel`'s entity.
use super::*;
use super::review::ShownReview;
use crate::settings::{PANEL_MAX, PANEL_MIN, PANEL_WIDTH, PanelTab};
use crate::files::roots::{self, WorkspaceRoot, root_key};
use crate::files::runner::Host;
use crate::ui::side_panel::PanelContext;
use gpui::AnyElement;
use std::path::PathBuf;

/// Where a workspace's root is kept (`roots::root_key`) and what the workspace looked like (`roots::fingerprint`),
/// taken when a guess or the chooser starts so its answer lands on that workspace.
#[derive(Clone)]
struct RootSlot {
    place: String,
    workspace: String,
    key: String,
    fingerprint: String,
}

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

    /// The shown place's key (`Place::key`), once the places are listed.
    fn shown_place(&self) -> Option<String> {
        self.places.iter().find(|p| self.places_shown(p)).map(|p| p.key())
    }

    /// Where the shown workspace's root is kept and what it must fit; `None` until the places are listed (before
    /// then the place has no key that `prune_roots` would also use) and the snapshot lists the workspace.
    fn root_slot(&self) -> Option<RootSlot> {
        let place = self.shown_place()?;
        let workspace = self.shown_workspace.clone()?;
        let w = self.snapshot.as_ref()?.workspaces.iter().find(|w| w.workspace_id == workspace)?;
        let fingerprint = roots::fingerprint(&w.label, w.worktree.as_ref().map(|t| t.checkout_path.as_str()));
        Some(RootSlot { key: root_key(&place, &workspace), place, workspace, fingerprint })
    }

    /// `slot`'s workspace is still the one shown place's snapshot lists: an answer that took a while (a guess, the
    /// folder chooser) is not saved for a workspace closed or a place left meanwhile.
    fn slot_live(&self, slot: &RootSlot) -> bool {
        self.shown_place().as_deref() == Some(slot.place.as_str()) && self.snapshot.as_ref().is_some_and(|s| s.workspaces.iter().any(|w| w.workspace_id == slot.workspace))
    }

    /// What the side panel's tabs need to know of the shown place and workspace; handed over every render.
    pub(super) fn panel_context(&self, window: &Window, cx: &App) -> PanelContext {
        // before the places are listed, the session's socket, which is a session's key
        let place = self.shown_place().unwrap_or_else(|| self.client.path().to_string_lossy().into_owned());
        let snapshot = self.snapshot.as_ref();
        let active_dir = crate::ui::statusbar::pane_dir(snapshot, self.active_pane(window, cx).as_deref());
        // this window's place, not the app's last (`herdr_machine`): another window may show a session
        let remote = self.machine_label.is_some() || active_dir.as_deref().is_some_and(|d| crate::herdr::split(d).0.is_some());
        let workspace_id = self.shown_workspace.clone();
        // the worktree herdr made the workspace for, then the first tab's focused pane: the pane you happen to be in
        // may have wandered off to a dependency's folder, the workspace's first one rarely has
        let guess_dirs = workspace_id.as_deref().zip(snapshot).map(|(ws, snapshot)| {
            let worktree = snapshot.workspaces.iter().find(|w| w.workspace_id == ws).and_then(|w| w.worktree.as_ref());
            let worktree = worktree.map(|t| t.checkout_path.as_str()).filter(|p| !p.is_empty()).map(crate::herdr::locate);
            let first = snapshot.tabs_of(ws).first().and_then(|t| snapshot.layouts.iter().find(|l| l.tab_id == t.tab_id));
            let first = first.and_then(|l| crate::ui::statusbar::pane_dir(Some(snapshot), Some(&l.focused_pane_id)));
            worktree.into_iter().chain(first).collect()
        });
        let slot = self.root_slot();
        let stored = slot.as_ref().and_then(|s| SettingsGlobal::of(cx).settings.workspace_roots.get(&s.key).filter(|r| r.fits(&s.fingerprint)));
        let root = stored.map(|r| r.path.clone());
        // no slot yet is a root not known yet, as a guess running is
        let guessing = slot.is_none_or(|s| self.root_guesses.get(&s.key) == Some(&true));
        PanelContext { place, workspace_id, guess_dirs: guess_dirs.unwrap_or_default(), active_dir, remote, root, guessing }
    }

    /// Guesses the shown workspace's root (`roots::guess`) when it has none that fits, once per workspace and run;
    /// true when a guess started. Its answer is saved only for a workspace still there, and not over a folder chosen
    /// by hand meanwhile.
    fn guess_root(&mut self, context: &PanelContext, cx: &mut Context<Self>) -> bool {
        if context.remote || context.root.is_some() || context.guess_dirs.is_empty() {
            return false;
        }
        let Some(slot) = self.root_slot() else { return false };
        if self.root_guesses.contains_key(&slot.key) {
            return false;
        }
        self.root_guesses.insert(slot.key.clone(), true);
        let dirs: Vec<String> = context.guess_dirs.iter().map(|d| crate::herdr::split(d).1.to_string()).collect();
        let job = self.rt.spawn(async move { roots::guess(&Host::Local, &dirs, &crate::platform::home()).await });
        cx.spawn(async move |this, cx| {
            let root = job.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                // `prune_roots` took the mark: the workspace closed while git answered
                if this.root_guesses.get(&slot.key) != Some(&true) {
                    return;
                }
                if !this.slot_live(&slot) {
                    // the place was left: guessed again when it is back
                    this.root_guesses.remove(&slot.key);
                    return;
                }
                this.root_guesses.insert(slot.key.clone(), false);
                let chosen = SettingsGlobal::of(cx).settings.workspace_roots.get(&slot.key).is_some_and(|r| r.fits(&slot.fingerprint));
                if let Some(root) = root.filter(|_| !chosen) {
                    let root = WorkspaceRoot { path: root.to_string_lossy().into_owned(), fingerprint: slot.fingerprint };
                    save_from_sidebar(cx, |s| {
                        s.workspace_roots.insert(slot.key, root);
                    });
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// Drops the roots of the shown place's workspaces that are gone, as of its latest snapshot. A snapshot without
    /// workspaces is a server still starting (or one restarted empty), not a reason to forget them all.
    pub(super) fn prune_roots(&mut self, cx: &mut Context<Self>) {
        let (Some(place), Some(snapshot)) = (self.shown_place(), self.snapshot.as_ref()) else { return };
        if snapshot.workspaces.is_empty() {
            return;
        }
        let live: Vec<String> = snapshot.workspaces.iter().map(|w| w.workspace_id.clone()).collect();
        let live: Vec<&str> = live.iter().map(String::as_str).collect();
        // a workspace id herdr gives again later is guessed afresh, and a guess running for a closed one is dropped
        roots::prune(&mut self.root_guesses, &place, &live);
        let mut kept = SettingsGlobal::of(cx).settings.workspace_roots.clone();
        if roots::prune(&mut kept, &place, &live) {
            save_from_sidebar(cx, |s| s.workspace_roots = kept);
        }
    }

    /// `ChooseRoot`, and the panel's "Choose folder…": the system's folder chooser, its pick the shown workspace's
    /// root. The panel opens on Files to show it, unless it shows Changes or Search; opened only for this (the chord
    /// pressed in a pane), keyboard focus goes back to the pane when the chooser closes.
    pub(super) fn choose_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let opened = !self.panel_open || self.shown_panel_tab() == PanelTab::Pr;
        if opened {
            self.open_panel_tab(PanelTab::Files, window, cx);
        }
        let remote = self.panel_context(window, cx).remote;
        // the workspace asked for, not the one shown when the chooser closes
        let Some(slot) = self.root_slot().filter(|_| !remote) else {
            if remote {
                crate::ui::toast::error(cx, "Files on herdr machines aren't supported yet");
            }
            return;
        };
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Use Folder".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let picked = picked.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match picked {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.set_root(slot, path, cx);
                        }
                    }
                    // a Linux desktop without the file chooser portal (a bare window manager) has no dialog to show
                    Ok(Err(e)) => crate::ui::toast::error(cx, format!("The folder chooser didn't open ({e:#}). Use the focused pane's folder from the panel's … menu.")),
                    // cancelled
                    _ => {}
                }
                if opened && this.side_panel_focused(window, cx) {
                    this.focus_pane(window, cx);
                }
            });
        })
        .detach();
    }

    /// "Use focused pane's folder": the folder the focused pane is in, as it is (not its repo's top).
    pub(super) fn use_pane_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let context = self.panel_context(window, cx);
        let (Some(slot), Some(dir)) = (self.root_slot(), context.active_dir.as_deref()) else { return };
        if !context.remote {
            self.set_root(slot, PathBuf::from(crate::herdr::split(dir).1), cx);
        }
    }

    /// Keeps `path` as the root of `slot`'s workspace, unless it is `/`, home or above it. That check resolves
    /// symlinks, which an automount or a hung network share can stall: it runs off the main thread.
    fn set_root(&mut self, slot: RootSlot, path: PathBuf, cx: &mut Context<Self>) {
        let home = crate::platform::home();
        let check = {
            let (path, home) = (path.clone(), home.clone());
            self.rt.spawn_blocking(move || roots::is_too_broad(&path, &home))
        };
        cx.spawn(async move |this, cx| {
            let broad = check.await.unwrap_or(true);
            let _ = this.update(cx, |this, cx| {
                let path = path.to_string_lossy().into_owned();
                if broad {
                    let short = crate::ui::statusbar::short_path(&path, Some(&home));
                    return crate::ui::toast::error(cx, format!("{short} holds too much to list: choose a project's folder"));
                }
                if !this.slot_live(&slot) {
                    return;
                }
                let root = WorkspaceRoot { path, fingerprint: slot.fingerprint };
                save_from_sidebar(cx, |s| {
                    s.workspace_roots.insert(slot.key, root);
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// The root's row over Files, Changes and Search: where they look, and the `…` menu that changes it.
    fn root_row(&self, theme: &Theme, root: &str, cx: &mut Context<Self>) -> Div {
        let short = crate::ui::statusbar::short_path(root, Some(&crate::platform::home()));
        let open = self.root_menu.as_ref().is_some_and(|m| m.is_open());
        div()
            .h(px(30.))
            .flex_none()
            .pl(px(12.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .border_b_1()
            .border_color(theme.border)
            .child(gpui::svg().path("icons/folder.svg").size(px(14.)).flex_none().text_color(theme.text_muted))
            .child(
                div()
                    .id("panel-root-path")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(short)
                    .tooltip(tip(root.to_string(), None)),
            )
            .child(
                titlebar_button(theme, "panel-root-menu", "icons/more-horizontal.svg")
                    .tooltip(tip("Folder", None))
                    .when(open, |el| el.bg(theme.hover))
                    // Capture, so a press on the open menu's trigger closes it instead of the menu's click-outside
                    // closing it and the click reopening it (as the sidebar's view menu).
                    .capture_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        match this.root_menu.as_mut() {
                            Some(menu) if menu.is_open() => menu.close(),
                            _ => this.root_menu = Some(crate::ui::Popup::new(e.position)),
                        }
                        cx.notify();
                    })),
            )
    }

    /// The root row's `…` menu, while it is up; only drawn with the row (`root` is the row's folder).
    fn root_menu(&self, theme: &Theme, root: &str, active_dir: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.root_menu.as_ref()?;
        let at = menu.value;
        let root = root.to_string();
        let close = |this: &mut Shell| {
            if let Some(m) = this.root_menu.as_mut() {
                m.close()
            }
        };
        let item = |id: &'static str, icon: &'static str, label: &'static str| menu_choice(theme, id, icon, label, false);
        let card = menu_card(theme, "root-menu", 220.)
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                close(this);
                cx.notify();
            }))
            .child(item("root-menu-choose", "icons/solar/folder-open.svg", "Choose folder…").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                close(this);
                this.choose_root(window, cx);
            })))
            .when(active_dir, |el| {
                el.child(item("root-menu-pane", "icons/terminal.svg", "Use focused pane's folder").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    close(this);
                    this.use_pane_folder(window, cx);
                })))
            })
            .child(div().mx(px(-4.)).my(px(4.)).h(px(1.)).bg(theme.border))
            .child(item("root-menu-copy", "icons/solar/copy.svg", "Copy path").on_click(cx.listener({
                let root = root.clone();
                move |this, _: &ClickEvent, _, cx| {
                    close(this);
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(root.clone()));
                    cx.notify();
                }
            })))
            .child(item("root-menu-reveal", "icons/folder.svg", "Reveal in file manager").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                close(this);
                let root = root.clone();
                this.rt.spawn(async move {
                    let _ = tokio::process::Command::new(crate::platform::OPEN).arg(&root).status().await;
                });
                cx.notify();
            })));
        let anchored = gpui::anchored().position(at).snap_to_window_with_margin(px(8.)).child(crate::ui::menu_motion("root-menu-in", menu, card));
        Some(gpui::deferred(anchored).with_priority(1).into_any_element())
    }

    /// The Files tab's header buttons: hidden files, git-ignored files (each lit while shown, and saved), and a
    /// refresh that reads the open folders again and asks git afresh (the poll only asks when names change).
    fn files_header(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let settings = &SettingsGlobal::of(cx).settings;
        let (hidden, ignored) = (settings.panel_show_hidden, settings.panel_show_ignored);
        let toggle = |id: &'static str, icon: &'static str, on: bool, label: &'static str| {
            titlebar_button(theme, id, icon).when(on, |el| el.bg(theme.selected)).tooltip(tip(label, None))
        };
        vec![
            toggle("files-hidden", "icons/solar/eye.svg", hidden, if hidden { "Hide hidden files" } else { "Show hidden files" })
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| save_from_sidebar(cx, |s| s.panel_show_hidden = !hidden)))
                .into_any_element(),
            toggle("files-ignored", "icons/solar/eye-closed.svg", ignored, if ignored { "Hide git-ignored files" } else { "Show git-ignored files" })
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| save_from_sidebar(cx, |s| s.panel_show_ignored = !ignored)))
                .into_any_element(),
            titlebar_button(theme, "files-refresh", "icons/solar/refresh.svg")
                .tooltip(tip("Refresh", None))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.side_panel.update(cx, |p, cx| p.refresh_files(cx))))
                .into_any_element(),
        ]
    }

    /// The panel, and its edge: drag to resize, double-click for the default width.
    pub(super) fn render_right_panel(&mut self, theme: &Theme, shown: &ShownReview, window: &mut Window, cx: &mut Context<Self>) -> [AnyElement; 2] {
        let tab = self.shown_panel_tab();
        let mut context = self.panel_context(window, cx);
        context.guessing |= self.guess_root(&context, cx);
        let root = context.root.clone().filter(|_| tab != PanelTab::Pr && !context.remote);
        if root.is_none() {
            // its row went (the PR tab, a workspace without a root, a machine's): the menu goes with it
            self.root_menu = None;
        }
        let root_row = root.as_deref().map(|root| self.root_row(theme, root, cx));
        let root_menu = root.as_deref().and_then(|root| self.root_menu(theme, root, context.active_dir.is_some(), cx));
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
            PanelTab::Files if root.is_some() => (tab.label().into(), self.files_header(theme, cx)),
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
            _ => {
                let body = div().flex_1().min_h_0().flex().flex_col().children(root_row).child(div().flex_1().min_h_0().child(self.side_panel.clone()));
                (body.into_any_element(), None)
            }
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
            .children(footer)
            .children(root_menu);
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
