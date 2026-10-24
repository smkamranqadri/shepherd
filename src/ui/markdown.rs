//! Zeron's markdown, drawn (adapted from Zeron's `crates/ui/src/markdown/render.rs` and `selection.rs`, MIT): bold,
//! italic, code, struck and linked runs in one wrapped text per paragraph, and text selection across the paragraphs:
//! a drag selects, a double click a word, a triple click a paragraph; Cmd+C copies.
//!
//! A link goes where [`resolve_link`] says (uherdr's `MarkdownLink`): http(s) and mailto to the browser or mail; a file
//! inside the workspace root, and a `#heading`, to whoever drew the markdown ([`Links`]); anything else, a file
//! outside the root or another scheme from an untrusted README, nowhere, drawn faint with the reason as its tooltip.
//!
//! gpui selects no plain text, so each paragraph registers itself in a per-frame list in paint order (document
//! order); a drag anchored in one resolves against that list into a span per paragraph, washed under its glyphs.
use crate::theme::Theme;
use crate::ui::review::{Block, InlineRun, runs_text};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, Font, FontStyle, FontWeight, Hitbox, HitboxBehavior, Hsla, InteractiveElement,
    InteractiveText, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, SharedString,
    StatefulInteractiveElement, StrikethroughStyle, Styled, StyledImage, StyledText, TextLayout, TextRun, UnderlineStyle, Window, canvas, div, point, px,
    size,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use zeron_markdown::parser::InlineImage;
use zeron_syntax::HighlightKind;

/// Where a link leads, worked out from its `href` by [`resolve_link`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Opens in the browser (or mail).
    External(String),
    /// A file inside the root, with the `#fragment` after its path (a heading, or a line as `L12`).
    File { path: PathBuf, fragment: Option<String> },
    /// A heading in the same document.
    Anchor(String),
    /// Followed nowhere, for this reason.
    Disabled(String),
}

/// What `href` does in a document at `place` (the markdown file and the workspace root, both absolute): `None` (the
/// PR tab's description, which is no file) follows web links only. A leading `/` is the root, as GitHub takes it
/// for a repository; `..` and `.` are resolved by the text, not the disk.
pub fn resolve_link(href: &str, place: Option<(&Path, &Path)>) -> Link {
    let href = href.trim();
    if href.is_empty() {
        return Link::Disabled("Empty link".into());
    }
    if let Some(scheme) = scheme(href) {
        return match scheme.as_str() {
            "http" | "https" | "mailto" => Link::External(href.to_string()),
            _ => Link::Disabled(format!("{scheme}: links don't open from here")),
        };
    }
    let Some((document, root)) = place else { return Link::Disabled("Only web links open from here".into()) };
    if let Some(anchor) = href.strip_prefix('#') {
        return Link::Anchor(decode(anchor));
    }
    let (target, fragment) = match href.split_once('#') {
        Some((target, fragment)) => (target, Some(decode(fragment)).filter(|f| !f.is_empty())),
        None => (href, None),
    };
    let target = decode(target.split_once('?').map_or(target, |(t, _)| t));
    let base = if target.starts_with('/') { root } else { document.parent().unwrap_or(root) };
    let path = normalize(&base.join(target.trim_start_matches('/')));
    if !path.starts_with(normalize(root)) {
        return Link::Disabled("Outside this workspace's folder".into());
    }
    Link::File { path, fragment }
}

/// `href`'s scheme, lowercased, when it starts with one (`https:`, `file:`, `vscode:`, …).
fn scheme(href: &str) -> Option<String> {
    let (scheme, _) = href.split_once(':')?;
    let mut chars = scheme.chars();
    (chars.next()?.is_ascii_alphabetic() && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))).then(|| scheme.to_ascii_lowercase())
}

fn decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text).decode_utf8().map_or_else(|_| text.to_string(), |t| t.into_owned())
}

/// `path` with `.` dropped and `..` taking the folder before it (never above `/`).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            _ => {}
        }
    }
    out
}

/// A line in a link's fragment, GitHub's way (`L12`, or `L12-L20` for its first): 1-based.
pub fn fragment_line(fragment: &str) -> Option<u32> {
    let digits = fragment.strip_prefix('L')?;
    let digits = digits.split_once('-').map_or(digits, |(first, _)| first);
    digits.parse().ok().filter(|n| *n > 0)
}

