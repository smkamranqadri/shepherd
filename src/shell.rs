//! The window: workspaces sidebar, the tab bar, and a GPUI terminal per pane of the shown tab, refreshed on
//! every herdr event. Look and titlebar handling follow Collie (Zeron style).
use crate::bindings::*;
use crate::commands::{Command, Direction};
use crate::herdr::{Client, EVENT_TYPES};
use crate::terminal::pane::{GridHold, PaneQueries};
use crate::ui::divider::{Throttle, drag_ratio};
use gpui_base::input::{Input, InputEvent, InputState};
use crate::model::{AgentSort, AgentStatus, PaneFrame, Snapshot, SplitFrame, attachable};
use crate::terminal::PaneTerminal;
use crate::settings::{SIDEBAR_MAX, SIDEBAR_MIN, SIDEBAR_WIDTH, SettingsGlobal, SidebarView, WindowFrame};
use crate::theme::Theme;
use gpui::{
    linear_color_stop, linear_gradient, AnimationExt, App, Bounds, ClickEvent, Context, Div, Entity, Focusable, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    ParentElement, Render, SharedString, Stateful, Styled, TitlebarOptions, Window, WindowBackgroundAppearance,
    Subscription, WindowControlArea, WindowOptions, canvas, div, point, prelude::*, px, relative, size,
};
use serde_json::json;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

mod attention;
mod discover;
mod divider;
mod floating;
mod motion;
mod pane_drag;
mod popups;
mod quick_open;
mod review;
mod right_panel;
mod kept_views;
mod session_views;
mod sessions;
mod sidebar;
mod sidebar_search;
mod sidebar_tree;
mod needs_you;
mod watchers;
mod sync;
mod file_tabs;
mod hints;
mod key_actions;
mod tab_bar;
mod tree_drag;
mod web;
pub(crate) mod widgets;
mod workspaces;
use motion::*;
use sync::sync_loop;
pub(crate) use widgets::{menu_card, modal, row, status_dot, status_icon, tip};
use widgets::*;
pub(crate) use tab_bar::agent_favicon;
pub(crate) use sessions::icon_mark;
pub use sidebar_tree::Goal;

const TITLEBAR_HEIGHT: f32 = 38.0;
/// The titlebar's left cluster: traffic lights, then the sidebar toggle at Zeron's `titlebar_cluster_start` (88).
/// Elsewhere there are no traffic lights, and the toggle starts the row.
const TOGGLE_LEFT: f32 = if cfg!(target_os = "macos") { 88.0 } else { 12.0 };
/// Whether the window's own buttons are drawn: off macOS, unless Settings turned them off (Windows clicks them as
/// its caption buttons, so they stay there).
fn window_buttons(cx: &App) -> bool {
    !cfg!(target_os = "macos") && (cfg!(windows) || SettingsGlobal::of(cx).settings.window_buttons)
}

/// What the titlebar's right end keeps clear for the window's buttons, while they are drawn.
fn window_controls_w(cx: &App) -> f32 {
    if window_buttons(cx) { 100.0 } else { 0.0 }
}
/// Centre line of the traffic lights (placed at y 14), measured on a 2x screen; the toggle centres on it, not on the titlebar.
const TRAFFIC_LIGHTS_CENTER: f32 = 21.25;
/// The Agents list's least height when dragged: one agent row and a bit.
const TITLEBAR_CLUSTER: f32 = TOGGLE_LEFT + 24.0 + 8.0;

pub struct Shell {
    snapshot: Option<Snapshot>,
    error: Option<String>,
    /// Since when the place shown has sent no snapshot of its own yet: the one shown, if any, is the last it sent.
    connecting: Option<Instant>,
    panes: HashMap<String, Entity<PaneTerminal>>,
    /// The panes shown last, most recent first: their views (and herdr sessions) outlive a tab switch.
    recent_panes: Vec<String>,
    /// The sessions not shown, as they were left: their tab and workspace, and their panes' grids, parked.
    views: session_views::SessionViews<Entity<PaneTerminal>>,
    /// The panes a view brought back, with their terminals: checked against the session's first snapshot of its own.
    parked_check: Option<Vec<(String, String)>>,
    /// Every session's keyboard pane per tab, kept for the next run (`kept_views`).
    keyboard: session_views::KeyboardMemory,
    /// The tab whose keyboard pane was last given back: another one shown gives its own back.
    keyboard_tab: Option<String>,
    /// The keyboard pane to give back once its tab and pane show.
    restore: Option<session_views::Restore>,
    /// `session-views.json`'s reads and writes.
    kept_views: kept_views::KeptState,
    own_pane: Option<String>,
    /// herdr's focused pane we last moved keyboard focus to; refocusing on every render would undo clicks.
    focused: Option<String>,
    /// herdr's popup just closed: the keyboard goes back to herdr's focused pane unless a press outside it landed
    /// on a pane, which keeps it.
    popup_closed: bool,
    /// The last key or click in Shepherd: herdr pane focus changes soon after one are Shepherd's own.
    touched: Instant,
    /// The tab Shepherd shows. Local on purpose: herdr's focus is shared with its TUI clients.
    selected_tab: Option<String>,
    /// Titlebar drag: armed by a press in the strip, consumed by the first move (Collie's `drag_strip`).
    should_move: Rc<Cell<bool>>,
    client: Client,
    rt: tokio::runtime::Handle,
    /// herdr's plugin actions as (id, title), for the command panel; read again whenever it opens.
    plugin_actions: Vec<(String, String)>,
    /// The pane grid's bounds from the last paint, to turn a divider drag into a ratio.
    grid_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    divider: Option<DividerDrag>,
    /// A released divider's ratio, shown until herdr's snapshot catches up (no jump back to the old layout).
    settling: Option<(Vec<bool>, f32)>,
    /// The tab or workspace label being edited in place.
    renaming: Option<Rename>,
    folded: std::collections::HashSet<String>,
    /// A workspace's right-click menu and where it opened.
    ws_menu: Option<crate::ui::Popup<(String, gpui::Point<Pixels>)>>,
    /// Groups mid fold or unfold, by workspace id.
    fold_motion: HashMap<String, FoldMotion>,
    /// Resize seams' hover: (hovered, flip count), keyed by the strip id.
    seam_hover: HashMap<SharedString, (bool, u64)>,
    /// The workspace shown last, so closing its tab stays in it.
    shown_workspace: Option<String>,
    /// Saved in the settings when a drag ends.
    sidebar_width: f32,
    sidebar_open: bool,
    /// The sidebar's width tween while it shows or hides.
    sidebar_motion: Option<FoldMotion>,
    /// With the sidebar closed, its floating copy's slide in (to 1) or out (to 0).
    sidebar_peek: Option<FoldMotion>,
    /// The pointer is on the window's left edge or on the floating sidebar.
    peek_hovered: bool,
    /// The sidebar's edge is being dragged.
    sidebar_drag: bool,
    /// The drag hit a width limit (-1 min, +1 max) at this instant: the edge nudges past it and springs back.
    sidebar_edge: Option<(f32, Instant)>,
    /// The right panel (Files, Changes, Search, PR) shows; saved in the settings.
    panel_open: bool,
    /// Saved in the settings when a drag ends.
    panel_width: f32,
    /// The right panel's edge is being dragged.
    panel_drag: bool,
    /// The right panel's tab as picked; `shown_panel_tab` is the one it shows.
    panel_tab: crate::settings::PanelTab,
    /// The shown pane's repo has a remote, so the right panel has a PR tab; as of the last render.
    panel_has_pr: bool,
    /// The right panel's Files, Changes and Search tabs.
    side_panel: Entity<crate::ui::side_panel::SidePanel>,
    /// Each workspace's file tabs (`roots::root_key`), drawn after its herdr tabs (`file_tabs`).
    file_tabs: HashMap<String, crate::files::tabs::WorkspaceTabs>,
    /// The file tabs' views, the most recently shown first; a tab without one gets it when next shown.
    tab_views: Vec<(crate::files::tabs::TabId, file_tabs::TabView)>,
    /// Views of tabs that closed, retired on the next render (which has the window their focus is in).
    views_gone: Vec<file_tabs::TabView>,
    /// Each diff tab's change, by its tab: what its view is made from when (again) shown.
    diff_specs: HashMap<crate::files::tabs::TabId, crate::ui::diff_view::DiffSpec>,
    /// The file tab drawn at the last render: when another takes its place, its view's image leaves gpui's cache.
    view_drawn: Option<crate::files::tabs::TabId>,
    /// The last file tab id handed out.
    next_tab_id: u64,
    /// Files' real paths, as file tabs are kept by (`file_tabs::real_path`), for the run.
    real_paths: HashMap<std::path::PathBuf, std::path::PathBuf>,
    /// Numbers each file open: a preview whose real path came back after a later open began is dropped, so
    /// previews land in the order they were asked for.
    open_seq: u64,
    /// Each workspace's root as last seen by its file tabs: when it moves, clean tabs outside it close.
    tab_roots: HashMap<String, String>,
    /// The pinned file tabs kept for the next run.
    kept_tabs: file_tabs::KeptTabs,
    /// Where the cursor and scroll were in tabs whose views went to save memory, for the views they get next.
    tab_places: HashMap<crate::files::tabs::TabId, crate::ui::editor::Place>,
    /// A Save / Don't Save / Cancel prompt is up: no second one over it.
    asking: bool,
    /// Don't Save was chosen on quitting or closing: the window goes without asking again.
    leaving: bool,
    /// Workspaces (`roots::root_key`) whose root was guessed this run, true while the guess runs: one guess per
    /// workspace and run, so one that found nothing (home, `/`) leaves "No folder" up rather than ask git again.
    root_guesses: std::collections::BTreeMap<String, bool>,
    /// The panel's root row's `…` menu, at the button.
    root_menu: Option<crate::ui::Popup<gpui::Point<Pixels>>>,
    /// The Files header's View menu (hidden and ignored files), at the button.
    files_view_menu: Option<crate::ui::Popup<gpui::Point<Pixels>>>,
    /// The shown branch (its review's key), while its checks run: asked again every `REVIEW_POLL`.
    review_poll: Option<String>,
    /// The shown branch's key at the last render and whether its checks ran: when they stop, CI finished.
    review_last: Option<(String, bool)>,
    /// The CLI calls the review panel's buttons started and that have not finished, by `Check::command`.
    review_acting: std::collections::HashSet<String>,
    /// The pipeline (or workflow) the review panel pages to, picked for a branch's review (its key); else it opens
    /// on `default_page`.
    review_page: Option<(String, String)>,
    /// A two-finger swipe over the review panel's checks in progress: whether it goes across (decided by its first
    /// clear movement) and how far the fingers have moved.
    review_swipe: Option<(Option<bool>, f32)>,
    /// The checks springing home from this offset since then, after a page change, beside the page they left.
    review_spring: Option<(f32, Instant, Option<String>)>,
    /// The job whose log the review panel shows under it, for a branch's review (its key).
    review_log: Option<(String, String)>,
    /// Asks before the review panel's Merge.
    merge_confirm: Option<crate::ui::Popup<review::MergeAsk>>,
    /// The review panel shows the whole description, not its first lines.
    review_expanded: bool,
    /// The description's whole height at the last paint: whether it has more than its folded lines.
    review_body_h: Rc<Cell<f32>>,
    agent_sort: AgentSort,
    /// The sidebar's `…` menu, open where it was clicked.
    view_menu: Option<crate::ui::Popup<()>>,
    /// The done agent's pane herdr was last told you looked at, so it is told once, not every frame.
    marked_seen: Option<String>,
    /// An agent's pane to give the keyboard once it is shown, over herdr's focused pane of its tab.
    focus_next: Option<String>,
    /// The `…` menu's Show submenu is out: the pointer went over its row.
    show_submenu: bool,
    /// Where the Show submenu was painted: a press there is inside the `…` menu, though outside its card.
    show_submenu_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The Agents section's `…` menu, open under its button.
    agents_menu: Option<crate::ui::Popup<gpui::Point<Pixels>>>,
    /// Worktrees herdr is removing: their rows show a spinner until they leave the snapshot.
    removing: std::collections::HashSet<String>,
    /// Remove a worktree? The workspace, its label, and whether herdr found changes (then removal is forced).
    remove_confirm: Option<crate::ui::Popup<(String, String, bool)>>,
    /// The New worktree dialog, open over the window.
    worktree_dialog: Option<crate::ui::Popup<WorktreeDialog>>,
    /// Worktrees herdr is still creating, as (workspace the dialog was opened on, branch): herdr answers
    /// `worktree.create` only after git and plugin hooks finish, so the sidebar shows a placeholder until then.
    pending_worktrees: Vec<(String, String)>,
    /// A tab's right-click menu: the tab and where it was clicked.
    tab_menu: Option<crate::ui::Popup<(String, gpui::Point<Pixels>)>>,
    /// A pane's right-click menu: the pane and where it was clicked.
    pane_menu: Option<crate::ui::Popup<(String, gpui::Point<Pixels>)>>,
    /// The status bar's spend popup, opened at this point (its bottom-left).
    spent_menu: Option<crate::ui::Popup<gpui::Point<Pixels>>>,
    /// The spend popup lists the most tokens first.
    spent_by_tokens: bool,
    /// The status bar's limits popup, at its anchor.
    usage_menu: Option<crate::ui::Popup<gpui::Point<Pixels>>>,
    /// The status bar's right-click menu of its labels, where it was clicked.
    statusbar_menu: Option<crate::ui::Popup<gpui::Point<Pixels>>>,
    /// The sidebar's Agents view's scroll.
    agents_view_scroll: gpui::ScrollHandle,
    tab_bar: tab_bar::TabBar,
    /// Holds keyboard focus when no pane can (renames ended, a tab of only Shepherd's own pane), so the app's
    /// actions still dispatch.
    focus: gpui::FocusHandle,
    /// Holds keyboard focus while a delete or remove confirm is up, for its Enter and Escape.
    confirm_focus: gpui::FocusHandle,
    /// The Settings page, shown in place of the sidebar and the panes while open.
    settings: Option<(Entity<crate::ui::settings::SettingsView>, Subscription)>,
    /// A pane being dragged by its grip.
    pane_drop: Option<pane_drag::PaneDrop>,
    pane_spring_back: Option<pane_drag::SpringBack>,
    /// The command panel (cmd-k), open over the window.
    command_panel: Option<crate::ui::Popup<(Entity<crate::ui::command_panel::Panel>, Subscription)>>,
    /// What was told of each agent in every session, so a change is told once.
    attention: attention::Attention,
    /// The count on the Dock icon now.
    badge: usize,
    /// The window's frame not saved yet, and the save it waits for: half a second after the last move, closing or quitting.
    frame: Option<WindowFrame>,
    frame_save: gpui::Task<()>,
    /// Lets the panes' grids go 120 ms after the window last changed size (`GridHold` while it does).
    resize_hold: gpui::Task<()>,
    /// Since when each agent has had its status: how long the waiting ones have waited.
    since: crate::model::Since,
    /// The loop that keeps the window in step with herdr; replacing it (a switch of session) stops the last one.
    sync: gpui::Task<()>,
    /// Wakes the sync loop for a fresh snapshot without an event: a command named a tab or pane herdr no longer has.
    resync: std::sync::Arc<tokio::sync::Notify>,
    /// herdr's popup over the panes, and the client shell that hears of it.
    floating: Option<floating::Floating>,
    popup_watch: gpui::Task<()>,
    /// The client shell's surface and the grid it was last sized to.
    popup_surface: Option<(std::sync::Arc<crate::herdr::popup::Surface>, ((u16, u16), (u32, u32)))>,
    /// The tab last given herdr's focus (`follow_shown_tab`).
    focused_tab: Option<String>,
    /// The tabs Shepherd gave herdr's focus lately, and when: their echoes in later snapshots are not moves.
    own_focus: Vec<(String, Instant)>,
    /// herdr's Kitty images per pane, as last sent, for panes made after it.
    pane_images: crate::herdr::graphics::PaneImages,
    /// Images no pane shows any more: freed from the window's atlas at the next render (a hidden pane never renders).
    retired_images: Vec<std::sync::Arc<gpui::RenderImage>>,
    /// herdr's `[[keys.command]]` ids for `command.invoke`, with their keys, from the client shell.
    herdr_commands: Vec<(String, Vec<String>)>,
    /// Panes `[[keys.command]]` of `type = "pane"` opened, by the command's keys: pressed again in their tab, the
    /// command brings its pane back instead of stacking another (herdr opens one per press).
    command_panes: Vec<(Vec<String>, String)>,
    /// herdr's sessions and machines, in the sidebar's tree order.
    places: Vec<sessions::Place>,
    /// A two-finger swipe over the sidebar in progress (the sessions bar).
    swipe: Option<sessions::Swipe>,
    /// The workspace list springing home from this offset since then, after a swipe, beside the place it left.
    slide: Option<(f32, Instant, Option<sessions::Place>)>,
    /// The bar's row of places, when it overflows.
    places_scroll: gpui::ScrollHandle,
    /// Where the bar's lit square was last kept in view: it is scrolled to again only when it moves.
    lit_seen: Cell<f32>,
    /// The bar's row gliding from one scroll offset to another since then, to keep the lit square in its middle.
    bar_glide: Cell<Option<(f32, f32, Instant)>>,
    /// Where the bar was painted: a swipe there scrolls the bar, not to the next place.
    bar_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The bar's buttons sliding to their slots, and the one being dragged.
    place_motion: Rc<std::cell::RefCell<sessions::PlaceMotion>>,
    /// Web integrations' pages, and the one shown in place of the tabs and panes.
    pages: web::Pages,
    /// A place's menu in the sidebar's tree, and where it was clicked.
    place_menu: Option<crate::ui::Popup<(sessions::Place, gpui::Point<Pixels>)>>,
    /// The Edit dialog of a place's name and icon.
    place_edit: Option<crate::ui::Popup<sessions::PlaceEdit>>,
    /// The places not shown, each polled in the background for the sidebar's tree, with its last snapshot.
    watchers: watchers::Watchers,
    /// herdr's sessions folder is watched (`start_session_watch`), or cannot be: no more tries. False while the
    /// folder is not there yet, tried again as each read of herdr's list lands.
    session_watch_settled: bool,
    /// Each live watcher's feed of herdr's events, with the `feed_gen` it was opened at: dropping one hangs it up.
    feeds: HashMap<watchers::SessionKey, (u64, gpui::Task<()>)>,
    /// The tree's goal after a switch and its machine menu.
    tree: sidebar_tree::TreeState,
    /// The sidebar's search field, while it is up (FilterSidebar).
    sidebar_search: Option<sidebar_search::SidebarSearch>,
    /// The New session dialog, open over the window.
    session_dialog: Option<crate::ui::Popup<sessions::SessionDialog>>,
    /// Delete a session, or forget a machine? Asked first.
    session_delete: Option<crate::ui::Popup<sessions::Place>>,
    /// The saved machine shown, by its label; `None` for a session on this Mac.
    machine_label: Option<String>,
    /// And by its id, which keys it (`machine:<id>`) before herdr's list names it.
    machine_id: Option<String>,
    /// The shortcut hints held up by Ctrl (Cmd on macOS) or Ctrl+Shift (Ctrl on macOS).
    hints: hints::Hints,
    /// The wake-up when the hint modifier has been held long enough.
    hints_wait: Option<gpui::Task<()>>,
    /// Every key down in this window puts the hints away, a key bound to an action too (dispatched before any
    /// key-down listener hears it).
    _hint_keys: Subscription,
    /// A modifier tapped on its own as the prefix (`tap`): watched from the window's modifier changes.
    prefix_tap: crate::tap::Tap,
}

