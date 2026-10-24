//! The sidebar: its slide, resize and folds, and the agents list's sort.
use super::*;

/// How long the floating sidebar stays after the pointer leaves it.
const PEEK_LINGER: Duration = Duration::from_millis(200);

impl Shell {
    /// Folds or unfolds a worktree group, tweening from wherever its body is now.
    pub(super) fn toggle_fold(&mut self, id: &str, full: f32, cx: &mut Context<Self>) {
        // a search shows every group open: a click then must not change the folds it gives back
        if self.search_query(cx).is_some() {
            return;
        }
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
        // turned off in Settings: nothing slides out, and a card still out goes as the pointer leaving it would
        let on = hover_shows(SettingsGlobal::of(cx).settings.sidebar_peek, on);
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
                    || open(shell.web_menu.as_ref().map(|m| m.is_open()))
                    || open(shell.view_menu.as_ref().map(|m| m.is_open()))
                    || open(shell.agents_menu.as_ref().map(|m| m.is_open()))
                    || open(shell.place_menu.as_ref().map(|m| m.is_open()))
                    || shell.tree.menu_open()
                    // searching in it: the card stays until the search closes
                    || shell.sidebar_search.is_some()
                    // a drag out of it would drop it mid-way
                    || cx.has_active_drag();
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

    /// The sidebar's view, Workspaces or Agents, kept in the settings. A drop being aimed in the tree goes: its rows
    /// are not there to take it in the Agents view.
    pub(super) fn set_sidebar_view(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        // the segment already shown: nothing to save
        if SettingsGlobal::of(cx).settings.sidebar_view == view {
            return;
        }
        self.tree.drop = None;
        // a ticker scrolling the tree mid drag stops at its next frame: the tree is not shown
        self.tree.scroll_speed = 0.;
        save_from_sidebar(cx, |s| s.sidebar_view = view);
        cx.notify();
    }

    /// The ToggleAgentsView action: the other view, while the toggle shows (the Agents section is on).
    pub(super) fn toggle_agents_view(&mut self, cx: &mut Context<Self>) {
        let settings = &SettingsGlobal::of(cx).settings;
        if !settings.show_agents {
            return;
        }
        let next = if settings.sidebar_view == SidebarView::Agents { SidebarView::Workspaces } else { SidebarView::Agents };
        self.set_sidebar_view(next, cx);
    }

    /// The agents' `…`: their sort and filter menu, opened under it, or closed when it is open.
    fn agents_sort_button(&self, theme: &Theme, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        icon_button(theme, id, "")
            .tooltip(tip("Sort agents", None))
            .size(px(20.))
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
            }))
    }

    /// The toggle at the sidebar's top, uherdr's: Workspaces | Agents with ●N of the agents waiting everywhere (the
    /// Dock's count), two equal segments drawn as the right panel's tabs; in the Agents view, its sort menu beside.
    fn view_toggle(&self, theme: &Theme, agents: bool, n: usize, cx: &mut Context<Self>) -> Div {
        let hover = theme.hover;
        let segment = |id: &'static str, label: &'static str, on: bool, view: SidebarView| {
            div()
                .id(id)
                .flex_1()
                .min_w_0()
                .h(px(26.))
                .px(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(5.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(if on { theme.text } else { theme.text_muted })
                .when(on, |el| el.bg(theme.selected).font_weight(gpui::FontWeight::MEDIUM))
                .when(!on, |el| el.hover(move |s| s.bg(hover)))
                .child(div().min_w_0().truncate().child(label))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_sidebar_view(view, cx)))
        };
        div()
            .flex_none()
            .px(px(8.))
            .pb(px(6.))
            .flex()
            .items_center()
            .gap(px(2.))
            .child(segment("sidebar-view-workspaces", "Workspaces", !agents, SidebarView::Workspaces))
            .child(segment("sidebar-view-agents", "Agents", agents, SidebarView::Agents).when(n > 0, |el| el.child(waiting_pill(theme, n))))
            .when(agents, |el| el.child(self.agents_sort_button(theme, "agents-view-sort", cx)))
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
    /// `hints` holds the numbered workspaces' chords across the tree; `filter`, what a search keeps of them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn workspace_rows(
        &self,
        theme: &Theme,
        snapshot: Option<&Snapshot>,
        shown_ws: &Option<String>,
        mode: sidebar_tree::RowMode,
        hints: Option<&sidebar_tree::Chords>,
        active: bool,
        filter: sidebar_tree::RowFilter,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let live = mode == sidebar_tree::RowMode::Live;
        // folds are kept per session, and ids told apart by session: workspace ids repeat across sessions
        let (session, idx) = self.row_keys(&mode);
        let fold_key = |id: &str| crate::files::roots::root_key(&session, id);
        // a drop herdr hasn't answered yet shows its order already
        let key = watchers::SessionKey(session.clone());
        let pending = snapshot.and_then(|s| self.pending_order(&key, s));
        let snapshot = pending.as_ref().or(snapshot);
        // a workspace row's height, one line; a worktree's, which always has a branch line unless compact
        let (ws_h, kid_h) = if SettingsGlobal::of(cx).settings.compact_workspaces { (26., 26.) } else { (32., WS_TWO_LINES) };
        // tabs listed under their workspace instead of in the bar along the top
        let tabs_in = SettingsGlobal::of(cx).settings.tabs_in_sidebar;
        let tab_count = |ws: &str| match (tabs_in, snapshot.as_ref()) {
            (true, Some(s)) => self.sidebar_tab_count(s, ws, live),
            _ => 0,
        };
        // as Workspace1–9 count them: with a search, what it keeps
        let groups: Vec<(crate::model::Workspace, Vec<crate::model::Workspace>)> = snapshot
            .as_ref()
            .map(|s| sidebar_tree::shown_groups(s, filter.keep).into_iter().map(|(w, kids)| (w.clone(), kids.into_iter().cloned().collect())).collect())
            .unwrap_or_default();
        // a workspace moves with its worktrees among the others, a worktree among its siblings; not one being removed,
        // nor any while the tree is searched
        let movable = filter.allows(self.can_drag_workspaces(&key));
        let draggable: std::collections::HashSet<String> = groups
            .iter()
            .flat_map(|(w, kids)| {
                let group = (groups.len() > 1).then_some(w);
                let kids = kids.iter().filter(|_| kids.len() > 1);
                group.into_iter().chain(kids).map(|w| w.workspace_id.clone())
            })
            .filter(|id| movable && !(live && self.removing.contains(id)))
            .collect();
        // this session's drag line, by the workspace it is drawn at
        let line = self.tree.drop.as_ref().filter(|d| matches!(&d.over, tree_drag::Over::Workspace(k, _) if *k == key)).map(|d| d.line.clone());
        let group_line = |id: &str| match &line {
            Some(tree_drag::Line::Group(g, edge)) if g == id => Some(*edge),
            _ => None,
        };
        let kid_line = |id: &str| match &line {
            Some(tree_drag::Line::Kid(k, edge)) if k == id => Some(*edge),
            _ => None,
        };
        // held Ctrl+Shift (Ctrl on macOS): each of the first nine workspaces' chord, in `go_to_workspace`'s order
        let hint = hints;
        // `takes_drop`: the row takes a drop; a folded group's hidden worktrees keep their bounds under the rows after it
        let ws_row = |w: &crate::model::Workspace, nested: bool, chevron: Option<gpui::AnyElement>, folded: Option<usize>, takes_drop: bool, cx: &mut Context<Self>| {
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
                .when(draggable.contains(&w.workspace_id), |el| {
                    let item = if nested { tree_drag::Item::Kid(w.workspace_id.clone()) } else { tree_drag::Item::Group(w.workspace_id.clone()) };
                    let dragged = tree_drag::DraggedTreeItem { from: key.clone(), item, label: w.label.clone(), theme: theme.clone() };
                    el.on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
                })
                .when(filter.allows(takes_drop), |el| {
                    let over = tree_drag::Over::Workspace(key.clone(), w.workspace_id.clone());
                    el.on_drag_move(cx.listener(move |this, e: &gpui::DragMoveEvent<tree_drag::DraggedTreeItem>, _, cx| this.tree_drag_over(over.clone(), e, cx)))
                })
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
        // a workspace's row with its tabs under it
        let with_tabs = |row: gpui::Stateful<Div>, w: &crate::model::Workspace, nested: bool, cx: &mut Context<Self>| -> gpui::AnyElement {
            let n = tab_count(&w.workspace_id);
            match snapshot.as_ref().filter(|_| n > 0) {
                Some(s) => div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .child(row)
                    .child(div().flex().flex_col().flex_none().pt(px(2.)).gap(px(2.)).children(self.sidebar_tab_rows(theme, s, &w.workspace_id, nested, &mode, idx, cx)))
                    .into_any_element(),
                None => row.into_any_element(),
            }
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
                return tree_drag::with_line(with_tabs(ws_row(&w, false, None, None, true, cx), &w, false, cx), theme, group_line(&w.workspace_id));
            }
            // A folded group opens while one of its worktrees is being created, and while the tree is searched.
            let fold = fold_key(&w.workspace_id);
            let open = sidebar_tree::kids_shown(self.folded.contains(&fold), filter.keep.is_some()) || !pending.is_empty();
            let full = kids.iter().map(|k| kid_h + 2. + tab_bar::tab_block_h(tab_count(&k.workspace_id))).sum::<f32>() + pending.len() as f32 * (kid_h + 2.);
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
            let header = ws_row(&w, false, Some(chevron.into_any_element()), (!open).then_some(kids.len()), true, cx);
            let kids_take_drops = open && tween.is_none();
            let body = div()
                .w_full()
                .flex_none()
                .overflow_hidden()
                .flex()
                .flex_col()
                .pt(px(2.))
                .gap(px(2.))
                .children(kids.iter().map(|k| tree_drag::with_line(with_tabs(ws_row(k, true, None, None, kids_take_drops, cx), k, true, cx), theme, kid_line(&k.workspace_id))).collect::<Vec<_>>())
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
            let group = div().flex().flex_col().flex_none().child(with_tabs(header, &w, false, cx)).child(body);
            tree_drag::with_line(group, theme, group_line(&w.workspace_id))
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
        let active = window.is_window_active();
        let strip = update_strip(theme, cx);
        // a drag's line and drops herdr carried out are tidied whichever view shows
        self.tidy_tree_drag(window, cx);
        self.retitle_sidebar_search(window, cx);
        // every session's agents, read once for the Agents view
        let sessions = self.session_agents(cx);
        let settings = &SettingsGlobal::of(cx).settings;
        let layout = sidebar_layout(settings.sidebar_view, settings.show_agents);
        let bar = settings.sessions_bar;
        let (body, listed) = if layout.tree {
            let (tree, listed) = self.tree_rows(theme, snapshot, shown_ws, active, cx);
            let body = div()
                .id("workspaces")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.tree.scroll)
                // a sideways swipe moves between places, not down the list
                .map(|mut el| {
                    if bar {
                        el.style().restrict_scroll_to_axis = Some(true);
                    }
                    el
                })
                // a row dragged in the tree: its rows find where it lands, a release anywhere on it drops it
                .on_drag_move(cx.listener(|this, e: &gpui::DragMoveEvent<tree_drag::DraggedTreeItem>, _, cx| this.tree_drag_left(e, cx)))
                .on_drop(cx.listener(|this, d: &tree_drag::DraggedTreeItem, window, cx| this.drop_tree_item(d, window.mouse_position(), cx)))
                .px(px(8.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(tree);
            (body, listed)
        } else {
            // every session's agents under its header, the whole height: no drags here
            let (rows, listed) = self.agents_view_rows(theme, &sessions, shown_tab.as_deref(), cx);
            let body = div()
                .id("agents-view")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.agents_view_scroll)
                .px(px(8.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(rows)
                // the last row scrolls clear of the sidebar's foot
                .child(div().flex_none().h(px(4.)));
            (body, listed)
        };
        let toggle = layout.toggle.then(|| self.view_toggle(theme, layout.agents_view, self.waiting_everywhere(), cx));
        // focus mode narrows the Workspaces tree alone: its chip shows there
        let focus = layout.tree && SettingsGlobal::of(cx).settings.sidebar_focus && !SettingsGlobal::of(cx).settings.sessions_bar;
        let header = self.sidebar_header(theme, listed, focus, cx);
        let waiting = self.error.is_none() && self.connecting.is_some_and(|at| crate::ui::loader::due(at, window));
        // the sessions bar: the list goes under a swipe's fingers, then springs home, and beside it the place it goes to
        // (or comes from)
        let (list_offset, beside) = if bar { self.list_offset(window, cx) } else { (0., None) };
        let list_fade = 1. - (list_offset.abs() / 400.).min(0.5);
        let beside = beside.map(|(place, at)| {
            div().absolute().top_0().bottom_0().w(px(self.sidebar_width - 1.)).left(px(at)).child(self.place_preview(&place, theme, window, cx))
        });
        // Laid out at full width and clipped by its tweened box, so rows never reflow mid slide.
        div()
            .when(bar, |el| el.on_scroll_wheel(cx.listener(|this, e: &gpui::ScrollWheelEvent, _, cx| this.sidebar_scroll(e, cx))))
            .w(px(self.sidebar_width - 1.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .pt(px(TITLEBAR_HEIGHT))
            .child(
                div().relative().flex_1().min_h_0().overflow_hidden().children(beside).child(
            div()
                .relative()
                .left(px(list_offset))
                .opacity(list_fade)
                .size_full()
                .flex()
                .flex_col()
            .child(header.when(waiting, |el| el.child(crate::ui::loader::mini_spinner(2., theme.text_faint, window))))
            .children(toggle)
            .children(self.web_rows(self.shown_place_key().as_deref(), true, theme, window, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(body),
            )
            ))
            .children(strip)
            .children(bar.then(|| self.place_bar(theme, list_offset, window, cx)))
            .child(self.footer(theme, cx))
    }

    /// The sidebar's head: the shown session's name when the tree names none (with one session, or in focus mode),
    /// the Focus chip that turns focus mode off, and the magnifier that opens the search; while it is up, its field
    /// in their place, Escape or × closing it. The place's spinner goes after it.
    fn sidebar_header(&self, theme: &Theme, listed: bool, focus: bool, cx: &mut Context<Self>) -> Div {
        let head = div().px(px(16.)).pt(px(8.)).pb(px(6.)).flex().items_center().gap(px(6.)).text_size(px(11.)).text_color(theme.text_faint);
        let hover_bg = theme.hover;
        if let Some(search) = &self.sidebar_search {
            let field = div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.))
                .text_color(theme.text)
                .capture_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, _, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close_sidebar_search(cx);
                    }
                }))
                .child(Input::new(&search.input));
            let close = div()
                .id("sidebar-search-close")
                .size(px(18.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover_bg))
                .tooltip(tip("Close the search", None))
                .child(gpui::svg().path("icons/close.svg").size(px(11.)).text_color(theme.text_faint))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_sidebar_search(cx)));
            return head.child(gpui::svg().path("icons/solar/magnifier.svg").size(px(12.)).flex_none().text_color(theme.text_faint)).child(field).child(close);
        }
        let chip = focus.then(|| {
            let accent = theme.accent;
            div()
                .id("sidebar-focus-chip")
                .flex_none()
                .h(px(18.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(3.))
                .rounded(px(9.))
                .bg(accent.opacity(0.14))
                .text_color(accent)
                .cursor_pointer()
                .hover(move |s| s.bg(accent.opacity(0.22)))
                .tooltip(tip("Show every session again", Some("ToggleFocusMode")))
                .child(gpui::svg().path("icons/solar/target.svg").size(px(10.)).text_color(accent))
                .child("Focus")
                .child(gpui::svg().path("icons/close.svg").size(px(9.)).text_color(accent))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_focus_mode(cx)))
        });
        let magnifier = icon_button(theme, "sidebar-search", "")
            .size(px(18.))
            .rounded(px(4.))
            .tooltip(tip("Filter the sidebar", Some("FilterSidebar")))
            .child(gpui::svg().path("icons/solar/magnifier.svg").size(px(12.)).text_color(theme.text_faint))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_sidebar_search(window, cx)));
        head
            // the tree names every session; with one, or in focus mode, its name heads the list
            .child(div().flex_1().min_w_0().truncate().when(!listed, |el| el.child(self.place_label(cx))))
            .children(chip)
            .when(is_dev_build(), |el| el.child(dev_badge(theme)))
            .child(magnifier)
    }


    /// Arc's space bar (the sessions bar setting): a button for each herdr session and machine, in the order dragged
    /// to, the shown one lit.
    /// `offset` is the workspace list's: the lit square under the buttons moves as far toward the next place as the list
    /// has moved toward it.
    fn place_bar(&self, theme: &Theme, offset: f32, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let hover_bg = theme.hover;
        let places = self.bar_places(cx);
        let shown_at = places.iter().position(|p| self.places_shown(p));
        // each button at its slot, springing there when the order or a drag's placeholder moves it
        let keys: Vec<String> = places.iter().map(sessions::Place::key).collect();
        let dragging = cx.has_active_drag();
        let (slots, moving) = self.place_motion.borrow_mut().layout(&keys, dragging, Instant::now());
        if moving {
            window.request_animation_frame();
        }
        let held = self.place_motion.borrow().drag.as_ref().filter(|_| dragging).map(|d| d.key.clone());
        // the slot it would land in, its mark faint there
        let placeholder = held.as_ref().and_then(|key| keys.iter().position(|k| k == key)).map(|i| {
            let place = &places[i];
            let (name, icon) = place.look(cx);
            let mark = sessions::place_mark(&name, icon.as_deref(), matches!(place, sessions::Place::Machine(_)), theme.text_muted);
            div()
                .absolute()
                .top_0()
                .left(px(slots[i].0))
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.wash(0.04))
                .child(div().opacity(0.4).child(mark))
        });
        let buttons = places.iter().enumerate().filter(|(i, _)| held.as_ref() != Some(&keys[*i])).map(|(i, place)| {
            let shown = self.places_shown(place);
            let (name, icon) = place.look(cx);
            let machine = matches!(place, sessions::Place::Machine(_));
            let mark = sessions::place_mark(&name, icon.as_deref(), machine, if shown { theme.accent } else { theme.text_faint });
            let dragged = sessions::DraggedPlace {
                index: i,
                key: keys[i].clone(),
                name: name.clone(),
                icon,
                machine,
                theme: theme.clone(),
                grab: Default::default(),
                motion: self.place_motion.clone(),
            };
            let (to, menu) = (place.clone(), place.clone());
            let (x, y) = slots[i];
            div()
                .id(("place", i))
                .tooltip(tip(name.clone(), None))
                .absolute()
                .left(px(x))
                .top(px(y))
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .text_color(if shown { theme.accent } else { theme.text_faint })
                .when(!shown, |el| el.hover(move |s| s.bg(hover_bg)))
                .child(mark)
                .on_drag(dragged, |d, grab, _, cx| {
                    d.motion.borrow_mut().drag = Some(sessions::PlaceDrag { key: d.key.clone(), slot: d.index, ghost: None });
                    cx.new(|_| sessions::DraggedPlace { grab, ..d.clone() })
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if !this.places_shown(&to) {
                        this.switch_to(to.clone(), cx)
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| this.open_place_menu(menu.clone(), e.position, cx)),
                )
        });
        let step = sessions::PLACE_STEP;
        if let Some((from, to, at)) = self.bar_glide.get() {
            let t = at.elapsed().as_secs_f32() / FOLD_DURATION.as_secs_f32();
            let x = if t >= 1. { to } else { from + (to - from) * ease_out(t) };
            self.places_scroll.set_offset(gpui::point(px(-x), px(0.)));
            if t >= 1. {
                self.bar_glide.set(None);
            } else {
                window.request_animation_frame();
            }
        }
        let lit = shown_at.map(|at| {
            let x = (at as f32 - offset / self.sidebar_width).clamp(0., (places.len() - 1) as f32) * step;
            // a moved square glides to the row's middle; one that stays put leaves the row where it was scrolled
            if x != self.lit_seen.get() {
                self.lit_seen.set(x);
                let width = f32::from(self.places_scroll.bounds().size.width);
                let to = (x + 15. - width / 2.).clamp(0., f32::from(self.places_scroll.max_offset().x));
                let from = -f32::from(self.places_scroll.offset().x);
                if (to - from).abs() > 0.5 {
                    self.bar_glide.set(Some((from, to, Instant::now())));
                }
            }
            // the square stays on its button as that springs to another slot
            let x = (x + slots[at].0 - at as f32 * step).clamp(0., (places.len() - 1) as f32 * step);
            div().absolute().top_0().left(px(x)).size(px(30.)).rounded(px(8.)).bg(theme.selected)
        });
        if self.bar_glide.get().is_some() {
            window.request_animation_frame(); // a glide set just now starts on the next frame
        }
        let bar_bounds = self.bar_bounds.clone();
        let motion = self.place_motion.clone();
        div()
            .relative()
            .flex_none()
            // a place dragged over the bar holds the slot under the pointer; let go there, it moves to it
            .on_drag_move(cx.listener(|this, e: &gpui::DragMoveEvent<sessions::DraggedPlace>, _, cx| {
                let slot = this.place_motion.borrow().slot_at(e.event.position.x, this.places.len());
                if let Some(drag) = this.place_motion.borrow_mut().drag.as_mut().filter(|d| d.slot != slot) {
                    drag.slot = slot;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this, d: &sessions::DraggedPlace, _, cx| {
                let slot = this.place_motion.borrow().drag.as_ref().map_or(d.index, |drag| drag.slot);
                this.move_place(d.index, slot, cx);
            }))
            .pt(px(2.))
            .pb(px(4.))
            .px(px(8.))
            .flex()
            .items_center()
            .child(
                gpui::canvas(move |bounds, _, _| bar_bounds.set(Some(bounds)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            // centred while they fit, scrolled sideways (faded at the edges) once they don't
            .child(
                div().flex_1().min_w_0().child(crate::ui::edge_fade::edge_faded_x(16., 
                    &self.places_scroll,
                    div()
                        .id("places")
                        .w_full()
                        .flex()
                        .overflow_x_scroll()
                        .track_scroll(&self.places_scroll)
                        // auto margins centre the row while it fits and fall to 0 once it scrolls
                        .child(
                            div()
                                .flex_none()
                                .mx_auto()
                                .relative()
                                .w(px(places.len() as f32 * step - (step - 30.)))
                                .h(px(30.))
                                .child(gpui::canvas(move |bounds, _, _| motion.borrow_mut().row = Some(bounds), |_, _, _, _| {}).absolute().size_full())
                                .children(lit)
                                .children(placeholder)
                                .children(buttons),
                        ),
                )),
            )
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

/// What the sidebar shows: the view toggle whenever the Agents view is on; the tree, or in the Agents view every
/// session's agents in its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    pub toggle: bool,
    pub tree: bool,
    pub agents_view: bool,
}

/// The sidebar's [`Layout`] for its saved view: with the Agents view off it is Workspaces, the view saved left as it
/// is for when the view comes back.
pub(super) fn sidebar_layout(view: SidebarView, show_agents: bool) -> Layout {
    let agents_view = show_agents && view == SidebarView::Agents;
    Layout { toggle: show_agents, tree: !agents_view, agents_view }
}

/// The window's left edge brings the hidden sidebar out floating: only with it hidden all the way (not mid-fold),
/// off a Settings page, and with the peek on in Settings.
pub(super) fn peeks(sidebar_open: bool, sidebar_w: f32, on_page: bool, peek_on: bool) -> bool {
    !sidebar_open && sidebar_w == 0. && !on_page && peek_on
}

/// Whether the pointer on the edge or the card (`on`) holds the floating sidebar out: never with the peek off.
pub(super) fn hover_shows(peek_on: bool, on: bool) -> bool {
    peek_on && on
}

#[cfg(test)]
mod tests {
    use super::{Layout, hover_shows, peeks, sidebar_layout};
    use crate::settings::SidebarView;

    #[test]
    fn the_agents_view_takes_the_tree_s_place() {
        let layout = |toggle, tree| Layout { toggle, tree, agents_view: !tree };
        for (view, show, want) in [
            (SidebarView::Workspaces, true, layout(true, true)),
            (SidebarView::Workspaces, false, layout(false, true)),
            (SidebarView::Agents, true, layout(true, false)),
            // the view off: Workspaces, whatever was saved
            (SidebarView::Agents, false, layout(false, true)),
        ] {
            assert_eq!(sidebar_layout(view, show), want, "{view:?} show_agents {show}");
        }
    }

    #[test]
    fn with_the_peek_off_a_hover_holds_nothing_out() {
        assert!(hover_shows(true, true));
        assert!(!hover_shows(true, false), "the pointer left");
        assert!(!hover_shows(false, true), "turned off in Settings");
    }

    #[test]
    fn the_edge_peeks_only_with_the_sidebar_hidden_and_the_peek_on() {
        assert!(peeks(false, 0., false, true));
        assert!(!peeks(false, 0., false, false), "turned off in Settings");
        assert!(!peeks(true, 240., false, true), "the sidebar is out");
        assert!(!peeks(false, 120., false, true), "still folding away");
        assert!(!peeks(false, 0., true, true), "on a Settings page");
    }
}
