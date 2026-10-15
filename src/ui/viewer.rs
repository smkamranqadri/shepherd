//! The file viewer: one file, read-only, a file tab's view in the pane grid's place (the panes keep their size).
//! It closes as its tab does (Ctrl/Cmd+W, the tab's `×`), so it has no Esc or close button of its own. Text shows at
//! once in the terminal's font and size, highlighting lands when the runtime has it (`files::highlight`); markdown
//! previews through `ui::markdown`; images show through gpui; anything else says why not and offers the editor.
//!
//! The code view is a `uniform_list` (a 100k-line file lays out only the rows on screen), so gpui's own text
//! selection, which needs every paragraph painted, cannot serve it: it keeps its own, a line-and-byte anchor and
//! head, hit-tested against the rows painted last frame and washed through the rows' text runs.
use crate::files::content::{self, Content, MAX_TEXT, Text};
use crate::files::highlight::{self, Highlights, Token};
use crate::shell::widgets::{tip, titlebar_button};
use crate::theme::Theme;
use crate::ui::review::Block;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, DispatchPhase, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    ListHorizontalSizingBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, ScrollHandle, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, StyledText, TextLayout, TextRun, UniformListScrollHandle, Window, canvas, div, point, prelude::*, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::runtime::Handle;
use unicode_width::UnicodeWidthChar;

/// How long a read may take before the viewer gives up on it: a file on a hung mount never answers.
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
/// A tab draws as this many spaces (no tab stops: a byte's place on screen stays a sum, cheap both ways).
const TAB: usize = 4;
/// A line draws its first this many characters, then `…`: one minified line would otherwise be shaped whole on
/// every frame it shows, and stretch the horizontal scroll to megapixels. Copy still takes all of it.
const MAX_SHOWN: usize = 4000;
/// Markdown over this size opens on its source: the preview lays every block out on each frame.
const MAX_PREVIEW: usize = 512 * 1024;
/// The lines kept worked out for drawing (`Drawn`); past this many the cache starts over (a long scroll).
const MAX_DRAWN: usize = 2048;

pub enum ViewerEvent {
    /// "Open in editor", at a line when the viewer has one to give: the Shell shows the terminals (the editor's pane
    /// opens among them) and opens the file.
    OpenInEditor { path: PathBuf, line: Option<u32> },
}

/// A selection in the code view: where the press landed and where the drag is, as (line, byte in the line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sel {
    anchor: (usize, usize),
    head: (usize, usize),
}

impl Sel {
    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    /// Line `ix`'s selected bytes (of `len`), and whether its line end is selected too.
    fn on_line(&self, ix: usize, len: usize) -> Option<(Range<usize>, bool)> {
        let (start, end) = self.ordered();
        if ix < start.0 || ix > end.0 || start == end {
            return None;
        }
        let from = if ix == start.0 { start.1.min(len) } else { 0 };
        let to = if ix == end.0 { end.1.min(len) } else { len };
        Some((from..to.max(from), ix < end.0))
    }

    fn text(&self, text: &Text) -> String {
        let (start, end) = self.ordered();
        let mut out = String::new();
        for (ix, line) in text.lines().enumerate().take(end.0 + 1).skip(start.0) {
            if let Some((range, newline)) = self.on_line(ix, line.len()) {
                out.push_str(&line[range]);
                if newline {
                    out.push('\n');
                }
            }
        }
        out
    }
}

/// What the viewer shows of its file. Text (markdown too) lives in the viewer's own fields, shared with its
/// highlight; everything else is the `Content` the load answered: an image, a PDF, or why it is not shown.
enum Shown {
    Loading,
    Text,
    /// Never `Content::Text` or `Content::Markdown`.
    Other(Content),
}

/// What a load hands back from the runtime: the content, a markdown file's blocks and the text's widest line in
/// columns, all worked out there.
struct Loaded {
    content: Content,
    blocks: Option<Vec<Block>>,
    columns: usize,
}

/// A line as drawn, worked out once (when it first shows, and again when its highlight lands): every frame after
/// only splits its runs at the selection.
struct Drawn {
    /// Tabs widened, cut after `MAX_SHOWN` characters with a `…`.
    text: SharedString,
    /// The drawn bytes before the `…` (all of them when uncut).
    body: usize,
    /// The line's bytes the drawn text covers (up to the cut).
    raw_cut: usize,
    /// Where the line's tabs are (before the cut), for mapping a byte either way in a binary search.
    tabs: Vec<u32>,
    /// The tokens over `0..body`, in drawn bytes, gaps as `Plain`, in order.
    runs: Vec<(Range<usize>, Token)>,
}

