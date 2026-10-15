//! The tab bar: Zeron's terminal-panel tabs (28 px, 8 px radius, a wash under the active one) with what runs in
//! each tab as a colored favicon (Superlogical's tabs), sized to their labels. A new tab grows in, a closed one
//! stays mounted while it shrinks out (the `Popup` recipe), the active wash slides between tabs, a reorder
//! slides the tabs to their new places, and a dropped tab flies from where it was let go back into its slot.
//! The workspace's file tabs (`file_tabs`) follow its herdr tabs past a thin divider: a file's icon, its name (in
//! italics while a preview), and the same trailing slot, which holds an unsaved-changes dot instead of a status.
use super::*;
use crate::files::tabs::TabId;
use std::cell::RefCell;

/// Zeron's `motion::COLLAPSE` for a tab growing in or shrinking out; the wash and a reorder slide a bit quicker
/// than its `TAB_SLIDE`.
const ENTER: Duration = Duration::from_millis(180);
const EXIT: Duration = Duration::from_millis(150);
const SLIDE: Duration = Duration::from_millis(110);
/// A dropped tab's flight home.
const LAND: Duration = Duration::from_millis(220);

const TAB_H: f32 = 28.;
/// The tabs' top inside the bar.
const TAB_TOP: f32 = (TITLEBAR_HEIGHT - TAB_H) / 2.;
/// Space after each tab; it collapses with a closing tab so neighbours close in smoothly.
const GAP: f32 = 2.;
const PAD_L: f32 = 6.;
const PAD_R: f32 = 4.;
const INNER_GAP: f32 = 6.;
/// The favicon tile.
const ICON: f32 = 18.;
/// The trailing slot: the status dot, swapped in place for `×` while the tab is hovered.
const SLOT: f32 = 20.;
const ZOOM_MARK: f32 = 12.;
const MIN_W: f32 = 96.;
const MAX_W: f32 = 220.;
const FONT_SIZE: f32 = 12.5;
/// The divider between herdr's tabs and the file tabs: its slot's width, and the line's height in it.
const DIVIDER_W: f32 = 9.;
const DIVIDER_H: f32 = 16.;
/// A file tab's icon, in the favicon's slot: the Files tree's size.
const FILE_ICON: f32 = 16.;

/// Tab-bar state that outlives a frame: the scroll, the tabs as last drawn (closing ones included), the wash,
/// and the tab being dragged with its ghost's top-left in the window (written by the ghost as it follows the pointer).
#[derive(Default)]
pub(super) struct TabBar {
    pub scroll: gpui::ScrollHandle,
    workspace: Option<String>,
    entries: Vec<Entry>,
    wash: Option<Wash>,
    drag: Rc<RefCell<Option<(String, gpui::Point<Pixels>)>>>,
    /// The tab last scrolled into view; the bar follows a newly shown tab once, then leaves the scroll to you.
    revealed: Option<String>,
}

/// What a slot in the bar is.
#[derive(Clone, Copy, PartialEq)]
enum Face {
    Herdr,
    /// A file tab: italic while a preview; `dirty` puts a dot in the trailing slot.
    File { tab: TabId, preview: bool, dirty: bool },
    /// The line between herdr's tabs and the file tabs.
    Divider,
}

/// One tab as drawn.
#[derive(Clone)]
struct Shown {
    face: Face,
    /// herdr's tab id; a file tab's is `file:<n>`.
    id: String,
    label: String,
    status: AgentStatus,
    icon: &'static str,
    /// The favicon's tile and mark colors.
    tint: (Hsla, Hsla),
    /// One of its panes is zoomed to fill it: the other panes are hidden.
    zoomed: bool,
    /// The full width: label measured, clamped (`tab_width`). The slot is laid out and drawn at this width, so the
    /// wash, a slide and the scroll into view land on the tab itself.
    width: f32,
}

#[derive(Clone)]
struct Entry {
    tab: Shown,
    enter: Option<Instant>,
    exit: Option<Instant>,
    /// Off its slot: sliding after a reorder, or flying home after a drop (`lifted`, drawn over everything).
    moved: Option<Move>,
    /// Where it was drawn last frame: content x and the offset from the bar's row.
    drawn: (f32, f32),
}

#[derive(Clone, Copy)]
struct Move {
    from: (f32, f32),
    started: Instant,
    duration: Duration,
    lifted: bool,
}

impl Entry {
    fn new(tab: Shown, enter: Option<Instant>) -> Self {
        Entry { tab, enter, exit: None, moved: None, drawn: (0., 0.) }
    }

    /// How far the tab is in: 0 collapsed, 1 full width.
    fn presence(&self, now: Instant) -> f32 {
        match (self.exit, self.enter) {
            (Some(since), _) => 1. - ease_out(progress(since, EXIT, now)),
            (None, Some(since)) => ease_out(progress(since, ENTER, now)),
            (None, None) => 1.,
        }
    }

    fn moving(&self, now: Instant) -> bool {
        let running = |since: Instant, d: Duration| now.duration_since(since) < d;
        self.exit.is_some_and(|s| running(s, EXIT))
            || self.enter.is_some_and(|s| running(s, ENTER))
            || self.moved.is_some_and(|m| running(m.started, m.duration))
    }

