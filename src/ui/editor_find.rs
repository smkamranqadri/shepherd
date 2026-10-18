//! The editor's find bar and Go to Line. The bar sits under the file's header: the query with its Aa (match case),
//! ab (whole word) and .* (regex) switches, which match of how many is current, previous and next, and a disclosure
//! for the replace row (hidden in a read-only file). Every match is marked faintly in the text and the current one
//! strongly, as decorations that follow edits; the matching (`files::find`) runs off the main thread a moment after
//! the query or the buffer changes, and only the latest search's answer is shown. The switches are kept for the run
//! and shared by every tab's bar.
//!
//! Go to Line is a field over the top of the text: a line, `line:column` or `:column`, clamped to the file.
//!
//! While one of their fields has the keyboard, the file tab's view hands out that field's focus handle (`Find::
//! focus_handle`), so the Shell sees the view as focused and gives the keyboard back to the field, not the text.
use crate::bindings::{EDITOR_FIND, FindNext, FindPrevious};
use crate::files::find::{self, FindQuery, MAX_MATCHES, Matches};
use crate::shell::widgets::{tip, titlebar_button};
use crate::theme::Theme;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EntityId, FocusHandle, Focusable, Global, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, div, prelude::*, px,
};
use gpui_base::input::{EditorState, Input, InputEvent, InputState, Rope, RopeExt, TextDecoration, TextDecorationCollection};
use std::ops::Range;
use std::time::Duration;

/// How long after the last keystroke (in the query or the text) the matches are found again.
const SEARCH_AFTER: Duration = Duration::from_millis(150);
/// The longest selection that prefills the query.
const MAX_PREFILL: usize = 1000;

/// The bar's switches, for the run, the same in every tab.
#[derive(Debug, Clone, Copy, Default)]
struct Switches {
    case: bool,
    word: bool,
    regex: bool,
}

impl Global for Switches {}

fn switches(cx: &App) -> Switches {
    cx.try_global::<Switches>().copied().unwrap_or_default()
}

/// A file tab's find bar and Go to Line, made when first asked for, on the editor's text they were opened on.
#[derive(Default)]
pub struct Find {
    bar: Option<Entity<FindBar>>,
    goto: Option<Entity<GoToLine>>,
}

impl Find {
    /// Find (`replace` false) or Replace: the bar opens (or takes the keyboard again) on `state`, its query the
    /// selection when that is one line, and all of it selected.
    pub fn find(&mut self, state: Entity<EditorState>, replace: bool, window: &mut Window, cx: &mut App) {
        let bar = match self.bar.clone().filter(|b| b.read(cx).state_id == state.entity_id()) {
            Some(bar) => bar,
            None => {
                let bar = cx.new(|cx| FindBar::new(&state, window, cx));
                self.bar = Some(bar.clone());
                bar
            }
        };
        bar.update(cx, |b, cx| b.open(replace, window, cx));
    }

    /// Go to Line: its field over the text, empty, with the keyboard.
    pub fn go_to_line(&mut self, state: Entity<EditorState>, window: &mut Window, cx: &mut App) {
        let goto = match self.goto.clone().filter(|g| g.read(cx).state_id == state.entity_id()) {
            Some(goto) => goto,
            None => {
                let goto = cx.new(|cx| GoToLine::new(&state, window, cx));
                self.goto = Some(goto.clone());
                goto
            }
        };
        goto.update(cx, |g, cx| g.open(window, cx));
    }

    /// Esc anywhere in the text (`state`, `shown`: not under a preview or a Compare), its bar or Go to Line: closes
    /// Go to Line, else the bar, and is taken; with neither open, or the text not shown, it is left alone.
    pub fn escape(&mut self, state: &Entity<EditorState>, shown: bool, window: &mut Window, cx: &mut App) -> bool {
        let id = state.entity_id();
        let goto = self.goto.clone().filter(|g| g.read(cx).state_id == id && g.read(cx).open);
        let bar = self.bar.clone().filter(|b| b.read(cx).state_id == id && b.read(cx).open);
        match escape_closes(shown, goto.is_some(), bar.is_some()) {
            Some(Closes::GoToLine) => goto.into_iter().for_each(|g| g.update(cx, |g, cx| g.close(window, cx))),
            Some(Closes::Bar) => bar.into_iter().for_each(|b| b.update(cx, |b, cx| b.close(window, cx))),
            None => return false,
        }
        true
    }

