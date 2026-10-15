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
use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, Font, FontStyle, FontWeight, Hitbox, HitboxBehavior, InteractiveText, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, SharedString, StrikethroughStyle, Styled, StyledText,
    TextLayout, TextRun, UnderlineStyle, Window, canvas, div, point, px, size,
};
use std::cell::RefCell;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

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

/// `blocks`, keyed under `key` (their paragraphs' selection keys and element ids start with it), their links
/// followed as `links` says. A view of them paints one [`frame_reset`] before the first, however many times it
/// renders some.
pub fn render(blocks: &[Block], key: &str, links: &Links, theme: &Theme) -> Vec<AnyElement> {
    blocks.iter().enumerate().map(|(i, b)| block(b, &format!("{key}-{i}"), links, theme)).collect()
}

fn block(b: &Block, key: &str, links: &Links, theme: &Theme) -> AnyElement {
    match b {
        Block::Paragraph { runs } => text(runs, key, theme.text_muted, false, links, theme),
        Block::Heading { level, runs } => div()
            .pt(px(4.))
            .text_size(px(if *level <= 1 { 15. } else if *level == 2 { 14. } else { 13. }))
            .child(text(runs, key, theme.text, true, links, theme))
            .into_any_element(),
        Block::CodeBlock { code: c, .. } => code(c, key, px(11.5), theme),
        Block::BlockQuote { children } => div()
            .pl(px(8.))
            .border_l_2()
            .border_color(theme.border_strong)
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(children.iter().enumerate().map(|(i, b)| block(b, &format!("{key}-{i}"), links, theme)))
            .into_any_element(),
        Block::List { ordered_start, items } => div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(items.iter().enumerate().map(|(i, item)| {
                let marker = ordered_start.map_or("•".to_string(), |n| format!("{}.", n + i as u64));
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
                            .children(item.iter().enumerate().map(|(j, b)| block(b, &format!("{key}-{i}-{j}"), links, theme))),
                    )
            }))
            .into_any_element(),
        Block::Table { header, rows, .. } => {
            let row = |cells: &[Vec<InlineRun>], r: usize, head: bool| {
                div().flex().border_t_1().border_color(theme.border).children(cells.iter().enumerate().map(|(c, cell)| {
                    let color = if head { theme.text } else { theme.text_muted };
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
        .child(text_in(&[run], key, theme.text_muted, false, theme.font_terminal.clone(), &Links::web(), theme))
        .into_any_element()
}

fn text(runs: &[InlineRun], key: &str, color: gpui::Hsla, bold: bool, links: &Links, theme: &Theme) -> AnyElement {
    text_in(runs, key, color, bold, theme.font_ui.clone(), links, theme)
}

/// One paragraph: its runs styled, its links clickable (a disabled one faint, its reason its tooltip), selectable.
fn text_in(runs: &[InlineRun], key: &str, color: gpui::Hsla, bold: bool, family: SharedString, follow: &Links, theme: &Theme) -> AnyElement {
    let flat: SharedString = runs_text(runs).into();
    let mut links: Vec<(Range<usize>, Link)> = Vec::new();
    let mut at = 0;
    let text_runs: Vec<TextRun> = runs
        .iter()
        .map(|r| {
            let s = &r.style;
            let range = at..at + r.text.len();
            at = range.end;
            // a link's colour says whether it goes anywhere
            let link = s.link.as_ref().map(|href| follow.resolve(href));
            let tint = link.as_ref().map(|l| if matches!(l, Link::Disabled(_)) { theme.text_faint } else { theme.accent });
            if let Some(link) = link {
                links.push((range, link));
            }
            TextRun {
                len: r.text.len(),
                font: Font {
                    // Geist has no bold: semibold reads as it
                    weight: if bold || s.bold { FontWeight::SEMIBOLD } else { FontWeight::NORMAL },
                    style: if s.italic { FontStyle::Italic } else { FontStyle::Normal },
                    ..gpui::font(if s.code { theme.font_terminal.clone() } else { family.clone() })
                },
                color: tint.unwrap_or(color),
                background_color: s.code.then(|| theme.wash(0.06)),
                underline: tint.map(|tint| UnderlineStyle { thickness: px(1.), color: Some(tint.opacity(0.5)), wavy: false }),
                strikethrough: s.strikethrough.then(|| StrikethroughStyle { thickness: px(1.), color: None }),
            }
        })
        .collect();
    let styled = StyledText::new(flat.clone()).with_runs(text_runs);
    let layout = styled.layout().clone();
    let wash = theme.accent.opacity(0.25);
    let key: SharedString = key.to_string().into();
    let underlay = {
        let key = key.clone();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| paint_selection(window, hitbox, &key, &flat, &layout, wash),
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

/// Drops the selection and the paint-order list, for when no markdown is shown: nothing is left to copy.
pub fn forget() {
    SELECTION.with(|s| s.borrow_mut().take());
    PAINTED.with(|p| p.borrow_mut().clear());
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
    spans.iter().map(|s| &s.text[s.range.clone()]).collect::<Vec<_>>().join("\n")
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

    #[test]
    fn spans_cover_the_paragraphs_between_either_way() {
        let painted: Vec<(SharedString, SharedString)> = [("a", "first"), ("b", "second"), ("c", "third")].map(|(k, t)| (k.into(), t.into())).into();
        let spans = resolve(&painted, (2, 2), (0, 3));
        assert_eq!(join(&spans), "st\nsecond\nth");
        assert_eq!(spans.iter().map(|s| s.key.as_ref()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert!(resolve(&painted, (1, 2), (1, 2)).is_empty());
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
}