/// A heading's anchor, GitHub's way: lowercased, spaces as `-`, punctuation but `-` and `_` dropped.
pub fn slug(text: &str) -> String {
    text.trim()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// The top-level block a `#anchor` leads to in `blocks`: the heading whose slug it is (the second of two alike is
/// `-1`, as GitHub numbers them).
pub fn anchor_block(blocks: &[Block], anchor: &str) -> Option<usize> {
    let anchor = anchor.to_lowercase();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    blocks.iter().position(|b| {
        let Block::Heading { runs, .. } = b else { return false };
        let base = slug(&runs_text(runs));
        let n = seen.entry(base.clone()).or_insert(0);
        let id = if *n == 0 { base } else { format!("{base}-{n}") };
        *n += 1;
        id == anchor
    })
}

/// Follows a `File` or an `Anchor` link.
type Follow = Rc<dyn Fn(Link, &mut Window, &mut App)>;

/// How a surface's links are followed: where its markdown sits, and who opens a file or jumps to a heading.
#[derive(Clone, Default)]
pub struct Links {
    /// The markdown file and the workspace root, absolute; `None` follows web links only.
    place: Option<Rc<(PathBuf, PathBuf)>>,
    follow: Option<Follow>,
}

impl Links {
    /// Web links only (the PR tab's description).
    pub fn web() -> Links {
        Links::default()
    }

    /// Links in `document` under `root`: `follow` takes a `File` or an `Anchor` (the web opens here).
    pub fn local(document: PathBuf, root: PathBuf, follow: impl Fn(Link, &mut Window, &mut App) + 'static) -> Links {
        Links { place: Some(Rc::new((document, root))), follow: Some(Rc::new(follow)) }
    }

    fn resolve(&self, href: &str) -> Link {
        resolve_link(href, self.place.as_deref().map(|(d, r)| (d.as_path(), r.as_path())))
    }
}

/// How a surface draws its markdown: the review panel's compact way by default; the reader's as a document, with
/// its images.
#[derive(Clone, Default)]
pub struct Look {
    /// A document's type scale and spacing, and its code colored by syntax.
    pub document: bool,
    /// An image's file here, by its source; `None` (or no file yet) keeps its alt text.
    pub image: Option<Rc<dyn Fn(&str) -> Option<PathBuf>>>,
}

/// `blocks`, keyed under `key` (their paragraphs' selection keys and element ids start with it), their links
/// followed as `links` says. A view of them paints one [`frame_reset`] before the first, however many times it
/// renders some.
pub fn render(blocks: &[Block], key: &str, links: &Links, theme: &Theme) -> Vec<AnyElement> {
    render_with(blocks, key, links, theme, &Look::default())
}

/// `blocks` drawn the `look` way.
pub fn render_with(blocks: &[Block], key: &str, links: &Links, theme: &Theme, look: &Look) -> Vec<AnyElement> {
    // a paragraph of images without alt text has nothing to show where images are not drawn
    let shown = |b: &Block| look.image.is_some() || !matches!(b, Block::Paragraph { runs } if runs_text(runs).trim().is_empty());
    blocks.iter().enumerate().filter(|(_, b)| shown(b)).map(|(i, b)| block(b, &format!("{key}-{i}"), links, theme, look)).collect()
}

fn block(b: &Block, key: &str, links: &Links, theme: &Theme, look: &Look) -> AnyElement {
    let body = if look.document { theme.text } else { theme.text_muted };
    match b {
        Block::Paragraph { runs } if look.image.is_some() && runs.iter().any(|r| r.style.image.is_some()) => figure(runs, key, body, links, theme, look),
        Block::Paragraph { runs } => text(runs, key, body, false, links, theme),
        // GitHub's scale: the top two levels ruled off below
        Block::Heading { level, runs } if look.document => {
            let size = match level {
                1 => 26.,
                2 => 21.,
                3 => 17.,
                _ => 15.,
            };
            div()
                .pt(px(if *level <= 2 { 14. } else { 8. }))
                .text_size(px(size))
                .line_height(px(size * 1.3))
                .when(*level <= 2, |el| el.pb(px(6.)).border_b_1().border_color(theme.border))
                .child(text(runs, key, theme.text, true, links, theme))
                .into_any_element()
        }
        Block::Heading { level, runs } => div()
            .pt(px(4.))
            .text_size(px(if *level <= 1 { 15. } else if *level == 2 { 14. } else { 13. }))
            .child(text(runs, key, theme.text, true, links, theme))
            .into_any_element(),
        Block::CodeBlock { code: c, language } if look.document => highlighted(c, language.as_deref(), key, theme),
        Block::CodeBlock { code: c, .. } => code(c, key, px(11.5), theme),
        Block::BlockQuote { children } => div()
            .pl(px(8.))
            .border_l_2()
            .border_color(theme.border_strong)
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(children.iter().enumerate().map(|(i, b)| block(b, &format!("{key}-{i}"), links, theme, look)))
            .into_any_element(),
        Block::List { ordered_start, items } => div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(items.iter().enumerate().map(|(i, item)| {
                // a task's box is its marker
                let task = matches!(item.first(), Some(Block::Paragraph { runs }) if runs.first().is_some_and(|r| r.style.task.is_some()));
                let marker = if task { String::new() } else { ordered_start.map_or("•".to_string(), |n| format!("{}.", n + i as u64)) };
                div()
                    .flex()
                    .gap(px(6.))
                    .child(div().flex_none().min_w(px(10.)).text_color(theme.text_faint).child(marker))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .children(item.iter().enumerate().map(|(j, b)| block(b, &format!("{key}-{i}-{j}"), links, theme, look))),
                    )
            }))
            .into_any_element(),
        Block::Table { header, rows, .. } => {
            let row = |cells: &[Vec<InlineRun>], r: usize, head: bool| {
                div().flex().border_t_1().border_color(theme.border).children(cells.iter().enumerate().map(|(c, cell)| {
                    let color = if head { theme.text } else { body };
                    div().flex_1().min_w_0().px(px(6.)).py(px(4.)).child(text(cell, &format!("{key}-{r}-{c}"), color, head, links, theme))
                }))
            };
            div()
                .flex()
                .flex_col()
                .border_b_1()
                .border_color(theme.border)
                .child(row(header, 0, true))
                .children(rows.iter().enumerate().map(|(r, cells)| row(cells, r + 1, false)))
                .into_any_element()
        }
        Block::Rule => div().h(px(1.)).bg(theme.border).into_any_element(),
    }
}