impl Drawn {
    fn new(line: &str, spans: &[highlight::Span]) -> Drawn {
        let raw_cut = line.char_indices().nth(MAX_SHOWN).map_or(line.len(), |(i, _)| i);
        let tabs: Vec<u32> = line.as_bytes()[..raw_cut].iter().enumerate().filter(|(_, b)| **b == b'\t').map(|(i, _)| i as u32).collect();
        let mut text = line[..raw_cut].replace('\t', &" ".repeat(TAB));
        let body = text.len();
        if raw_cut < line.len() {
            text.push('…');
        }
        let mut drawn = Drawn { text: text.into(), body, raw_cut, tabs, runs: Vec::new() };
        let mut at = 0;
        for span in spans {
            let (start, end) = (drawn.to_shown(span.start as usize), drawn.to_shown(span.end as usize));
            if start >= body {
                break;
            }
            if start > at {
                drawn.runs.push((at..start, Token::Plain));
            }
            if end > start {
                drawn.runs.push((start..end, span.token));
            }
            at = at.max(end);
        }
        if at < body {
            drawn.runs.push((at..body, Token::Plain));
        }
        drawn
    }

    /// A byte of the line as a byte of the drawn text; past the cut, the cut.
    fn to_shown(&self, raw: usize) -> usize {
        let raw = raw.min(self.raw_cut);
        raw + self.tabs.partition_point(|t| (*t as usize) < raw) * (TAB - 1)
    }

    /// A byte of the drawn text as a byte of the line (`len`): inside a tab's spaces, the tab's start or end,
    /// whichever is nearer; on the `…`, the line's end.
    fn to_raw(&self, shown: usize, len: usize) -> usize {
        if shown >= self.body {
            return if self.raw_cut < len && shown > self.body { len } else { self.raw_cut };
        }
        // the tabs whose drawn start is at or before `shown`
        let before = self.tabs.iter().enumerate().take_while(|(j, t)| **t as usize + j * (TAB - 1) <= shown).count();
        let Some(j) = before.checked_sub(1) else { return shown };
        let (tab, start) = (self.tabs[j] as usize, self.tabs[j] as usize + j * (TAB - 1));
        match shown - start {
            0 => tab,
            inside if inside < TAB => tab + usize::from(inside * 2 >= TAB),
            _ => shown - before * (TAB - 1),
        }
    }

    /// The runs again, split where the selection (`sel`, drawn bytes) starts and ends: one pass.
    fn segments(&self, sel: Option<Range<usize>>) -> Vec<(Range<usize>, Token, bool)> {
        let Some(sel) = sel.filter(|s| !s.is_empty()) else { return self.runs.iter().map(|(r, t)| (r.clone(), *t, false)).collect() };
        let mut out = Vec::with_capacity(self.runs.len() + 2);
        for (r, t) in &self.runs {
            let cuts = [r.start, sel.start.clamp(r.start, r.end), sel.end.clamp(r.start, r.end), r.end];
            for pair in cuts.windows(2).filter(|p| p[0] < p[1]) {
                out.push((pair[0]..pair[1], *t, pair[0] >= sel.start && pair[1] <= sel.end));
            }
        }
        out
    }
}

/// A line's width in columns as drawn: tabs widened, wide characters two, cut after `MAX_SHOWN` with a `…`.
fn columns(line: &str) -> usize {
    let mut width = 0;
    for (count, c) in line.chars().enumerate() {
        if count == MAX_SHOWN {
            return width + 1;
        }
        width += if c == '\t' { TAB } else { c.width().unwrap_or(0) };
    }
    width
}

pub struct FileViewer {
    focus: FocusHandle,
    rt: Handle,
    path: PathBuf,
    /// The root the header shows the path under (the Files tab's), if the file is in it.
    root: Option<PathBuf>,
    /// Bumped by every open: a load or highlight that started for another file is dropped when it lands.
    generation: u64,
    shown: Shown,
    /// The text, shared with the highlight running on the runtime.
    text: Arc<Text>,
    /// The tokens of the first lines, as far as the highlight's budget went (the rest draw plain).
    highlights: Highlights,
    /// The lines on screen lately, worked out for drawing.
    drawn: HashMap<usize, Rc<Drawn>>,
    /// Set to stop the highlight running for the file shown before.
    cancel: Arc<AtomicBool>,
    blocks: Option<Vec<Block>>,
    /// Markdown shows as its preview, not its source.
    preview: bool,
    /// The widest line in columns, which every row is as wide as: the horizontal scroll's extent.
    columns: usize,
    scroll: UniformListScrollHandle,
    preview_scroll: ScrollHandle,
    /// The line to bring into view once the text is in.
    goto: Option<usize>,
    sel: Option<Sel>,
    /// When `sel` was made (`markdown::next_stamp`): Cmd+C copies the newer of it and a markdown selection.
    sel_at: u64,
    dragging: bool,
    /// The code rows painted last frame and their text layouts, for hit-testing a press or a drag.
    painted: Rc<RefCell<Vec<(usize, TextLayout)>>>,
}

