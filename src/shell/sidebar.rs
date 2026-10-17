//! The sidebar: its slide, resize and folds, and the agents list's height and sort.
use super::*;

/// How long the floating sidebar stays after the pointer leaves it.
const PEEK_LINGER: Duration = Duration::from_millis(200);

impl Shell {
    /// Folds or unfolds a worktree group, tweening from wherever its body is now.
    pub(super) fn toggle_fold(&mut self, id: &str, full: f32, cx: &mut Context<Self>) {
        let open = !self.folded.contains(id);
        let prev = self.fold_motion.get(id).copied();
        let from = prev.filter(|m| m.animating()).map(FoldMotion::current).unwrap_or(if open { full } else { 0. });
        let epoch = prev.map_or(1, |m| m.epoch + 1);
        self.fold_motion.insert(id.to_string(), FoldMotion { epoch, from, to: if open { 0. } else { full }, started: Instant::now() });
        if open {
            self.folded.insert(id.to_string());
        } else {
            self.folded.remove(id);
        }
        // ponytail: ids of workspaces since closed stay in the list; prune against the snapshot if it grows
        let mut folded: Vec<String> = self.folded.iter().cloned().collect();
        folded.sort();
        save_from_sidebar(cx, |s| s.folded = folded);
    }

    /// The sidebar's width this frame: mid tween, or where it rests.
    pub(super) fn sidebar_now(&self) -> f32 {
        match self.sidebar_motion.filter(|m| m.animating()) {
            Some(m) => m.current(),
            None if self.sidebar_open => self.sidebar_width + self.sidebar_nudge().unwrap_or(0.),
            None => 0.,
        }
    }

    /// The edge spring's offset while it plays.
    pub(super) fn sidebar_nudge(&self) -> Option<f32> {
        let (sign, started) = self.sidebar_edge?;
        let t = started.elapsed().as_secs_f32() / EDGE_SPRING.as_secs_f32();
        (t < 1.).then(|| sign * edge_nudge(t))
    }

    /// Hides or shows the sidebar with the fold tween; panes keep their grids until it lands.
    pub(super) fn hover_seam(&mut self, id: impl Into<SharedString>, on: bool, cx: &mut Context<Self>) {
        let entry = self.seam_hover.entry(id.into()).or_insert((false, 0));
        if entry.0 != on {
            *entry = (on, entry.1 + 1);
            cx.notify();
        }
    }

    /// The floating sidebar's slide this frame: 0 hidden, 1 shown.
    pub(super) fn peek_now(&self) -> f32 {
        self.sidebar_peek.map_or(0., FoldMotion::current)
    }