/// A code block of `code` in the terminal font at `size`, selectable as a paragraph keyed `key`.
pub fn code(code: &str, key: &str, size: Pixels, theme: &Theme) -> AnyElement {
    let run = InlineRun { text: code.trim_end().to_string(), style: Default::default() };
    div()
        .p(px(8.))
        .rounded(px(6.))
        .bg(theme.wash(0.05))
        .overflow_hidden()
        .text_size(size)
        .child(text_in(&[run], key, theme.text_muted, false, theme.font_terminal.clone(), &Links::web(), theme, None))
        .into_any_element()
}

/// A fenced block in the terminal font, colored by its language's syntax (Zeron's tree-sitter grammars); one it
/// does not know stays plain.
fn highlighted(source: &str, language: Option<&str>, key: &str, theme: &Theme) -> AnyElement {
    let source = source.trim_end();
    let mut runs = Vec::new();
    let mut colors = Vec::new();
    let mut at = 0;
    let mut push = |range: Range<usize>, color: Hsla| {
        if !range.is_empty() {
            runs.push(InlineRun { text: source[range].to_string(), style: Default::default() });
            colors.push(color);
        }
    };
    for (range, kind) in syntax(source, language).iter() {
        if range.start < at || range.end > source.len() {
            continue; // overlapping or stale: the plain text covers it
        }
        push(at..range.start, theme.text);
        push(range.clone(), syntax_color(*kind, theme));
        at = range.end;
    }
    push(at..source.len(), theme.text);
    div()
        .p(px(12.))
        .rounded(px(8.))
        .bg(theme.wash(0.05))
        .overflow_hidden()
        .text_size(px(12.5))
        .line_height(px(19.))
        .child(text_in(&runs, key, theme.text, false, theme.font_terminal.clone(), &Links::web(), theme, Some(&colors)))
        .into_any_element()
}

// ponytail: the cache empties whole at 256 blocks; an LRU if switching between big documents ever re-highlights visibly.
/// `source`'s highlight spans, from the start of `source`, kept per block text and language.
fn syntax(source: &str, language: Option<&str>) -> Rc<Vec<(Range<usize>, HighlightKind)>> {
    thread_local! {
        static DONE: RefCell<HashMap<(String, Option<String>), Rc<Vec<(Range<usize>, HighlightKind)>>>> = RefCell::new(HashMap::new());
    }
    let key = (source.to_string(), language.map(str::to_string));
    if let Some(spans) = DONE.with(|d| d.borrow().get(&key).cloned()) {
        return spans;
    }
    let request = zeron_syntax::HighlightRequest { source, path: None, fence_tag: language };
    let mut spans = Vec::new();
    if let Ok(doc) = zeron_syntax::highlight(request) {
        let mut start = 0;
        for (line, line_spans) in source.split('\n').zip(&doc.lines) {
            spans.extend(line_spans.iter().map(|s| (start + s.range.start..start + s.range.end, s.kind)));
            start += line.len() + 1;
        }
    }
    let spans = Rc::new(spans);
    DONE.with(|d| {
        let mut d = d.borrow_mut();
        if d.len() >= 256 {
            d.clear();
        }
        d.insert(key, spans.clone());
    });
    spans
}

/// A syntax kind in the terminal theme's ANSI colors, so code reads as it does in the panes.
fn syntax_color(kind: HighlightKind, theme: &Theme) -> Hsla {
    let ansi = &theme.terminal.ansi;
    match kind {
        HighlightKind::Comment => theme.text_faint,
        HighlightKind::Keyword => ansi[5],
        HighlightKind::String | HighlightKind::StringSpecial | HighlightKind::MarkupRaw => ansi[2],
        HighlightKind::Escape => ansi[6],
        HighlightKind::Number | HighlightKind::Boolean | HighlightKind::Constant => ansi[3],
        HighlightKind::Type | HighlightKind::TypeBuiltin | HighlightKind::Constructor => ansi[6],
        HighlightKind::Function | HighlightKind::FunctionBuiltin | HighlightKind::Macro | HighlightKind::MarkupHeading => ansi[4],
        HighlightKind::Property | HighlightKind::Attribute | HighlightKind::Tag | HighlightKind::Label | HighlightKind::VariableSpecial => ansi[1],
        HighlightKind::Operator | HighlightKind::Punctuation | HighlightKind::Embedded => theme.text_muted,
        HighlightKind::MarkupLink | HighlightKind::MarkupReference => theme.accent,
        HighlightKind::Invalid => theme.danger,
        _ => theme.text,
    }
}