/// New worktree: the repo's workspace and the branch name being typed.
struct WorktreeDialog {
    workspace_id: String,
    /// Set when the dialog names this pane instead of a new worktree's branch.
    rename_pane: Option<String>,
    input: Entity<InputState>,
    _events: Subscription,
}

#[derive(Clone, PartialEq)]
enum RenameTarget {
    Tab(String),
    Workspace(String),
}

struct Rename {
    target: RenameTarget,
    input: Entity<InputState>,
    _events: Subscription,
}

struct DividerDrag {
    tab_id: String,
    path: Vec<bool>,
    vertical: bool,
    start_px: f32,
    len_px: f32,
    ratio: f32,
    throttle: Throttle,
}

impl Shell {
    pub fn open(rt: tokio::runtime::Handle, cx: &mut gpui::App) {
        let (display_id, window_bounds) = saved_window(cx).map_or_else(
            || (None, gpui::WindowBounds::Windowed(Bounds::centered(None, size(px(1180.), px(760.)), cx))),
            |(display, bounds)| (Some(display), bounds),
        );
        let options = WindowOptions {
            window_bounds: Some(window_bounds),
            display_id,
            window_min_size: Some(size(px(820.), px(560.))),
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(14.))),
            }),
            app_owns_titlebar_drag: true,
            window_background: window_background(SettingsGlobal::of(cx).settings.translucent_window),
            app_id: Some(if is_dev_build() { "dev.freethinkel.shepherd-dev" } else { "dev.freethinkel.shepherd" }.into()),
            // off macOS the titlebar is Shepherd's own, buttons and all, not the desktop's bar over it
            window_decorations: (!cfg!(target_os = "macos")).then_some(gpui::WindowDecorations::Client),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            cx.set_global(SettingsGlobal::of(cx).settings.theme(window.appearance()));
            window
                .observe_window_appearance(|window, cx| {
                    cx.set_global(SettingsGlobal::of(cx).settings.theme(window.appearance()));
                    cx.refresh_windows();
                })
                .detach();
            cx.new(|cx| {
                cx.observe_window_activation(window, |s: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                    reapply_traffic_lights(window);
                    // in the background Shepherd leaves panes' size to herdr's other clients; back, it takes it again
                    let active = window.is_window_active();
                    if active {
                        s.touched = Instant::now();
                    }
                    s.hint_activation(active, window, cx);
                    s.watchers.set_active(active, Instant::now());
                    cx.set_global(crate::terminal::pane::Yielded(!active));
                    if active {
                        s.refresh_places(cx); // sessions made or deleted elsewhere
                        s.push_host_theme(cx);
                    }
                    cx.notify(); // the shown tab's agents count as seen once the window is active
                })
                .detach();
                cx.observe_global::<Theme>(|s: &mut Shell, cx| s.push_host_theme(cx)).detach();
                cx.observe_global::<crate::terminal::pane::Yielded>(|s: &mut Shell, cx| {
                    for pane in s.panes.values() {
                        pane.update(cx, |p, cx| p.follow_yield(cx));
                    }
                })
                .detach();
                // The Mac left alone with Shepherd still in front (you went on to herdr's TUI over ssh, from a phone
                // say): the client used last sizes the panes, so Shepherd lets go; a key or click takes them back.
                cx.spawn(async move |this, cx| loop {
                    cx.background_executor().timer(Duration::from_secs(5)).await;
                    let idle = mac_idle() > IDLE_YIELD || screen_locked();
                    let alive = this.update(cx, |_, cx| {
                        if idle && !cx.try_global::<crate::terminal::pane::Yielded>().is_some_and(|y| y.0) {
                            cx.set_global(crate::terminal::pane::Yielded(true));
                        }
                    });
                    if alive.is_err() {
                        return;
                    }
                })
                .detach();
                // A click on a notification brings its agent's tab up, in its own session; Approve answers the agent
                // and leaves Shepherd where it is. Pane ids repeat across sessions: the tag names the session too.
                let (this, handle) = (cx.entity().downgrade(), window.window_handle());
                cx.on_system_notification_response(move |response, cx| {
                    let approve = response.action_id.as_deref() == Some(crate::notify::APPROVE);
                    let _ = this.update(cx, |s, cx| {
                        let shown = s.shown_key();
                        use attention::Route;
                        match attention::route(&response.tag, shown.as_ref(), |k| s.watchers.get(k).is_some()) {
                            // only the prompt it showed, still waiting: a late press must not type Enter into an agent
                            // at work, nor into a prompt you have not seen
                            Route::Shown(target) if approve => {
                                if s.snapshot.as_ref().is_some_and(|snap| attention::approvable(snap, &target)) {
                                    s.send(Command::Approve { pane_id: target.pane }, cx, |_, _, _| {});
                                } else {
                                    crate::ui::toast::error(cx, "That prompt has changed");
                                }
                            }
                            // an Approve whose session the tag does not name answers nothing
                            Route::Unkeyed(_) if approve => {}
                            Route::Shown(target) | Route::Unkeyed(target) => {
                                let pane = s.snapshot.as_ref().and_then(|snap| snap.panes.iter().find(|p| p.pane_id == target.pane));
                                if let Some((tab, pane)) = pane.map(|p| (p.tab_id.clone(), p.pane_id.clone())) {
                                    s.show_agent(tab, pane, cx);
                                }
                            }
                            Route::Other(key, target) if approve => s.approve_elsewhere(key, target, cx),
                            Route::Other(key, target) => s.open_elsewhere(key, target, cx),
                            Route::Gone if !approve => crate::ui::toast::error(cx, "That session is no longer there"),
                            Route::Gone => {}
                        }
                    });
                    if !approve {
                        // gpui's app-level activate does nothing on Linux and the BSDs: the window requests activation
                        // itself (xdg-activation on Wayland, _NET_ACTIVE_WINDOW on X11), which the compositor may grant
                        // or answer by marking the window for attention
                        if cfg!(any(target_os = "macos", windows)) {
                            cx.activate(true);
                        } else {
                            let _ = handle.update(cx, |_, window, _| window.activate_window());
                        }
                    }
                });
                cx.observe_window_bounds(window, |s: &mut Shell, window, cx| {
                    let last = s.frame.clone().or_else(|| SettingsGlobal::of(cx).settings.window.clone());
                    s.frame = window_frame(window, last.as_ref(), cx);
                    s.frame_save = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(500)).await;
                        let _ = this.update(cx, |s, cx| s.save_frame(cx));
                    });
                    // a live resize reflows the panes as a divider drag does, not one SIGWINCH per frame
                    cx.set_global(GridHold(true));
                    s.resize_hold = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(120)).await;
                        let _ = this.update(cx, |s, cx| {
                            // a drag or the sidebar's fold owns the hold now
                            if s.divider.is_none() && !s.sidebar_drag && !s.panel_drag && s.sidebar_motion.is_none() {
                                cx.set_global(GridHold(false));
                                cx.refresh_windows();
                            }
                        });
                    });
                })
                .detach();
                cx.on_app_quit(|s: &mut Shell, cx| {
                    s.save_frame(cx);
                    let kept = s.flush_kept_tabs(cx);
                    let views = s.flush_kept_views(cx);
                    // a quit that did not ask (the Dock's, logging out): unsaved tabs are saved as far as the quit
                    // waits, writes under way finished
                    // ponytail: E4's drafts keep what does not make it in time
                    let edits = s.flush_editors(cx);
                    // the updater's relaunch, once the quit goes through
                    file_tabs::AfterQuit::run(cx);
                    async move {
                        for write in [kept, views].into_iter().flatten() {
                            write.await;
                        }
                        futures::future::join_all(edits).await;
                    }
                })
                .detach();
                let this = cx.entity().downgrade();
                // unsaved file tabs ask Save All / Don't Save / Cancel first, and close the window after
                window.on_window_should_close(cx, move |window, cx| {
                    this.update(cx, |s, cx| {
                        s.save_frame(cx);
                        !s.ask_unsaved(file_tabs::Exit::Close, window, cx)
                    })
                    .unwrap_or(true)
                });
                let tab = SettingsGlobal::of(cx).settings.panel_tab;
                let side_panel = cx.new(|cx| crate::ui::side_panel::SidePanel::new(rt.clone(), tab, cx));
                // a search looks in the file tabs' unsaved buffers; asked from the panel's own timer, never while the
                // Shell is being updated
                let shell = cx.entity().downgrade();
                let unsaved: crate::ui::side_panel::UnsavedBuffers = Box::new(move |cx: &App| shell.upgrade().map(|s| s.read(cx).unsaved_buffers(cx)).unwrap_or_default());
                side_panel.update(cx, |p, _| p.set_unsaved_buffers(unsaved));
                cx.subscribe_in(&side_panel, window, |s: &mut Shell, _, event: &crate::ui::side_panel::SidePanelEvent, window, cx| match event {
                    crate::ui::side_panel::SidePanelEvent::Dismiss => s.focus_pane(window, cx),
                    crate::ui::side_panel::SidePanelEvent::ChooseRoot => s.choose_root(window, cx),
                    crate::ui::side_panel::SidePanelEvent::UsePaneFolder => s.use_pane_folder(window, cx),
                    crate::ui::side_panel::SidePanelEvent::OpenFile { path, line, preview } => s.open_file(path.clone(), *line, *preview, window, cx),
                    crate::ui::side_panel::SidePanelEvent::OpenInEditor(path) => s.open_in_editor_from_panel(path.clone(), window, cx),
                    // its diff in a tab; a conflict's file, with its markers
                    crate::ui::side_panel::SidePanelEvent::OpenChange { path, change, preview } => s.open_change(path.clone(), change.clone(), *preview, window, cx),
                    // a search match: its file at the match, selected
                    crate::ui::side_panel::SidePanelEvent::OpenMatch { path, line, cols, preview } => s.open_match(path.clone(), *line, cols.clone(), *preview, window, cx),
                    // the badge is in the Shell's tab strip
                    crate::ui::side_panel::SidePanelEvent::ChangesCounted => cx.notify(),
                    crate::ui::side_panel::SidePanelEvent::FilesRemembered => s.save_kept_views(cx),
                    crate::ui::side_panel::SidePanelEvent::ChangesPolled => {
                        s.reload_shown_diff(true, cx);
                        // the editors' gutters too (W1's watch on .git calls `files_git_changed` as well)
                        s.files_git_changed(cx);
                    }
                })
                .detach();
                let (this, handle) = (cx.entity().downgrade(), window.window_handle());
                let hint_keys = cx.intercept_keystrokes(move |_, window, cx| {
                    if window.window_handle() == handle {
                        let _ = this.update(cx, |s, cx| {
                            s.hint_cancel(cx);
                            // a key with the modifier held: a chord, no tap
                            s.prefix_tap_event(crate::tap::Event::Key, None, cx);
                        });
                    }
                });
                let mut shell = Shell {
                    snapshot: None,
                    error: None,
                    connecting: Some(Instant::now()),
                    panes: HashMap::new(),
                    recent_panes: Vec::new(),
                    views: Default::default(),
                    parked_check: None,
                    keyboard: Default::default(),
                    keyboard_tab: None,
                    restore: None,
                    kept_views: Default::default(),
                    own_pane: std::env::var("HERDR_PANE_ID").ok(),
                    focused: None,
                    popup_closed: false,
                    touched: Instant::now(),
                    selected_tab: std::env::var("SHEPHERD_TAB").ok(),
                    should_move: Rc::new(Cell::new(false)),
                    client: SettingsGlobal::of(cx)
                        .settings
                        .herdr_socket
                        .clone()
                        .filter(|p| !p.trim().is_empty())
                        .map_or_else(Client::local, |p| Client::at(p.into())),
                    rt: rt.clone(),
                    plugin_actions: Vec::new(),
                    grid_bounds: Rc::new(Cell::new(None)),
                    divider: None,
                    settling: None,
                    renaming: None,
                    folded: SettingsGlobal::of(cx).settings.folded.iter().cloned().collect(),
                    fold_motion: Default::default(),
                    seam_hover: Default::default(),
                    ws_menu: None,
                    shown_workspace: None,
                    sidebar_width: SettingsGlobal::of(cx).settings.sidebar_width,
                    sidebar_open: SettingsGlobal::of(cx).settings.sidebar_open,
                    sidebar_motion: None,
                    sidebar_peek: None,
                    peek_hovered: false,
                    sidebar_drag: false,
                    sidebar_edge: None,
                    panel_open: SettingsGlobal::of(cx).settings.panel_open,
                    panel_width: SettingsGlobal::of(cx).settings.panel_width,
                    panel_drag: false,
                    panel_tab: SettingsGlobal::of(cx).settings.panel_tab,
                    panel_has_pr: false,
                    side_panel: side_panel.clone(),
                    file_tabs: HashMap::new(),
                    tab_views: Vec::new(),
                    views_gone: Vec::new(),
                    diff_specs: HashMap::new(),
                    view_drawn: None,
                    next_tab_id: 0,
                    real_paths: HashMap::new(),
                    open_seq: 0,
                    tab_roots: HashMap::new(),
                    kept_tabs: Default::default(),
                    tab_places: HashMap::new(),
                    asking: false,
                    leaving: false,
                    root_guesses: Default::default(),
                    root_menu: None,
                    files_view_menu: None,
                    review_poll: None,
                    review_last: None,
                    review_acting: Default::default(),
                    merge_confirm: None,
                    review_log: None,
                    review_page: None,
                    review_swipe: None,
                    swipe: None,
                    slide: None,
                    places_scroll: gpui::ScrollHandle::new(),
                    bar_bounds: Default::default(),
                    place_motion: Default::default(),
                    lit_seen: Cell::new(-1.),
                    bar_glide: Cell::new(None),
                    pages: Default::default(),
                    review_spring: None,
                    review_expanded: false,
                    review_body_h: Default::default(),
                    agent_sort: SettingsGlobal::of(cx).settings.agent_sort,
                    view_menu: None,
                    focus_next: None,
                    marked_seen: None,
                    show_submenu: false,
                    show_submenu_bounds: Default::default(),
                    agents_menu: None,
                    pane_menu: None,
                    spent_menu: None,
                    usage_menu: None,
                    statusbar_menu: None,
                    spent_by_tokens: false,
                    tab_menu: None,
                    worktree_dialog: None,
                    removing: Default::default(),
                    remove_confirm: None,
                    pending_worktrees: Vec::new(),
                    agents_view_scroll: gpui::ScrollHandle::new(),
                    tab_bar: Default::default(),
                    focus: cx.focus_handle(),
                    confirm_focus: cx.focus_handle(),
                    settings: None,
                    pane_drop: None,
                    pane_spring_back: None,
                    command_panel: None,
                    attention: Default::default(),
                    badge: 0,
                    frame: None,
                    frame_save: gpui::Task::ready(()),
                    resize_hold: gpui::Task::ready(()),
                    since: Default::default(),
                    sync: gpui::Task::ready(()),
                    resync: Default::default(),
                    floating: None,
                    popup_watch: gpui::Task::ready(()),
                    popup_surface: None,
                    focused_tab: None,
                    own_focus: Vec::new(),
                    pane_images: Default::default(),
                    retired_images: Vec::new(),
                    herdr_commands: Vec::new(),
                    command_panes: Vec::new(),
                    places: Vec::new(),
                    place_menu: None,
                    place_edit: None,
                    watchers: watchers::Watchers::new(),
                    session_watch_settled: false,
                    feeds: HashMap::new(),
                    tree: Default::default(),
                    sidebar_search: None,
                    session_dialog: None,
                    session_delete: None,
                    machine_label: None,
                    machine_id: None,
                    hints: Default::default(),
                    hints_wait: None,
                    _hint_keys: hint_keys,
                    prefix_tap: Default::default(),
                };
                shell.resume_machine(cx);
                shell.connect(cx);
                shell.refresh_places(cx);
                shell.start_watching(cx);
                shell.start_session_watch(cx);
                shell.refresh_plugin_actions(cx);
                shell.load_kept_tabs(cx);
                shell.load_kept_views(cx);
                // Waiting times move on between snapshots.
                // ponytail: a minute from launch, not from each wait, so a label can lag by up to a minute.
                cx.spawn(async move |this, cx| loop {
                    cx.background_executor().timer(Duration::from_secs(60)).await;
                    let alive = this.update(cx, |s, cx| {
                        let elsewhere = s.watchers.snapshots().any(|snap| sidebar_tree::waiting(snap) > 0);
                        if elsewhere || s.since.values().any(|(status, _)| crate::notify::wants_attention(*status)) {
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        return; // window closed
                    }
                })
                .detach();
                cx.spawn(async move |this, cx| loop {
                    cx.background_executor().timer(review::review_poll(crate::power::on_battery())).await;
                    if this.update(cx, |s, cx| s.poll_review(cx)).is_err() {
                        return; // window closed
                    }
                })
                .detach();
                shell
            })
        })
        .expect("open window");
    }

    /// Shows Settings in the active Shepherd window (or the first one), or hides it if it is shown there. For
    /// callers outside a window (the menu with no window focused, launch); deferred because a window busy
    /// dispatching cannot be updated.
    pub fn toggle_settings(cx: &mut App) {
        cx.defer(|cx| {
            let active = cx.active_window().and_then(|w| w.downcast::<Shell>());
            let Some(window) = active.or_else(|| cx.windows().into_iter().find_map(|w| w.downcast::<Shell>())) else { return };
            let _ = window.update(cx, |shell, window, cx| shell.toggle_settings_here(window, cx));
        });
    }

    /// Follows herdr over `self.client` from scratch: its snapshot and the subscription that refreshes it.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let (client, rt, resync) = (self.client.clone(), self.rt.clone(), self.resync.clone());
        if crate::mock::dir().is_some() {
            // the fixture: its one snapshot (sync_loop), no popups, no pane queries
            self.sync = cx.spawn(async move |this, cx| sync_loop(this, rt, client, resync, cx).await);
            self.floating = None;
            self.popup_watch = gpui::Task::ready(());
            return;
        }
        cx.set_global(PaneQueries(client.clone(), rt.clone()));
        self.sync = cx.spawn(async move |this, cx| sync_loop(this, rt, client, resync, cx).await);
        self.floating = None;
        // the last session's popup watch stops with its loop, which never gets to clear its surface: done here,
        // before keys could go through it to the same pane id in the last session
        self.popup_surface = None;
        if cx.has_global::<crate::terminal::pane::TypingShell>() {
            cx.remove_global::<crate::terminal::pane::TypingShell>();
        }
        self.popup_watch = cx.spawn(async move |this, cx| floating::popup_loop(this, cx).await);
    }

    fn toggle_settings_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return self.close_settings(cx);
        }
        let view = cx.new(|cx| crate::ui::settings::SettingsView::new(window, cx));
        let close = cx.subscribe(&view, |this, _, _: &crate::ui::settings::Close, cx| this.close_settings(cx));
        window.focus(&view.focus_handle(cx), cx);
        self.settings = Some((view, close));
        cx.notify();
    }

    /// Settings on its Usage page, opened if it is not up: the limits popup's way there.
    pub(crate) fn open_usage_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_none() {
            self.toggle_settings_here(window, cx);
        }
        if let Some((view, _)) = &self.settings {
            view.update(cx, |v, cx| v.show_usage(cx));
        }
    }

    /// Settings on its Shortcuts page, recording `action` a new chord: the shortcuts panel's way there.
    fn open_shortcut_settings(&mut self, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_none() {
            self.toggle_settings_here(window, cx);
        }
        if let Some((view, _)) = &self.settings {
            window.focus(&view.focus_handle(cx), cx);
            view.update(cx, |v, cx| v.show_shortcut(action, window, cx));
        }
    }

    /// cmd-k (commands) or the agents key: opens that panel over the window, or closes it when it is open.
    pub fn toggle_command_panel(&mut self, kind: crate::ui::command_panel::Kind, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::command_panel::{Kind, PanelEvent, Run};
        if self.command_panel.as_ref().is_some_and(|p| p.is_open() && p.value.0.read(cx).kind == kind) {
            self.close_command_panel(cx);
            return;
        }
        // the shown pane's repo, for the branches panel
        let dir = crate::ui::statusbar::pane_dir(self.snapshot.as_ref(), self.active_pane(window, cx).as_deref());
        let items = match kind {
            Kind::Commands => {
                let bindings = cx.try_global::<ActiveBindings>().map(|b| b.0.clone()).unwrap_or_default();
                // ponytail: this open lists the actions read last time; one installed since shows next time
                self.refresh_plugin_actions(cx);
                crate::ui::command_panel::items(self.shown_in_order().as_deref(), &bindings, &self.plugin_actions)
            }
            Kind::Shortcuts => crate::ui::command_panel::shortcut_items(&cx.try_global::<ActiveBindings>().map(|b| b.0.clone()).unwrap_or_default()),
            Kind::Workspaces => {
                let sessions = self.session_agents(cx);
                // each session's workspaces in the order of drops herdr hasn't carried out yet, as the tree shows them
                let ordered: Vec<Option<Snapshot>> = sessions.iter().map(|s| self.pending_order(&s.key, s.snapshot)).collect();
                crate::ui::command_panel::session_workspace_items(&sidebar_tree::panel_workspaces(&sessions, &ordered, &self.watchers))
            }
            Kind::Branches => match dir.as_deref() {
                Some(d) => crate::ui::command_panel::branch_items(&crate::ui::statusbar::read_branches(d), crate::ui::statusbar::branch_of(d, cx).as_deref()),
                None => Vec::new(),
            },
            Kind::Worktrees => Vec::new(), // herdr's list fills it: see `open_worktrees_panel`
            Kind::Files => Vec::new(), // the shown workspace's files fill it: see `quick_open`
            Kind::Places => crate::ui::command_panel::place_items(
                self.places
                    .iter()
                    .map(|place| {
                        let (name, icon) = place.look(cx);
                        crate::ui::command_panel::PlaceRow {
                            key: place.key(),
                            name,
                            icon,
                            remote: match place {
                                sessions::Place::Machine(m) => Some(m.target.clone()),
                                sessions::Place::Session(_) => None,
                            },
                            shown: self.places_shown(place),
                            waiting: sidebar_tree::session_waiting(&self.watchers, &watchers::SessionKey::of(place), self.snapshot.as_ref()),
                        }
                    })
                    .collect(),
            ),
            Kind::Themes => {
                let appearance = Theme::of(cx).appearance;
                let dark = appearance == crate::theme::Appearance::Dark;
                let s = &SettingsGlobal::of(cx).settings;
                let current = if dark { &s.theme_dark } else { &s.theme_light };
                crate::ui::command_panel::theme_items(crate::theme::variants_for(appearance), dark, current)
            }
        };
        let panel = cx.new(|cx| crate::ui::command_panel::Panel::new(kind, items, window, cx));
        let events = cx.subscribe_in(&panel, window, move |this, _, event: &PanelEvent, window, cx| {
            if !this.close_command_panel(cx) {
                return;
            }
            match event {
                // from the root, as the key binding would (focus goes back to herdr's pane on the next render)
                PanelEvent::Run(Run::Action(name)) => {
                    if let Ok(action) = cx.build_action(&format!("shepherd::{name}"), None) {
                        window.focus(&this.focus, cx);
                        window.dispatch_action(action, cx);
                    }
                }
                PanelEvent::Run(Run::EditKeys(action)) => this.open_shortcut_settings(action, window, cx),
                PanelEvent::Run(Run::Herdr(command)) => {
                    window.focus(&this.focus, cx);
                    this.run_herdr_command(command, window, cx);
                }
                PanelEvent::Run(Run::Tab(tab_id)) => this.show_herdr_tab(Some(tab_id.clone()), true, cx),
                // as a click on its row in the tree: switched to first when in another session (a machine out of
                // reach says so and stays)
                PanelEvent::Run(Run::Go(key, goal)) => {
                    if this.settings.is_some() {
                        this.close_settings(cx);
                    }
                    window.focus(&this.focus, cx);
                    this.go_to(watchers::SessionKey(key.clone()), goal.clone(), cx);
                }
                PanelEvent::Run(Run::Branch(branch)) => {
                    let (Some(dir), branch) = (dir.clone(), branch.clone()) else { return };
                    cx.spawn(async move |this, cx| {
                        let d = dir.clone();
                        let done = cx.background_spawn(async move { crate::ui::statusbar::switch_branch(&d, &branch) }).await;
                        let _ = this.update(cx, |_, cx| match done {
                            Ok(()) => crate::ui::statusbar::refresh_git(&dir, cx),
                            Err(e) => crate::ui::toast::error(cx, e),
                        });
                    })
                    .detach();
                }
                PanelEvent::Run(Run::Theme(id)) => {
                    let dark = Theme::of(cx).appearance == crate::theme::Appearance::Dark;
                    let id = id.clone();
                    crate::ui::settings::update(cx, |s| *if dark { &mut s.theme_dark } else { &mut s.theme_light } = id);
                }
                PanelEvent::Run(Run::Worktree { repo, path, workspace_id }) => {
                    let open_tab = workspace_id.as_ref().and_then(|id| {
                        let s = this.snapshot.as_ref()?;
                        let w = s.workspaces.iter().find(|w| &w.workspace_id == id)?;
                        w.active_tab_id.clone().or_else(|| s.tabs_of(id).first().map(|t| t.tab_id.clone()))
                    });
                    match open_tab {
                        // the worktree's workspace, as it was left (a file tab it showed included)
                        Some(tab) => this.show_herdr_tab(Some(tab), false, cx),
                        None => {
                            let command = Command::OpenWorktree { workspace_id: repo.clone(), path: path.clone() };
                            this.send_then(command, cx, |shell, result, cx| {
                                match result {
                                    Ok(value) => shell.show_herdr_tab(crate::commands::created_tab_id(&value), true, cx),
                                    Err(err) => crate::ui::toast::error(cx, format!("{err:#}")),
                                }
                                cx.notify();
                            });
                        }
                    }
                }
                PanelEvent::Run(Run::OpenFile { path, line }) => this.open_quick_file(path.clone(), *line, window, cx),
                PanelEvent::Run(Run::Place(key)) => {
                    let places = &this.places;
                    let (Some(to), at) = (places.iter().position(|p| &p.key() == key), places.iter().position(|p| this.places_shown(p))) else { return };
                    // from the right when it sits after the one shown in the bar
                    this.show_place(places[to].clone(), at.is_none_or(|at| to > at), cx);
                }
                PanelEvent::Dismiss => {}
            }
        });
        self.command_panel = Some(crate::ui::Popup::new((panel, events)));
        cx.notify();
    }

    /// A key or click in Shepherd: it is the client in use, so it takes its panes' size back.
    fn touch(&mut self, cx: &mut Context<Self>) {
        self.touched = Instant::now();
        crate::terminal::pane::claim(cx);
    }

    /// Fades the command panel out; false if it was not open.
    fn close_command_panel(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(panel) = self.command_panel.as_mut().filter(|p| p.is_open()) else { return false };
        panel.close();
        // The panel's input held focus: give it back to herdr's focused pane on the next render.
        self.focused = None;
        cx.notify();
        true
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings = None;
        // Settings held focus: give it back to herdr's focused pane on the next render.
        self.focused = None;
        cx.notify();
    }

    /// Send a command to herdr; `then` gets the result. Errors show over the grid for 4 s.
    fn send(
        &mut self,
        command: Command,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shell, &serde_json::Value, &mut Context<Shell>) + 'static,
    ) {
        if crate::mock::dir().is_some() {
            return; // a fixture takes no commands
        }
        self.send_then(command, cx, |shell, result, cx| match result {
            Ok(value) => then(shell, &value, cx),
            // the window was behind herdr (a tab closed elsewhere, a double click): catch up instead of complaining
            Err(err) if stale(&err) => shell.resync.notify_one(),
            Err(err) => crate::ui::toast::error(cx, format!("{err:#}")),
        });
    }

    /// [`Self::send`] that sees failures too.
    fn send_then(
        &mut self,
        command: Command,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shell, anyhow::Result<serde_json::Value>, &mut Context<Shell>) + 'static,
    ) {
        let (method, params) = command.request();
        let client = self.client.clone();
        let call = self.rt.spawn(async move { client.call(method, params).await });
        cx.spawn(async move |this, cx| {
            let result = call.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |shell, cx| then(shell, result, cx));
        })
        .detach();
    }

    /// Where a pane of the tab shown landed: its cell metrics are the window's now (a hidden pane keeps those of
    /// the scale or font it was last drawn at).
    fn shown_geometry(&self, cx: &App) -> Option<crate::terminal::pane::GridGeometry> {
        let (snapshot, tab) = (self.snapshot.as_ref()?, self.shown_tab()?);
        snapshot.panes.iter().filter(|p| p.tab_id == tab.tab_id).find_map(|p| self.panes.get(&p.pane_id)?.read(cx).geometry())
    }

    fn shown_tab(&self) -> Option<crate::model::Tab> {
        self.snapshot.as_ref()?.pick_tab_in(self.selected_tab.as_deref(), self.shown_workspace.as_deref()).cloned()
    }

    /// The tab you are looking at: none while the window is inactive, or Settings or a file tab covers the panes.
    fn tab_in_view(&self, window_active: bool) -> Option<String> {
        (window_active && self.settings.is_none() && self.shown_file_tab().is_none()).then(|| self.shown_tab()).flatten().map(|t| t.tab_id)
    }

    /// Agents of the session shown that just started waiting or finished out of sight: a notification each.
    fn agents_changed(&mut self, next: &Snapshot, cx: &mut Context<Self>) {
        let in_view = self.tab_in_view(cx.active_window().is_some());
        let Some(old) = self.snapshot.as_ref() else { return };
        let told: Vec<_> = attention::alerts(old, next)
            .filter(|(p, _)| in_view.as_deref() != Some(p.tab_id.as_str()))
            .map(|(p, worked)| (p.clone(), worked, attention::message(p, next, None)))
            .collect();
        let key = self.shown_key();
        for (pane, worked, message) in told {
            self.tell(key.as_ref(), &pane, worked, message, cx);
        }
    }

    /// The Dock count: every agent waiting or finished, in the session shown and in every other one its watcher
    /// keeps, so a switch of session leaves it as it is. Agents in the tab in view count too: they still wait.
    fn sync_badge(&mut self, cx: &App) {
        // ponytail: the sidebar's Agents toggle counts with the same rule (`waiting_everywhere`)
        let count = if SettingsGlobal::of(cx).settings.dock_badge { self.waiting_everywhere() } else { 0 };
        if count != self.badge {
            self.badge = count;
            crate::notify::set_badge(count);
        }
    }

    /// herdr keeps an agent done until its pane has herdr's focus; the pane with the keyboard in the active window
    /// counts as looked at, so herdr is told and the agent turns idle.
    fn mark_seen(&mut self, window: &Window, cx: &mut Context<Self>) {
        let focused = window.is_window_active().then(|| {
            self.panes.iter().find(|(_, pane)| pane.read(cx).focus_handle(cx).is_focused(window)).map(|(id, _)| id.clone())
        });
        let done = focused.flatten().filter(|id| {
            self.snapshot.as_ref().is_some_and(|s| s.panes.iter().any(|p| &p.pane_id == id && p.agent_status == AgentStatus::Done))
        });
        if done == self.marked_seen {
            return;
        }
        self.marked_seen = done.clone();
        if let Some(pane_id) = done {
            self.send_then(Command::MarkSeen { pane_id }, cx, |_, _, _| {});
        }
    }

    /// The pane keyboard actions apply to: the one with keyboard focus, else herdr's focused pane of the shown tab.
    fn active_pane(&self, window: &Window, cx: &App) -> Option<String> {
        if let Some((id, _)) = self.panes.iter().find(|(_, pane)| pane.read(cx).focus_handle(cx).is_focused(window)) {
            return Some(id.clone());
        }
        let (snapshot, tab) = (self.snapshot.as_ref()?, self.shown_tab()?);
        snapshot.pane_frames(&tab.tab_id).into_iter().find(|f| f.focused).map(|f| f.pane_id)
    }

    fn pane_command(&mut self, window: &Window, cx: &mut Context<Self>, make: impl FnOnce(String) -> Command) {
        // a page shown hides the panes: nothing unseen is split or closed
        if self.pages.shown.is_some() {
            return;
        }
        self.show_terminals(cx);
        if let Some(pane_id) = self.active_pane(window, cx) {
            self.send(make(pane_id), cx, |_, _, _| {});
        }
    }

    /// herdr paints the terminals' default colors itself: it is told the theme's over the popup watch, which then
    /// is the client in front again (back from herdr's TUI on another machine, say).
    fn push_host_theme(&self, cx: &App) {
        if let Some((surface, _)) = &self.popup_surface {
            surface.theme(floating::host_theme(Theme::of(cx)));
        }
    }

    fn refresh_plugin_actions(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let call = self.rt.spawn(async move { client.call("plugin.action.list", json!({})).await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(result)) = call.await else { return };
            let actions = result["actions"].as_array().into_iter().flatten().filter_map(|a| {
                Some((format!("{}.{}", a["plugin_id"].as_str()?, a["action_id"].as_str()?), a["title"].as_str()?.to_string()))
            });
            let actions = actions.collect();
            let _ = this.update(cx, |s, _| s.plugin_actions = actions);
        })
        .detach();
    }

    /// A `[[keys.command]]` of herdr's config, for the pane you are in.
    fn run_herdr_command(&mut self, command: &crate::herdr::keys::HerdrCommand, window: &Window, cx: &mut Context<Self>) {
        let pane_id = self.active_pane(window, cx);
        // herdr gives a popup to the tab in its focus as the popup opens, and only a client shell on that tab gets it:
        // herdr's focus moves to the tab shown first, so a popup the command opens comes to this window
        let Some(tab) = self.shown_tab() else { return self.run_herdr_command_in(command, pane_id, cx) };
        let (client, command) = (self.client.clone(), command.clone());
        let focused = self.rt.spawn(async move { client.call("tab.focus", json!({"tab_id": tab.tab_id})).await });
        cx.spawn(async move |this, cx| {
            let _ = focused.await;
            let _ = this.update(cx, |s, cx| s.run_herdr_command_in(&command, pane_id, cx));
        })
        .detach();
    }

    fn run_herdr_command_in(&mut self, command: &crate::herdr::keys::HerdrCommand, pane_id: Option<String>, cx: &mut Context<Self>) {
        use crate::herdr::keys::CommandKind;
        let pane = self.snapshot.as_ref().and_then(|s| s.panes.iter().find(|p| Some(&p.pane_id) == pane_id.as_ref())).cloned();
        match command.kind {
            CommandKind::PluginAction => {
                self.send(Command::InvokePluginAction { action_id: command.command.clone(), pane }, cx, |_, _, _| {})
            }
            // herdr's `shell`: detached, in the pane's directory, with herdr's socket for the `herdr` CLI
            CommandKind::Shell => {
                let mut sh = crate::process::shell(&command.command);
                sh.env("HERDR_SOCKET_PATH", self.client.path())
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                if let Some(p) = &pane {
                    sh.env("HERDR_ACTIVE_PANE_ID", &p.pane_id)
                        .env("HERDR_ACTIVE_TAB_ID", &p.tab_id)
                        .env("HERDR_ACTIVE_WORKSPACE_ID", &p.workspace_id);
                }
                if let Some(dir) = crate::ui::statusbar::pane_dir(self.snapshot.as_ref(), pane_id.as_deref()) {
                    // a machine's directory is no place to run in here
                    let (machine, path) = crate::herdr::split(&dir);
                    sh.env("HERDR_ACTIVE_PANE_CWD", path);
                    if machine.is_none() {
                        sh.current_dir(path);
                    }
                }
                match sh.spawn() {
                    Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
                    Err(e) => crate::ui::toast::error(cx, format!("{}: {e}", command.command)),
                }
            }
            // herdr runs these itself: its pane lands in the tab, its popup comes back over the client shell
            CommandKind::Pane | CommandKind::Popup => {
                self.show_terminals(cx);
                let panes = self.snapshot.as_ref().map_or(&[][..], |s| &s.panes[..]);
                self.command_panes.retain(|(_, id)| panes.iter().any(|p| &p.pane_id == id));
                let open = self.command_panes.iter().find(|(keys, id)| {
                    *keys == command.keys && pane.as_ref().is_some_and(|shown| panes.iter().any(|p| &p.pane_id == id && p.tab_id == shown.tab_id))
                });
                let open = open.map(|(_, id)| id.clone());
                if let Some(id) = open.filter(|_| command.kind == CommandKind::Pane) {
                    self.focus_next = Some(id.clone());
                    let client = self.client.clone();
                    self.rt.spawn(async move { client.call("pane.focus", json!({"pane_id": id})).await });
                    cx.notify();
                    return;
                }
                let Some((id, _)) = self.herdr_commands.iter().find(|(_, keys)| *keys == command.keys) else {
                    crate::ui::toast::error(cx, format!("{}: herdr has no such command (is its config reloaded?)", command.command));
                    return;
                };
                let mut params = json!({"command_id": id});
                if let Some(p) = &pane {
                    params["workspace_id"] = json!(p.workspace_id);
                    params["tab_id"] = json!(p.tab_id);
                    params["pane_id"] = json!(p.pane_id);
                }
                let (client, label, keys) = (self.client.clone(), command.command.clone(), command.keys.clone());
                let before: Vec<String> = self.snapshot.iter().flat_map(|s| s.panes.iter().map(|p| p.pane_id.clone())).collect();
                let tab = pane.as_ref().map(|p| p.tab_id.clone());
                // herdr opens the pane before it answers: the one in the tab that was not there is it
                let call = self.rt.spawn(async move {
                    client.call("command.invoke", params).await?;
                    client.call("pane.list", json!({})).await
                });
                cx.spawn(async move |this, cx| match call.await {
                    Ok(Ok(list)) => {
                        let new = list["panes"].as_array().into_iter().flatten().find(|p| {
                            p["tab_id"].as_str() == tab.as_deref() && p["pane_id"].as_str().is_some_and(|id| !before.iter().any(|b| b == id))
                        });
                        if let Some(id) = new.and_then(|p| p["pane_id"].as_str()) {
                            let _ = this.update(cx, |s, _| s.command_panes.push((keys, id.to_string())));
                        }
                    }
                    Ok(Err(e)) => {
                        cx.update(|cx| crate::ui::toast::error(cx, format!("{label}: {e:#}")));
                    }
                    Err(_) => {}
                })
                .detach();
            }
        }
    }

    fn step_workspace(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (Some(snapshot), Some(tab)) = (self.shown_in_order(), self.shown_tab()) else { return };
        // sidebar order, so Cmd+Up/Down walk the tree as shown, a drop herdr hasn't carried out yet included
        let workspaces: Vec<_> = snapshot.workspace_tree().into_iter().map(|(w, _)| w).collect();
        let Some(i) = workspaces.iter().position(|w| w.workspace_id == tab.workspace_id) else { return };
        let n = workspaces.len();
        let next = &workspaces[if forward { (i + 1) % n } else { (i + n - 1) % n }];
        let tab = next.active_tab_id.clone().or_else(|| snapshot.tabs_of(&next.workspace_id).first().map(|t| t.tab_id.clone()));
        // the workspace as it was left, a file tab it showed included
        self.show_herdr_tab(tab, false, cx);
    }

    /// Workspace1–9: the `index`th workspace the sidebar's tree shows (worktrees of a folded repo left out), in
    /// whichever session, as it was left, a file tab it showed included.
    fn go_to_workspace(&mut self, index: usize, cx: &mut Context<Self>) {
        self.go_to_tree_workspace(index, cx);
    }

    fn new_tab(&mut self, workspace_id: String, cx: &mut Context<Self>) {
        self.send(Command::NewTab { workspace_id }, cx, |shell, result, cx| {
            shell.show_herdr_tab(crate::commands::created_tab_id(result), true, cx);
        });
    }

    /// While a divider or the sidebar is being dragged, the whole window shows the resize cursor:
    /// the pointer outruns the narrow hit strip, whose own cursor would otherwise flicker with the terminal's.
    fn resize_cursor(&self) -> Option<impl IntoElement> {
        let vertical = if let Some(d) = &self.divider {
            d.vertical
        } else if self.sidebar_drag || self.panel_drag {
            true
        } else {
            return None;
        };
        let style = if vertical { gpui::CursorStyle::ResizeColumn } else { gpui::CursorStyle::ResizeRow };
        Some(canvas(|_, _, _| (), move |_, _, window, _| window.set_window_cursor_style(style)).absolute().size_0())
    }

    /// The strip under the transparent titlebar drags the window; a double click zooms it.
    fn drag_strip(&self) -> Stateful<Div> {
        let (down, up, out, moved) =
            (self.should_move.clone(), self.should_move.clone(), self.should_move.clone(), self.should_move.clone());
        div()
            .id("titlebar-drag")
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(TITLEBAR_HEIGHT))
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down_out(move |_, _, _| out.set(false))
            .on_mouse_up(MouseButton::Left, move |_, _, _| up.set(false))
            .on_mouse_down(MouseButton::Left, move |_, _, _| down.set(true))
            // where Shepherd draws its own titlebar (Wayland, X11), a right-click opens the window manager's window
            // menu, as a titlebar of its own would: minimize and the rest, with or without the window buttons
            .when(!cfg!(any(target_os = "macos", windows)), |el| {
                el.on_mouse_down(MouseButton::Right, |e: &MouseDownEvent, window, _| window.show_window_menu(e.position))
            })
            .on_mouse_move(move |e: &MouseMoveEvent, window, _| {
                if moved.get() && e.pressed_button == Some(MouseButton::Left) {
                    moved.set(false);
                    window.start_window_move();
                }
            })
            .on_click(|e: &ClickEvent, window, _| {
                if e.click_count() == 2 {
                    // macOS does what System Settings says; gpui has no such call elsewhere, so it maximizes
                    if cfg!(target_os = "macos") { window.titlebar_double_click() } else { window.zoom_window() }
                }
            })
    }
}