    /// The focus handle of the field of theirs that has the keyboard, on `state`, if one does.
    pub fn focus_handle(&self, state: &Entity<EditorState>, cx: &App) -> Option<FocusHandle> {
        let id = state.entity_id();
        let goto = self.goto.as_ref().map(|g| g.read(cx)).filter(|g| g.state_id == id && g.open && g.keys).map(|g| g.input.focus_handle(cx));
        goto.or_else(|| self.bar.as_ref().map(|b| b.read(cx)).filter(|b| b.state_id == id && b.open).and_then(|b| b.keys.map(|f| b.field(f).focus_handle(cx))))
    }

    /// The bar (in the column, above the text) and Go to Line (over it), when open on `state` and the text shows.
    pub fn elements(&self, state: &Entity<EditorState>, shown: bool, cx: &App) -> Vec<AnyElement> {
        let id = state.entity_id();
        let mut out = Vec::new();
        if !shown {
            return out;
        }
        if let Some(bar) = self.bar.as_ref().filter(|b| b.read(cx).state_id == id && b.read(cx).open) {
            out.push(bar.clone().into_any_element());
        }
        if let Some(goto) = self.goto.as_ref().filter(|g| g.read(cx).state_id == id && g.read(cx).open) {
            // painted after the text laid out below it
            out.push(gpui::deferred(goto.clone().into_any_element()).with_priority(1).into_any_element());
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closes {
    GoToLine,
    Bar,
}

/// What Esc closes: Go to Line over the bar, else the bar; nothing when neither is open or the text is not shown
/// (Esc is the text's, or the preview's, then).
fn escape_closes(shown: bool, goto_open: bool, bar_open: bool) -> Option<Closes> {
    if !shown {
        None
    } else if goto_open {
        Some(Closes::GoToLine)
    } else if bar_open {
        Some(Closes::Bar)
    } else {
        None
    }
}

/// The rope as one string (off the main thread: a 4 MB copy).
fn rope_text(rope: &Rope) -> String {
    let mut text = String::with_capacity(rope.len());
    for chunk in rope.chunks() {
        text.push_str(chunk);
    }
    text
}

fn field_style(theme: &Theme) -> gpui_base::input::InputEditorStyle {
    crate::ui::input_style(theme)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Query,
    With,
}

pub struct FindBar {
    state: WeakEntity<EditorState>,
    state_id: EntityId,
    open: bool,
    query: Entity<InputState>,
    with: Entity<InputState>,
    /// The replace row was asked for (it shows only in a file that can be written).
    replace: bool,
    /// The field of the bar's that has the keyboard.
    keys: Option<Field>,
    matches: Vec<Range<usize>>,
    capped: bool,
    error: Option<String>,
    current: Option<usize>,
    /// The next answer selects the match current for the cursor (the query changed, a replace): typing in the text
    /// only marks them again.
    reveal: bool,
    /// Numbers the searches: only the latest one's answer is shown.
    generation: u64,
    /// Counts the text's changes, so a Replace All whose text moved meanwhile is dropped.
    edits: u64,
    searching: Option<Task<()>>,
    replacing: Option<Task<()>>,
    /// The current match, then all: the first collection wins where they overlap.
    current_marks: TextDecorationCollection,
    all_marks: TextDecorationCollection,
    _events: Vec<Subscription>,
}

impl FindBar {
    fn new(state: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> FindBar {
        let theme = Theme::of(cx).clone();
        let mut field = |placeholder: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                input.set_editor_style(field_style(&theme));
                input
            })
        };
        let (query, with) = (field("Find", cx), field("Replace", cx));
        let (current_marks, all_marks) =
            state.update(cx, |s, cx| (s.create_decorations_collection(Vec::new(), cx), s.create_decorations_collection(Vec::new(), cx)));
        let events = vec![
            cx.subscribe_in(&query, window, |b: &mut FindBar, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    b.reveal = true;
                    b.schedule(SEARCH_AFTER, cx);
                }
                InputEvent::PressEnter { shift, .. } => b.step(!shift, window, cx),
                InputEvent::Focus => b.keys = Some(Field::Query),
                InputEvent::Blur => b.blurred(Field::Query),
            }),
            cx.subscribe_in(&with, window, |b: &mut FindBar, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => b.replace_one(window, cx),
                InputEvent::Focus => b.keys = Some(Field::With),
                InputEvent::Blur => b.blurred(Field::With),
                InputEvent::Change => {}
            }),
            cx.subscribe(state, |b: &mut FindBar, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    b.edits += 1;
                    if b.open {
                        b.schedule(SEARCH_AFTER, cx);
                    }
                }
            }),
        ];
        FindBar {
            state: state.downgrade(),
            state_id: state.entity_id(),
            open: false,
            query,
            with,
            replace: false,
            keys: None,
            matches: Vec::new(),
            capped: false,
            error: None,
            current: None,
            reveal: false,
            generation: 0,
            edits: 0,
            searching: None,
            replacing: None,
            current_marks,
            all_marks,
            _events: events,
        }
    }

    fn field(&self, field: Field) -> &Entity<InputState> {
        match field {
            Field::Query => &self.query,
            Field::With => &self.with,
        }
    }

    fn blurred(&mut self, field: Field) {
        if self.keys == Some(field) {
            self.keys = None;
        }
    }

    fn readonly(&self, cx: &App) -> bool {
        self.state.upgrade().is_none_or(|s| !s.read(cx).is_editable())
    }

    fn query_now(&self, cx: &App) -> FindQuery {
        let s = switches(cx);
        FindQuery { text: self.query.read(cx).value().to_string(), case: s.case, word: s.word, regex: s.regex }
    }

    fn open(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.upgrade() else { return };
        let was_open = std::mem::replace(&mut self.open, true);
        if replace {
            self.replace = true;
        } else if !was_open {
            self.replace = false;
        }
        // a selection on one line is what to find
        let selected = {
            let s = state.read(cx);
            let range = s.selected_range();
            (range.len() <= MAX_PREFILL).then(|| s.text().slice(range).to_string())
        };
        if let Some(text) = selected.as_deref().and_then(find::prefill)
            && *self.query.read(cx).value() != *text
        {
            let text = text.to_string();
            self.query.update(cx, |q, cx| q.set_value(text, window, cx));
        }
        // Replace with a query already there goes to the replacement
        let to = if replace && find::shows_replace(true, self.readonly(cx)) && !self.query.read(cx).value().is_empty() { Field::With } else { Field::Query };
        let input = self.field(to).clone();
        input.update(cx, |i, cx| {
            i.select_all(window, cx);
            i.focus(window, cx);
        });
        self.keys = Some(to);
        self.reveal = true;
        self.schedule(Duration::ZERO, cx);
        cx.notify();
    }

    /// Esc: the bar closes, its marks go, and the text has the keyboard again.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.keys = None;
        (self.searching, self.replacing) = (None, None);
        self.generation += 1;
        self.current_marks.clear(cx);
        self.all_marks.clear(cx);
        if let Some(state) = self.state.upgrade() {
            state.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    /// Finds the matches again, after `after` (a debounce: a newer change starts it over), off the main thread.
    fn schedule(&mut self, after: Duration, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.searching = Some(cx.spawn(async move |this, cx| {
            if !after.is_zero() {
                cx.background_executor().timer(after).await;
            }
            // the text and the query as they are once the pause is over
            let Ok(Some((rope, q))) = this.update(cx, |b, cx| b.state.upgrade().map(|s| (s.read(cx).text().clone(), b.query_now(cx)))) else { return };
            let found = cx.background_spawn(async move { find::find_all(&rope_text(&rope), &q, MAX_MATCHES) }).await;
            let _ = this.update(cx, |b, cx| b.found(generation, found, cx));
        }));
    }

    fn found(&mut self, generation: u64, found: Result<Matches, String>, cx: &mut Context<Self>) {
        if generation != self.generation || !self.open {
            return;
        }
        let Some(state) = self.state.upgrade() else { return };
        (self.matches, self.capped, self.error) = match found {
            Ok(m) => (m.ranges, m.capped, None),
            Err(why) => (Vec::new(), false, Some(why)),
        };
        let cursor = state.read(cx).selected_range().start;
        self.current = find::nearest(&self.matches, cursor);
        if std::mem::take(&mut self.reveal)
            && let Some(range) = self.current.map(|i| self.matches[i].clone())
        {
            state.update(cx, |s, cx| s.set_selected_range(range, cx));
        }
        self.mark(cx);
        cx.notify();
    }

    /// The matches marked in the text: all faintly, the current one strongly.
    fn mark(&mut self, cx: &mut App) {
        let theme = Theme::of(cx);
        let style = |opacity: f32| gpui::HighlightStyle { background_color: Some(theme.warning.opacity(opacity)), ..Default::default() };
        let (faint, strong) = (style(0.22), style(0.55));
        let all = self.matches.iter().map(|r| TextDecoration::new(r.clone(), faint)).collect();
        let current = self.current.map(|i| TextDecoration::new(self.matches[i].clone(), strong)).into_iter().collect();
        self.all_marks.set(all, cx);
        self.current_marks.set(current, cx);
    }

    /// Next (or previous) match from the selection, selected and scrolled to. The marks follow edits, so they are
    /// where the matches are even before the next search answers.
    fn step(&mut self, forward: bool, _: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.upgrade() else { return };
        let ranges = self.all_marks.get_ranges(cx);
        let selection = state.read(cx).selected_range();
        // from a selected match, the one after (or before) it
        let next = if forward { find::nearest(&ranges, selection.end) } else { find::previous(&ranges, selection.start) };
        let Some(i) = next else { return };
        let range = ranges[i].clone();
        state.update(cx, |s, cx| s.set_selected_range(range.clone(), cx));
        self.matches = ranges;
        self.current = Some(i);
        self.mark(cx);
        cx.notify();
    }

    /// Replace: the current match (the first at or after the selection's start), checked to be one still, then the
    /// next is current.
    fn replace_one(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.upgrade() else { return };
        if !find::shows_replace(self.replace, self.readonly(cx)) {
            return;
        }
        let ranges = self.all_marks.get_ranges(cx);
        let Some(range) = find::nearest(&ranges, state.read(cx).selected_range().start).map(|i| ranges[i].clone()) else { return };
        let (q, with) = (self.query_now(cx), self.with.read(cx).value().to_string());
        // checked on the lines it spans, not a copy of the whole text
        let (from, lines) = {
            let t = state.read(cx).text();
            let from = t.line_start_offset(t.offset_to_point(range.start).row);
            let to = t.line_end_offset(t.offset_to_point(range.end).row);
            (from, t.slice(from..to).to_string())
        };
        self.reveal = true;
        match find::replace_one(&lines, range.start - from..range.end - from, &q, &with) {
            Some(out) => state.update(cx, |s, cx| {
                s.set_selected_range(range, cx);
                s.replace(out, window, cx);
            }),
            // the text moved under the marks: found again, nothing replaced
            None => self.schedule(Duration::ZERO, cx),
        }
    }

    /// Replace All, worked out off the main thread and put in as one edit (one undo), the scroll kept and the cursor
    /// carried across the replacements; a toast says how many. gpui-base has no undo group to put the replacements
    /// in one by one, so the whole text is set (and highlighted again).
    fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.upgrade() else { return };
        if !find::shows_replace(self.replace, self.readonly(cx)) {
            return;
        }
        let (q, with) = (self.query_now(cx), self.with.read(cx).value().to_string());
        let rope = state.read(cx).text().clone();
        let edits = self.edits;
        self.replacing = Some(cx.spawn_in(window, async move |this, cx| {
            let done = cx.background_spawn(async move { find::replace_all_text(&rope_text(&rope), &q, &with) }).await;
            let _ = this.update_in(cx, |b, window, cx| {
                let Some(state) = b.state.upgrade() else { return };
                if b.edits != edits {
                    return crate::ui::toast::error(cx, "The file changed during Replace All: nothing was replaced");
                }
                let (text, replaced) = match done {
                    Ok((_, edits)) if edits.is_empty() => return,
                    Ok(done) => done,
                    Err(why) => return crate::ui::toast::error(cx, why),
                };
                let n = replaced.len();
                state.update(cx, |s, cx| {
                    let (scroll, cursor) = (s.scroll_offset(), s.cursor());
                    s.replace_all(text, window, cx);
                    let cursor = find::map_offset(cursor, &replaced).min(s.text().len());
                    s.set_selected_range(cursor..cursor, cx);
                    s.set_scroll_offset(scroll, cx);
                });
                let what = if n == 1 { "Replaced 1 match".to_string() } else { format!("Replaced {} matches", find::group(n)) };
                crate::ui::toast::info(cx, what, None);
            });
        }));
    }

    fn set_switch(&mut self, f: impl FnOnce(&mut Switches), cx: &mut Context<Self>) {
        f(cx.default_global::<Switches>());
        self.reveal = true;
        self.schedule(Duration::ZERO, cx);
        cx.notify();
    }
}