impl EventEmitter<ViewerEvent> for FileViewer {}

impl Focusable for FileViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FileViewer {
    pub fn new(rt: Handle, cx: &mut Context<Self>) -> FileViewer {
        FileViewer {
            focus: cx.focus_handle(),
            rt,
            path: PathBuf::new(),
            root: None,
            generation: 0,
            shown: Shown::Loading,
            text: Arc::default(),
            highlights: Highlights::default(),
            drawn: HashMap::new(),
            cancel: Arc::default(),
            blocks: None,
            preview: true,
            columns: 0,
            scroll: UniformListScrollHandle::new(),
            preview_scroll: ScrollHandle::new(),
            goto: None,
            sel: None,
            sel_at: 0,
            dragging: false,
            painted: Rc::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Markdown's preview is on screen: its selection is `ui::markdown`'s, which must not be forgotten.
    pub fn shows_markdown(&self) -> bool {
        self.blocks.is_some() && self.preview
    }

    /// Shows `path` (at `line`, 1-based, when given) in place of what was shown. The read runs on the runtime under
    /// `LOAD_TIMEOUT`; the old file's highlight stops.
    pub fn open(&mut self, path: PathBuf, line: Option<u32>, root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.release(Some(window), cx);
        self.generation += 1;
        self.cancel = Arc::default();
        (self.path, self.root, self.shown) = (path.clone(), root, Shown::Loading);
        (self.text, self.highlights, self.blocks, self.columns) = (Arc::default(), Highlights::default(), None, 0);
        self.drawn.clear();
        (self.sel, self.dragging, self.preview) = (None, false, true);
        self.goto = line.map(|l| l.saturating_sub(1) as usize);
        self.scroll = UniformListScrollHandle::new();
        self.preview_scroll = ScrollHandle::new();
        let generation = self.generation;
        let job = self.rt.spawn(async move {
            let read = tokio::task::spawn_blocking(move || {
                let content = content::load(&path);
                let (blocks, columns) = match &content {
                    Content::Text(text) | Content::Markdown(text) => {
                        let markdown = matches!(content, Content::Markdown(_)) && text.text.len() <= MAX_PREVIEW;
                        (markdown.then(|| crate::ui::review::markdown(&text.text)), text.lines().map(columns).max().unwrap_or(0))
                    }
                    _ => (None, 0),
                };
                Loaded { content, blocks, columns }
            });
            // ponytail: a read that never ends keeps its blocking thread; the viewer says so and moves on
            let failed = |why: &str| Loaded { content: Content::Failed(why.into()), blocks: None, columns: 0 };
            match tokio::time::timeout(LOAD_TIMEOUT, read).await {
                Ok(Ok(loaded)) => loaded,
                Ok(Err(_)) => failed("Could not read this file"),
                Err(_) => failed(NOT_RESPONDING),
            }
        });
        cx.spawn(async move |this, cx| {
            if let Ok(loaded) = job.await {
                let _ = this.update(cx, |v, cx| v.loaded(generation, loaded, cx));
            }
        })
        .detach();
        cx.notify();
    }

    /// Lets go of what the viewer holds outside itself: its highlight, and the decoded image in gpui's cache.
    /// `window` is the one being updated, if any (gpui cannot reach its tiles otherwise).
    pub fn release(&mut self, window: Option<&mut Window>, cx: &mut App) {
        self.evict_image(window, cx);
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Drops the shown image's pixels from gpui's cache (an image shown again is read afresh): when the viewer
    /// goes out of sight, not only when it goes.
    pub fn evict_image(&mut self, window: Option<&mut Window>, cx: &mut App) {
        if matches!(self.shown, Shown::Other(Content::Image(..) | Content::Svg(..))) {
            gpui::ImageSource::from(self.path.clone()).evict(window, cx);
        }
    }

    fn loaded(&mut self, generation: u64, loaded: Loaded, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        self.shown = match loaded.content {
            Content::Text(text) | Content::Markdown(text) => {
                self.text = Arc::new(text);
                self.columns = loaded.columns;
                self.blocks = loaded.blocks;
                self.highlight(cx);
                if let Some(line) = self.goto.take() {
                    self.scroll.scroll_to_item(line.min(self.text.len().saturating_sub(1)), ScrollStrategy::Center);
                }
                Shown::Text
            }
            other => Shown::Other(other),
        };
        cx.notify();
    }

    /// Highlights the lines on the runtime's blocking pool; plain until it lands, dropped if another file opened.
    fn highlight(&mut self, cx: &mut Context<Self>) {
        let (text, path, cancel, generation) = (self.text.clone(), self.path.clone(), self.cancel.clone(), self.generation);
        let job = self.rt.spawn_blocking(move || {
            let syntax = highlight::syntax_for(&path, text.line(0))?;
            Some(highlight::highlight(syntax, text.lines(), highlight::BUDGET, &cancel))
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Some(highlights)) = job.await {
                let _ = this.update(cx, |v, cx| {
                    if v.generation == generation {
                        v.highlights = highlights;
                        v.drawn.clear();
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn is_text(&self) -> bool {
        matches!(self.shown, Shown::Text)
    }

    /// Line `ix` as drawn, worked out on its first frame on screen.
    fn drawn(&mut self, ix: usize) -> Rc<Drawn> {
        if self.drawn.len() >= MAX_DRAWN && !self.drawn.contains_key(&ix) {
            self.drawn.clear();
        }
        let (text, highlights) = (&self.text, &self.highlights);
        self.drawn.entry(ix).or_insert_with(|| Rc::new(Drawn::new(text.line(ix), highlights.line(ix)))).clone()
    }

    /// The line at the top of the code view, 1-based, for the editor to open at.
    fn top_line(&self, cx: &App) -> Option<u32> {
        if !self.is_text() || self.shows_markdown() {
            return None;
        }
        let theme = Theme::of(cx);
        let line = (theme.terminal_font_size * theme.terminal_line_height).round().max(1.);
        let scrolled = -f32::from(self.scroll.0.borrow().base_handle.offset().y);
        Some((scrolled / line).floor().max(0.) as u32 + 1)
    }

    fn open_in_editor(&mut self, cx: &mut Context<Self>) {
        let line = self.top_line(cx);
        cx.emit(ViewerEvent::OpenInEditor { path: self.path.clone(), line });
    }

    /// Cmd+C: the newer of the viewer's own selection and a markdown one (the preview's, or the PR tab's).
    fn copy(&mut self, cx: &mut Context<Self>) {
        let own = self.sel.filter(|_| !self.shows_markdown()).map(|s| s.text(&self.text)).filter(|t| !t.is_empty());
        let markdown = crate::ui::markdown::selection_stamp().filter(|at| own.is_none() || *at > self.sel_at).and_then(|_| crate::ui::markdown::selected_text());
        if let Some(text) = markdown.or(own) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.is_text() && !self.shows_markdown() && !self.text.is_empty() {
            let last = self.text.len() - 1;
            self.sel = Some(Sel { anchor: (0, 0), head: (last, self.text.line(last).len()) });
            self.sel_at = crate::ui::markdown::next_stamp();
            cx.notify();
        }
    }

    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (key, m) = (e.keystroke.key.as_str(), &e.keystroke.modifiers);
        // Cmd on macOS; Ctrl on Linux, where Ctrl+C has no shell here to interrupt (Ctrl+Shift+C works as in a pane)
        let primary = if cfg!(target_os = "macos") { m.platform && !m.control } else { m.control && !m.platform };
        let copy = crate::platform::terminal_chord(key, m) == Some("c") || (primary && !m.alt && key == "c");
        if copy {
            self.copy(cx);
        } else if primary && !m.alt && !m.shift && key == "a" {
            self.select_all(cx);
        } else if primary && !m.alt && !m.shift && key == "enter" {
            self.open_in_editor(cx);
        } else if !m.modified() || (primary && matches!(key, "up" | "down")) {
            if !self.scroll_key(key, primary, window, cx) {
                return;
            }
        } else {
            return;
        }
        cx.stop_propagation();
    }

    /// Arrows, Page Up/Down, Home and End move the view; true when `key` was one of them.
    fn scroll_key(&mut self, key: &str, primary: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let handle = if self.shows_markdown() { self.preview_scroll.clone() } else { self.scroll.0.borrow().base_handle.clone() };
        let theme = Theme::of(cx);
        let line = px((theme.terminal_font_size * theme.terminal_line_height).round());
        let page = handle.bounds().size.height - line * 2.;
        let (offset, max) = (handle.offset(), handle.max_offset());
        let y = match key {
            "up" if primary => px(0.),
            "down" if primary => -max.y,
            "up" => offset.y + line * 3.,
            "down" => offset.y - line * 3.,
            "pageup" => offset.y + page,
            "pagedown" => offset.y - page,
            "home" => px(0.),
            "end" => -max.y,
            _ => return false,
        };
        handle.set_offset(point(offset.x, y.clamp(-max.y, px(0.))));
        window.refresh();
        cx.notify();
        true
    }

    /// The (line, byte) under `position`: the painted row whose band holds it, else the nearest above or below.
    fn hit(&mut self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let (ix, layout) = {
            let painted = self.painted.borrow();
            let distance = |b: Bounds<Pixels>| f32::from((b.top() - position.y).max(position.y - b.bottom()).max(px(0.)));
            painted.iter().min_by(|(_, a), (_, b)| distance(a.bounds()).total_cmp(&distance(b.bounds()))).cloned()?
        };
        if ix >= self.text.len() {
            return None;
        }
        let bounds = layout.bounds();
        let at = point(position.x.max(bounds.left()), bounds.top() + layout.line_height() / 2.);
        let (Ok(shown) | Err(shown)) = layout.index_for_position(at);
        let len = self.text.line(ix).len();
        Some((ix, self.drawn(ix).to_raw(shown, len)))
    }

    fn press(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some((ix, byte)) = self.hit(e.position) else { return };
        let line = self.text.line(ix);
        self.sel = Some(match e.click_count {
            1 if e.modifiers.shift => Sel { head: (ix, byte), ..self.sel.unwrap_or(Sel { anchor: (ix, byte), head: (ix, byte) }) },
            1 => Sel { anchor: (ix, byte), head: (ix, byte) },
            2 => {
                let word = crate::ui::markdown::word(line, byte);
                Sel { anchor: (ix, word.start), head: (ix, word.end) }
            }
            // the whole line, with its line end when another follows
            _ => Sel { anchor: (ix, 0), head: if ix + 1 < self.text.len() { (ix + 1, 0) } else { (ix, line.len()) } },
        });
        self.sel_at = crate::ui::markdown::next_stamp();
        self.dragging = true;
        cx.notify();
    }

    /// A drag's move, anywhere in the window; past the list's top or bottom it scrolls a line that way.
    fn drag(&mut self, position: Point<Pixels>, list: Bounds<Pixels>, cx: &mut Context<Self>) {
        if !self.dragging {
            return;
        }
        let handle = self.scroll.0.borrow().base_handle.clone();
        let (offset, max) = (handle.offset(), handle.max_offset());
        let step = Theme::of(cx).terminal_font_size * 1.5;
        if position.y < list.top() {
            handle.set_offset(point(offset.x, (offset.y + px(step)).min(px(0.))));
        } else if position.y > list.bottom() {
            handle.set_offset(point(offset.x, (offset.y - px(step)).max(-max.y)));
        }
        let Some(head) = self.hit(position) else { return };
        if let Some(sel) = self.sel.as_mut().filter(|s| s.head != head) {
            sel.head = head;
            cx.notify();
        }
    }

    fn release_drag(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.dragging) {
            return;
        }
        let text = self.sel.map(|s| s.text(&self.text)).filter(|t| !t.is_empty());
        // as a pane's selection does, when the settings ask for it
        if let Some(text) = text.filter(|_| crate::settings::SettingsGlobal::of(cx).settings.copy_on_select) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
        cx.notify();
    }

    /// The header's path: under the root when the file is in it, else from home (`~/…`).
    fn shown_path(&self) -> String {
        match self.root.as_deref().and_then(|r| self.path.strip_prefix(r).ok()) {
            Some(rel) => rel.to_string_lossy().into_owned(),
            None => match self.path.strip_prefix(crate::platform::home()) {
                Ok(rest) => format!("~/{}", rest.to_string_lossy()),
                Err(_) => self.path.to_string_lossy().into_owned(),
            },
        }
    }

    fn header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let icon = crate::files::icons::material_icon(&name, false, false);
        let tag = |label: &'static str| {
            div().flex_none().px(px(5.)).rounded(px(4.)).border_1().border_color(theme.border).text_size(px(10.5)).text_color(theme.text_faint).child(label)
        };
        let toggle = self.blocks.is_some().then(|| {
            let choice = |id: &'static str, label: &'static str, on: bool, preview: bool| {
                let hover = theme.hover;
                div()
                    .id(id)
                    .px(px(8.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .text_color(if on { theme.text } else { theme.text_muted })
                    .when(on, |el| el.bg(theme.selected))
                    .when(!on, |el| el.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |v: &mut FileViewer, _: &ClickEvent, _, cx| {
                        if v.preview != preview {
                            v.preview = preview;
                            v.sel = None;
                            cx.notify();
                        }
                    }))
                    .child(label)
            };
            div()
                .flex()
                .flex_none()
                .gap(px(2.))
                .p(px(2.))
                .rounded(px(7.))
                .border_1()
                .border_color(theme.border)
                .child(choice("viewer-preview", "Preview", self.preview, true))
                .child(choice("viewer-source", "Source", !self.preview, false))
        });
        let path = self.path.to_string_lossy().into_owned();
        div()
            .h(px(36.))
            .flex_none()
            .pl(px(12.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.wash(0.04))
            .font_family(theme.font_ui.clone())
            .text_size(px(12.5))
            // a coloured SVG, so img(), not the tinted svg(); the Files tree's size
            .child(gpui::img(icon).size(px(16.)).flex_none().object_fit(gpui::ObjectFit::Contain))
            .child(div().min_w_0().truncate().text_color(theme.text).child(self.shown_path()))
            .when(self.is_text() && self.text.crlf, |el| el.child(tag("CRLF")))
            .when(self.is_text(), |el| el.child(tag("Read only")))
            .child(div().flex_1())
            .children(toggle)
            .child(
                titlebar_button(theme, "viewer-editor", "icons/solar/pen-new-square.svg")
                    .tooltip(tip("Open in editor", None))
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_in_editor(cx))),
            )
            .child(
                titlebar_button(theme, "viewer-copy-path", "icons/solar/copy.svg")
                    .tooltip(tip("Copy path", None))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone())))),
            )
    }

    /// A message in place of the file, with the editor offered when the file is there to open.
    fn notice(&self, theme: &Theme, title: impl Into<SharedString>, hint: impl Into<SharedString>, editor: bool, cx: &mut Context<Self>) -> AnyElement {
        let hover = theme.hover;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .font_family(theme.font_ui.clone())
            .child(div().text_color(theme.text).child(title.into()))
            .child(div().text_size(px(12.)).text_color(theme.text_muted).child(hint.into()))
            .when(editor, |el| {
                el.child(
                    div()
                        .id("viewer-notice-editor")
                        .mt(px(8.))
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.border)
                        .text_size(px(12.))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_in_editor(cx)))
                        .child("Open in editor"),
                )
            })
            .into_any_element()
    }

    fn body(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let content = match &self.shown {
            Shown::Loading => return div().into_any_element(),
            Shown::Text if self.shows_markdown() => return self.preview(theme),
            Shown::Text => return self.code(theme, window, cx),
            Shown::Other(content) => content,
        };
        let image = |w: f32, h: f32| {
            // a small image shows at its size, not stretched to the pane
            let image = gpui::img(self.path.clone()).object_fit(gpui::ObjectFit::Contain).max_w_full().max_h_full().w(px(w)).h(px(h));
            div().size_full().p(px(16.)).flex().items_center().justify_center().child(image).into_any_element()
        };
        match content {
            Content::Image(w, h) => image(*w as f32 / window.scale_factor(), *h as f32 / window.scale_factor()),
            Content::Svg(w, h) => image(*w as f32, *h as f32),
            Content::Binary => self.notice(theme, "This file is binary", "The viewer shows text and images.", true, cx),
            Content::TooLarge(size) => {
                let hint = format!("It is {}; the viewer shows text up to {} and images up to {}.", content::human_size(*size), content::human_size(MAX_TEXT), content::human_size(content::MAX_IMAGE));
                self.notice(theme, "This file is too large to show", hint, true, cx)
            }
            Content::TooManyLines(n) => {
                let hint = format!("It has {n} lines; the viewer shows up to {}.", content::MAX_LINES);
                self.notice(theme, "This file is too long to show", hint, true, cx)
            }
            Content::TooManyPixels(w, h) => self.notice(theme, "This image is too large to show", format!("{w} × {h} pixels"), true, cx),
            Content::TooManyFrames(w, h, frames) => {
                // `frames` stops one past the budget: the animation may hold more
                let hint = format!("{frames} or more frames of {w} × {h} pixels");
                self.notice(theme, "This animation is too large to show", hint, true, cx)
            }
            Content::UnknownSize => self.notice(theme, "This image can't be shown", "Its header does not say its size.", true, cx),
            Content::Failed(why) => {
                let editor = why == NOT_RESPONDING;
                self.notice(theme, "Could not show this file", why.clone(), editor, cx)
            }
            // ponytail: M4b draws the pages here
            Content::Pdf => self.notice(theme, "PDF preview is coming", "The viewer will show PDFs in a coming version.", true, cx),
            Content::Text(_) | Content::Markdown(_) => div().into_any_element(),
        }
    }

    /// Markdown's preview: Zeron's blocks, selectable through `ui::markdown`, in a column a page wide.
    fn preview(&self, theme: &Theme) -> AnyElement {
        let blocks = self.blocks.as_deref().unwrap_or_default();
        div()
            .id("viewer-md")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.preview_scroll)
            .font_family(theme.font_ui.clone())
            .text_size(px(13.5))
            .child(
                div()
                    .max_w(px(820.))
                    .mx_auto()
                    .px(px(24.))
                    .py(px(20.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .children(crate::ui::markdown::render(blocks, "viewer-md", theme)),
            )
            .into_any_element()
    }

    /// The text as a list of rows, a gutter of line numbers and the line, no wrapping: wider lines scroll across.
    /// Every row is as wide as the widest line (its columns, worked out at load): the list measures its first row,
    /// cheaply, rather than lay out the widest on every frame.
    fn code(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let font = gpui::font(theme.font_terminal.clone());
        let size = px(theme.terminal_font_size);
        let em = window.text_system().em_advance(window.text_system().resolve_font(&font), size).unwrap_or(size * 0.6);
        let digits = self.text.len().to_string().len().max(3);
        let gutter = em * digits as f32 + px(28.);
        // the line end's selection mark is one column more
        let width = gutter + em * (self.columns + 1) as f32 + px(24.);
        let line_h = px((theme.terminal_font_size * theme.terminal_line_height).round());
        // the rows register as they paint, after this clears the last frame's (a view painted again without a
        // render paints them again too)
        let painted = self.painted.clone();
        let reset = canvas(|_, _, _| (), move |_, _, _, _| painted.borrow_mut().clear()).absolute().size_0();
        let list = gpui::uniform_list(
            "viewer-lines",
            self.text.len(),
            cx.processor(move |v: &mut FileViewer, range: Range<usize>, _, cx| v.rows(range, gutter, width, line_h, cx)),
        )
        .track_scroll(&self.scroll)
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .size_full()
        .pb(px(8.));
        let scroll = self.scroll.clone();
        let this = cx.entity().downgrade();
        let dragging = self.dragging;
        // while a drag runs: its moves and its release anywhere in the window, past the list's edges too
        let drag = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                if !dragging {
                    return;
                }
                let moved = this.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && e.dragging() {
                        let _ = moved.update(cx, |v, cx| v.drag(e.position, bounds, cx));
                    }
                });
                let released = this.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = released.update(cx, |v, cx| v.release_drag(cx));
                    }
                });
            },
        )
        .absolute()
        .size_full();
        div()
            .id("viewer-code")
            .relative()
            .size_full()
            .font(font)
            .text_size(size)
            .line_height(line_h)
            .text_color(theme.terminal.foreground)
            .cursor(gpui::CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, window, cx| v.press(e, window, cx)))
            // a click released before the next frame registered the drag's window-wide listeners
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _: &MouseUpEvent, _, cx| v.release_drag(cx)))
            .child(reset)
            .child(list)
            .child(drag)
            .child(crate::ui::scrollbar("viewer-scrollbar", &scroll, theme))
            .into_any_element()
    }

    fn rows(&mut self, range: Range<usize>, gutter: Pixels, width: Pixels, line_h: Pixels, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let font = gpui::font(theme.font_terminal.clone());
        let wash = theme.terminal.selection;
        let run = |len: usize, color: gpui::Hsla, background: Option<gpui::Hsla>| TextRun { len, font: font.clone(), color, background_color: background, underline: None, strikethrough: None };
        let lines = self.text.len();
        range
            .filter(|ix| *ix < lines)
            .map(|ix| {
                let drawn = self.drawn(ix);
                let sel = self.sel.and_then(|s| s.on_line(ix, self.text.line(ix).len()));
                let picked = sel.as_ref().map(|(r, _)| drawn.to_shown(r.start)..drawn.to_shown(r.end));
                let mut runs: Vec<TextRun> = drawn.segments(picked).into_iter().map(|(r, token, selected)| run(r.len(), token_color(token, &theme), selected.then_some(wash))).collect();
                if drawn.text.len() > drawn.body {
                    runs.push(run(drawn.text.len() - drawn.body, theme.text_faint, None));
                }
                // the line end, when the selection runs on past it: a space's worth of wash (an empty line's only mark)
                let text = if sel.is_some_and(|(_, newline)| newline) {
                    runs.push(run(1, theme.terminal.foreground, Some(wash)));
                    SharedString::from(format!("{} ", drawn.text))
                } else {
                    drawn.text.clone()
                };
                let styled = StyledText::new(text).with_runs(runs);
                let layout = styled.layout().clone();
                let painted = self.painted.clone();
                div()
                    .h(line_h)
                    .min_w(width)
                    .flex()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(div().flex_none().w(gutter).pr(px(16.)).flex().justify_end().text_color(theme.text_faint).child(SharedString::from((ix + 1).to_string())))
                    .child(div().relative().flex_none().child(styled).child(
                        // the row is on screen: its layout is the one a press here hit-tests against
                        canvas(|_, _, _| (), move |_, _, _, _| painted.borrow_mut().push((ix, layout.clone()))).absolute().size_0(),
                    ))
                    .into_any_element()
            })
            .collect()
    }
}