    /// The pointer came onto the window's left edge or the floating sidebar (`on`), or left them. It shows at once
    /// and hides a moment after the pointer leaves, so crossing from the edge to the card doesn't flicker.
    pub(super) fn hover_peek(&mut self, on: bool, cx: &mut Context<Self>) {
        self.peek_hovered = on;
        if on {
            self.set_peek(true, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PEEK_LINGER).await;
            let _ = this.update(cx, |shell, cx| {
                // ponytail: a sidebar menu holds it out; one closed with the pointer elsewhere leaves it out until
                // the pointer passes over it again
                let open = |m: Option<bool>| m.unwrap_or(false);
                let menu = open(shell.ws_menu.as_ref().map(|m| m.is_open()))
                    || open(shell.view_menu.as_ref().map(|m| m.is_open()))
                    || open(shell.agents_menu.as_ref().map(|m| m.is_open()))
                    || open(shell.place_menu.as_ref().map(|m| m.is_open()))
                    || shell.tree.menu_open();
                if !shell.peek_hovered && !menu {
                    shell.set_peek(false, cx);
                }
            });
        })
        .detach();
    }

    fn set_peek(&mut self, open: bool, cx: &mut Context<Self>) {
        let to = if open { 1. } else { 0. };
        if self.sidebar_peek.map_or(0., |m| m.to) == to {
            return;
        }
        self.sidebar_peek = Some(FoldMotion { epoch: 0, from: self.peek_now(), to, started: Instant::now() });
        cx.notify();
    }

    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        let from = self.sidebar_now();
        self.sidebar_peek = None;
        self.sidebar_edge = None;
        self.sidebar_open = !self.sidebar_open;
        let open = self.sidebar_open;
        save_from_sidebar(cx, |s| s.sidebar_open = open);
        let to = if self.sidebar_open { self.sidebar_width } else { 0. };
        let motion = FoldMotion { epoch: 0, from, to, started: Instant::now() };
        self.sidebar_motion = Some(motion);
        cx.set_global(GridHold(true));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FOLD_DURATION + Duration::from_millis(120)).await;
            let _ = this.update(cx, |shell, cx| {
                // a later toggle or a drag owns the hold now
                if shell.sidebar_motion.is_some_and(|m| m.started == motion.started) && !shell.sidebar_drag && shell.divider.is_none() {
                    shell.sidebar_motion = None;
                    cx.set_global(GridHold(false));
                    cx.refresh_windows();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn drag_sidebar(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if !self.sidebar_drag {
            return;
        }
        let x = f32::from(x);
        let edge = if x <= SIDEBAR_MIN { -1. } else if x >= SIDEBAR_MAX { 1. } else { 0. };
        // one spring per arrival at a limit, not one per mouse move while held there
        if edge == 0. {
            self.sidebar_edge = None;
        } else if self.sidebar_edge.map(|(s, _)| s) != Some(edge) {
            self.sidebar_edge = Some((edge, Instant::now()));
        }
        self.sidebar_width = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        cx.notify();
    }

    pub(super) fn end_sidebar_drag(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.sidebar_drag) {
            cx.set_global(GridHold(false));
            self.save_sidebar_width(cx);
            cx.refresh_windows();
        }
    }

    pub(super) fn save_frame(&mut self, cx: &mut Context<Self>) {
        if let Some(frame) = self.frame.take() {
            save_from_sidebar(cx, |s| s.window = Some(frame));
        }
    }

    pub(super) fn save_sidebar_width(&self, cx: &mut Context<Self>) {
        let width = self.sidebar_width;
        save_from_sidebar(cx, |s| s.sidebar_width = width);
    }

    pub(super) fn drag_agents(&mut self, y: Pixels, window: &Window, cx: &mut Context<Self>) {
        let Some((y0, h0)) = self.agents_drag else { return };
        // ponytail: capped at 60 % of the window so the workspaces list keeps some room
        let max = f32::from(window.viewport_size().height) * 0.6;
        self.agents_height = Some((h0 + y0 - f32::from(y)).clamp(AGENTS_MIN, max.max(AGENTS_MIN)));
        cx.notify();
    }

    pub(super) fn end_agents_drag(&mut self, cx: &mut Context<Self>) {
        if self.agents_drag.take().is_some() {
            let height = self.agents_height;
            save_from_sidebar(cx, |s| s.agents_height = height);
        }
    }

    /// Folds the Agents section to its header, or opens it again at the height it had.
    pub(super) fn toggle_agents_collapsed(&mut self, cx: &mut Context<Self>) {
        self.agents_collapsed = !self.agents_collapsed;
        self.agents_drag = None;
        let collapsed = self.agents_collapsed;
        save_from_sidebar(cx, |s| s.agents_collapsed = collapsed);
        cx.notify();
    }

    /// Brings the agent's tab up and its pane to the keyboard.
    pub(super) fn show_agent(&mut self, tab_id: String, pane_id: String, cx: &mut Context<Self>) {
        self.show_herdr_tab(Some(tab_id), true, cx);
        self.focus_next = Some(pane_id);
        self.restore = None;
    }

    pub(super) fn set_agent_sort(&mut self, sort: AgentSort, cx: &mut Context<Self>) {
        self.agent_sort = sort;
        if let Some(m) = self.agents_menu.as_mut() { m.close() }
        save_from_sidebar(cx, |s| s.agent_sort = sort);
        cx.notify();
    }

    /// The workspace list's rows for `snapshot`: each workspace with its worktrees folded under it. `Live` rows answer
    /// clicks, renames and drags and show worktrees being made; another session's (`Other`) switch there on a click.
    /// `hints` holds the numbered workspaces' chords across the tree.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn workspace_rows(
        &self,
        theme: &Theme,
        snapshot: Option<&Snapshot>,
        shown_ws: &Option<String>,
        mode: sidebar_tree::RowMode,
        hints: Option<&sidebar_tree::Chords>,
        active: bool,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let live = mode == sidebar_tree::RowMode::Live;
        // folds are kept per session, and ids told apart by session: workspace ids repeat across sessions
        let (session, idx) = self.row_keys(&mode);
        let fold_key = |id: &str| crate::files::roots::root_key(&session, id);
        // a workspace row's height, one line; a worktree's, which always has a branch line unless compact
        let (ws_h, kid_h) = if SettingsGlobal::of(cx).settings.compact_workspaces { (26., 26.) } else { (32., WS_TWO_LINES) };
        let groups: Vec<(crate::model::Workspace, Vec<crate::model::Workspace>)> = snapshot
            .as_ref()
            .map(|s| {
                crate::model::groups(s.workspace_tree())
                    .into_iter()
                    .map(|(w, kids)| (w.clone(), kids.into_iter().cloned().collect()))
                    .collect()
            })
            .unwrap_or_default();
        // held Ctrl+Shift (Ctrl on macOS): each of the first nine workspaces' chord, in `go_to_workspace`'s order
        let hint = hints;
        let ws_row = |w: &crate::model::Workspace, nested: bool, chevron: Option<gpui::AnyElement>, folded: Option<usize>, cx: &mut Context<Self>| {
            let chip = hint.and_then(|(opacity, chords)| {
                let (_, _, chord) = chords.iter().find(|(k, id, _)| k.0 == session && *id == w.workspace_id)?;
                Some(hints::chip(theme, chord, *opacity))
            });
            let show = SettingsGlobal::of(cx).settings.workspace_show;
            let compact = SettingsGlobal::of(cx).settings.compact_workspaces;
            let selected = shown_ws.as_deref() == Some(w.workspace_id.as_str());
            let first_tab =
                snapshot.as_ref().and_then(|s| s.tabs_of(&w.workspace_id).first().map(|t| t.tab_id.clone()));
            let target = w.active_tab_id.clone().or(first_tab);
            let rename_target = RenameTarget::Workspace(w.workspace_id.clone());
            let editing = live.then(|| self.rename_field(&rename_target, cx)).flatten();
            let (ws_id, label) = (w.workspace_id.clone(), w.label.clone());
            // a workspace herdr knows no worktree of lives where its panes were opened
            let dir = w.worktree.as_ref().map(|t| t.checkout_path.as_str()).filter(|d| !d.is_empty()).or_else(|| {
                snapshot.as_ref()?.panes.iter().find(|p| p.workspace_id == w.workspace_id)?.cwd.as_deref()
            }).and_then(|d| self.row_dir(&mode, d));
            // another session's rows read their branch from HEAD alone: no status, ahead or behind for them
            let git = dir.as_deref().filter(|_| show.branch && live).and_then(|d| crate::ui::statusbar::git_of(d, cx));
            let head = dir.as_deref().filter(|_| show.branch && !live).and_then(|d| Shell::other_branch(d, cx));
            // herdr's commits ahead of and behind the upstream, also on a compact row
            let sync = git.as_ref().filter(|g| g.ahead > 0 || g.behind > 0).map(|g| {
                div()
                    .flex_none()
                    .flex()
                    .gap(px(3.))
                    .text_size(px(11.))
                    .when(g.ahead > 0, |el| el.child(div().text_color(theme.success.opacity(0.8)).child(format!("↑{}", g.ahead))))
                    .when(g.behind > 0, |el| el.child(div().text_color(theme.danger.opacity(0.8)).child(format!("↓{}", g.behind))))
            });
            let branch = git.map(|g| g.branch).or(head).filter(|_| !compact);
            let (sync, sync_inline) = if compact { (None, sync) } else { (sync, None) };
            // compact rows are the name and its ↑↓ alone: gh and git's diff aren't even asked
            // another session's rows ask neither gh nor its diff
            let (changes, pr) = match dir.as_deref().filter(|_| nested && !compact && live) {
                Some(d) => crate::ui::statusbar::changes_of(d, active, cx),
                None => (None, None),
            };
            let changes = changes.filter(|c| show.changes && *c != (0, 0)).map(|(added, removed)| {
                div()
                    .flex()
                    .gap(px(3.))
                    .when(added > 0, |el| el.child(div().text_color(theme.success.opacity(0.7)).child(format!("+{added}"))))
                    .when(removed > 0, |el| el.child(div().text_color(theme.danger.opacity(0.7)).child(format!("−{removed}"))))
            });
            let pr = pr.filter(|_| show.pull_request);
            let extras = (changes.is_some() || pr.is_some()).then(|| {
                div().flex_none().flex().gap(px(4.)).text_size(px(11.)).text_color(theme.text_faint).children(changes).children(pr)
            });
            // Zeron's card: the name, and under it the branch with what it changed; one line when compact
            // a worktree always has its branch line, so a folding group knows its height
            let two = !compact && (nested || branch.is_some() || extras.is_some());
            let (inline, below) = if two { (None, extras) } else { (extras, None) };
            // a repo's own artwork or its monogram; its worktrees, indented under its name, go without
            // compact, a worktree has no branch line: a branch mark in front of its name tells it apart
            let icon = if nested {
                compact.then(|| {
                    div()
                        .size(px(14.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(gpui::svg().path("icons/git-branch.svg").size(px(12.)).text_color(theme.text_muted))
                        .into_any_element()
                })
            } else {
                show.icon.then(|| {
                    let seed = w.worktree.as_ref().map_or(w.label.as_str(), |t| t.repo_key.as_str());
                    crate::ui::project_icon::project_icon(&w.label, seed, dir.as_deref(), 14., selected, theme, cx)
                })
            };
            let name = match editing {
                Some(field) => field.into_any_element(),
                None => div().min_w_0().truncate().child(w.label.clone()).into_any_element(),
            };
            row(theme, selected, SharedString::from(format!("ws-{idx}-{}", w.workspace_id)))
                .h(px(if two { WS_TWO_LINES } else { ws_h }))
                .when(live, |el| pane_drag::accept_pane_in_workspace(el, &w.workspace_id, theme.accent.opacity(0.15), cx))
                .when(nested, |el| el.pl(px(if show.icon { 30. } else { 20. })))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .children(icon)
                                .child(name)
                                .children(folded.map(|n| count_badge(theme, n)))
                                .children(sync_inline)
                                .child(div().flex_1())
                                .children(inline),
                        )
                        .when(two, |el| {
                            el.child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(11.))
                                    .text_color(theme.text_muted)
                                    .when(show.icon && !nested, |el| el.pl(px(22.)))
                                    .children(branch.map(|b| {
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.))
                                            .child(gpui::svg().path("icons/git-branch.svg").size(px(11.)).flex_none().text_color(theme.text_muted))
                                            .child(div().min_w_0().truncate().child(b))
                                    }))
                                    .children(sync)
                                    .child(div().flex_1())
                                    .children(below),
                            )
                        }),
                )
                .map(|el| {
                    if live && self.removing.contains(&w.workspace_id) {
                        el.text_color(theme.text_faint).child(spinner(theme, format!("ws-removing-{idx}-{}", w.workspace_id)))
                    } else {
                        el.when(show.status, |el| el.child(status_dot(w.agent_status, theme)))
                    }
                })
                .children(chevron)
                // over the row's right end (its status and chevron), so nothing in it moves
                .when_some(chip, |el, chip| el.relative().child(chip))
                .when_some(match &mode {
                    sidebar_tree::RowMode::Other(key) => Some(key.clone()),
                    _ => None,
                }, |el, key| {
                    let id = w.workspace_id.clone();
                    el.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go_to(key.clone(), sidebar_tree::Goal::Workspace(id.clone()), cx)))
                })
                .when(live, |el| {
                    el.on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        if e.click_count() == 2 {
                            this.start_rename(RenameTarget::Workspace(ws_id.clone()), label.clone(), window, cx);
                            return;
                        }
                        // the workspace as it was left, a file tab it showed included
                        this.show_herdr_tab(target.clone(), false, cx);
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener({
                        let id = w.workspace_id.clone();
                        move |this, e: &MouseDownEvent, _, cx| {
                            this.ws_menu = Some(crate::ui::Popup::new((id.clone(), e.position)));
                            cx.notify();
                        }
                    }))
                })
        };
        groups.into_iter().map(|(w, kids)| {
            // Pending worktrees join the group of the workspace their dialog was opened on.
            // herdr opens the worktree's workspace before it answers (hooks still run): once
            // its checkout shows up, the real row replaces the placeholder.
            let checked_out = |branch: &str| {
                let slug = crate::commands::branch_path_slug(branch);
                kids.iter().any(|k| {
                    k.worktree.as_ref().is_some_and(|t| {
                        std::path::Path::new(&t.checkout_path).file_name().is_some_and(|n| *n == *slug)
                    })
                })
            };
            let pending: Vec<String> = self
                .pending_worktrees
                .iter()
                .filter(|(id, _)| *id == w.workspace_id || kids.iter().any(|k| k.workspace_id == *id))
                .filter(|(_, branch)| live && !checked_out(branch))
                .map(|(_, branch)| branch.clone())
                .collect();
            if kids.is_empty() && pending.is_empty() {
                return ws_row(&w, false, None, None, cx).into_any_element();
            }
            // A folded group opens while one of its worktrees is being created.
            let fold = fold_key(&w.workspace_id);
            let open = !self.folded.contains(&fold) || !pending.is_empty();
            let full = (kids.len() + pending.len()) as f32 * (kid_h + 2.);
            let tween = self.fold_motion.get(&fold).copied().filter(|m| m.animating());
            let chevron = fold_chevron(theme, &format!("{idx}-{}", w.workspace_id), open, full, tween).map(|el| {
                el.on_click(cx.listener({
                    let id = fold.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_fold(&id, full, cx);
                        cx.notify();
                    }
                }))
            });
            let header = ws_row(&w, false, Some(chevron.into_any_element()), (!open).then_some(kids.len()), cx);
            let body = div()
                .w_full()
                .flex_none()
                .overflow_hidden()
                .flex()
                .flex_col()
                .pt(px(2.))
                .gap(px(2.))
                .children(kids.iter().map(|k| ws_row(k, true, None, None, cx).into_any_element()).collect::<Vec<_>>())
                .children(pending.iter().map(|b| pending_worktree_row(theme, b).h(px(kid_h)).into_any_element()));
            let body = match tween {
                None => body.h(px(if open { full } else { 0. })).into_any_element(),
                // Zeron's disclosure: the height tweens, the rows fade up from 35 % and slide 3 px.
                Some(m) => body
                    .with_animation(
                        SharedString::from(format!("fold-{idx}-{}-{}", w.workspace_id, m.epoch)),
                        fold_animation(),
                        move |el, t| {
                            let h = m.from + (m.to - m.from) * t;
                            let reveal = (h / full).clamp(0., 1.);
                            el.h(px(h)).opacity(0.35 + 0.65 * reveal).relative().top(px(-3. * (1. - reveal)))
                        },
                    )
                    .into_any_element(),
            };
            div().flex().flex_col().flex_none().child(header).child(body).into_any_element()
        }).collect::<Vec<_>>()
    }

    /// The sidebar's rows: workspaces with their worktrees folded under them, the agents list and the update strip.
    pub(super) fn render_sidebar(
        &mut self,
        theme: &Theme,
        snapshot: &Option<Snapshot>,
        shown_ws: &Option<String>,
        shown_tab: &Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        // every session's agents; the section stays while any agent runs, so its menu can turn the filter off again
        let (agents, any_agents) = self.agents_everywhere(SettingsGlobal::of(cx).settings.attention_agents, cx);
        let active = window.is_window_active();

        let strip = update_strip(theme, cx);
        let (tree, listed) = self.tree_rows(theme, snapshot, shown_ws, active, cx);
        let waiting = self.error.is_none() && self.connecting.is_some_and(|at| crate::ui::loader::due(at, window));
        // Laid out at full width and clipped by its tweened box, so rows never reflow mid slide.
        div()
            .w(px(self.sidebar_width - 1.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .pt(px(TITLEBAR_HEIGHT))
            .child(
                div().relative().flex_1().min_h_0().overflow_hidden().child(
            div()
                .relative()
                .size_full()
                .flex()
                .flex_col()
            .child(
                div()
                    .px(px(16.))
                    .pt(px(8.))
                    .pb(px(6.))
                    .flex()
                    .items_center()
                    .text_size(px(11.))
                    .text_color(theme.text_faint)
                    // the tree names every session; with one, its name heads the list
                    .child(div().flex_1().truncate().when(!listed, |el| el.child(self.place_label(cx))))
                    .when(is_dev_build(), |el| el.child(dev_badge(theme)))
                    // the place's own snapshot hasn't come yet: the list is its last, or none
                    .when(waiting, |el| el.child(crate::ui::loader::mini_spinner(2., theme.text_faint, window))),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(
                div()
                    .id("workspaces")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(tree)
            ),
            )
            .when(SettingsGlobal::of(cx).settings.show_agents && any_agents, |el| {
                let seam = theme.accent;
                let dragging = self.agents_drag.is_some();
                let collapsed = self.agents_collapsed;
                // A click on the header folds the section to it, at the sidebar's foot (the tree above takes the
                // room), and opens it again at the height it had.
                el.child(
                    div()
                        .id("agents-header")
                        .relative()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_agents_collapsed(cx)))
                        .px(px(16.))
                        .pt(px(12.))
                        // the list's top fade starts just under the label
                        .pb(px(4.))
                        // the resize edges' resting line (`border_strong`, as the pane dividers'), so the section
                        // reads apart from the tree above while the seam lights only on hover or drag
                        .border_t_1()
                        .border_color(theme.border_strong)
                        .text_size(px(11.))
                        .text_color(theme.text_faint)
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .ml(px(-4.))
                                .child(fold_chevron(theme, "agents", !collapsed, 1., None))
                                .child("Agents"),
                        )
                        .child(
                            icon_button(theme, "agents-view", "")
                                .tooltip(tip("Sort agents", None))
                                .size(px(20.))
                                .my(px(-4.))
                                .child(gpui::svg().path("icons/more-horizontal.svg").size(px(14.)).text_color(theme.text_faint))
                                .when(self.agents_menu.as_ref().is_some_and(|m| m.is_open()), |el| el.bg(theme.hover))
                                // capture, like the sidebar's `…`: a press on the open menu's trigger closes it
                                .capture_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    match this.agents_menu.as_mut() {
                                        Some(menu) if menu.is_open() => menu.close(),
                                        _ => this.agents_menu = Some(crate::ui::Popup::new(e.position)),
                                    }
                                    cx.notify();
                                })),
                        )
                        // The section's top edge: drag to resize the list, double-click for the default height. Folded,
                        // there is no list to size, so the edge is only its resting line.
                        .when(!collapsed, |el| el.child(
                            div()
                                .id("agents-resize")
                                .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover_seam("agents-resize", *on, cx)))
                                .absolute()
                                .top(px(-3.))
                                .left_0()
                                .right_0()
                                .h(px(6.))
                                .cursor_row_resize()
                                .child(resize_seam("agents-resize".into(), false, 3., seam, dragging, self.seam_hover.get("agents-resize").copied()))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        let height = f32::from(this.agents_scroll.bounds().size.height);
                                        this.agents_drag = Some((f32::from(e.position.y), height));
                                        cx.notify();
                                    }),
                                )
                                .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                                    if e.click_count() == 2 {
                                        this.agents_height = None;
                                        save_from_sidebar(cx, |s| s.agents_height = None);
                                        cx.notify();
                                    }
                                })),
                        )),
                )
                .when(!collapsed, |el| el.child(
                    div()
                        .relative()
                        .flex_none()
                        .map(|el| match self.agents_height {
                            Some(h) => el.max_h(px(h)),
                            None => el.max_h(relative(0.4)),
                        })
                        .flex()
                        .flex_col()
                        .child(
                        // Zeron's fade where rows scroll under the edges (the bar below)
                        crate::ui::edge_fade::edge_faded(12., &self.agents_scroll, div()
                            .id("agents")
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.agents_scroll)
                            .px(px(8.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .when(agents.is_empty(), |el| {
                                el.child(div().px(px(8.)).py(px(6.)).text_size(px(11.)).text_color(theme.text_faint).child("Nothing needs you"))
                            })
                            .children(agent_rows(agents, self.agent_sort, SettingsGlobal::of(cx).settings.compact_agents, shown_tab.as_deref(), theme, |row, e| {
                                let (key, tab, pane) = (e.key.clone(), e.pane.tab_id.clone(), e.pane.pane_id.clone());
                                let goal = sidebar_tree::Goal::Pane { tab, pane };
                                row.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go_to(key.clone(), goal.clone(), cx)))
                            }))
                            // the last row scrolls clear of the list's faded edge
                            .child(div().flex_none().h(px(4.)))),
                    )
                    .child(crate::ui::scrollbar("agents-scrollbar", &self.agents_scroll, theme)),
                ))
            }),
            ))
            .children(strip)
            .child(self.footer(theme, cx))
    }

    /// Under the sidebar: Settings, a new workspace in the session shown, and + for a new session or SSH machine.
    fn footer(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let hover_bg = theme.hover;
        let button = |id: &'static str, icon: &'static str| {
            div()
                .id(id)
                .size(px(30.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover_bg))
                .child(gpui::svg().path(icon).size(px(16.)).text_color(theme.text_muted))
        };
        div()
            .flex_none()
            // close under the agents list, the window's edge kept below
            .pt(px(2.))
            .pb(px(8.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(4.))
            .child(
                button("open-settings", "icons/settings.svg")
                    .tooltip(tip("Settings", Some("OpenSettings")))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_settings_here(window, cx))),
            )
            .child(
                div()
                    .id("new-workspace")
                    .flex_1()
                    .min_w_0()
                    .h(px(30.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(8.))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .tooltip(tip("New workspace", Some("NewWorkspace")))
                    .child(div().text_size(px(15.)).child("+"))
                    .child(div().truncate().child("New workspace"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.new_workspace(cx))),
            )
            .child(
                button("place-new", "icons/plus.svg")
                    .tooltip(tip("New session or SSH machine", None))
                    .when(self.tree.add_menu_open(), |el| el.bg(theme.hover))
                    // capture, like the sidebar's `…`: a press on the open menu's trigger closes it
                    .capture_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_add_menu(e.position, cx);
                    })),
            )
    }
}