/// A text button of the bar's: Replace, Replace All. It leaves the keyboard where it is.
fn text_button(theme: &Theme, id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .flex_none()
        .px(px(8.))
        .h(px(22.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.))
        .text_color(theme.text)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .child(label)
}

/// A field as the Search tab draws its query: a washed rounded box, red edged when `error` says why.
fn field_box(theme: &Theme, id: &'static str, input: &Entity<InputState>, error: Option<&str>) -> gpui::Stateful<gpui::Div> {
    let field = div()
        .id(id)
        .h(px(26.))
        .flex_1()
        .min_w_0()
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(if error.is_some() { theme.danger } else { gpui::transparent_black() })
        .bg(theme.wash(0.06))
        .text_size(px(12.5))
        .child(div().flex_1().min_w_0().child(Input::new(input)));
    match error {
        Some(why) => field.tooltip(tip(why.to_string(), None)),
        None => field,
    }
}

impl Render for FindBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &Theme::of(cx).clone();
        let s = switches(cx);
        let readonly = self.readonly(cx);
        let replace = find::shows_replace(self.replace, readonly);
        let hover = theme.hover;
        let switch = |id: &'static str, label: &'static str, on: bool, what: &'static str| {
            div()
                .id(id)
                .size(px(22.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(if on { theme.accent } else { theme.text_muted })
                .when(on, |el| el.bg(theme.accent.opacity(0.15)))
                .when(!on, |el| el.hover(move |s| s.bg(hover)))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .child(label)
                .tooltip(tip(what, None))
        };
        let count = if self.query.read(cx).value().is_empty() || self.error.is_some() {
            SharedString::default()
        } else {
            find::count_label(self.current, self.matches.len(), self.capped).into()
        };
        let none = !count.is_empty() && self.matches.is_empty();
        let disclosure = (!readonly).then(|| {
            let icon = if replace { "icons/alt-arrow-down.svg" } else { "icons/alt-arrow-right.svg" };
            titlebar_button(theme, "find-toggle-replace", icon).size(px(22.)).tooltip(tip("Toggle Replace", Some("ReplaceInFile"))).on_click(cx.listener(
                |b: &mut FindBar, _: &ClickEvent, window, cx| {
                    b.replace = !b.replace;
                    if !b.replace && b.keys == Some(Field::With) {
                        b.query.update(cx, |q, cx| q.focus(window, cx));
                    }
                    cx.notify();
                },
            ))
        });
        let find_row = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .children(disclosure)
            .child(div().flex_1().min_w_0().mr(px(2.)).flex().child(field_box(theme, "find-query", &self.query, self.error.as_deref())))
            .child(switch("find-case", "Aa", s.case, "Match Case").on_click(cx.listener(|b, _: &ClickEvent, _, cx| b.set_switch(|s| s.case = !s.case, cx))))
            .child(switch("find-word", "ab", s.word, "Match Whole Word").on_click(cx.listener(|b, _: &ClickEvent, _, cx| b.set_switch(|s| s.word = !s.word, cx))))
            .child(switch("find-regex", ".*", s.regex, "Use Regular Expression").on_click(cx.listener(|b, _: &ClickEvent, _, cx| b.set_switch(|s| s.regex = !s.regex, cx))))
            .child(
                div()
                    .flex_none()
                    .min_w(px(72.))
                    .px(px(6.))
                    .text_size(px(11.5))
                    .text_color(if none { theme.warning } else { theme.text_muted })
                    .child(count),
            )
            .child(
                titlebar_button(theme, "find-previous", "icons/arrow-up.svg")
                    .size(px(22.))
                    .tooltip(tip("Previous Match (Shift+Enter)", None))
                    .on_click(cx.listener(|b, _: &ClickEvent, window, cx| b.step(false, window, cx))),
            )
            .child(
                titlebar_button(theme, "find-next", "icons/arrow-down.svg")
                    .size(px(22.))
                    .tooltip(tip("Next Match (Enter)", None))
                    .on_click(cx.listener(|b, _: &ClickEvent, window, cx| b.step(true, window, cx))),
            )
            .child(
                titlebar_button(theme, "find-close", "icons/close.svg")
                    .size(px(22.))
                    .tooltip(tip("Close (Escape)", None))
                    .on_click(cx.listener(|b, _: &ClickEvent, window, cx| b.close(window, cx))),
            );
        let replace_row = replace.then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(div().flex_none().w(px(22.)))
                .child(div().flex_1().min_w_0().flex().child(field_box(theme, "find-with", &self.with, None)))
                .child(text_button(theme, "find-replace", "Replace").on_click(cx.listener(|b, _: &ClickEvent, window, cx| b.replace_one(window, cx))))
                .child(text_button(theme, "find-replace-all", "Replace All").on_click(cx.listener(|b, _: &ClickEvent, window, cx| b.replace_all(window, cx))))
                // as wide as the count and the three buttons above, so both fields end together
                .child(div().flex_none().w(px(4.)))
        });
        div()
            .id("editor-find")
            .key_context(EDITOR_FIND)
            .flex_none()
            .px(px(8.))
            .py(px(4.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.wash(0.04))
            .font_family(theme.font_ui.clone())
            .text_size(px(12.5))
            .on_action(cx.listener(|b, _: &FindNext, window, cx| b.step(true, window, cx)))
            .on_action(cx.listener(|b, _: &FindPrevious, window, cx| b.step(false, window, cx)))
            .child(find_row)
            .children(replace_row)
    }
}

