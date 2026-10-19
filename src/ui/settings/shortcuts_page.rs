//! The Shortcuts page: herdr's keys, herdr's custom commands and Shepherd's own actions in one searchable list,
//! each row's chords recorded in place and written where they live (herdr's config.toml, or Shepherd's settings).
use super::*;
use crate::herdr::keymap;
use crate::shortcuts::{self, Kind, Row, RowId, Slot, Source};

/// The chord being recorded: which row and chip, and how the keystrokes are read.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Recording {
    pub id: RowId,
    pub slot: Slot,
    pub kind: Kind,
    /// The prefix was pressed: the next keystroke is the chord after it.
    pub pending_prefix: bool,
}

impl Recording {
    /// Records a Shepherd action's chord.
    pub fn shepherd(action: &'static str) -> Self {
        Recording { id: RowId::Shepherd(action), slot: Slot::Replace(0), kind: Kind::Shepherd, pending_prefix: false }
    }

    /// `herdr:<name>`, `command:<n>` or `shepherd:<Action>`, with a trailing `+` to add a chord: the launch hook
    /// (`SHEPHERD_RECORD`) for screenshots and tests.
    pub fn from_spec(spec: &str, config: &HerdrConfig, keys: &std::collections::BTreeMap<String, String>) -> Option<Self> {
        if spec == "tap" {
            return Some(Recording { id: RowId::PrefixTap, slot: Slot::Replace(0), kind: Kind::PrefixTap, pending_prefix: false });
        }
        let (spec, add) = spec.strip_suffix('+').map_or((spec, false), |s| (s, true));
        let rows = shortcuts::rows(keys, keymap::defaults(), config);
        let row = rows.iter().find(|r| match &r.id {
            RowId::Herdr(n) => spec == format!("herdr:{n}"),
            RowId::Command(i) => spec == format!("command:{i}"),
            RowId::PrefixTap => false,
            RowId::Shepherd(n) => spec == format!("shepherd:{n}"),
        })?;
        Some(Self::of(row, if add { Slot::Add } else { Slot::Replace(0) }))
    }

    /// Records chord `slot` of `row`.
    fn of(row: &Row, slot: Slot) -> Self {
        let kind = match (row.source, row.indexed, row.prefix) {
            (Source::Shepherd, ..) if row.id == RowId::PrefixTap => Kind::PrefixTap,
            (Source::Shepherd, ..) => Kind::Shepherd,
            _ if row.id == RowId::PrefixTap => Kind::PrefixTap,
            (_, _, true) => Kind::Prefix,
            (_, true, _) => Kind::HerdrIndexed,
            _ => Kind::Herdr,
        };
        Recording { id: row.id.clone(), slot, kind, pending_prefix: false }
    }
}

impl SettingsView {
    /// herdr's prefix chords as the page has them, in GPUI's spelling.
    pub(super) fn prefix_chords(&self) -> Vec<String> {
        let config = self.herdr_config.clone().unwrap_or_default();
        shortcuts::prefixes_of(keymap::defaults(), &config)
    }

    /// Rereads herdr's config.toml: it changed (herdr's own screen, an editor, Shepherd's write).
    pub(super) fn reload_herdr_keys(&mut self, cx: &mut Context<Self>) {
        self.herdr_config = HerdrConfig::load(&config_path()).map_err(|e| format!("{e:#}"));
        cx.notify();
    }

    /// Records `chord` (None: takes the chord away) in the row being recorded, in the place it lives.
    pub(super) fn commit_chord(&mut self, recording: &Recording, chord: Option<String>, cx: &mut Context<Self>) {
        self.keys_note = None;
        match &recording.id {
            RowId::Shepherd(action) => {
                let chord = chord.unwrap_or_default();
                update(cx, |s| s.record_chord(action, &chord));
            }
            // Backspace on the tap row: no tap prefix
            RowId::PrefixTap => update(cx, |s| s.prefix_tap = None),
            id => self.edit_row(id, |labels| shortcuts::apply_slot(labels, recording.slot, chord), cx),
        }
    }

    /// The window's modifiers changed: while a prefix row records, a modifier (or pair) pressed and let go with no key
    /// in between is the tap prefix.
    pub(super) fn recording_modifiers(&mut self, event: crate::tap::Event, cx: &mut Context<Self>) {
        if !self.recording.as_ref().is_some_and(|r| matches!(r.kind, Kind::Prefix | Kind::PrefixTap)) {
            return;
        }
        if let Some(mods) = self.tap_recorder.step(event) {
            self.commit_tap(mods, cx);
        }
    }