    /// Mid-flight home after a drop.
    fn lifted(&self, now: Instant) -> bool {
        self.moved.is_some_and(|m| m.lifted && now.duration_since(m.started) < m.duration)
    }
}

fn progress(since: Instant, d: Duration, now: Instant) -> f32 {
    (now.duration_since(since).as_secs_f32() / d.as_secs_f32()).min(1.)
}

/// The active wash: where it slides from, when it set off, and where it was last drawn (x, width).
struct Wash {
    tab: String,
    from: (f32, f32),
    started: Instant,
    shown: (f32, f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Stay,
    New,
    Gone,
}

/// The tabs to draw: `now` in herdr's order, with each tab gone since `prev` kept right after the tab it
/// followed (or first), so it shrinks out in place.
fn merge(prev: &[&str], now: &[&str]) -> Vec<(String, Phase)> {
    let mut out: Vec<(String, Phase)> =
        now.iter().map(|id| (id.to_string(), if prev.contains(id) { Phase::Stay } else { Phase::New })).collect();
    for (i, id) in prev.iter().enumerate() {
        if now.contains(id) {
            continue;
        }
        let anchor = prev[..i].iter().rev().find_map(|p| out.iter().position(|(o, _)| o == p));
        out.insert(anchor.map_or(0, |a| a + 1), (id.to_string(), Phase::Gone));
    }
    out
}

/// The icon for what runs in a tab: the agent's mark (Zeron's harness icons), a bot for agents without one,
/// a terminal for a plain shell.
fn agent_icon(agent: Option<&str>) -> &'static str {
    match agent.map(crate::ui::command_panel::agent_icon) {
        None => "icons/prompt.svg",
        Some("icons/bot.svg") => "icons/bot-filled.svg",
        Some(mark) => mark,
    }
}

/// The favicon's tile and mark: the brand's colors (black for monochrome brands, as their app icons are), an
/// inverted tile for agents without a mark, a muted one for a shell.
fn agent_tint(agent: Option<&str>, theme: &Theme) -> (Hsla, Hsla) {
    let brand = |bg: u32, fg: u32| (gpui::rgb(bg).into(), gpui::rgb(fg).into());
    match agent {
        None => (theme.text_muted.opacity(0.55), theme.surface),
        Some("claude") => brand(0xD97757, 0xFFFFFF),
        Some("codex") => brand(0x10A37F, 0xFFFFFF),
        Some("agy" | "gemini") => brand(0x4285F4, 0xFFFFFF),
        // cursor.com's signature orange
        Some("cursor") => brand(0xF54E00, 0xFFFFFF),
        // Cognition's Devin palette blue
        Some("devin") => brand(0x0294DE, 0xFFFFFF),
        // ponytail: pi.dev's yellow accent; pi has no published brand color
        Some("pi" | "omp") => brand(0xF1BE58, 0x1A1A1A),
        // ponytail: Qwen's logo purple, by eye
        Some("qwen") => brand(0x615CED, 0xFFFFFF),
        // monochrome brands
        Some("grok" | "opencode" | "copilot" | "droid" | "kimi" | "kilo" | "qoder" | "qodercli" | "mastra" | "mastracode") => {
            brand(0x0A0A0A, 0xFFFFFF)
        }
        Some(_) => (theme.text, theme.surface),
    }
}

/// `a` toward `b` by `t`, channel by channel.
fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let l = |x: f32, y: f32| x + (y - x) * t;
    gpui::hsla(l(a.h, b.h), l(a.s, b.s), l(a.l, b.l), l(a.a, b.a))
}

/// The scroll (px scrolled from the left) that brings the slot `x..x + w` into a `view`-wide bar, with a
/// fade's width to spare; `None` when it already shows.
fn reveal(scrolled: f32, view: f32, x: f32, w: f32) -> Option<f32> {
    const SPARE: f32 = 32.;
    if x < scrolled {
        Some((x - SPARE).max(0.))
    } else if x + w > scrolled + view {
        Some(x + w - view + SPARE)
    } else {
        None
    }
}

/// A tab's full width: its parts around the measured label, clamped; the widest while its name is being edited,
/// for the field.
fn tab_width(label_w: f32, zoomed: bool, renaming: bool) -> f32 {
    if renaming {
        return MAX_W;
    }
    let mark = if zoomed { ZOOM_MARK + INNER_GAP } else { 0. };
    (PAD_L + ICON + INNER_GAP + label_w + mark + INNER_GAP + SLOT + PAD_R).clamp(MIN_W, MAX_W).ceil()
}

/// After the label of a tab with a zoomed pane, the zoom action's icon.
fn zoom_mark(theme: &Theme) -> gpui::Svg {
    gpui::svg().path("icons/expand-arrows.svg").size(px(ZOOM_MARK)).flex_none().text_color(theme.text_muted)
}