/// A paragraph with images: each drawn (a linked one clickable), the text between them as text.
fn figure(runs: &[InlineRun], key: &str, color: Hsla, links: &Links, theme: &Theme, look: &Look) -> AnyElement {
    let mut parts: Vec<AnyElement> = Vec::new();
    let mut words: Vec<InlineRun> = Vec::new();
    for run in runs.iter().chain([&InlineRun { text: String::new(), style: Default::default() }]) {
        let last = run.text.is_empty() && run.style.image.is_none();
        if (run.style.image.is_some() || last) && !runs_text(&words).trim().is_empty() {
            parts.push(text(&words, &format!("{key}-{}", parts.len()), color, false, links, theme));
        }
        match &run.style.image {
            Some(image) => {
                words.clear();
                parts.push(picture(image, &run.text, &format!("{key}-image-{}", parts.len()), links, theme, look));
            }
            None if !last => words.push(run.clone()),
            None => {}
        }
    }
    div().flex().flex_wrap().items_center().gap(px(4.)).children(parts).into_any_element()
}

/// An image at its own size, no wider than the column; its alt text until (or unless) it is fetched. A linked one
/// follows its link as a text link does.
fn picture(image: &InlineImage, alt: &str, key: &str, links: &Links, theme: &Theme, look: &Look) -> AnyElement {
    let alt: SharedString = alt.to_string().into();
    let Some(path) = look.image.as_ref().and_then(|file| file(&image.source)) else {
        return div().text_color(theme.text_faint).child(alt).into_any_element();
    };
    let shown = gpui::img(path).max_w_full().with_fallback({
        let (alt, faint) = (alt.clone(), theme.text_faint);
        move || div().text_color(faint).child(alt.clone()).into_any_element()
    });
    match image.link.as_deref().map(|href| links.resolve(href)) {
        Some(Link::Disabled(_)) | None => shown.into_any_element(),
        Some(link) => {
            let follow = links.follow.clone();
            div()
                .id(SharedString::from(key.to_string()))
                .cursor_pointer()
                .child(shown)
                .on_click(move |_, window, cx| match &link {
                    Link::External(url) => cx.open_url(url),
                    link => {
                        if let Some(follow) = &follow {
                            follow(link.clone(), window, cx);
                        }
                    }
                })
                .into_any_element()
        }
    }
}

fn text(runs: &[InlineRun], key: &str, color: Hsla, bold: bool, links: &Links, theme: &Theme) -> AnyElement {
    text_in(runs, key, color, bold, theme.font_ui.clone(), links, theme, None)
}

/// One paragraph: its runs styled (`colors`, one a run, in place of `color`), its links clickable (a disabled one
/// faint, its reason its tooltip), selectable. A code run sits on a rounded pill between thin spaces.
#[allow(clippy::too_many_arguments)]
fn text_in(
    runs: &[InlineRun],
    key: &str,
    color: Hsla,
    bold: bool,
    family: SharedString,
    follow: &Links,
    theme: &Theme,
    colors: Option<&[Hsla]>,
) -> AnyElement {
    let mut flat = String::new();
    let mut links: Vec<(Range<usize>, Link)> = Vec::new();
    let mut code: Vec<Range<usize>> = Vec::new();
    let mut text_runs: Vec<TextRun> = Vec::new();
    let plain = |len: usize| TextRun { len, font: gpui::font(family.clone()), color, background_color: None, underline: None, strikethrough: None };
    for (i, r) in runs.iter().enumerate() {
        let color = colors.and_then(|c| c.get(i).copied()).unwrap_or(color);
        let s = &r.style;
        // a code run sits between thin spaces inside its background, apart from the words around it
        let start = flat.len();
        if s.code {
            flat.push_str(PAD);
            text_runs.push(plain(PAD.len()));
        }
        let range = flat.len()..flat.len() + r.text.len();
        flat.push_str(&r.text);
        // a link's colour says whether it goes anywhere
        let link = s.link.as_ref().map(|href| follow.resolve(href));
        let tint = link.as_ref().map(|l| if matches!(l, Link::Disabled(_)) { theme.text_faint } else { theme.accent });
        if let Some(link) = link {
            links.push((range, link));
        }
        text_runs.push(TextRun {
            len: r.text.len(),
            font: Font {
                // Geist has no bold: semibold reads as it
                weight: if bold || s.bold { FontWeight::SEMIBOLD } else { FontWeight::NORMAL },
                style: if s.italic { FontStyle::Italic } else { FontStyle::Normal },
                ..gpui::font(if s.code { INLINE_MONO.into() } else { family.clone() })
            },
            color: tint.unwrap_or(if s.code { theme.accent } else { color }),
            background_color: None,
            underline: tint.map(|tint| UnderlineStyle { thickness: px(1.), color: Some(tint.opacity(0.5)), wavy: false }),
            strikethrough: s.strikethrough.then(|| StrikethroughStyle { thickness: px(1.), color: None }),
        });
        if s.code {
            flat.push_str(PAD);
            text_runs.push(plain(PAD.len()));
            code.push(start..flat.len());
        }
    }
    let flat: SharedString = flat.into();
    let styled = StyledText::new(flat.clone()).with_runs(text_runs);
    let layout = styled.layout().clone();
    let wash = theme.accent.opacity(0.25);
    let pill = theme.accent.opacity(0.1);
    let key: SharedString = key.to_string().into();
    let underlay = {
        let key = key.clone();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                // a code run's background, rounded (a run's own background is square), a little inside its line
                for rect in code.iter().flat_map(|r| rects(&layout, r)) {
                    let rect = Bounds::new(point(rect.left(), rect.top() + px(2.)), size(rect.size.width, rect.size.height - px(4.)));
                    window.paint_quad(gpui::fill(rect, pill).corner_radii(px(4.)));
                }
                paint_selection(window, hitbox, &key, &flat, &layout, wash)
            },
        )
        .absolute()
        .size_full()
    };
    let (live, dead): (Vec<_>, Vec<_>) = links.into_iter().partition(|(_, link)| !matches!(link, Link::Disabled(_)));
    let ranges = live.iter().map(|(range, _)| range.clone()).collect();
    let targets: Vec<Link> = live.into_iter().map(|(_, link)| link).collect();
    let follow = follow.follow.clone();
    let mut text = InteractiveText::new(gpui::ElementId::Name(key), styled).on_click(ranges, move |ix, window, cx: &mut App| match &targets[ix] {
        Link::External(url) => cx.open_url(url),
        link => {
            if let Some(follow) = &follow {
                follow(link.clone(), window, cx);
            }
        }
    });
    if !dead.is_empty() {
        text = text.tooltip(move |ix, window, cx| {
            let (_, Link::Disabled(why)) = dead.iter().find(|(range, _)| range.contains(&ix))? else { return None };
            Some(crate::shell::widgets::tip(why.clone(), None)(window, cx))
        });
    }
    div().relative().cursor(CursorStyle::IBeam).child(underlay).child(text).into_any_element()
}