    /// A modifier tapped alone while the prefix rows record: Shepherd's tap prefix, saved in its settings.
    pub(super) fn commit_tap(&mut self, mods: crate::tap::Mods, cx: &mut Context<Self>) {
        self.keys_note = None;
        let label = mods.label();
        update(cx, |s| s.prefix_tap = label);
        self.recording = None;
        cx.notify();
    }

    /// Changes the chords of a herdr row (an action or a command): `change` gets the ones now. Reads the file
    /// again first, so edits made elsewhere in the meantime are kept; the first write keeps a copy of the file.
    fn edit_row(&mut self, id: &RowId, change: impl FnOnce(&[String]) -> Vec<String>, cx: &mut Context<Self>) {
        let keys = SettingsGlobal::of(cx).settings.keys.clone();
        let result = self.write_config(cx, |config| {
            let rows = shortcuts::rows(&keys, keymap::defaults(), config);
            let row = rows.iter().find(|r| &r.id == id).ok_or_else(|| anyhow::anyhow!("that shortcut is gone from herdr's config"))?;
            let labels = change(&row.chords);
            match id {
                RowId::Command(index) => config.set_command_labels(*index, &labels),
                _ => shortcuts::write_herdr(config, row, &labels),
            }
        });
        self.finish_keys_write(result, cx);
    }

    /// Takes a row back to its default: Shepherd's entry goes, or herdr's line.
    pub(super) fn reset_row(&mut self, row: &Row, cx: &mut Context<Self>) {
        self.keys_note = None;
        match &row.id {
            RowId::Shepherd(action) => update(cx, |s| {
                s.keys.remove(*action);
            }),
            RowId::PrefixTap => update(cx, |s| s.prefix_tap = None),
            id => {
                let default = row.default.clone();
                self.edit_row(id, move |_| default, cx);
            }
        }
    }

    /// Reset all of a group: Shepherd's entries, or only the `[keys]` lines the page has rows for.
    pub(super) fn reset_group(&mut self, source: Source, cx: &mut Context<Self>) {
        self.keys_note = None;
        self.recording = None;
        match source {
            Source::Shepherd => update(cx, |s| s.keys.clear()),
            Source::Herdr => {
                update(cx, |s| s.prefix_tap = None);
                let keys = SettingsGlobal::of(cx).settings.keys.clone();
                let result = self.write_config(cx, |config| {
                    let rows = shortcuts::rows(&keys, keymap::defaults(), config);
                    shortcuts::reset_herdr_group(config, &rows);
                    Ok(())
                });
                self.finish_keys_write(result, cx);
            }
            Source::HerdrCommand => {}
        }
        cx.notify();
    }

    /// Loads herdr's config, lets `edit` change it, keeps a copy of the original (once), and saves.
    fn write_config(&mut self, _cx: &mut Context<Self>, edit: impl FnOnce(&mut HerdrConfig) -> anyhow::Result<()>) -> anyhow::Result<HerdrConfig> {
        let path = config_path();
        let mut config = HerdrConfig::load(&path)?;
        edit(&mut config)?;
        crate::herdr::backup_once(&path)?;
        config.save(&path)?;
        Ok(config)
    }