/// herdr's answer to a command on something it no longer has (`tab_not_found: …`, `pane_not_found: …`).
fn stale(err: &anyhow::Error) -> bool {
    err.to_string().split(':').next().is_some_and(|code| code.ends_with("_not_found"))
}

/// Keep views of the `kept` panes; focus is forgotten with its view so a recreated one gets keyboard focus again.
fn prune<S>(surfaces: &mut HashMap<String, S>, focused: &mut Option<String>, kept: &[String], exited: impl Fn(&S) -> bool) {
    surfaces.retain(|id, s| kept.contains(id) && !exited(s));
    if focused.as_ref().is_some_and(|id| !surfaces.contains_key(id)) {
        *focused = None;
    }
}

/// How many panes keep their views once their tab is hidden: each holds a herdr session that keeps streaming, and
/// herdr renders every one of them (24 kept two fifths of herdr's CPU busy on panes out of sight).
const KEPT_PANES: usize = 6;

/// Puts the `shown` panes first in `recent`, drops panes herdr no longer has, and keeps the newest `KEPT_PANES`:
/// switching back to a recent tab shows its panes at once instead of attaching to herdr again.
fn remember_shown(recent: &mut Vec<String>, shown: &[PaneFrame], live: impl Fn(&str) -> bool) {
    for f in shown.iter().rev() {
        recent.retain(|id| id != &f.pane_id);
        recent.insert(0, f.pane_id.clone());
    }
    recent.retain(|id| live(id));
    recent.truncate(KEPT_PANES);
}