/// The thin space either side of a code run, inside its background; copies leave it out.
const PAD: &str = "\u{2009}";

/// Geist Mono with its glyphs at 88% (`assets/fonts/ShepherdInlineMono.ttf`): a line has one font size, so inline
/// code looks smaller by its font.
const INLINE_MONO: &str = "Shepherd Inline Mono";

/// One painted paragraph, registered per frame in paint order.
struct Painted {
    key: SharedString,
    text: SharedString,
    layout: TextLayout,
}

/// A paragraph's slice of the selection.
#[derive(Clone, Debug, PartialEq)]
struct Span {
    key: SharedString,
    range: Range<usize>,
    text: SharedString,
}

/// A drag anchored at a paragraph's byte `anchor`, and the spans it reaches, in document order.
#[derive(Default)]
struct Selection {
    anchor_key: SharedString,
    anchor: usize,
    dragging: bool,
    spans: Vec<Span>,
    /// When it was made (`next_stamp`): the file viewer's own selection, made later, copies over it.
    at: u64,
}

// ponytail: one selection and one list for the app; a second markdown surface would need Zeron's scoped keys
thread_local! {
    static PAINTED: RefCell<Vec<Painted>> = const { RefCell::new(Vec::new()) };
    static SELECTION: RefCell<Option<Selection>> = const { RefCell::new(None) };
    static STAMP: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A clock for selections: the one made last is the one Cmd+C copies, whichever surface made it.
pub fn next_stamp() -> u64 {
    STAMP.with(|s| {
        s.set(s.get() + 1);
        s.get()
    })
}

/// When the markdown selection was made, while there is one.
pub fn selection_stamp() -> Option<u64> {
    SELECTION.with(|s| s.borrow().as_ref().map(|s| s.at))
}

/// Clears the paint-order list; painted before the first paragraph.
pub fn frame_reset() -> impl IntoElement {
    canvas(|_, _, _| (), |_, _, _, _| PAINTED.with(|p| p.borrow_mut().clear())).absolute().size_0()
}

/// The reader's markdown selection keys start with it.
pub const READER_KEY: &str = "reader-md";

/// Drops the selection and the paint-order list, for when no panel's markdown is shown: nothing is left to copy.
/// The reader's selection stays: its dialog is not one of those panels.
pub fn forget() {
    SELECTION.with(|s| {
        let mut s = s.borrow_mut();
        if s.as_ref().is_some_and(|s| !s.anchor_key.starts_with(READER_KEY)) {
            s.take();
        }
    });
    PAINTED.with(|p| p.borrow_mut().clear());
}

/// Drops the reader's selection, for when its dialog closes.
pub fn forget_reader() {
    SELECTION.with(|s| {
        let mut s = s.borrow_mut();
        if s.as_ref().is_some_and(|s| s.anchor_key.starts_with(READER_KEY)) {
            s.take();
        }
    });
}

/// The selected text, paragraphs a line apart; `None` when nothing is selected, or its paragraphs were not all
/// painted last frame with the text it was made on (a closed log, another PR's description).
pub fn selected_text() -> Option<String> {
    SELECTION.with(|s| {
        let s = s.borrow();
        let spans = &s.as_ref()?.spans;
        let shown = PAINTED.with(|p| spans.iter().all(|s| p.borrow().iter().any(|e| e.key == s.key && e.text == s.text)));
        (shown && !spans.iter().all(|s| s.range.is_empty())).then(|| join(spans))
    })
}

fn join(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text[s.range.clone()].replace(PAD, "")).collect::<Vec<_>>().join("\n")
}