/// The agents list's rows, `act` making each one do something.
pub(super) fn agent_rows(
    agents: Vec<sidebar_tree::AgentEntry>,
    sort: AgentSort,
    compact: bool,
    shown_tab: Option<&str>,
    theme: &Theme,
    act: impl Fn(Stateful<Div>, &sidebar_tree::AgentEntry) -> Stateful<Div>,
) -> Vec<gpui::AnyElement> {
    let workspaces: Vec<(usize, String)> = agents.iter().map(|e| (e.n, e.pane.workspace_id.clone())).collect();
    let grouped = sort == AgentSort::Grouped;
    agents
        .into_iter()
        .enumerate()
        .flat_map(|(i, entry)| {
            let (pane, workspace, waited) = (&entry.pane, entry.workspace.clone(), entry.waited.clone());
            // Grouped: a heading where the workspace (or its session) changes, which the rows then leave out.
            let heading = (grouped && (i == 0 || workspaces[i - 1] != workspaces[i])).then(|| {
                div()
                    .flex_none()
                    .px(px(8.))
                    .pt(px(if i == 0 { 2. } else { 8. }))
                    .pb(px(2.))
                    .truncate()
                    .text_size(px(11.))
                    .text_color(theme.text_faint)
                    .child(workspace.clone())
                    .into_any_element()
            });
            let selected = entry.shown && shown_tab == Some(pane.tab_id.as_str());
            let title = pane.terminal_title_stripped.clone().unwrap_or_default();
            let mark = crate::ui::command_panel::agent_icon(pane.agent.as_deref().unwrap_or_default());
            let mark = || gpui::svg().path(mark).size(px(14.)).flex_none().text_color(crate::ui::command_panel::icon_ink(mark, theme.text_muted));
            let agent = row(theme, selected, SharedString::from(format!("agent-{}-{}", entry.n, pane.pane_id)))
                .h(px(if compact { 30. } else { 44. }))
                .child(status_dot(pane.agent_status, theme))
                .when(compact, |el| {
                    el.child(mark())
                        .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(theme.text_muted).child(title.clone()))
                        .children(waited.clone().map(|w| div().flex_none().text_size(px(11.)).text_color(theme.text_faint).child(w)))
                })
                .when(!compact, |el| {
                    el.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.))
                                    .items_center()
                                    .child(mark())
                                    .when(!grouped, |el| el.child(div().min_w_0().truncate().text_color(theme.text_faint).child(workspace)))
                                    .children(waited.map(|w| div().ml_auto().flex_none().text_size(px(11.)).text_color(theme.text_faint).child(w))),
                            )
                            .child(div().truncate().text_size(px(11.)).text_color(theme.text_muted).child(title)),
                    )
                });
            heading.into_iter().chain([act(agent, &entry).into_any_element()])
        })
        .collect()
}