/// The window's background for the Translucent window setting: blurred where the system or compositor blurs.
pub(crate) fn window_background(translucent: bool) -> WindowBackgroundAppearance {
    if translucent { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Opaque }
}

/// Whether this is a dev build, which says so (title, app id, a DEV badge): a debug build, unless `SHEPHERD_DEV_BADGE`
/// is `0` or `1` to force it off or on.
pub(crate) fn is_dev_build() -> bool {
    static DEV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DEV.get_or_init(|| match std::env::var("SHEPHERD_DEV_BADGE").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => cfg!(debug_assertions),
    })
}

/// AppKit lays the traffic lights out again while the window is inactive (a screen recorder takes focus), and gpui
/// moves them back only when it becomes key; setting the title (hidden) re-applies `traffic_light_position`.
fn reapply_traffic_lights(window: &mut Window) {
    window.set_window_title(if is_dev_build() { "Shepherd (dev)" } else { "Shepherd" });
}

/// How long the Mac may sit without a key or a pointer move before Shepherd gives its panes' size away.
// ponytail: a guess at "gone to another device"; a phone attaching sooner keeps Shepherd's size until then
const IDLE_YIELD: Duration = Duration::from_secs(60);

/// Since the last key, click or pointer move anywhere on the Mac, not only in Shepherd.
#[cfg(target_os = "macos")]
fn mac_idle() -> Duration {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    // SAFETY: plain values: kCGEventSourceStateHIDSystemState, kCGAnyInputEventType
    let secs = unsafe { CGEventSourceSecondsSinceLastEventType(1, u32::MAX) };
    Duration::try_from_secs_f64(secs).unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn mac_idle() -> Duration {
    Duration::ZERO
}

/// The Mac's screen is locked (Ctrl+Cmd+Q, the lid closed): gone at once, no idle minute to wait out.
#[cfg(target_os = "macos")]
fn screen_locked() -> bool {
    use std::ffi::c_void;
    type Ref = *const c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> Ref;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDictionaryGetValue(dict: Ref, key: Ref) -> Ref;
        fn CFStringCreateWithCString(alloc: Ref, s: *const std::ffi::c_char, encoding: u32) -> Ref;
        fn CFBooleanGetValue(b: Ref) -> bool;
        fn CFRelease(r: Ref);
    }
    const UTF8: u32 = 0x0800_0100;
    // SAFETY: CoreFoundation calls on objects created here and released here; the value is borrowed from the dict
    unsafe {
        let session = CGSessionCopyCurrentDictionary();
        if session.is_null() {
            return false;
        }
        let key = CFStringCreateWithCString(std::ptr::null(), c"CGSSessionScreenIsLocked".as_ptr(), UTF8);
        let value = CFDictionaryGetValue(session, key);
        let locked = !value.is_null() && CFBooleanGetValue(value);
        CFRelease(key);
        CFRelease(session);
        locked
    }
}