/// Go to Line's field, over the top of the text.
pub struct GoToLine {
    state: WeakEntity<EditorState>,
    state_id: EntityId,
    open: bool,
    input: Entity<InputState>,
    /// The field has the keyboard.
    keys: bool,
    _events: Vec<Subscription>,
}

impl GoToLine {
    fn new(state: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> GoToLine {
        let style = field_style(Theme::of(cx));
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_editor_style(style);
            input
        });
        let events = vec![cx.subscribe_in(&input, window, |g: &mut GoToLine, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => g.go(window, cx),
            InputEvent::Focus => g.keys = true,
            // clicked away: it goes, as a quick pick does
            InputEvent::Blur => {
                g.keys = false;
                g.open = false;
                cx.notify();
            }
            InputEvent::Change => {}
        })];
        GoToLine { state: state.downgrade(), state_id: state.entity_id(), open: false, input, keys: false, _events: events }
    }

    /// 1-based: the cursor's line, and how many.
    fn lines(&self, cx: &App) -> Option<(u32, u32)> {
        let state = self.state.upgrade()?;
        let s = state.read(cx);
        let row = s.text().offset_to_position(s.cursor()).line;
        Some((row + 1, s.text().lines_len() as u32))
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((line, lines)) = self.lines(cx) else { return };
        self.open = true;
        let placeholder = format!("Line {line} of {lines}. Type a line, or line:column");
        self.input.update(cx, |i, cx| {
            i.set_value("", window, cx);
            i.set_placeholder(placeholder, window, cx);
            i.focus(window, cx);
        });
        self.keys = true;
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.keys = false;
        if let Some(state) = self.state.upgrade() {
            state.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    /// Enter: the cursor to the place typed, and the text has the keyboard; anything else typed leaves it open.
    fn go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(state), Some((line, lines))) = (self.state.upgrade(), self.lines(cx)) else { return };
        let Some(to) = find::parse_goto(&self.input.read(cx).value(), line, lines) else { return };
        state.update(cx, |s, cx| {
            let row = to.line as usize - 1;
            let start = s.text().line_start_offset(row);
            let column = to.column.map_or(0, |c| find::column_offset(&s.text().slice_line(row).to_string(), c));
            s.set_selected_range(start + column..start + column, cx);
        });
        self.close(window, cx);
    }
}

impl Render for GoToLine {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &Theme::of(cx).clone();
        div().absolute().top(px(44.)).left_0().right_0().flex().justify_center().child(
            div()
                .id("editor-go-to-line")
                .w(px(380.))
                .max_w_full()
                .p(px(6.))
                .rounded(px(8.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.card)
                .shadow_md()
                .occlude()
                .font_family(theme.font_ui.clone())
                .child(div().flex().child(field_box(theme, "go-to-line-field", &self.input, None))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_go_to_line_then_the_bar_and_else_is_the_text_s() {
        assert_eq!(escape_closes(true, true, true), Some(Closes::GoToLine));
        assert_eq!(escape_closes(true, true, false), Some(Closes::GoToLine));
        assert_eq!(escape_closes(true, false, true), Some(Closes::Bar));
        assert_eq!(escape_closes(true, false, false), None, "neither open: Esc is not taken");
        // under the preview or a Compare the bar is not drawn: Esc is theirs
        assert_eq!(escape_closes(false, true, true), None);
        assert_eq!(escape_closes(false, false, true), None);
    }
}