/// Selects every paragraph painted last frame under keys starting `key`, for Cmd+A.
pub fn select_all(key: &str) {
    let spans: Vec<Span> = PAINTED.with(|p| {
        p.borrow().iter().filter(|e| e.key.starts_with(key)).map(|e| Span { key: e.key.clone(), range: 0..e.text.len(), text: e.text.clone() }).collect()
    });
    let Some(first) = spans.first() else { return };
    let anchor_key = first.key.clone();
    SELECTION.with(|s| *s.borrow_mut() = Some(Selection { anchor_key, anchor: 0, dragging: false, spans, at: next_stamp() }));
}

/// The spans between `(paragraph, byte)` points `a` and `b` over `painted` (key, text), either way round.
fn resolve(painted: &[(SharedString, SharedString)], a: (usize, usize), b: (usize, usize)) -> Vec<Span> {
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    painted
        .iter()
        .enumerate()
        .take(end.0 + 1)
        .skip(start.0)
        .filter_map(|(i, (key, text))| {
            let from = if i == start.0 { start.1 } else { 0 }.min(text.len());
            let to = if i == end.0 { end.1 } else { text.len() }.min(text.len());
            (from < to).then(|| Span { key: key.clone(), range: from..to, text: text.clone() })
        })
        .collect()
}

/// The alphanumeric run around byte `ix` for a double click, or the one character there.
pub(crate) fn word(text: &str, ix: usize) -> Range<usize> {
    let ix = ix.min(text.len());
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..ix].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map_or(ix, |(i, _)| i);
    let end = text[ix..].char_indices().find(|(_, c)| !is_word(*c)).map_or(text.len(), |(i, _)| ix + i);
    if start < end {
        return start..end;
    }
    text[ix..].chars().next().filter(|c| !c.is_whitespace()).map_or(ix..ix, |c| ix..ix + c.len_utf8())
}

/// The paragraph whose band holds `position` (else the nearest above or below) and the byte under it.
fn point_at(position: gpui::Point<Pixels>) -> Option<(usize, usize)> {
    PAINTED.with(|p| {
        let painted = p.borrow();
        let distance = |b: Bounds<Pixels>| f32::from((b.top() - position.y).max(position.y - b.bottom()).max(px(0.)));
        let (i, entry) = painted.iter().enumerate().min_by(|(_, a), (_, b)| distance(a.layout.bounds()).total_cmp(&distance(b.layout.bounds())))?;
        let (Ok(ix) | Err(ix)) = entry.layout.index_for_position(position);
        Some((i, ix))
    })
}

/// Washes the paragraph's selected range, registers it and listens for presses and drags over it.
fn paint_selection(window: &mut Window, hitbox: Hitbox, key: &SharedString, text: &SharedString, layout: &TextLayout, wash: gpui::Hsla) {
    let range = SELECTION.with(|s| s.borrow().as_ref()?.spans.iter().find(|s| &s.key == key).map(|s| s.range.clone()));
    for rect in range.map(|r| rects(layout, &r)).unwrap_or_default() {
        window.paint_quad(gpui::fill(rect, wash));
    }
    PAINTED.with(|p| {
        let mut p = p.borrow_mut();
        // ponytail: a surface paints frame_reset before its first paragraph; one that forgets grows this every
        // frame, so it is cut loudly here instead of leaking
        if p.len() >= 10_000 {
            debug_assert!(false, "markdown painted without a frame_reset");
            p.clear();
        }
        p.push(Painted { key: key.clone(), text: text.clone(), layout: layout.clone() });
    });

    let (down_key, down_text, down_layout) = (key.clone(), text.clone(), layout.clone());
    window.on_mouse_event(move |e: &MouseDownEvent, phase, window, _| {
        if phase != DispatchPhase::Bubble || e.button != MouseButton::Left {
            return;
        }
        if hitbox.is_hovered(window) && down_layout.bounds().contains(&e.position) {
            let (Ok(ix) | Err(ix)) = down_layout.index_for_position(e.position);
            let range = match e.click_count {
                1 => ix..ix,
                2 => word(&down_text, ix),
                _ => 0..down_text.len(),
            };
            let spans = vec![Span { key: down_key.clone(), range: range.clone(), text: down_text.clone() }];
            SELECTION.with(|s| *s.borrow_mut() = Some(Selection { anchor_key: down_key.clone(), anchor: range.start, dragging: true, spans, at: next_stamp() }));
            window.refresh();
        } else if SELECTION.with(|s| s.borrow().as_ref().is_some_and(|s| s.anchor_key == down_key && !s.dragging)) {
            // a press anywhere else (another paragraph claims it right after) clears the selection
            SELECTION.with(|s| *s.borrow_mut() = None);
            window.refresh();
        }
    });
    let move_key = key.clone();
    window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, _| {
        if phase != DispatchPhase::Bubble || !e.dragging() {
            return;
        }
        // only the anchor paragraph's listener drives the drag
        let Some(anchor) = SELECTION.with(|s| s.borrow().as_ref().filter(|s| s.dragging && s.anchor_key == move_key).map(|s| s.anchor)) else { return };
        let Some(head) = point_at(e.position) else { return };
        let painted: Vec<(SharedString, SharedString)> = PAINTED.with(|p| p.borrow().iter().map(|e| (e.key.clone(), e.text.clone())).collect());
        let Some(anchor_ix) = painted.iter().position(|(k, _)| *k == move_key) else { return };
        let spans = resolve(&painted, (anchor_ix, anchor), head);
        let changed = SELECTION.with(|s| {
            let mut s = s.borrow_mut();
            let s = s.as_mut().expect("dragging");
            (s.spans != spans).then(|| s.spans = spans).is_some()
        });
        if changed {
            window.refresh();
        }
    });
    let up_key = key.clone();
    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let done = SELECTION.with(|s| {
            let mut s = s.borrow_mut();
            let sel = s.as_mut().filter(|s| s.dragging && s.anchor_key == up_key)?;
            sel.dragging = false;
            if sel.spans.iter().all(|s| s.range.is_empty()) {
                *s = None;
                return None;
            }
            Some(join(&sel.spans))
        });
        // as a pane's selection does, when the settings ask for it
        if let Some(text) = done.filter(|_| crate::settings::SettingsGlobal::of(cx).settings.copy_on_select) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    });
}