impl TabBar {
    /// Takes this frame's tabs; another workspace's tabs replace the old ones without motion. A reorder slides
    /// the tabs from where they were drawn.
    fn sync(&mut self, workspace: Option<&str>, tabs: Vec<Shown>, now: Instant) {
        if self.workspace.as_deref() != workspace {
            self.workspace = workspace.map(str::to_string);
            self.entries = tabs.into_iter().map(|tab| Entry::new(tab, None)).collect();
            self.wash = None;
            return;
        }
        let prev: Vec<&str> = self.entries.iter().map(|e| e.tab.id.as_str()).collect();
        let ids: Vec<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
        let kept = |list: &[&str]| -> Vec<String> {
            list.iter().filter(|id| prev.contains(id) && ids.contains(id)).map(|id| id.to_string()).collect()
        };
        let reordered = kept(&prev) != kept(&ids);
        let order = merge(&prev, &ids);
        let entries = order
            .into_iter()
            .filter_map(|(id, phase)| {
                let old = self.entries.iter().find(|e| e.tab.id == id);
                let fresh = || tabs.iter().find(|t| t.id == id).cloned();
                match phase {
                    Phase::New => Some(Entry::new(fresh()?, Some(now))),
                    Phase::Stay => {
                        let old = old?;
                        let moved = if reordered {
                            let lifted = old.lifted(now);
                            let duration = if lifted { LAND } else { SLIDE };
                            Some(Move { from: old.drawn, started: now, duration, lifted })
                        } else {
                            old.moved
                        };
                        Some(Entry { tab: fresh()?, exit: None, moved, ..old.clone() })
                    }
                    Phase::Gone => {
                        let old = old?;
                        let exit = old.exit.unwrap_or(now);
                        (now.duration_since(exit) < EXIT).then(|| Entry { exit: Some(exit), ..old.clone() })
                    }
                }
            })
            .collect();
        self.entries = entries;
    }

    /// Sends the tab `id` flying home from `from` (content x, offset from the row).
    fn land(&mut self, id: &str, from: (f32, f32), now: Instant) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.tab.id == id) {
            e.moved = Some(Move { from, started: now, duration: LAND, lifted: true });
        }
    }

    /// Each entry's slot x, full width and presence, laid out left to right; records where each is drawn
    /// (its slot, or on its way there).
    fn layout(&mut self, now: Instant) -> Vec<(f32, f32, f32)> {
        let mut x = 0.;
        self.entries
            .iter_mut()
            .map(|e| {
                let k = e.presence(now);
                let at = x;
                x += (e.tab.width + GAP) * k;
                e.drawn = match e.moved {
                    Some(m) => {
                        let t = ease_out(progress(m.started, m.duration, now));
                        (m.from.0 + (at - m.from.0) * t, m.from.1 * (1. - t))
                    }
                    None => (at, 0.),
                };
                (at, e.tab.width, k)
            })
            .collect()
    }

    /// Where the active wash is drawn this frame (x, width): sliding from where it was toward `selected`.
    /// Hidden while the tab is dragged or flies home (the tab carries its own look), it then reappears in the
    /// slot without sliding.
    fn wash(&mut self, selected: Option<&str>, dragged: Option<&str>, layout: &[(f32, f32, f32)], now: Instant) -> Option<(f32, f32)> {
        let i = self.entries.iter().position(|e| Some(e.tab.id.as_str()) == selected && e.exit.is_none())?;
        if dragged == selected || self.entries[i].lifted(now) {
            self.wash = None;
            return None;
        }
        let (_, w, k) = layout[i];
        let target = (self.entries[i].drawn.0, w * k);
        let wash = match self.wash.take() {
            Some(w) if w.tab == self.entries[i].tab.id => w,
            Some(w) => Wash { tab: self.entries[i].tab.id.clone(), from: w.shown, started: now, shown: w.shown },
            None => Wash { tab: self.entries[i].tab.id.clone(), from: target, started: now - SLIDE, shown: target },
        };
        let t = ease_out(progress(wash.started, SLIDE, now));
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        let shown = (lerp(wash.from.0, target.0), lerp(wash.from.1, target.1));
        self.wash = Some(Wash { shown, ..wash });
        Some(shown)
    }

    fn sliding(&self, now: Instant) -> bool {
        self.wash.as_ref().is_some_and(|w| now.duration_since(w.started) < SLIDE)
    }
}

/// The favicon: the mark on a filled tile of its color, the same size for every kind; a file's icon, bare.
fn favicon(t: &Shown) -> Div {
    match t.face {
        Face::File { .. } => div().size(px(ICON)).flex_none().flex().items_center().justify_center().child(gpui::img(t.icon).size(px(FILE_ICON)).object_fit(gpui::ObjectFit::Contain)),
        _ => tile(t.icon, t.tint),
    }
}

/// A tab's favicon for `agent`, for lists of agents elsewhere.
pub(crate) fn agent_favicon(agent: Option<&str>, theme: &Theme) -> Div {
    tile(agent_icon(agent), agent_tint(agent, theme))
}