#[cfg(not(target_os = "macos"))]
fn screen_locked() -> bool {
    false
}

/// Turns macOS Secure Keyboard Entry on or off. The system counts enables, so it is called only on a change.
#[cfg(target_os = "macos")]
fn secure_input(on: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn EnableSecureEventInput() -> i32;
        fn DisableSecureEventInput() -> i32;
    }
    static ON: AtomicBool = AtomicBool::new(false);
    if ON.swap(on, Ordering::Relaxed) != on {
        // SAFETY: no arguments; balanced by `ON`
        unsafe {
            if on { EnableSecureEventInput() } else { DisableSecureEventInput() };
        }
    }
}

/// No other system has it.
#[cfg(not(target_os = "macos"))]
fn secure_input(_: bool) {}

/// The window's last frame and its display, while that display is plugged in and the frame on it.
fn saved_window(cx: &App) -> Option<(gpui::DisplayId, gpui::WindowBounds)> {
    let frame = SettingsGlobal::of(cx).settings.window.clone()?;
    let display = cx.displays().into_iter().find(|d| d.uuid().is_ok_and(|u| u.to_string() == frame.display))?;
    let bounds = frame.bounds_on(display.bounds().size)?;
    let bounds = match (frame.fullscreen, frame.maximized) {
        (true, _) => gpui::WindowBounds::Fullscreen(bounds),
        (_, true) => gpui::WindowBounds::Maximized(bounds),
        _ => gpui::WindowBounds::Windowed(bounds),
    };
    Some((display.id(), bounds))
}

/// The window's frame to save: in full screen, the one it leaves full screen to; zoomed, the one before (`last`).
fn window_frame(window: &Window, last: Option<&WindowFrame>, cx: &App) -> Option<WindowFrame> {
    let display = window.display(cx)?.uuid().ok()?.to_string();
    let (bounds, fullscreen) = match window.window_bounds() {
        gpui::WindowBounds::Fullscreen(b) => (b, true),
        b => (b.get_bounds(), false),
    };
    let (x, y, width, height) = (bounds.origin.x.into(), bounds.origin.y.into(), bounds.size.width.into(), bounds.size.height.into());
    let frame = WindowFrame { display, x, y, width, height, fullscreen, maximized: false };
    Some(if !fullscreen && window.is_maximized() { frame.zoomed(last) } else { frame })
}