    /// After a write: Shepherd binds what herdr's config says now, and herdr is told to reload it.
    fn finish_keys_write(&mut self, result: anyhow::Result<HerdrConfig>, cx: &mut Context<Self>) {
        match result {
            Ok(config) => {
                self.herdr_config = Ok(config);
                crate::bindings::refresh_herdr(cx);
                self.herdr_call("server.reload_config", json!({}), cx, |v, result| {
                    v.keys_note = result.err().map(|e| format!("Saved to herdr's config. herdr did not reload it ({e:#}); it will read it when it starts."));
                });
            }
            Err(e) => self.keys_note = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    /// The page: the search field, then the three groups of rows.
    pub(super) fn shortcuts_page(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        // the page is a 760 px column beside the 320 px nav: a window under ~1100 px has no room for chips beside the names
        let wide = window.viewport_size().width > px(1100.);
        let config = self.herdr_config.clone().unwrap_or_default();
        let rows = shortcuts::rows_with_tap(&settings.keys, settings.prefix_tap.as_deref(), keymap::defaults(), &config);
        let prefixes = self.prefix_chords();
        let clashes = shortcuts::conflicts(&rows, &prefixes);
        let query = self.shortcut_search.read(cx).value().trim().to_string();
        let shown: Vec<&Row> = shortcuts::filter(&rows, &query, &shortcuts::show);
        let recording = self.recording.clone();
        let groups = [
            (Source::Shepherd, "Shepherd", "Saved in Shepherd's settings; a Shepherd chord wins over a herdr one in a pane."),
            (Source::HerdrCommand, "herdr commands", "The [[keys.command]] entries of the same file: re-key them here, add or remove them in the file."),
            (Source::Herdr, "herdr", "Written to herdr's config.toml, which herdr reloads. The prefix key starts the chords marked Prefix."),
        ];
        let mut page = div()
            .child(
                div()
                    .child(page_header(theme, "Shortcuts", "Click a chord, then press the new one. Press the prefix, then a key, for a prefix chord. Esc cancels, \u{232b} unbinds. A chord is recorded as Shepherd receives it: a desktop that takes or rewrites Super chords (a window manager's own shortcuts) never passes them on."))
                    .child(
                        div()
                            .mt(px(16.))
                            .h(px(36.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(8.))
                            .bg(theme.wash(0.06))
                            .child(gpui::svg().path("icons/palette-search.svg").size(px(16.)).flex_none().text_color(theme.text_muted))
                            .child(div().flex_1().min_w_0().flex().items_center().line_height(px(16.)).child(Input::new(&self.shortcut_search))),
                    ),
            )
            .children(self.keys_note.clone().map(|n| note(theme, n)))
            .children(self.herdr_config.as_ref().err().map(|e| note(theme, format!("herdr's config.toml cannot be read, so herdr's shortcuts show their defaults: {e}"))));
        if shown.is_empty() {
            page = page.child(note(theme, format!("No shortcut matches \u{201c}{query}\u{201d}.")));
        }
        for (source, title, about) in groups {
            let group: Vec<&Row> = shown.iter().copied().filter(|r| r.source == source).collect();
            if group.is_empty() {
                continue;
            }
            let any_changed = rows.iter().any(|r| r.source == source && r.changed);
            page = page
                .child(
                    div()
                        .mt(px(28.))
                        .px(px(8.))
                        .flex()
                        .items_end()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                                .child(div().text_size(px(12.)).line_height(px(16.)).text_color(theme.text_muted).child(about)),
                        )
                        .when(source != Source::HerdrCommand, |el| {
                            el.child(
                                action(theme, SharedString::from(format!("reset-group-{title}")), "Reset all")
                                    .when(!any_changed, |a| a.opacity(0.4))
                                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.reset_group(source, cx))),
                            )
                        }),
                )
                .child(card(theme).mt(px(8.)).children(group.iter().enumerate().map(|(i, row)| self.shortcut_row(theme, wide, row, i == 0, &clashes, recording.as_ref(), cx))));
        }
        page
    }