fn tile(icon: &'static str, (bg, fg): (Hsla, Hsla)) -> Div {
    div()
        .size(px(ICON))
        .flex_none()
        .rounded(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        // a black tile would sink into a dark bar: a hairline keeps its edge
        .when(bg.l < 0.1, |el| el.border_1().border_color(gpui::white().opacity(0.14)))
        .child(gpui::svg().path(icon).size(px(11.)).text_color(fg))
}

/// A tab's face at full width: favicon, label, trailing slot.
fn face(theme: &Theme, t: &Shown, width: f32, selected: bool) -> Div {
    div()
        .w(px(width))
        .h(px(TAB_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(INNER_GAP))
        .pl(px(PAD_L))
        .pr(px(PAD_R))
        .rounded(px(8.))
        .font_family(theme.font_ui.clone())
        .text_size(px(FONT_SIZE))
        .text_color(if selected { theme.text } else { theme.text_muted })
        .when(matches!(t.face, Face::File { preview: true, .. }), |el| el.italic())
        .child(favicon(t))
}

/// The trailing slot: a herdr tab's status dot; a file tab's unsaved-changes dot, else nothing.
fn dot_slot(theme: &Theme, t: &Shown) -> Div {
    let slot = div().size(px(SLOT)).flex_none().flex().items_center().justify_center();
    match t.face {
        Face::File { dirty: true, .. } => slot.child(div().size(px(8.)).rounded_full().bg(theme.text_muted)),
        Face::File { .. } | Face::Divider => slot,
        Face::Herdr => slot.child(status_dot(t.status, theme)),
    }
}

/// The divider's slot: a hairline, centred.
fn divider(theme: &Theme, k: f32) -> gpui::AnyElement {
    let line = div().w(px(1.)).h(px(DIVIDER_H)).bg(theme.border_strong);
    div().flex_none().h(px(TAB_H)).w(px(DIVIDER_W * k)).mr(px(GAP * k)).opacity(k).flex().items_center().justify_center().child(line).into_any_element()
}

/// A file tab's id in the bar.
fn file_entry_id(tab: TabId) -> String {
    format!("file:{}", tab.0)
}

/// A tab being dragged along the tab bar: its own face under the pointer, lifted on a card.
#[derive(Clone)]
pub(super) struct DraggedTab {
    tab: Shown,
    index: usize,
    theme: Theme,
    /// Where the pointer grabbed the tab, from its top-left.
    grab: gpui::Point<Pixels>,
    drag: Rc<RefCell<Option<(String, gpui::Point<Pixels>)>>>,
}

impl Render for DraggedTab {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // gpui draws the ghost at the pointer less the grab: remember it, a drop flies home from there
        *self.drag.borrow_mut() = Some((self.tab.id.clone(), window.mouse_position() - self.grab));
        let theme = &self.theme;
        face(theme, &self.tab, self.tab.width, true)
            .bg(theme.card)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .child(div().flex_1().min_w_0().truncate().child(self.tab.label.clone()))
            .when(self.tab.zoomed, |el| el.child(zoom_mark(theme)))
            .child(dot_slot(theme, &self.tab))
    }
}

impl Shell {
    pub(super) fn render_tab_bar(
        &mut self,
        theme: &Theme,
        shown_ws: Option<String>,
        shown_tab: Option<String>,
        sidebar_w: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let now = Instant::now();
        let font = gpui::font(theme.font_ui.clone());
        let label_w = |label: &str, window: &mut Window| {
            let run = gpui::TextRun { len: label.len(), font: font.clone(), color: theme.text, background_color: None, underline: None, strikethrough: None };
            f32::from(window.text_system().shape_line(label.to_string().into(), px(FONT_SIZE), &[run], None).width)
        };
        let mut tabs: Vec<Shown> = match (&self.snapshot, &shown_ws) {
            (Some(s), Some(ws)) => {
                let agents = s.agents();
                s.tabs_of(ws)
                    .into_iter()
                    .map(|t| {
                        let label_w = label_w(&t.label, window);
                        let agent = agents.iter().find(|p| p.tab_id == t.tab_id).and_then(|p| p.agent.as_deref());
                        let zoomed = s.layouts.iter().any(|l| l.tab_id == t.tab_id && l.zoomed);
                        let renaming = self.renaming.as_ref().is_some_and(|r| r.target == RenameTarget::Tab(t.tab_id.clone()));
                        Shown {
                            face: Face::Herdr,
                            id: t.tab_id.clone(),
                            label: t.label.clone(),
                            status: t.agent_status,
                            icon: agent_icon(agent),
                            tint: agent_tint(agent, theme),
                            zoomed,
                            width: tab_width(label_w, zoomed, renaming),
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        // the workspace's file tabs, past a divider
        let files_key = shown_ws.as_deref().and_then(|ws| self.tabs_key(ws));
        let file_tabs = files_key.as_ref().and_then(|k| self.file_tabs.get(k)).map(|t| t.files.clone()).unwrap_or_default();
        if !file_tabs.is_empty() {
            let blank = (theme.text_muted, theme.text_muted);
            tabs.push(Shown { face: Face::Divider, id: "file-divider".into(), label: String::new(), status: AgentStatus::Idle, icon: "", tint: blank, zoomed: false, width: DIVIDER_W });
        }
        for f in &file_tabs {
            // a diff's label has its group after the name
            let label = f.label();
            let width = tab_width(label_w(&label, window), false, false);
            // ponytail: nothing is dirty until tabs edit (E1)
            let face = Face::File { tab: f.id, preview: f.preview, dirty: false };
            let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let icon = crate::files::icons::material_icon(&name, false, false);
            tabs.push(Shown { face, id: file_entry_id(f.id), label, status: AgentStatus::Idle, icon, tint: (theme.text_muted, theme.text_muted), zoomed: false, width });
        }
        // the bar's selected slot: the file tab shown, else herdr's tab
        let shown_tab = match self.shown_file_tab() {
            Some((_, f)) => Some(file_entry_id(f.id)),
            None => shown_tab,
        };
        self.tab_bar.sync(shown_ws.as_deref(), tabs, now);

        // The drag ended (dropped on a tab or anywhere): the tab flies home from where its ghost was let go.
        let bar = self.tab_bar.scroll.bounds();
        let (bar_x, bar_y) = (f32::from(bar.origin.x) + f32::from(self.tab_bar.scroll.offset().x), f32::from(bar.origin.y) + TAB_TOP);
        let dragging = cx.has_active_drag();
        let dragged = self.tab_bar.drag.borrow().clone();
        if let (false, Some((id, at))) = (dragging, &dragged) {
            self.tab_bar.drag.borrow_mut().take();
            self.tab_bar.land(id, (f32::from(at.x) - bar_x, f32::from(at.y) - bar_y), now);
        }
        let lifted_id = dragged.filter(|_| dragging).map(|(id, _)| id);

        let layout = self.tab_bar.layout(now);
        let wash = self.tab_bar.wash(shown_tab.as_deref(), lifted_id.as_deref(), &layout, now);
        // A newly shown tab scrolls into view, followed while it grows in (gpui clamps the offset to the content).
        if let Some(i) = self.tab_bar.entries.iter().position(|e| Some(&e.tab.id) == shown_tab.as_ref() && e.exit.is_none()) {
            let entry = &self.tab_bar.entries[i];
            if self.tab_bar.revealed.as_ref() != shown_tab.as_ref() || entry.enter.is_some_and(|s| now.duration_since(s) < ENTER) {
                let scroll = &self.tab_bar.scroll;
                let view = f32::from(scroll.bounds().size.width);
                if view > 0. {
                    if let Some(to) = reveal(-f32::from(scroll.offset().x), view, layout[i].0, entry.tab.width) {
                        scroll.set_offset(gpui::point(px(-to), scroll.offset().y));
                    }
                    self.tab_bar.revealed = shown_tab.clone();
                }
            }
        }
        if self.tab_bar.entries.iter().any(|e| e.moving(now)) || self.tab_bar.sliding(now) {
            window.request_animation_frame();
        }

        // held Ctrl (Cmd on macOS): each tab's Tab1–9 chord over its right end, numbered along the strip as
        // `go_to_tab` counts (herdr's tabs, then the file tabs)
        let hint = self.hints_of(hints::HintMode::Tabs).map(|o| (o, TAB_ACTIONS.map(|a| chord_of(cx, a))));
        let mut strip_at = 0;
        let mut index = 0;
        let mut items = Vec::new();
        let mut flying = Vec::new();
        for (entry, (slot_x, full_w, k)) in self.tab_bar.entries.clone().into_iter().zip(layout) {
            let t = entry.tab.clone();
            if t.face == Face::Divider {
                items.push(divider(theme, k));
                continue;
            }
            let chip = match &hint {
                Some((opacity, chords)) if entry.exit.is_none() => {
                    let chord = chords.get(strip_at).cloned().flatten();
                    chord.map(|c| hints::chip(theme, &c, *opacity))
                }
                _ => None,
            };
            if entry.exit.is_none() {
                strip_at += 1;
            }
            let file = match t.face {
                Face::File { tab, .. } => files_key.clone().map(|key| (key, tab)),
                _ => None,
            };
            let closing = entry.exit.is_some();
            let selected = !closing && shown_tab.as_deref() == Some(t.id.as_str());
            let editing = (!closing && file.is_none()).then(|| self.rename_field(&RenameTarget::Tab(t.id.clone()), cx)).flatten();
            // ponytail: file tabs are reordered by drag in X1
            let draggable = editing.is_none() && file.is_none();
            // While dragged, the ghost carries the tab; while it flies home, a copy over everything does.
            let lifted = lifted_id.as_deref() == Some(t.id.as_str()) || entry.lifted(now);
            if entry.lifted(now) {
                let t_land = ease_out(entry.moved.map_or(1., |m| progress(m.started, m.duration, now)));
                let at = gpui::point(px(bar_x + entry.drawn.0), px(bar_y + entry.drawn.1));
                flying.push(
                    gpui::deferred(
                        gpui::anchored().position(at).child(
                            face(theme, &t, full_w, selected)
                                // the card settles into the tab's own look: the wash if active, else bare
                                .bg(mix(theme.card, if selected { theme.selected } else { theme.card.opacity(0.) }, t_land))
                                .border_1()
                                .border_color(theme.border.opacity(1. - t_land))
                                .child(div().flex_1().min_w_0().truncate().child(t.label.clone()))
                                .when(t.zoomed, |el| el.child(zoom_mark(theme)))
                                .child(dot_slot(theme, &t)),
                        ),
                    )
                    .with_priority(1)
                    .into_any_element(),
                );
            }
            let group = SharedString::from(format!("tab-row-{}", t.id));
            // The face keeps its full width and is clipped while the tab grows or shrinks.
            let content = face(theme, &t, full_w, selected)
                .map(|el| match editing {
                    Some(field) => el.child(field),
                    None => el.child(div().flex_1().min_w_0().truncate().child(t.label.clone())),
                })
                .when(t.zoomed && draggable, |el| el.child(zoom_mark(theme)))
                .child(
                    div()
                        .id(SharedString::from(format!("tab-x-{}", t.id)))
                        .tooltip(tip("Close tab", Some("CloseTab")))
                        .size(px(SLOT))
                        .flex_none()
                        .relative()
                        .rounded(px(6.))
                        .hover(|s| s.bg(theme.hover))
                        .child(dot_slot(theme, &t).absolute().inset_0().group_hover(group.clone(), |s| s.opacity(0.)))
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.)
                                .group_hover(group.clone(), |s| s.opacity(1.))
                                .child(gpui::svg().path("icons/close.svg").size(px(12.)).text_color(theme.text_muted)),
                        )
                        .when(!closing, |el| {
                            let (id, file) = (t.id.clone(), file.clone());
                            // the tab owns a drag: claim the press so it closes instead of starting one
                            el.on_mouse_down(MouseButton::Left, |_, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                match &file {
                                    Some((key, tab)) => this.close_file_tab(key, *tab, window, cx),
                                    None => this.send(Command::CloseTab { tab_id: id.clone() }, cx, |_, _, _| {}),
                                }
                            }))
                        }),
                );
            let tab = div()
                .id(SharedString::from(format!("tab-{}", t.id)))
                .group(group)
                .relative()
                .left(px(entry.drawn.0 - slot_x))
                .flex_none()
                .h(px(TAB_H))
                .w(px(full_w * k))
                .mr(px(GAP * k))
                .overflow_hidden()
                .opacity(if lifted { 0. } else { k })
                .rounded(px(8.))
                .cursor_pointer()
                // off the titlebar's drag strip: a press here drags the tab, not the window; the wheel still scrolls the bar
                .block_mouse_except_scroll()
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .when(!selected && !closing, |el| el.hover(|s| s.bg(theme.hover)))
                .child(content)
                .children(chip);
            if closing {
                items.push(tab.into_any_element());
                continue;
            }
            // a file tab: a click shows it, a double-click pins it, a middle-click closes it
            if let Some((key, file)) = file {
                let (pin, close) = (key.clone(), key.clone());
                items.push(
                    tab.on_mouse_down(MouseButton::Middle, cx.listener(move |this, _: &MouseDownEvent, window, cx| this.close_file_tab(&close, file, window, cx)))
                        .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                            this.show_file_tab(&key, file, cx);
                            if e.click_count() == 2 {
                                this.pin_file_tab(&pin, file, cx);
                            }
                        }))
                        .into_any_element(),
                );
                continue;
            }
            let i = index;
            index += 1;
            let id = t.id.clone();
            let dragged = draggable.then(|| DraggedTab {
                tab: t.clone(),
                index: i,
                theme: theme.clone(),
                grab: gpui::Point::default(),
                drag: self.tab_bar.drag.clone(),
            });
            let drop_bg = theme.accent.opacity(0.15);
            let label = t.label.clone();
            items.push(
                tab.on_mouse_down(MouseButton::Right, cx.listener({
                    let id = id.clone();
                    move |this, e: &MouseDownEvent, _, cx| {
                        this.tab_menu = Some(crate::ui::Popup::new((id.clone(), e.position)));
                        cx.notify();
                    }
                }))
                // Middle-click closes, like every tab strip (Zeron §1.10).
                .on_mouse_down(MouseButton::Middle, cx.listener({
                    let id = id.clone();
                    move |this, _: &MouseDownEvent, _, cx| {
                        this.send(Command::CloseTab { tab_id: id.clone() }, cx, |_, _, _| {});
                    }
                }))
                .when_some(dragged, |el, dragged| {
                    el.on_drag(dragged, |d, grab, _, cx| {
                        let d = DraggedTab { grab, ..d.clone() };
                        cx.new(|_| d)
                    })
                })
                .map(|el| pane_drag::accept_pane(el, &id, drop_bg, cx))
                .drag_over::<DraggedTab>(move |s, d, _, _| if d.index == i { s } else { s.bg(drop_bg) })
                // Dropped on a tab, the dragged one takes its place: herdr's slot is after it when moving right.
                .on_drop(cx.listener(move |this, d: &DraggedTab, _, cx| {
                    if d.index != i {
                        let insert_index = if d.index < i { i + 1 } else { i };
                        this.send(Command::MoveTab { tab_id: d.tab.id.clone(), insert_index }, cx, |_, _, _| {});
                    }
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    if e.click_count() == 2 {
                        this.start_rename(RenameTarget::Tab(id.clone()), label.clone(), window, cx);
                        return;
                    }
                    this.show_herdr_tab(Some(id.clone()), true, cx);
                }))
                .into_any_element(),
            );
        }

        let scroll = &self.tab_bar.scroll;
        div()
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            // opaque, so the edge fades (same color) blend in exactly
            .bg(theme.surface)
            .flex()
            .items_center()
            .gap(px(2.))
            // the bar's last button (the right panel's toggle) ends as far from the right edge as the tabs sit from
            // the top and bottom, and clear of the window's buttons while no right panel sits under them
            .pr(px(TAB_TOP + if self.panel_open { 0. } else { window_controls_w(cx) }))
            // clear the traffic lights and the sidebar toggle once the sidebar no longer sits under them
            .pl(px((TITLEBAR_CLUSTER - sidebar_w).max(8.)))
            .child(
                div()
                    // off macOS the `+` follows the last tab, apart from the window's buttons at the right end
                    .when(cfg!(target_os = "macos"), |el| el.flex_1())
                    .min_w_0()
                    .h_full()
                    .relative()
                    .child(
                        div()
                            .id("tabbar")
                            // GPUI tells Windows the control area of every hitbox under the pointer, scroll-only
                            // blockers included: without blocking here a press on a tab is the drag strip's, and
                            // Windows moves the window. The bar still scrolls, being the blocker.
                            .when(cfg!(windows), |el| el.occlude())
                            .size_full()
                            .flex()
                            .items_center()
                            .overflow_x_scroll()
                            .track_scroll(scroll)
                            // re-render so the edge fades follow the scroll position
                            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                            // the active wash, drawn under the tabs, slides between them
                            .when_some(wash, |el, (x, w)| {
                                el.child(
                                    div()
                                        .absolute()
                                        .top(px(TAB_TOP))
                                        .left(px(x))
                                        .w(px(w))
                                        .h(px(TAB_H))
                                        .rounded(px(8.))
                                        .bg(theme.selected),
                                )
                            })
                            .children(items),
                    )
                    // ponytail: gpui has no alpha mask, so the fades are gradients of the bar's own color over the tabs
                    .map(|el| {
                        let (offset, max) = (-f32::from(scroll.offset().x), f32::from(scroll.max_offset().x));
                        let fade = |angle: f32| {
                            div().absolute().top_0().bottom_0().w(px(32.)).bg(linear_gradient(
                                angle,
                                linear_color_stop(theme.surface, 0.),
                                linear_color_stop(theme.surface.opacity(0.), 1.),
                            ))
                        };
                        el.when(offset > 0.5, |el| el.child(fade(90.).left_0()))
                            .when(offset < max - 0.5, |el| el.child(fade(270.).right_0()))
                    }),
            )
            .children(flying)
            // `+` stays in view; only the tabs scroll
            .when_some(shown_ws, |el, ws| {
                el.child(
                    icon_button(theme, "new-tab", "+")
                        .tooltip(tip("New tab", Some("NewTab")))
                        .size(px(TAB_H))
                        .rounded(px(8.))
                        .text_size(px(19.))
                        .block_mouse_except_scroll()
                        .when(cfg!(windows), |el| el.occlude()) // as the tabs, off the drag strip
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.new_tab(ws.clone(), cx))),
                )
            })
            // off macOS the tabs and the `+` take only their width: the rest of the bar goes before the toggle
            .when(!cfg!(target_os = "macos"), |el| el.child(div().flex_1().min_w_0()))
            // The right panel's toggle, at the titlebar's right end (the left one's mirror): the bar ends where the
            // panel begins, so it sits beside the panel's header when open and in the corner when closed.
            .child(
                titlebar_button(theme, "toggle-right-panel", "icons/sidebar-minimalistic-right.svg")
                    .flex_none()
                    .ml(px(2.))
                    .when(self.panel_open, |el| el.bg(theme.selected))
                    .tooltip(tip("Toggle right panel", Some("ToggleReview")))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.toggle_review(window, cx);
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: &str) -> Shown {
        Shown { face: Face::Herdr, id: id.into(), label: id.into(), status: AgentStatus::Idle, icon: "", tint: (gpui::black(), gpui::white()), zoomed: false, width: 100. }
    }

    #[test]
    fn closed_tabs_stay_in_place_and_new_ones_join() {
        use Phase::*;
        let got = merge(&["a", "b", "c", "d"], &["a", "c", "d", "e"]);
        let want = [("a", Stay), ("b", Gone), ("c", Stay), ("d", Stay), ("e", New)];
        assert_eq!(got, want.map(|(id, p)| (id.to_string(), p)));
        // the first tab closing keeps the front; neighbours closing together keep their order
        let got = merge(&["a", "b", "c"], &["c"]);
        assert_eq!(got, [("a", Gone), ("b", Gone), ("c", Stay)].map(|(id, p)| (id.to_string(), p)));
        // a reorder is herdr's new order
        assert_eq!(merge(&["a", "b"], &["b", "a"]), [("b", Stay), ("a", Stay)].map(|(id, p)| (id.to_string(), p)));
    }

    fn file(n: u64, label: &str, preview: bool) -> Shown {
        let width = tab_width(label.len() as f32 * 7., false, false);
        Shown { face: Face::File { tab: TabId(n), preview, dirty: false }, id: file_entry_id(TabId(n)), label: label.into(), width, ..tab("") }
    }

    #[test]
    fn a_tab_is_as_wide_as_its_label_unless_renamed() {
        // a file tab's short name keeps it narrow: it is drawn at this width, not stretched to the widest
        assert_eq!(tab_width(60., false, false), (PAD_L + ICON + INNER_GAP + 60. + INNER_GAP + SLOT + PAD_R).ceil());
        assert_eq!(tab_width(10., false, false), MIN_W);
        assert_eq!(tab_width(500., false, false), MAX_W);
        assert_eq!(tab_width(10., false, true), MAX_W); // the rename field's room
    }

    #[test]
    fn file_tabs_hold_their_slots_through_a_preview_replaced() {
        let t0 = Instant::now();
        let divider = Shown { face: Face::Divider, id: "file-divider".into(), width: DIVIDER_W, ..tab("") };
        let mut bar = TabBar::default();
        // TASKS.md pinned, README.md previewed to its right
        bar.sync(Some("w"), vec![tab("a"), divider.clone(), file(1, "TASKS.md", false), file(2, "README.md", true)], t0);
        let tasks_w = bar.entries[2].tab.width;
        assert!(tasks_w < MAX_W);
        let layout = bar.layout(t0);
        assert_eq!(layout[3].0, 100. + GAP + DIVIDER_W + GAP + tasks_w + GAP);
        // the next single click replaces the preview in place: the same slot, its new name's width
        bar.sync(Some("w"), vec![tab("a"), divider, file(1, "TASKS.md", false), file(2, "main.rs", true)], t0);
        assert_eq!(bar.entries.len(), 4);
        assert_eq!(bar.entries[3].tab.label, "main.rs");
        assert_eq!(bar.entries[3].presence(t0), 1.);
        assert_eq!(bar.layout(t0)[3], (layout[3].0, file(2, "main.rs", true).width, 1.));
    }

    #[test]
    fn the_shown_tab_scrolls_into_view() {
        assert_eq!(reveal(0., 300., 100., 100.), None);
        assert_eq!(reveal(0., 300., 400., 100.), Some(232.)); // off the right: its right edge plus a fade
        assert_eq!(reveal(500., 300., 100., 100.), Some(68.)); // off the left
        assert_eq!(reveal(50., 300., 10., 100.), Some(0.));
    }

    #[test]
    fn agents_get_their_mark() {
        assert_eq!(agent_icon(None), "icons/prompt.svg");
        assert_eq!(agent_icon(Some("claude")), "icons/claude-mark.svg");
        assert_eq!(agent_icon(Some("codex")), "icons/openai-mark.svg");
        assert_eq!(agent_icon(Some("kimi")), "icons/kimi-mark.svg");
        assert_eq!(agent_icon(Some("letta")), "icons/bot-filled.svg");
    }

    #[test]
    fn a_closing_tab_shrinks_out_then_goes() {
        let t0 = Instant::now();
        let mut bar = TabBar::default();
        bar.sync(Some("w"), vec![tab("a"), tab("b")], t0);
        bar.sync(Some("w"), vec![tab("b")], t0);
        assert_eq!(bar.entries.len(), 2);
        assert_eq!(bar.layout(t0)[1].0, 102.); // `a` still holds its slot as it starts to close
        bar.sync(Some("w"), vec![tab("b")], t0 + EXIT);
        assert_eq!(bar.entries.len(), 1);
        // another workspace swaps the tabs without motion
        bar.sync(Some("v"), vec![tab("c")], t0 + EXIT);
        assert_eq!(bar.entries[0].presence(t0 + EXIT), 1.);
    }

    #[test]
    fn reordered_and_dropped_tabs_slide_home() {
        let t0 = Instant::now();
        let mut bar = TabBar::default();
        bar.sync(Some("w"), vec![tab("a"), tab("b")], t0);
        bar.layout(t0);
        bar.sync(Some("w"), vec![tab("b"), tab("a")], t0);
        bar.layout(t0);
        // `a` starts where it was drawn and ends in its new slot
        assert_eq!(bar.entries[1].drawn, (0., 0.));
        bar.layout(t0 + SLIDE);
        assert_eq!(bar.entries[1].drawn, (102., 0.));
        // dropped 40 px below the bar, `b` flies home from there
        bar.land("b", (300., 40.), t0);
        bar.layout(t0);
        assert_eq!(bar.entries[0].drawn, (300., 40.));
        assert!(bar.entries[0].lifted(t0));
        bar.layout(t0 + LAND);
        assert_eq!(bar.entries[0].drawn, (0., 0.));
    }
}