/// Saves a choice made in the sidebar, or where the window was moved. A settings file that failed to load is the user's to fix: the sidebar must
/// not overwrite it with defaults.
fn save_from_sidebar(cx: &mut App, change: impl FnOnce(&mut crate::settings::Settings)) {
    if SettingsGlobal::of(cx).error.is_none() {
        crate::ui::settings::update(cx, change);
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired_images.drain(..) {
            let _ = window.drop_image(image);
        }
        self.sync_badge(cx);
        self.mark_seen(window, cx);
        // any Shepherd window in front, not this one: a window in the back renders too
        secure_input(SettingsGlobal::of(cx).settings.secure_input && cx.active_window().is_some());
        crate::ui::reap_popup(&mut self.view_menu, window);
        crate::ui::reap_popup(&mut self.agents_menu, window);
        crate::ui::reap_popup(&mut self.ws_menu, window);
        crate::ui::reap_popup(&mut self.pane_menu, window);
        crate::ui::reap_popup(&mut self.spent_menu, window);
        crate::ui::reap_popup(&mut self.usage_menu, window);
        crate::ui::reap_popup(&mut self.statusbar_menu, window);
        crate::ui::reap_popup(&mut self.tab_menu, window);
        crate::ui::reap_popup(&mut self.root_menu, window);
        crate::ui::reap_popup(&mut self.files_view_menu, window);
        crate::ui::reap_popup(&mut self.worktree_dialog, window);
        crate::ui::reap_popup(&mut self.remove_confirm, window);
        crate::ui::reap_popup(&mut self.merge_confirm, window);
        crate::ui::reap_popup(&mut self.session_dialog, window);
        crate::ui::reap_popup(&mut self.place_menu, window);
        self.reap_tree_popups(window);
        crate::ui::reap_popup(&mut self.place_edit, window);
        crate::ui::reap_popup(&mut self.session_delete, window);
        crate::ui::reap_popup(&mut self.command_panel, window);
        if let Some(s) = &self.snapshot {
            self.removing.retain(|id| s.workspaces.iter().any(|w| &w.workspace_id == id));
        }
        reapply_traffic_lights(window);
        // a menu or dialog opening puts the shortcut hints away; they fade in and out on their own frames
        let now = Instant::now();
        if self.popup_open() {
            self.hints.cancel(now);
        }
        if self.hints.fading(now) {
            window.request_animation_frame();
        }
        let theme = Theme::of(cx).clone();
        let snapshot = self.snapshot.clone();
        let toasts = crate::ui::toast::stack(&theme, TITLEBAR_HEIGHT + 8., window, cx);
        let tab = self.shown_tab();
        self.shown_workspace = tab.as_ref().map(|t| t.workspace_id.clone());
        self.follow_shown_tab();
        let (mut frames, mut splits) = match (&snapshot, &tab) {
            (Some(s), Some(t)) => (s.pane_frames(&t.tab_id), s.split_frames(&t.tab_id)),
            _ => (Vec::new(), Vec::new()),
        };
        // herdr caught up with a released divider: its snapshot is the layout again.
        if let Some((path, ratio)) = &self.settling
            && splits.iter().any(|s| &s.path == path && (s.ratio - ratio).abs() < 0.02)
        {
            self.settling = None;
        }
        // While a divider moves (or settles), lay the panes out at its ratio, not herdr's last one.
        let live = self.divider.as_ref().map(|d| (d.path.clone(), d.ratio)).or(self.settling.clone());
        if let Some((path, ratio)) = live {
            (frames, splits) = crate::model::preview(&frames, &splits, &path, ratio);
        }
        let own = self.own_pane.clone();
        let shown: Vec<PaneFrame> = frames.iter().filter(|f| attachable(&f.pane_id, own.as_deref())).cloned().collect();
        if let Some(snapshot) = &snapshot {
            remember_shown(&mut self.recent_panes, &shown, |id| snapshot.panes.iter().any(|p| p.pane_id == id));
        }
        for (_, pane) in self.panes.iter().filter(|(id, _)| !self.recent_panes.contains(id)) {
            self.retired_images.extend(pane.update(cx, |pane, cx| pane.set_images(Vec::new(), cx)));
        }
        prune(&mut self.panes, &mut self.focused, &self.recent_panes, |_| false);
        if std::mem::take(&mut self.popup_closed) && !self.panes.values().any(|p| p.read(cx).focus_handle(cx).is_focused(window)) {
            self.focused = None;
        }
        let (viewer, _) = self.sync_file_tabs(window, cx);
        // a file tab shown in the grid's place keeps the keyboard from herdr's focused pane, as an overlay does
        let file_shown = self.shown_file_tab().is_some();
        let covered = self.settings.is_some() || self.command_panel.as_ref().is_some_and(|p| p.is_open()) || self.floating_open() || file_shown || self.side_panel.read(cx).focus_handle(cx).contains_focused(window, cx);
        // a field of the sidebar's (its search, a workspace being renamed) typed in keeps the keyboard too: herdr
        // moving its focus, or a switch, must not hand it back to a pane mid word
        let typing = self.sidebar_search.as_ref().is_some_and(|s| s.input.read(cx).focus_handle(cx).is_focused(window))
            || self.renaming.as_ref().is_some_and(|r| r.input.read(cx).focus_handle(cx).is_focused(window));
        let overlaid = sidebar_search::keyboard_elsewhere(covered, typing, self.dialog_up());
        // a session was left: the keyboard leaves its parked panes (out of `panes`) for the window, and herdr's
        // focused pane here takes it next
        if self.views.take_refocus() && self.settings.is_none() {
            window.focus(&self.focus, cx);
            self.focused = None;
        }
        let shown_key = self.shown_key();
        let tab_id = tab.as_ref().map(|t| t.tab_id.clone());
        self.note_tab_shown(shown_key.as_ref(), tab_id.as_deref(), now);
        for f in &shown {
            if !self.panes.contains_key(&f.pane_id) {
                let id = f.pane_id.clone();
                let images = self.pane_images.get(&id).cloned().unwrap_or_default();
                let pane = cx.new(|cx| {
                    let mut pane = PaneTerminal::new(id.clone(), cx);
                    pane.set_images(images, cx);
                    pane
                });
                // the reader's "Open in editor": a tab of Shepherd's own editor
                cx.subscribe_in(&pane, window, |s: &mut Shell, _, e: &crate::terminal::pane::OpenFile, window, cx| {
                    s.open_file(e.path.clone(), e.line, false, window, cx)
                })
                .detach();
                self.panes.insert(id, pane);
            }
            // a parked pane shown again attaches, to the session the window's socket names by now
            if let Some(pane) = self.panes.get(&f.pane_id).filter(|p| p.read(cx).is_parked()) {
                pane.update(cx, |p, cx| p.resume(cx));
            }
            // an open panel keeps its input's focus; a panel run from the command panel opens before this render
            if f.focused && !overlaid && self.focused.as_deref() != Some(f.pane_id.as_str()) {
                let handle = self.panes[&f.pane_id].read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                self.focused = Some(f.pane_id.clone());
            }
        }
        // an agent's pane asked for wins over the pane the tab remembers
        let goal = self.focus_next.is_some() || self.tree.goal_waiting();
        // after herdr's focus, so the agent's pane wins; its tab is already the shown one
        // not a pane under a file tab, which is not drawn
        if let Some(id) = self.focus_next.take().filter(|id| self.settings.is_none() && !file_shown && shown.iter().any(|f| &f.pane_id == id)) {
            let handle = self.panes[&id].read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        self.restore_keyboard(&shown, tab_id.as_deref(), goal, overlaid, now, window, cx);

        // A pane of a tab no longer shown keeps focus, and gpui then dispatches keys to the root, past our actions.
        let hidden = self
            .panes
            .iter()
            .any(|(id, p)| !shown.iter().any(|f| &f.pane_id == id) && p.read(cx).focus_handle(cx).is_focused(window));
        if (window.focused(cx).is_none() || hidden) && self.settings.is_none() {
            window.focus(&self.focus, cx);
            self.focused = None;
        }
        if !overlaid {
            self.note_keyboard(shown_key.as_ref(), tab_id.as_deref(), &shown, window, cx);
        }
        if let Some(viewer) = &viewer {
            self.view_takes_keys(viewer, window, cx);
        }
        let viewer_markdown = viewer.as_ref().is_some_and(|v| v.shows_markdown(cx));
        let shown_ws = tab.as_ref().map(|t| t.workspace_id.clone());
        let shown_tab = tab.as_ref().map(|t| t.tab_id.clone());
        let sidebar_w = self.sidebar_now();
        let sliding = self.sidebar_motion.is_some_and(|m| m.animating());
        let peek_on = SettingsGlobal::of(cx).settings.sidebar_peek;
        if !peek_on {
            // turned off in Settings while out: the card goes, rather than wait for a pointer that will not come back
            self.sidebar_peek = None;
            self.peek_hovered = false;
        }
        let peek = self.peek_now();
        // the sidebar hidden (or its peek gone): a search there would take keys nobody sees, and number hidden rows
        if self.sidebar_search.is_some() && !sidebar_search::sidebar_drawn(self.sidebar_open, peek) {
            self.close_sidebar_search(cx);
        }
        if sliding || self.sidebar_nudge().is_some() || self.sidebar_peek.is_some_and(|m| m.animating()) {
            window.request_animation_frame();
        }

        let covered = self.overlay_open(cx);
        self.sync_pages(covered, window, cx);
        let web = self.shown_page(cx);
        // a page in place of the tabs: no workspace looks selected
        let sidebar_ws = if web.is_some() { None } else { shown_ws.clone() };
        let sidebar = self.render_sidebar(&theme, &snapshot, &sidebar_ws, &shown_tab, window, cx);
        let (docked, peeking) = if sidebar_w > 0. { (Some(sidebar), None) } else { (None, Some(sidebar)) };

        let tabbar = self.render_tab_bar(&theme, shown_ws.clone(), shown_tab.clone(), sidebar_w, window, cx);
        let (tabbar, web_area) = match &web {
            Some((key, entry)) => (self.render_web_bar(key, entry, &theme, sidebar_w, cx), Some(self.render_web_area(key, &theme, window, cx))),
            None => (tabbar, None),
        };

        // A file tab shown: its view in the grid's place. The panes are not drawn, so they are not laid out either
        // and keep their size (herdr's PTYs are not resized); their views stay, as a hidden tab's do.
        if viewer.is_some() {
            (frames, splits) = (Vec::new(), Vec::new());
        }
        let shown_tab_id = shown_tab.clone().unwrap_or_default();
        let grid_bounds = self.grid_bounds.clone();
        let ring = theme.accent.opacity(0.5);
        let split = frames.len() > 1;
        let pane_drop = self.pane_drop_overlay(&frames, &shown_tab_id, &theme, window, cx);
        let edges: Vec<f32> = splits.iter().map(|s| crate::model::split_edge(&frames, s)).collect();
        let floating = self.render_floating(&theme, window, cx);
        let grid = div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .bg(theme.terminal.background)
            .child(
                canvas(move |bounds, _, _| grid_bounds.set(Some(bounds)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .when_some(self.error.clone(), |el, err| el.child(div().p_4().text_color(theme.text_muted).child(err)))
            .when(snapshot.is_none() && self.error.is_none() && self.connecting.is_some_and(|at| crate::ui::loader::due(at, window)), |el| {
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(12.))
                        .child(crate::ui::loader::spinner(2.5, window))
                        .child(div().text_size(px(12.)).text_color(theme.text_muted).child(format!("Connecting to {}…", self.place_label(cx)))),
                )
            })
            .children(frames.into_iter().map(|f| {
                let cell = div()
                    .absolute()
                    .left(relative(f.frame.x))
                    .top(relative(f.frame.y))
                    .w(relative(f.frame.w))
                    .h(relative(f.frame.h));
                match self.panes.get(&f.pane_id) {
                    Some(pane) => {
                        // a lone pane needs no marker of which pane has focus
                        let focused = split && pane.read(cx).focus_handle(cx).is_focused(window);
                        let pane_id = f.pane_id.clone();
                        let group = SharedString::from(format!("pane-{}", f.pane_id));
                        let grip = self.pane_grip(&f.pane_id, &shown_tab_id, &group, &theme, cx);
                        cell.group(group).on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                this.pane_menu = Some(crate::ui::Popup::new((pane_id.clone(), e.position)));
                                cx.notify();
                            }),
                        )
                        .child(
                            // cached: a frame the pane did not cause (a spinner, a hover elsewhere) reuses its grid
                            div().size_full().relative().child(pane.clone().cached(gpui::StyleRefinement::default().size_full())).when(focused, |el| {
                                // ponytail: overlay, not a border on the container, which shrank the grid by 1px
                                el.child(div().absolute().inset_0().border_1().border_color(ring))
                            }),
                        )
                        .child(grip)
                    }
                    None => cell.child(
                        div()
                            .size_full()
                            .p_4()
                            .bg(theme.terminal.background)
                            .text_color(theme.text_muted)
                            .child(format!("{} is this Shepherd's own pane", f.pane_id)),
                    ),
                }
            }));
        let (seam, seam_lit) = (theme.border_strong, theme.accent);
        let grid = grid
            .children(splits.into_iter().zip(edges).map(|(split, edge)| {
                let dragging = self.divider.as_ref().is_some_and(|d| d.path == split.path);
                let id = SharedString::from(format!("split-{}", split.path.iter().map(|b| if *b { '1' } else { '0' }).collect::<String>()));
                // The hit strip is centred on the panes' shared edge (`edge`, already live while dragging).
                let strip = if split.vertical {
                    div()
                        .left(relative(edge))
                        .ml(px(-PANE_SEAM_HIT / 2.))
                        .top(relative(split.frame.y))
                        .h(relative(split.frame.h))
                        .w(px(PANE_SEAM_HIT))
                        .cursor_col_resize()
                } else {
                    div()
                        .top(relative(edge))
                        .mt(px(-PANE_SEAM_HIT / 2.))
                        .left(relative(split.frame.x))
                        .w(relative(split.frame.w))
                        .h(px(PANE_SEAM_HIT))
                        .cursor_row_resize()
                };
                let tab_id = shown_tab_id.clone();
                strip
                    .id(id.clone())
                    .on_hover({
                        let id = id.clone();
                        cx.listener(move |this, on: &bool, _, cx| this.hover_seam(id.clone(), *on, cx))
                    })
                    .absolute()
                    .child(pane_seam(id.clone(), split.vertical, seam, seam_lit, dragging, self.seam_hover.get(&id).copied()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.start_divider(tab_id.clone(), &split, cx);
                        }),
                    )
            }))
            .children(pane_drop)
            // under herdr's popup, which is modal
            .children(viewer.map(|v| div().absolute().inset_0().child(v.element())))
            .children(floating);

        let show_bar = SettingsGlobal::of(cx).settings.show_status_bar;
        // nothing to show outside a repo with a remote: the PR tab waits for one. Asked of git (cheap, cached), so
        // gh or glab only run for the status bar's label or the PR tab, as before the tabs.
        let active_dir = crate::ui::statusbar::pane_dir(snapshot.as_ref(), self.active_pane(window, cx).as_deref());
        self.panel_has_pr = active_dir.as_deref().is_some_and(|d| crate::ui::statusbar::has_origin(d, cx));
        let pr_shown = self.panel_open && self.shown_panel_tab() == crate::settings::PanelTab::Pr;
        let gate = crate::ui::statusbar::gate(show_bar, pr_shown);
        let shown = if gate.review {
            self.shown_review(snapshot.as_ref(), window, cx)
        } else {
            // nothing shows the review: its checks are not polled
            self.review_poll = None;
            Default::default()
        };
        // hidden, the bar's own lookups (git status, tokens, plan limits) are not asked: only drawing it asks
        let statusbar = gate.bar.then(|| {
            let pane = self.active_pane(window, cx);
            let on_branch = cx.listener(|this, _: &ClickEvent, window, cx| {
                this.toggle_command_panel(crate::ui::command_panel::Kind::Branches, window, cx)
            });
            // Popups sit on the label's edge, just above the bar.
            let on_spent = cx.listener(|this, label: &Bounds<Pixels>, window, cx| {
                let at = gpui::point(label.left(), window.viewport_size().height - px(crate::ui::statusbar::HEIGHT + 4.));
                this.spent_menu = Some(crate::ui::Popup::new(at));
                cx.notify();
            });
            let on_usage = cx.listener(|this, label: &Bounds<Pixels>, window, cx| {
                // anchored at its right edge: the limits sit at the bar's right end
                let at = gpui::point(label.right(), window.viewport_size().height - px(crate::ui::statusbar::HEIGHT + 4.));
                this.usage_menu = Some(crate::ui::Popup::new(at));
                cx.notify();
            });
            use crate::ui::statusbar::Item;
            let branches = self.command_panel.as_ref().is_some_and(|p| p.is_open() && p.value.0.read(cx).kind == crate::ui::command_panel::Kind::Branches);
            let open = if branches {
                Some(Item::Branch)
            } else if self.spent_menu.as_ref().is_some_and(|m| m.is_open()) {
                Some(Item::Spent)
            } else {
                self.usage_menu.as_ref().is_some_and(|m| m.is_open()).then_some(Item::Usage)
            };
            let on_review = cx.listener(|this, _: &ClickEvent, window, cx| this.show_panel_tab(crate::settings::PanelTab::Pr, window, cx));
            let show = SettingsGlobal::of(cx).settings.status_bar_show;
            div()
                .flex_none()
                .on_mouse_down(MouseButton::Right, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.statusbar_menu = Some(crate::ui::Popup::new(e.position));
                    cx.notify();
                }))
                .child(crate::ui::statusbar::bar(show, snapshot.as_ref(), pane.as_deref(), shown.review.as_ref(), &theme, open, pr_shown, on_branch, on_spent, on_usage, on_review, window, cx))
        });
        let spring_back = self.pane_spring_back(window);
        let settings_page = self.settings.as_ref().map(|(view, _)| view.clone());
        let on_page = settings_page.is_some();
        let panel = (self.panel_open && !on_page).then(|| self.render_right_panel(&theme, &shown, window, cx));
        // the side panel gone from view (closed, the PR tab now there, the settings page) keeps no keyboard focus
        let side_shown = self.panel_open && !on_page && self.shown_panel_tab() != crate::settings::PanelTab::Pr;
        if !side_shown && self.side_panel.read(cx).focus_handle(cx).contains_focused(window, cx) {
            self.focus_pane(window, cx);
        }
        let files_shown = side_shown && self.shown_panel_tab() == crate::settings::PanelTab::Files;
        let active = window.is_window_active();
        let changes_shown = side_shown && self.shown_panel_tab() == crate::settings::PanelTab::Changes;
        let panel_shown = self.panel_open && !on_page;
        self.side_panel.update(cx, |p, cx| {
            p.set_files_shown(files_shown, active, cx);
            p.set_changes_shown(panel_shown, changes_shown, active, cx);
        });
        if !(pr_shown && !on_page) && !viewer_markdown {
            // a hidden panel's selection is not Cmd+C's to copy
            crate::ui::markdown::forget();
        }
        let root = div()
            .id("root")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|s, e: &gpui::KeyDownEvent, window, cx| {
                s.touch(cx);
                // Cmd+C (Ctrl+C off macOS) over a selection in the review panel copies it, not the pane's; the
                // viewer, with the keyboard, copies its own
                let m = &e.keystroke.modifiers;
                let viewer = s.shown_file_view().is_some_and(|v| v.focus_handle(cx).contains_focused(window, cx));
                let copy = if cfg!(target_os = "macos") { m.platform && !(m.shift || m.alt || m.control) } else { m.control && !(m.alt || m.platform) };
                if copy
                    && !viewer
                    && let Some(text) = crate::ui::markdown::selected_text()
                {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    cx.stop_propagation();
                }
            }))
            .capture_any_mouse_down(cx.listener(|s, _: &MouseDownEvent, _, cx| {
                s.touch(cx);
                s.hint_cancel(cx);
                s.prefix_tap_event(crate::tap::Event::Click, None, cx);
            }))
            .on_modifiers_changed(cx.listener(|s, e: &gpui::ModifiersChangedEvent, window, cx| {
                s.hint_modifiers(&e.modifiers, cx);
                s.prefix_tap_event(crate::tap::Event::Mods(crate::tap::Mods::of(&e.modifiers)), Some(window), cx);
            }))
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(theme.glass())
            .text_color(theme.text)
            .font_family(theme.font_ui.clone())
            .text_size(px(13.))
            .on_action(cx.listener(|this, _: &NewTab, _, cx| {
                if let Some(tab) = this.shown_tab() {
                    this.new_tab(tab.workspace_id, cx);
                }
            }))
            // a file tab shown closes, never a pane or herdr's tab under it
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if this.close_shown_file_tab(window, cx) {
                    return;
                }
                if let Some(tab) = this.shown_tab().filter(|_| this.pages.shown.is_none()) {
                    this.send(Command::CloseTab { tab_id: tab.tab_id }, cx, |_, _, _| {});
                }
            }))
            // bound in the editor only (`bindings::context_of`); the editor saves itself on SaveFile
            .on_action(cx.listener(|this, _: &SaveAllFiles, window, cx| this.save_all_files(window, cx)))
            .on_action(cx.listener(|this, _: &NewWorkspace, _, cx| this.new_workspace(cx)))
            .on_action(cx.listener(|this, _: &SplitRight, w, cx| this.pane_command(w, cx, |pane_id| Command::Split { pane_id, down: false })))
            .on_action(cx.listener(|this, _: &SplitDown, w, cx| this.pane_command(w, cx, |pane_id| Command::Split { pane_id, down: true })))
            .on_action(cx.listener(|this, _: &ClosePane, w, cx| {
                if !this.close_shown_file_tab(w, cx) {
                    this.pane_command(w, cx, |pane_id| Command::ClosePane { pane_id });
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleZoom, w, cx| this.pane_command(w, cx, |pane_id| Command::Zoom { pane_id })))
            .on_action(cx.listener(|this, _: &FocusLeft, w, cx| this.pane_command(w, cx, |pane_id| Command::FocusPane { pane_id, direction: Direction::Left })))
            .on_action(cx.listener(|this, _: &FocusRight, w, cx| this.pane_command(w, cx, |pane_id| Command::FocusPane { pane_id, direction: Direction::Right })))
            .on_action(cx.listener(|this, _: &FocusUp, w, cx| this.pane_command(w, cx, |pane_id| Command::FocusPane { pane_id, direction: Direction::Up })))
            .on_action(cx.listener(|this, _: &FocusDown, w, cx| this.pane_command(w, cx, |pane_id| Command::FocusPane { pane_id, direction: Direction::Down })))
            .on_action(cx.listener(|this, _: &Tab1, _, cx| this.go_to_tab(0, cx)))
            .on_action(cx.listener(|this, _: &Tab2, _, cx| this.go_to_tab(1, cx)))
            .on_action(cx.listener(|this, _: &Tab3, _, cx| this.go_to_tab(2, cx)))
            .on_action(cx.listener(|this, _: &Tab4, _, cx| this.go_to_tab(3, cx)))
            .on_action(cx.listener(|this, _: &Tab5, _, cx| this.go_to_tab(4, cx)))
            .on_action(cx.listener(|this, _: &Tab6, _, cx| this.go_to_tab(5, cx)))
            .on_action(cx.listener(|this, _: &Tab7, _, cx| this.go_to_tab(6, cx)))
            .on_action(cx.listener(|this, _: &Tab8, _, cx| this.go_to_tab(7, cx)))
            .on_action(cx.listener(|this, _: &Tab9, _, cx| this.go_to_tab(8, cx)))
            .on_action(cx.listener(|this, _: &PreviousTab, _, cx| this.step_tab(false, cx)))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.step_tab(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousWorkspace, _, cx| this.step_workspace(false, cx)))
            .on_action(cx.listener(|this, _: &NextWorkspace, _, cx| this.step_workspace(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousSession, _, cx| this.step_place(false, cx)))
            .on_action(cx.listener(|this, _: &NextSession, _, cx| this.step_place(true, cx)))
            .on_action(cx.listener(|this, _: &SwitchSession, w, cx| this.toggle_command_panel(crate::ui::command_panel::Kind::Places, w, cx)))
            .on_action(cx.listener(|this, _: &ToggleSessionCollapsed, _, cx| this.toggle_shown_session(cx)))
            .on_action(cx.listener(|this, _: &Workspace1, _, cx| this.go_to_workspace(0, cx)))
            .on_action(cx.listener(|this, _: &Workspace2, _, cx| this.go_to_workspace(1, cx)))
            .on_action(cx.listener(|this, _: &Workspace3, _, cx| this.go_to_workspace(2, cx)))
            .on_action(cx.listener(|this, _: &Workspace4, _, cx| this.go_to_workspace(3, cx)))
            .on_action(cx.listener(|this, _: &Workspace5, _, cx| this.go_to_workspace(4, cx)))
            .on_action(cx.listener(|this, _: &Workspace6, _, cx| this.go_to_workspace(5, cx)))
            .on_action(cx.listener(|this, _: &Workspace7, _, cx| this.go_to_workspace(6, cx)))
            .on_action(cx.listener(|this, _: &Workspace8, _, cx| this.go_to_workspace(7, cx)))
            .on_action(cx.listener(|this, _: &Workspace9, _, cx| this.go_to_workspace(8, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleAgentsView, _, cx| this.toggle_agents_view(cx)))
            .on_action(cx.listener(|this, _: &FilterSidebar, w, cx| this.open_sidebar_search(w, cx)))
            .on_action(cx.listener(|this, _: &ToggleFocusMode, _, cx| this.toggle_focus_mode(cx)))
            .on_action(cx.listener(|this, _: &ToggleReview, w, cx| this.toggle_review(w, cx)))
            .on_action(cx.listener(|this, _: &ShowFiles, w, cx| this.show_panel_tab(crate::settings::PanelTab::Files, w, cx)))
            .on_action(cx.listener(|this, _: &ShowChanges, w, cx| this.show_panel_tab(crate::settings::PanelTab::Changes, w, cx)))
            .on_action(cx.listener(|this, _: &ShowSearch, w, cx| this.show_panel_tab(crate::settings::PanelTab::Search, w, cx)))
            .on_action(cx.listener(|this, _: &ShowPullRequest, w, cx| this.show_panel_tab(crate::settings::PanelTab::Pr, w, cx)))
            // the shown pane's repo: also the status bar branch's click
            .on_action(cx.listener(|this, _: &SwitchBranch, w, cx| this.toggle_command_panel(crate::ui::command_panel::Kind::Branches, w, cx)))
            .on_action(cx.listener(|this, _: &QuickOpen, w, cx| this.quick_open(w, cx)))
            // ponytail: until reveal (M8) lands, it opens the Files tab
            .on_action(cx.listener(|this, _: &RevealActiveFile, w, cx| this.open_panel_tab(crate::settings::PanelTab::Files, w, cx)))
            .on_action(cx.listener(|this, _: &ChooseRoot, w, cx| this.choose_root(w, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.toggle_settings_here(window, cx)))
            .on_action(cx.listener(|this, _: &CommandPanel, w, cx| this.toggle_command_panel(crate::ui::command_panel::Kind::Commands, w, cx)))
            .on_action(cx.listener(|this, _: &ChangeTheme, w, cx| this.toggle_command_panel(crate::ui::command_panel::Kind::Themes, w, cx)))
            .on_action(cx.listener(|this, _: &NextNeedsYou, _, cx| this.next_needs_you(cx)))
            .on_action(cx.listener(|this, _: &WorkspacesPanel, w, cx| this.toggle_command_panel(crate::ui::command_panel::Kind::Workspaces, w, cx)))
            .on_action(cx.listener(|this, _: &ShowShortcuts, w, cx| this.toggle_command_panel(crate::ui::command_panel::Kind::Shortcuts, w, cx)))
            .on_action(cx.listener(|this, c: &crate::herdr::keys::HerdrCommand, w, cx| this.run_herdr_command(c, w, cx)))
            .on_action(cx.listener(|this, c: &crate::herdr::keys::HerdrOnly, _, cx| this.herdr_only(c, cx)))
            .on_action(cx.listener(|this, c: &crate::herdr::keys::HerdrUnbound, _, cx| this.herdr_unbound(c, cx)))
            // the rest of herdr's keymap, through its API (`key_actions`)
            .on_action(cx.listener(|this, _: &ReloadHerdrConfig, _, cx| this.reload_herdr_config(cx)))
            .on_action(cx.listener(|this, _: &RenameTab, w, cx| this.rename_shown_tab(w, cx)))
            .on_action(cx.listener(|this, _: &RenameWorkspace, w, cx| this.rename_shown_workspace(w, cx)))
            .on_action(cx.listener(|this, _: &RenamePane, w, cx| this.rename_active_pane(w, cx)))
            .on_action(cx.listener(|this, _: &CloseWorkspace, _, cx| this.close_shown_workspace(cx)))
            .on_action(cx.listener(|this, _: &NewWorktree, w, cx| this.worktree_key(key_actions::WorktreeKey::New, w, cx)))
            .on_action(cx.listener(|this, _: &OpenWorktree, w, cx| this.worktree_key(key_actions::WorktreeKey::Open, w, cx)))
            .on_action(cx.listener(|this, _: &RemoveWorktree, w, cx| this.worktree_key(key_actions::WorktreeKey::Remove, w, cx)))
            .on_action(cx.listener(|this, _: &MoveTabPrevious, _, cx| this.move_shown_tab(false, cx)))
            .on_action(cx.listener(|this, _: &MoveTabNext, _, cx| this.move_shown_tab(true, cx)))
            .on_action(cx.listener(|this, _: &CyclePaneNext, w, cx| this.cycle_pane(true, w, cx)))
            .on_action(cx.listener(|this, _: &CyclePanePrevious, w, cx| this.cycle_pane(false, w, cx)))
            .on_action(cx.listener(|this, _: &ClearPane, w, cx| this.pane_command(w, cx, |pane_id| Command::ClearPane { pane_id })))
            .on_action(cx.listener(|this, _: &EditScrollback, w, cx| this.pane_command(w, cx, |pane_id| Command::EditScrollback { pane_id })))
            .on_action(cx.listener(|this, _: &SwapPaneLeft, w, cx| this.pane_command(w, cx, |pane_id| Command::SwapPaneToward { pane_id, direction: Direction::Left })))
            .on_action(cx.listener(|this, _: &SwapPaneRight, w, cx| this.pane_command(w, cx, |pane_id| Command::SwapPaneToward { pane_id, direction: Direction::Right })))
            .on_action(cx.listener(|this, _: &SwapPaneUp, w, cx| this.pane_command(w, cx, |pane_id| Command::SwapPaneToward { pane_id, direction: Direction::Up })))
            .on_action(cx.listener(|this, _: &SwapPaneDown, w, cx| this.pane_command(w, cx, |pane_id| Command::SwapPaneToward { pane_id, direction: Direction::Down })))
            .on_action(cx.listener(|this, _: &ResizePaneLeft, w, cx| this.pane_command(w, cx, |pane_id| Command::ResizePane { pane_id, direction: Direction::Left })))
            .on_action(cx.listener(|this, _: &ResizePaneRight, w, cx| this.pane_command(w, cx, |pane_id| Command::ResizePane { pane_id, direction: Direction::Right })))
            .on_action(cx.listener(|this, _: &ResizePaneUp, w, cx| this.pane_command(w, cx, |pane_id| Command::ResizePane { pane_id, direction: Direction::Up })))
            .on_action(cx.listener(|this, _: &ResizePaneDown, w, cx| this.pane_command(w, cx, |pane_id| Command::ResizePane { pane_id, direction: Direction::Down })))
            .on_action(cx.listener(|this, _: &PreviousAgent, w, cx| this.step_agent(false, w, cx)))
            .on_action(cx.listener(|this, _: &NextAgent, w, cx| this.step_agent(true, w, cx)))
            .on_action(cx.listener(|this, _: &Agent1, _, cx| this.go_to_agent(0, cx)))
            .on_action(cx.listener(|this, _: &Agent2, _, cx| this.go_to_agent(1, cx)))
            .on_action(cx.listener(|this, _: &Agent3, _, cx| this.go_to_agent(2, cx)))
            .on_action(cx.listener(|this, _: &Agent4, _, cx| this.go_to_agent(3, cx)))
            .on_action(cx.listener(|this, _: &Agent5, _, cx| this.go_to_agent(4, cx)))
            .on_action(cx.listener(|this, _: &Agent6, _, cx| this.go_to_agent(5, cx)))
            .on_action(cx.listener(|this, _: &Agent7, _, cx| this.go_to_agent(6, cx)))
            .on_action(cx.listener(|this, _: &Agent8, _, cx| this.go_to_agent(7, cx)))
            .on_action(cx.listener(|this, _: &Agent9, _, cx| this.go_to_agent(8, cx)))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, w, cx| {
                this.drag_divider(e.position, cx);
                this.drag_sidebar(e.position.x, cx);
                this.drag_panel(e.position.x, w, cx);
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.end_divider(cx);
                this.end_sidebar_drag(cx);
                this.end_panel_drag(cx);
            }))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.end_divider(cx);
                this.end_sidebar_drag(cx);
                this.end_panel_drag(cx);
            }))
            // one reset of the selectable markdown's paint list for the frame, before the PR tab's description
            // and logs and the viewer's preview paint theirs
            .child(crate::ui::markdown::frame_reset())
            .child(self.drag_strip())
            .children(self.resize_cursor())
            .when(sidebar_w > 0. && !on_page, |el| {
                el.child(
                    div()
                        .w(px(sidebar_w))
                        .h_full()
                        .flex_none()
                        .overflow_hidden()
                        .bg(theme.wash(0.05))
                        // a resize edge: the pane dividers' resting line, the seam lit over it on hover or drag
                        .border_r_1()
                        .border_color(theme.border_strong)
                        .children(docked),
                )
            })
            .map(|el| match settings_page {
                Some(page) => el.child(page),
                None => el.child(div().flex_1().min_w_0().h_full().flex().flex_col().child(tabbar).map(|el| match web_area {
                    Some(area) => el.child(area),
                    None => el.child(grid),
                }).children(statusbar)),
            })
            .children(panel.into_iter().flatten())
            // The sidebar closed: the window's left edge brings it out floating over the panes, as Arc's does.
            .when(sidebar::peeks(self.sidebar_open, sidebar_w, on_page, peek_on), |el| {
                el.child(
                    div()
                        .id("sidebar-peek-edge")
                        .absolute()
                        .top(px(TITLEBAR_HEIGHT))
                        .bottom_0()
                        .left_0()
                        .w(px(6.))
                        .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover_peek(*on, cx))),
                )
                // deferred: drawn above a web page, which sits above the rest
                .when(peek > 0., |el| {
                    el.child(gpui::deferred(
                        div()
                            .id("sidebar-peek")
                            .occlude()
                            .absolute()
                            .top(px(6.))
                            .bottom(px(6.))
                            .left(px(6. - (self.sidebar_width + 12.) * (1. - peek)))
                            .w(px(self.sidebar_width))
                            .opacity(peek)
                            .overflow_hidden()
                            .rounded(px(10.))
                            .bg(theme.card)
                            .border_1()
                            .border_color(theme.border)
                            .shadow_lg()
                            .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover_peek(*on, cx)))
                            .children(peeking),
                    ))
                })
            })
            // The sidebar's edge: drag to resize, double-click for the default width.
            .when(self.sidebar_open && !sliding && !on_page, |el| {
                el.child(
                    div()
                        .id("sidebar-resize")
                        .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover_seam("sidebar-resize", *on, cx)))
                        .absolute()
                        .top(px(TITLEBAR_HEIGHT))
                        .bottom_0()
                        .left(px(sidebar_w - 3.))
                        .w(px(6.))
                        .cursor_col_resize()
                        // The sidebar's 1 px border sits at x = 2 of the strip.
                        .child(resize_seam(
                            "sidebar-resize".into(),
                            true,
                            2.,
                            theme.accent,
                            self.sidebar_drag,
                            self.seam_hover.get("sidebar-resize").copied(),
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.sidebar_drag = true;
                                cx.set_global(GridHold(true));
                                cx.notify();
                            }),
                        )
                        .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                            if e.click_count() == 2 {
                                this.sidebar_width = SIDEBAR_WIDTH;
                                this.save_sidebar_width(cx);
                                cx.notify();
                            }
                        })),
                )
            })
            .when(!on_page, |el| {
                el.child(
                    titlebar_button(&theme, "toggle-sidebar", "icons/sidebar-minimalistic-left.svg")
                        .tooltip(tip("Toggle sidebar", Some("ToggleSidebar")))
                        .absolute()
                        .top(px(TRAFFIC_LIGHTS_CENTER - 12.))
                        .left(px(TOGGLE_LEFT))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_sidebar(cx);
                        })),
                )
            })
            // Zeron's sidebar `…`: in the titlebar at the sidebar's right edge, hidden once the sidebar is too
            // narrow to hold it beside the toggle.
            .when(sidebar_w >= TITLEBAR_CLUSTER + 32. && !on_page, |el| {
                el.child(
                    titlebar_button(&theme, "sidebar-view", "icons/more-horizontal.svg")
                        .tooltip(tip("Sidebar view", None))
                        .absolute()
                        .top(px(TRAFFIC_LIGHTS_CENTER - 12.))
                        .left(px(sidebar_w - 8. - 24.))
                        .when(self.view_menu.as_ref().is_some_and(|m| m.is_open()), |el| el.bg(theme.hover))
                        // Capture, so a press on the open menu's trigger closes it instead of the menu's
                        // click-outside closing it and the click reopening it.
                        .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            match this.view_menu.as_mut() {
                                Some(menu) if menu.is_open() => menu.close(),
                                _ => {
                                    this.view_menu = Some(crate::ui::Popup::new(()));
                                    this.show_submenu = false;
                                }
                            }
                            cx.notify();
                        })),
                )
            });
        self.popups(root, &theme, &snapshot, sidebar_w, window, cx)
            .children(spring_back)
            .when(window_buttons(cx), |el| {
                el.children(window_controls(
                    &theme,
                    window,
                    cx.listener(|s, _: &ClickEvent, window, cx| {
                        s.save_frame(cx);
                        if !s.ask_unsaved(file_tabs::Exit::Close, window, cx) {
                            window.remove_window();
                        }
                    }),
                ))
            })
            .children(resize_edges(window))
            .child(toasts)
    }
}