    /// One row: its label and what it is, its chords, a reset when the user changed it, and any clash.
    fn shortcut_row(&self, theme: &Theme, wide: bool, row: &Row, first: bool, clashes: &std::collections::BTreeMap<RowId, Vec<String>>, recording: Option<&Recording>, cx: &mut Context<Self>) -> Div {
        let hover = theme.border_strong;
        let tag = match (row.source, row.herdr_only) {
            _ if row.id == RowId::PrefixTap => "Shepherd",
            (Source::Shepherd, _) => "Shepherd",
            (Source::HerdrCommand, false) => "herdr command",
            (_, false) => "herdr",
            (_, true) => "herdr only",
        };
        let id_text = match &row.id {
            RowId::Herdr(n) => format!("herdr-{n}"),
            RowId::Command(i) => format!("command-{i}"),
            RowId::Shepherd(n) => format!("shepherd-{n}"),
            RowId::PrefixTap => "prefix-tap".to_string(),
        };
        let own = recording.filter(|r| r.id == row.id);
        let chip = |slot: Slot, text: Option<String>, faint: bool| {
            let active = own.is_some_and(|r| r.slot == slot);
            let pending = own.is_some_and(|r| r.pending_prefix) && active;
            let n = match slot {
                Slot::Replace(i) => i,
                Slot::Add => 99,
            };
            let target = row.clone();
            div()
                .id(SharedString::from(format!("record-{id_text}-{n}")))
                .h(px(28.))
                .min_w(px(40.))
                .px(px(10.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .bg(theme.surface)
                .border_1()
                .font_family(theme.font_terminal.clone())
                .text_size(px(12.))
                .cursor_pointer()
                .map(|el| match (active, text) {
                    (true, _) if pending => el.border_color(theme.accent).text_color(theme.accent).child("Prefix + \u{2026}"),
                    (true, _) => el.border_color(theme.accent).text_color(theme.accent).child("Press keys\u{2026}"),
                    (false, Some(text)) if !faint => el.border_color(theme.border).hover(move |s| s.border_color(hover)).child(text),
                    (false, Some(text)) => el.border_color(theme.border).text_color(theme.text_faint).hover(move |s| s.border_color(hover)).child(text),
                    (false, None) => el.border_color(theme.border).text_color(theme.text_faint).hover(move |s| s.border_color(hover)).child("\u{2014}"),
                })
                .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                    v.recording = Some(Recording::of(&target, slot));
                    v.tap_recorder.reset();
                    v.keys_note = None;
                    window.focus(&v.focus, cx);
                    cx.notify();
                }))
        };
        let show = |c: &String| shortcuts::show(row, c);
        let mut chips: Vec<gpui::AnyElement> = Vec::new();
        if row.chords.is_empty() {
            chips.push(chip(if row.source == Source::Shepherd || row.id == RowId::PrefixTap { Slot::Replace(0) } else { Slot::Add }, None, false).into_any_element());
        } else {
            let shown = if row.source == Source::Shepherd { &row.chords[..row.chords.len().min(2)] } else { &row.chords[..] };
            chips.extend(shown.iter().enumerate().map(|(i, c)| chip(Slot::Replace(i), Some(show(c)), false).into_any_element()));
            if row.source != Source::Shepherd && row.id != RowId::PrefixTap && !(row.prefix && row.name == "prefix") {
                chips.push(chip(Slot::Add, Some("+".into()), true).into_any_element());
            }
        }
        let warnings = clashes.get(&row.id).cloned().unwrap_or_default();
        let target = row.clone();
        let head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .child(div().min_w_0().truncate().child(row.label.clone()))
            .child(
                div()
                    .flex_none()
                    .px(px(6.))
                    .rounded(px(4.))
                    .bg(theme.wash(0.08))
                    .text_size(px(10.5))
                    .text_color(if row.herdr_only { theme.warning } else { theme.text_muted })
                    .child(tag),
            );
        let detail = div().text_size(px(11.5)).line_height(px(15.)).text_color(theme.text_faint).child(match &row.detail {
            Some(d) => format!("{} \u{b7} {d}", row.name),
            None => row.name.clone(),
        });
        let warnings = warnings.into_iter().map(|w| div().text_size(px(11.5)).line_height(px(15.)).text_color(theme.warning).child(w));
        let reset = div().when(row.changed, |el| {
            el.child(
                div()
                    .id(SharedString::from(format!("reset-{id_text}")))
                    .px(px(6.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover({
                        let lifted = theme.wash(0.10);
                        move |s| s.bg(lifted)
                    })
                    .child("Reset")
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.reset_row(&target, cx))),
            )
        });
        let chips = div().flex().flex_wrap().items_center().gap(px(6.)).children(chips);
        if wide {
            card_row(theme, first)
                .min_h(px(52.))
                .py(px(8.))
                .child(div().flex_1().min_w_0().overflow_hidden().flex().flex_col().gap(px(2.)).child(head).child(detail).children(warnings))
                .child(chips.flex_none().justify_end().max_w(px(300.)))
                .child(reset.w(px(48.)).flex_none().flex().justify_end())
        } else {
            // a narrow window: the chips under the name, the reset beside it
            card_row(theme, first)
                .py(px(10.))
                .flex_col()
                .items_stretch()
                .gap(px(6.))
                .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().min_w_0().child(head)).child(reset.flex_none()))
                .child(detail)
                .child(chips)
                .children(warnings)
        }
    }
}