/// The wash boxes of a byte range, one per visual line it covers (Zeron's `range_rects`).
fn rects(layout: &TextLayout, range: &Range<usize>) -> Vec<Bounds<Pixels>> {
    let (bounds, line_height) = (layout.bounds(), layout.line_height());
    let mut out = Vec::new();
    let mut cur = range.start;
    while cur < range.end && out.len() < 256 {
        let Some(mut start) = layout.position_for_index(cur) else { break };
        // a soft wrap's index sits at the end of the row above
        if let Some(after) = layout.position_for_index(cur + 1).filter(|a| a.y > start.y) {
            start = point(bounds.left(), after.y);
        }
        let end = match layout.position_for_index(range.end) {
            Some(p) if p.y == start.y => range.end,
            _ => {
                let (mut lo, mut hi) = (cur, range.end);
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    match layout.position_for_index(mid) {
                        Some(p) if p.y == start.y => lo = mid,
                        _ => hi = mid,
                    }
                }
                lo
            }
        };
        if let Some(p) = layout.position_for_index(end).filter(|p| p.x > start.x) {
            out.push(Bounds::new(start, size(p.x - start.x, line_height)));
        }
        if end <= cur {
            break;
        }
        cur = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Page(Vec<Block>, gpui::ScrollHandle);

    impl gpui::Render for Page {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            crate::ui::viewer::markdown_preview("page", &self.0, &Links::web(), &self.1, &theme)
        }
    }

    /// The pairs of paragraphs painted last frame whose drawn lines run into each other, and the paragraphs whose
    /// box is shorter than their lines: what a reader sees as one block drawn over the next.
    fn overlaps() -> Vec<String> {
        let lines: Vec<(Bounds<Pixels>, Pixels, SharedString)> = PAINTED.with(|p| {
            p.borrow()
                .iter()
                .map(|e| {
                    let line = e.layout.line_height();
                    let drawn = e.layout.position_for_index(e.text.len()).map_or(px(0.), |p| p.y + line);
                    (e.layout.bounds(), drawn, e.text.clone())
                })
                .collect()
        });
        let slack = px(0.5);
        let head = |text: &SharedString| text.chars().take(30).collect::<String>();
        let rect = |(b, drawn, _): &(Bounds<Pixels>, Pixels, SharedString)| (b.left(), b.top(), b.right(), (*drawn).max(b.bottom()));
        let mut found = Vec::new();
        for (i, a) in lines.iter().enumerate() {
            if a.1 > a.0.bottom() + slack {
                found.push(format!("{:?} is drawn to {:?}, past its box's {:?}", head(&a.2), a.1, a.0.bottom()));
            }
            for b in &lines[i + 1..] {
                let (a_rect, b_rect) = (rect(a), rect(b));
                if a_rect.0 < b_rect.2 - slack && b_rect.0 < a_rect.2 - slack && a_rect.1 < b_rect.3 - slack && b_rect.1 < a_rect.3 - slack {
                    found.push(format!("{:?} ({:?}..{:?}) runs into {:?} ({:?}..{:?})", head(&a.2), a_rect.1, a_rect.3, head(&b.2), b_rect.1, b_rect.3));
                }
            }
        }
        found
    }

    /// Shepherd's own README at the widths a window has, laid out by the reader's preview: no paragraph drawn over
    /// another. The blocks once sat in a flex column, whose items are sized at the container's width before the
    /// page's `max_w` narrows them: a long paragraph got the box of its lines across the window, and the next block
    /// drew over its last ones.
    #[gpui::test]
    fn the_readme_paints_no_block_over_another(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let blocks = crate::ui::review::markdown(include_str!("../../README.md"));
        let (_, cx) = cx.add_window_view(|_, _| Page(blocks.clone(), gpui::ScrollHandle::new()));
        for width in [1348., 900., 600., 400.] {
            cx.simulate_resize(size(px(width), px(900.)));
            PAINTED.with(|p| p.borrow_mut().clear());
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            let painted = PAINTED.with(|p| p.borrow().len());
            assert!(painted > 100, "at {width} px only {painted} paragraphs were painted");
            let found = overlaps();
            assert!(found.is_empty(), "at {width} px: {found:#?}");
        }
    }

    #[test]
    fn spans_cover_the_paragraphs_between_either_way() {
        let painted: Vec<(SharedString, SharedString)> = [("a", "first"), ("b", "second"), ("c", "third")].map(|(k, t)| (k.into(), t.into())).into();
        let spans = resolve(&painted, (2, 2), (0, 3));
        assert_eq!(join(&spans), "st\nsecond\nth");
        assert_eq!(spans.iter().map(|s| s.key.as_ref()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert!(resolve(&painted, (1, 2), (1, 2)).is_empty());
        // a code run's padding stays out of a copy
        let code: SharedString = format!("run {PAD}cargo{PAD} now").into();
        assert_eq!(join(&[Span { key: "d".into(), range: 0..code.len(), text: code }]), "run cargo now");
    }

    #[test]
    fn links_resolve_to_the_web_a_file_in_the_root_a_heading_or_nowhere() {
        let (doc, root) = (Path::new("/w/docs/guide.md"), Path::new("/w"));
        let at = Some((doc, root));
        let file = |p: &str, f: Option<&str>| Link::File { path: p.into(), fragment: f.map(String::from) };
        assert_eq!(resolve_link("https://x.dev/a?b#c", at), Link::External("https://x.dev/a?b#c".into()));
        assert_eq!(resolve_link(" HTTP://x.dev ", at), Link::External("HTTP://x.dev".into()));
        assert_eq!(resolve_link("mailto:a@b.c", at), Link::External("mailto:a@b.c".into()));
        assert_eq!(resolve_link("intro.md", at), file("/w/docs/intro.md", None));
        assert_eq!(resolve_link("./intro.md#set-up", at), file("/w/docs/intro.md", Some("set-up")));
        assert_eq!(resolve_link("../src/main.rs#L12", at), file("/w/src/main.rs", Some("L12")));
        assert_eq!(resolve_link("/README.md", at), file("/w/README.md", None), "a leading / is the root");
        assert_eq!(resolve_link("my%20notes.md?plain=1#a%20b", at), file("/w/docs/my notes.md", Some("a b")));
        assert_eq!(resolve_link("../", at), file("/w", None));
        assert_eq!(resolve_link("#usage", at), Link::Anchor("usage".into()));
        assert_eq!(resolve_link("intro.md#", at), file("/w/docs/intro.md", None));
        assert_eq!(resolve_link("../../etc/passwd", at), Link::Disabled("Outside this workspace's folder".into()));
        assert_eq!(resolve_link("/../../etc/passwd", at), Link::Disabled("Outside this workspace's folder".into()));
        assert_eq!(resolve_link("../../wx/a.md", at), Link::Disabled("Outside this workspace's folder".into()), "a sibling sharing the prefix");
        assert_eq!(resolve_link("file:///etc/passwd", at), Link::Disabled("file: links don't open from here".into()));
        assert_eq!(resolve_link("vscode://open?x", at), Link::Disabled("vscode: links don't open from here".into()));
        assert_eq!(resolve_link("javascript:alert(1)", at), Link::Disabled("javascript: links don't open from here".into()));
        assert_eq!(resolve_link("", at), Link::Disabled("Empty link".into()));
        // no file to be relative to: the web only
        assert_eq!(resolve_link("https://x.dev", None), Link::External("https://x.dev".into()));
        assert!(matches!(resolve_link("docs/a.md", None), Link::Disabled(_)));
        assert!(matches!(resolve_link("#usage", None), Link::Disabled(_)));
    }

    #[test]
    fn fragments_name_lines_and_headings() {
        assert_eq!(fragment_line("L12"), Some(12));
        assert_eq!(fragment_line("L3-L9"), Some(3));
        assert_eq!(fragment_line("L0"), None);
        assert_eq!(fragment_line("usage"), None);
        assert_eq!(slug(" Set up: the `CLI` (v2) "), "set-up-the-cli-v2");
        assert_eq!(slug("Über_alles"), "über_alles");
        let heading = |t: &str| Block::Heading { level: 2, runs: vec![InlineRun { text: t.into(), style: Default::default() }] };
        let blocks = [heading("Usage"), Block::Rule, heading("Usage"), heading("Set up")];
        assert_eq!(anchor_block(&blocks, "usage"), Some(0));
        assert_eq!(anchor_block(&blocks, "usage-1"), Some(2));
        assert_eq!(anchor_block(&blocks, "Set-Up"), Some(3));
        assert_eq!(anchor_block(&blocks, "nope"), None);
    }

    #[test]
    fn a_double_click_takes_the_word() {
        assert_eq!(word("fix the login_bug.", 10), 8..17);
        assert_eq!(word("a + b", 2), 2..3);
        assert_eq!(word("a  b", 2), 2..2);
    }

    #[test]
    fn highlights_land_on_their_lines() {
        let source = "let a = 1;\nfn go() {}";
        let spans = syntax(source, Some("rust"));
        let at = |text: &str| spans.iter().find(|(r, _)| &source[r.clone()] == text).map(|(_, k)| *k);
        assert_eq!(at("let"), Some(HighlightKind::Keyword));
        assert_eq!(at("fn"), Some(HighlightKind::Keyword));
        assert_eq!(at("go"), Some(HighlightKind::Function));
        assert!(syntax(source, Some("no-such-language")).is_empty());
    }
}