/// What a read that timed out says (and offers the editor for: the file is there, its disk is slow).
const NOT_RESPONDING: &str = "Not responding";

/// The colour `token` draws in: the terminal's palette, so the code matches the panes beside it.
fn token_color(token: Token, theme: &Theme) -> gpui::Hsla {
    let ansi = &theme.terminal.ansi;
    match token {
        Token::Plain => theme.terminal.foreground,
        Token::Comment => theme.text_faint,
        Token::String => ansi[2],
        Token::Keyword => ansi[5],
        Token::Number | Token::Constant | Token::Attr => ansi[3],
        Token::Function => ansi[4],
        Token::Type => ansi[6],
        Token::Tag => ansi[1],
        Token::Punct => theme.text_muted,
    }
}

impl Render for FileViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &Theme::of(cx).clone();
        let body = self.body(theme, window, cx);
        div()
            .id("file-viewer")
            .track_focus(&self.focus)
            .key_context("FileViewer")
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| v.key(e, window, cx)))
            // a press anywhere in it gives it the keyboard (it has no inputs of its own to keep it from)
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| window.focus(&v.focus, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.terminal.background)
            .child(self.header(theme, cx))
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::highlight::Span;

    #[test]
    fn a_selection_copies_its_lines_and_line_ends() {
        let text = content::decode(b"one\ntwo\nthree\n").unwrap();
        let sel = Sel { anchor: (2, 2), head: (0, 1) };
        assert_eq!(sel.text(&text), "ne\ntwo\nth");
        assert_eq!(sel.on_line(1, 3), Some((0..3, true)));
        assert_eq!(sel.on_line(2, 5), Some((0..2, false)));
        assert_eq!(Sel { anchor: (1, 1), head: (1, 1) }.on_line(1, 3), None);
        let all = Sel { anchor: (0, 0), head: (2, 5) };
        assert_eq!(all.text(&text), "one\ntwo\nthree");
    }

    #[test]
    fn tabs_widen_and_map_both_ways() {
        let line = "\tab\tc";
        let d = Drawn::new(line, &[]);
        assert_eq!((d.text.as_ref(), d.body), ("    ab    c", 11));
        assert_eq!(d.to_shown(1), 4);
        assert_eq!(d.to_shown(4), 10);
        assert_eq!(d.to_raw(0, line.len()), 0);
        assert_eq!(d.to_raw(1, line.len()), 0, "inside the tab, nearer its start");
        assert_eq!(d.to_raw(3, line.len()), 1, "inside the tab, nearer its end");
        assert_eq!(d.to_raw(4, line.len()), 1);
        assert_eq!(d.to_raw(5, line.len()), 2);
        assert_eq!(d.to_raw(10, line.len()), 4);
        assert_eq!(d.to_raw(11, line.len()), line.len());
        let wide = Drawn::new("é\tx", &[]);
        assert_eq!(wide.to_raw(2, 4), 2);
        assert_eq!(wide.to_raw(6, 4), 3);
    }

    #[test]
    fn a_long_line_is_cut_but_copies_whole() {
        let line = "x".repeat(MAX_SHOWN + 10);
        let d = Drawn::new(&line, &[]);
        assert_eq!(d.body, MAX_SHOWN);
        assert!(d.text.ends_with('…'));
        assert_eq!(d.to_shown(MAX_SHOWN + 5), MAX_SHOWN);
        assert_eq!(d.to_raw(MAX_SHOWN + 1, line.len()), line.len(), "past the …");
        assert_eq!(d.to_raw(MAX_SHOWN, line.len()), MAX_SHOWN);
    }

    #[test]
    fn columns_count_tabs_wide_characters_and_the_cut() {
        assert_eq!(columns("ab"), 2);
        assert_eq!(columns("\t\t"), 8);
        assert_eq!(columns("日本"), 4);
        assert_eq!(columns(&"x".repeat(MAX_SHOWN + 10)), MAX_SHOWN + 1);
    }

    #[test]
    fn runs_cover_the_line_and_split_at_the_selection() {
        let span = |start, end, token| Span { start, end, token };
        let d = Drawn::new("\tfn x = \"s\";", &[span(1, 3, Token::Keyword), span(8, 11, Token::String)]);
        assert_eq!(d.runs, vec![(0..4, Token::Plain), (4..6, Token::Keyword), (6..11, Token::Plain), (11..14, Token::String), (14..15, Token::Plain)]);
        assert_eq!(d.segments(Some(5..12)), vec![
            (0..4, Token::Plain, false),
            (4..5, Token::Keyword, false),
            (5..6, Token::Keyword, true),
            (6..11, Token::Plain, true),
            (11..12, Token::String, true),
            (12..14, Token::String, false),
            (14..15, Token::Plain, false),
        ]);
        assert_eq!(d.segments(Some(3..3)).len(), 5);
        assert!(Drawn::new("", &[]).segments(None).is_empty());
    }
}