#[cfg(test)]
mod tests {
    use super::{KEPT_PANES, prune, remember_shown, stale};
    use crate::model::{Frac, PaneFrame};
    use std::collections::HashMap;

    #[test]
    fn a_vanished_target_is_stale_other_errors_are_not() {
        assert!(stale(&anyhow::anyhow!("tab_not_found: tab wAB:t1P not found")));
        assert!(stale(&anyhow::anyhow!("pane_not_found: pane  not found")));
        assert!(!stale(&anyhow::anyhow!("herdr is not reachable at /tmp/herdr.sock")));
        assert!(!stale(&anyhow::anyhow!("invalid_params: not_found in a message")));
    }

    fn shown(ids: &[&str]) -> Vec<PaneFrame> {
        let frame = Frac { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        ids.iter().map(|id| PaneFrame { pane_id: id.to_string(), frame, focused: false }).collect()
    }

    /// Surfaces stand in as "has its process exited?".
    fn surfaces(entries: &[(&str, bool)]) -> HashMap<String, bool> {
        entries.iter().map(|(id, exited)| (id.to_string(), *exited)).collect()
    }

    #[test]
    fn recent_tabs_keep_their_panes() {
        let mut recent = Vec::new();
        let all = |_: &str| true;
        remember_shown(&mut recent, &shown(&["w1:p1", "w1:p2"]), all);
        remember_shown(&mut recent, &shown(&["w2:p1"]), all); // another tab
        assert_eq!(recent, ["w2:p1", "w1:p1", "w1:p2"], "the hidden tab's panes stay");
        remember_shown(&mut recent, &shown(&["w1:p1", "w1:p2"]), |id| id != "w2:p1"); // back, w2:p1 closed
        assert_eq!(recent, ["w1:p1", "w1:p2"]);
        for i in 0..KEPT_PANES {
            remember_shown(&mut recent, &shown(&[&format!("w9:p{i}")]), all);
        }
        assert_eq!(recent.len(), KEPT_PANES);
        assert!(!recent.contains(&"w1:p1".to_string()), "the oldest go first");
    }

    #[test]
    fn hidden_panes_lose_their_surface_and_focus() {
        let mut s = surfaces(&[("w1:p1", false), ("w1:p2", false)]);
        let mut focused = Some("w1:p2".to_string());
        prune(&mut s, &mut focused, &["w1:p1".to_string()], |exited| *exited);
        assert_eq!(s.keys().collect::<Vec<_>>(), ["w1:p1"]);
        assert_eq!(focused, None, "a recreated surface for w1:p2 must get focus again");
    }

    #[test]
    fn exited_surfaces_are_dropped_to_be_recreated() {
        let mut s = surfaces(&[("w1:p1", true)]);
        let mut focused = Some("w1:p1".to_string());
        prune(&mut s, &mut focused, &["w1:p1".to_string()], |exited| *exited);
        assert!(s.is_empty());
        assert_eq!(focused, None);
    }

    #[test]
    fn live_shown_surfaces_keep_focus() {
        let mut s = surfaces(&[("w1:p1", false)]);
        let mut focused = Some("w1:p1".to_string());
        prune(&mut s, &mut focused, &["w1:p1".to_string()], |exited| *exited);
        assert_eq!(s.len(), 1);
        assert_eq!(focused.as_deref(), Some("w1:p1"));
    }
}
