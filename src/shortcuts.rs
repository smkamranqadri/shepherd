//! The Shortcuts page's data: one list of rows from three sources, herdr's `[keys]`, herdr's `[[keys.command]]` and
//! Shepherd's own actions; what a row's chords clash with; how a recorded keystroke becomes a chord; and the writes
//! to herdr's config. Pure (no windows), so every rule here has a test.
use crate::bindings::{ACTIONS, DEFAULTS, context_of, effective, normalize};
use crate::herdr::HerdrConfig;
use crate::herdr::keymap::{self, Action, Defaults, INDEXED, Runs};
use crate::herdr::keys::{chords_for, keystroke, label_of, prefix_chords};
use gpui::Keystroke;
use std::collections::BTreeMap;

/// Where a row's chords live, and so where an edit of them is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// An action of herdr's `[keys]`: config.toml.
    Herdr,
    /// A `[[keys.command]]` of herdr's: config.toml.
    HerdrCommand,
    /// One of Shepherd's actions: its settings.
    Shepherd,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowId {
    Herdr(String),
    Command(usize),
    Shepherd(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: RowId,
    pub source: Source,
    pub label: String,
    /// The action's name where it lives: `new_tab`, `NewTab`; a command's type.
    pub name: String,
    /// What the row does beyond its label: which of Shepherd's actions a herdr one runs, or that only herdr can.
    pub detail: Option<String>,
    /// herdr's action has no Shepherd equivalent (its chords are caught in a pane, and say so).
    pub herdr_only: bool,
    /// herdr's labels as the config spells them; Shepherd's chords in GPUI's spelling.
    pub chords: Vec<String>,
    pub default: Vec<String>,
    /// Set by the user: a line in config.toml, or an entry in Shepherd's settings. A reset puts the default back.
    pub changed: bool,
    /// One label for the nine digits (`prefix+1..9`).
    pub indexed: bool,
    /// The prefix key itself.
    pub prefix: bool,
    /// Chords herdr's screen reads in a mode of its own (or its remote client only): bound in no pane.
    pub local: bool,
    /// The Shepherd actions this row's chords run (a herdr row's equivalents, a Shepherd row's own): two rows that
    /// run the same action on one chord are no clash.
    pub runs: Vec<&'static str>,
}

/// All rows: herdr's actions (the prefix first), its commands, then every Shepherd action.
pub fn rows(keys: &BTreeMap<String, String>, defaults: &Defaults, config: &HerdrConfig) -> Vec<Row> {
    let actions = keymap::actions(defaults, config);
    let mut out: Vec<Row> = actions.iter().map(herdr_row).collect();
    out.extend(config.commands().into_iter().map(|c| {
        let kind = match c.kind.as_str() {
            "shell" => "Shell command",
            "pane" => "Command in a pane",
            "popup" => "Popup command",
            "plugin_action" => "Plugin action",
            other => other,
        };
        let known = matches!(c.kind.as_str(), "shell" | "pane" | "popup" | "plugin_action");
        Row {
            id: RowId::Command(c.index),
            source: Source::HerdrCommand,
            label: if c.command.is_empty() { format!("Command {}", c.index + 1) } else { c.command.clone() },
            name: c.kind.clone(),
            detail: Some(if known { kind.to_string() } else { format!("{kind}: herdr only") }),
            herdr_only: !known,
            chords: c.keys,
            default: Vec::new(),
            changed: false,
            indexed: false,
            prefix: false,
            local: false,
            runs: Vec::new(),
        }
    }));
    let bound = effective(keys);
    out.extend(ACTIONS.iter().map(|(name, label)| {
        let herdr: Vec<&str> = actions.iter().filter(|a| runs_action(a.runs, name)).map(|a| a.name.as_str()).collect();
        Row {
            id: RowId::Shepherd(name),
            source: Source::Shepherd,
            label: label.to_string(),
            name: name.to_string(),
            detail: (!herdr.is_empty()).then(|| format!("herdr: {}", herdr.join(", "))),
            herdr_only: false,
            chords: bound.iter().filter(|(a, _)| a == name).map(|(_, c)| c.clone()).collect(),
            default: DEFAULTS.iter().filter(|(a, _)| a == name).filter_map(|(_, c)| normalize(c)).collect(),
            changed: keys.contains_key(*name),
            indexed: false,
            prefix: false,
            local: false,
            runs: vec![name],
        }
    }));
    out
}

fn runs_action(runs: Runs, name: &str) -> bool {
    match runs {
        Runs::App(app) => app == name,
        Runs::Nine(apps) => apps.contains(&name),
        _ => false,
    }
}

fn herdr_row(action: &Action) -> Row {
    let shepherd = |app: &str| ACTIONS.iter().find(|(n, _)| *n == app).map_or(app, |(_, l)| *l).to_string();
    let (detail, herdr_only) = match action.runs {
        Runs::App(app) => (Some(format!("Shepherd: {}", shepherd(app))), false),
        Runs::Nine(apps) => (Some(format!("Shepherd: {}\u{2013}9", shepherd(apps[0]))), false),
        Runs::HerdrOnly if action.known => (Some("herdr only: Shepherd has no equivalent".into()), true),
        Runs::HerdrOnly => (Some("herdr only: newer than this Shepherd".into()), true),
        Runs::Local => (Some("herdr only: a mode of its own screen".into()), true),
        Runs::Prefix => (Some("Starts herdr's prefix mode".into()), false),
    };
    Row {
        id: RowId::Herdr(action.name.clone()),
        source: Source::Herdr,
        label: action.label.clone(),
        name: action.name.clone(),
        detail,
        herdr_only,
        chords: action.labels.clone(),
        default: action.default.clone(),
        changed: action.set && !same_labels(&action.labels, &action.default),
        indexed: matches!(action.runs, Runs::Nine(_)),
        prefix: action.runs == Runs::Prefix,
        local: action.runs == Runs::Local,
        runs: match action.runs {
            Runs::App(app) => vec![app],
            Runs::Nine(apps) => apps.to_vec(),
            _ => Vec::new(),
        },
    }
}

/// The same chords, in any order.
pub fn same_labels(a: &[String], b: &[String]) -> bool {
    let norm = |l: &[String]| {
        let mut l: Vec<String> = l.iter().map(|x| x.trim().to_ascii_lowercase()).filter(|x| !x.is_empty()).collect();
        l.sort();
        l
    };
    norm(a) == norm(b)
}

/// A herdr label as the page shows it: `prefix+shift+n` → `Prefix+Shift+N`, `super+p` → `Super+P` (`Cmd+P` on a Mac).
pub fn display_label(label: &str) -> String {
    let (alt, platform) = if cfg!(target_os = "macos") { ("Opt", "Cmd") } else { ("Alt", "Super") };
    label
        .split('+')
        .map(str::trim)
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "prefix" => "Prefix".to_string(),
            "ctrl" | "control" => "Ctrl".into(),
            "alt" | "option" | "meta" => alt.into(),
            "shift" => "Shift".into(),
            "super" | "cmd" | "command" => platform.into(),
            "minus" => "-".into(),
            "plus" => "+".into(),
            "comma" => ",".into(),
            "period" => ".".into(),
            "esc" | "escape" => "Esc".into(),
            _ if part.chars().count() == 1 => part.to_uppercase(),
            _ if part.is_empty() => String::new(),
            _ => part[..1].to_uppercase() + &part[1..],
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Whether `row` answers `query`: every word of it is in the row's label, name, detail or chords, in any order and
/// any case. A chord counts as written in the config (`prefix+shift+n`), as the page shows it (`Prefix+Shift+N`)
/// and as the words of its keys (`ctrl shift t`).
pub fn matches(row: &Row, query: &str, show: &dyn Fn(&Row, &str) -> String) -> bool {
    let mut haystack = vec![row.label.clone(), row.name.clone(), row.name.replace('_', " ")];
    haystack.extend(row.detail.clone());
    for chord in &row.chords {
        let shown = show(row, chord);
        haystack.push(shown.replace('+', " "));
        haystack.push(shown);
        haystack.push(chord.replace(['+', '-'], " "));
        haystack.push(chord.clone());
    }
    let haystack = haystack.join(" ").to_lowercase();
    query.split_whitespace().all(|word| haystack.contains(&word.to_lowercase()))
}

/// The page's rows that answer `query` (all of them when it is empty).
pub fn filter<'a>(rows: &'a [Row], query: &str, show: &dyn Fn(&Row, &str) -> String) -> Vec<&'a Row> {
    rows.iter().filter(|r| matches(r, query, show)).collect()
}

/// What a row's chords clash with, by row: another row whose chord is the same, or starts the same keystrokes (a
/// chord that is the first stroke of a longer one hides it). Shepherd's wins in a pane over herdr's, as it does
/// when binding, and the message says so. Chords bound only in a file's view are no clash with a pane's.
pub fn conflicts(rows: &[Row], prefixes: &[String]) -> BTreeMap<RowId, Vec<String>> {
    struct Entry<'a> {
        row: &'a Row,
        chord: String,
        /// The prefix key on its own: it begins every chord after it, which is no clash.
        prefix: bool,
        /// The Shepherd action this chord runs, when it runs one.
        runs: Option<&'static str>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    for row in rows {
        for label in &row.chords {
            if row.source == Source::Shepherd {
                if context_of(&row.name).is_none() {
                    entries.extend(normalize(label).map(|chord| Entry { row, chord, prefix: false, runs: row.runs.first().copied() }));
                }
                continue;
            }
            if row.prefix {
                entries.extend(keystroke(label).map(|chord| Entry { row, chord, prefix: true, runs: None }));
                continue;
            }
            // a mode of herdr's own screen, or a bare key: nothing is bound in a pane
            if row.local || !crate::herdr::keymap::bindable(label) {
                continue;
            }
            let labels: Vec<(String, Option<&'static str>)> = if row.indexed && label.contains(INDEXED) {
                (1..=9).map(|d| (label.replace(INDEXED, &d.to_string()), row.runs.get(d - 1).copied())).collect()
            } else {
                vec![(label.clone(), row.runs.first().copied().filter(|_| row.runs.len() == 1))]
            };
            for (l, runs) in labels {
                entries.extend(chords_for(&l, prefixes).into_iter().map(|chord| Entry { row, chord, prefix: false, runs }));
            }
        }
    }
    let clash = |a: &str, b: &str| a == b || a.strip_prefix(b).is_some_and(|r| r.starts_with(' ')) || b.strip_prefix(a).is_some_and(|r| r.starts_with(' '));
    let mut out: BTreeMap<RowId, Vec<String>> = BTreeMap::new();
    for (i, a) in entries.iter().enumerate() {
        for b in &entries[i + 1..] {
            let same_action = a.runs.is_some() && a.runs == b.runs && a.chord == b.chord;
            if a.row.id == b.row.id || same_action || (a.prefix || b.prefix) && a.chord != b.chord || !clash(&a.chord, &b.chord) {
                continue;
            }
            let shown = |e: &Entry| crate::shortcuts::spaced(&e.chord);
            for (me, other) in [(a, b), (b, a)] {
                let wins = match (me.row.source, other.row.source) {
                    (Source::Shepherd, Source::Shepherd) => String::new(),
                    (Source::Shepherd, _) => ", which loses to it in a pane".to_string(),
                    (_, Source::Shepherd) => ", which wins in a pane".to_string(),
                    _ => String::new(),
                };
                let whose = match other.row.source {
                    Source::Shepherd => "Shepherd's",
                    Source::Herdr => "herdr's",
                    Source::HerdrCommand => "herdr's command",
                };
                let note = format!("{} is also {whose} \u{201c}{}\u{201d}{wins}", shown(me), other.row.label);
                let list = out.entry(me.row.id.clone()).or_default();
                if !list.contains(&note) {
                    list.push(note);
                }
            }
        }
    }
    out
}

/// A row's chord as the page shows it: herdr's label as the config spells it, Shepherd's in GPUI's spelling.
pub fn show(row: &Row, chord: &str) -> String {
    if row.source == Source::Shepherd { spaced(chord) } else { display_label(chord) }
}

/// A chord of keystrokes for a note: `ctrl-b c` → `Ctrl+B C`.
pub fn spaced(chord: &str) -> String {
    chord.split_whitespace().map(crate::ui::settings::display_chord).collect::<Vec<_>>().join(" ")
}

/// Which chord a row is recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Replaces the chord at this place in the row.
    Replace(usize),
    /// Adds a chord to the row.
    Add,
}

/// What a recorded keystroke does to the row being recorded.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Not a chord yet (a bare letter, a modifier): keep waiting.
    Ignore,
    /// Esc: leave the row as it was.
    Cancel,
    /// Backspace: take the chord away.
    Clear,
    /// This chord: in GPUI's spelling for Shepherd's rows, as herdr's config spells it for herdr's.
    Commit(String),
    /// The prefix key was pressed: the next keystroke is the chord after it.
    WaitPrefix,
    /// Not usable here, and why.
    Reject(String),
}

/// What kind of row is recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Shepherd,
    Herdr,
    /// An indexed herdr action: the chord takes a digit key, written `1..9`.
    HerdrIndexed,
    /// The prefix key: any chord that can be pressed on its own.
    Prefix,
}

/// What `keystroke`, pressed while a row records, does. `pending_prefix`: the prefix was just pressed. `prefixes`:
/// herdr's prefix chords in GPUI's spelling.
pub fn record_step(kind: Kind, keystroke: &Keystroke, pending_prefix: bool, prefixes: &[String]) -> Step {
    let bare = keystroke.modifiers == Default::default();
    match keystroke.key.as_str() {
        "escape" if bare => return Step::Cancel,
        "backspace" if bare && !pending_prefix => return Step::Clear,
        _ => {}
    }
    let chord = keystroke.unparse();
    match kind {
        Kind::Shepherd => {
            if crate::bindings::recordable(keystroke) {
                Step::Commit(chord)
            } else {
                Step::Ignore
            }
        }
        Kind::Prefix => match crate::bindings::recordable(keystroke).then(|| label_of(&chord)) {
            Some(Some(label)) => Step::Commit(label),
            Some(None) => Step::Reject("herdr has no name for that key".into()),
            None => Step::Ignore,
        },
        Kind::Herdr | Kind::HerdrIndexed => {
            if !pending_prefix && prefixes.iter().any(|p| normalize(p).as_deref() == Some(chord.as_str())) {
                return Step::WaitPrefix;
            }
            if !pending_prefix && !crate::bindings::recordable(keystroke) {
                return Step::Ignore;
            }
            let Some(label) = label_of(&chord) else { return Step::Reject("herdr has no name for that key".into()) };
            let label = if kind == Kind::HerdrIndexed { match indexed(&label) {
                Some(l) => l,
                None => return Step::Reject("An indexed shortcut takes a number key, 1 to 9".into()),
            } } else { label };
            Step::Commit(if pending_prefix { format!("prefix+{label}") } else { label })
        }
    }
}

/// `alt+3` → `alt+1..9`; none when the key is no digit from 1 to 9.
fn indexed(label: &str) -> Option<String> {
    let (modifiers, key) = label.rsplit_once('+').map_or(("", label), |(m, k)| (m, k));
    if key.len() == 1 && key.chars().all(|c| ('1'..='9').contains(&c)) {
        Some(if modifiers.is_empty() { INDEXED.to_string() } else { format!("{modifiers}+{INDEXED}") })
    } else {
        None
    }
}

/// `labels` with `value` put at `slot` (or the chord at `slot` taken away, for `None`); no chord twice.
pub fn apply_slot(labels: &[String], slot: Slot, value: Option<String>) -> Vec<String> {
    let mut out = labels.to_vec();
    match (slot, value) {
        (Slot::Replace(i), Some(v)) if i < out.len() => out[i] = v,
        (Slot::Replace(i), None) if i < out.len() => {
            out.remove(i);
        }
        (Slot::Add, Some(v)) | (Slot::Replace(_), Some(v)) => out.push(v),
        _ => {}
    }
    let mut seen: Vec<String> = Vec::new();
    out.retain(|l| {
        let key = l.trim().to_ascii_lowercase();
        !seen.contains(&key) && {
            seen.push(key);
            true
        }
    });
    out
}

/// Writes the chords of one herdr action into `config`: nothing but the default removes the line, so herdr's own
/// default applies again; an emptied action that has a default is written empty (unbound).
pub fn write_herdr(config: &mut HerdrConfig, row: &Row, labels: &[String]) -> anyhow::Result<()> {
    if same_labels(labels, &row.default) {
        config.set_key_labels(&row.name, None)
    } else {
        config.set_key_labels(&row.name, Some(labels))
    }
}

/// The lines Reset all of the herdr group removes: every `[keys]` line the page shows a row for, the prefix
/// included; custom commands and everything else stay.
pub fn reset_herdr_group(config: &mut HerdrConfig, rows: &[Row]) {
    let names: Vec<&str> = rows.iter().filter(|r| r.source == Source::Herdr).map(|r| r.name.as_str()).collect();
    config.remove_key_lines(&names);
}

/// The prefix chords of `rows` in GPUI's spelling, for the conflict check and the recorder.
pub fn prefixes_of(defaults: &Defaults, config: &HerdrConfig) -> Vec<String> {
    prefix_chords(&keymap::actions(defaults, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::keymap::builtin_defaults;

    fn config(toml: &str) -> HerdrConfig {
        HerdrConfig::parse(toml).unwrap()
    }

    fn model(keys: &[(&str, &str)], toml: &str) -> Vec<Row> {
        let keys = keys.iter().map(|(a, c)| (a.to_string(), c.to_string())).collect();
        rows(&keys, &builtin_defaults(), &config(toml))
    }

    fn row(rows: &[Row], id: RowId) -> &Row {
        rows.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("{id:?}"))
    }

    fn herdr(name: &str) -> RowId {
        RowId::Herdr(name.into())
    }

    fn show(_: &Row, chord: &str) -> String {
        display_label(chord)
    }

    fn press(chord: &str) -> Keystroke {
        Keystroke::parse(chord).unwrap()
    }

    // --- rows ---

    #[test]
    fn three_sources_in_one_list() {
        let r = model(&[], "[[keys.command]]\nkey = \"prefix+g\"\ntype = \"popup\"\ncommand = \"lazygit\"\n");
        assert_eq!(r.iter().filter(|r| r.source == Source::Herdr).count(), keymap::KNOWN.len());
        assert_eq!(r.iter().filter(|r| r.source == Source::HerdrCommand).count(), 1);
        assert_eq!(r.iter().filter(|r| r.source == Source::Shepherd).count(), ACTIONS.len());
        let order: Vec<_> = r.iter().map(|r| r.source).collect();
        assert!(order.windows(2).all(|w| (w[0] as u8) <= (w[1] as u8)), "grouped: herdr, commands, Shepherd");
        let command = row(&r, RowId::Command(0));
        assert_eq!((command.label.as_str(), command.chords.as_slice()), ("lazygit", ["prefix+g".to_string()].as_slice()));
        assert_eq!(command.detail.as_deref(), Some("Popup command"));
    }

    #[test]
    fn every_herdr_action_has_a_shepherd_equivalent_or_says_it_has_none() {
        let r = model(&[], "");
        for row in r.iter().filter(|r| r.source == Source::Herdr) {
            let Some(Runs::App(app)) = keymap::KNOWN.iter().find(|k| k.name == row.name).map(|k| k.runs) else { continue };
            assert!(ACTIONS.iter().any(|(a, _)| *a == app), "{}: Shepherd has no action {app}", row.name);
            assert!(!row.herdr_only);
        }
        let only: Vec<_> = r.iter().filter(|r| r.source == Source::Herdr && r.herdr_only).map(|r| r.name.as_str()).collect();
        assert_eq!(
            only,
            [
                "detach", "remote_image_paste", "copy_mode", "last_pane", "resize_mode", "navigate_workspace_up", "navigate_workspace_down",
                "navigate_pane_left", "navigate_pane_down", "navigate_pane_up", "navigate_pane_right"
            ]
        );
        for name in ["detach", "copy_mode", "last_pane", "resize_mode"] {
            assert_eq!(row(&r, herdr(name)).detail.as_deref(), Some("herdr only: Shepherd has no equivalent"), "{name}");
        }
    }

    #[test]
    fn a_herdr_row_shows_its_chords_default_and_whether_the_user_set_it() {
        let r = model(&[], "[keys]\nnew_tab = [\"prefix+t\", \"alt+t\"]\nnext_tab = \"prefix+n\"\nclose_tab = \"\"\n");
        let new_tab = row(&r, herdr("new_tab"));
        assert_eq!((new_tab.chords.clone(), new_tab.default.clone(), new_tab.changed), (vec!["prefix+t".to_string(), "alt+t".into()], vec!["prefix+c".to_string()], true));
        assert!(!row(&r, herdr("next_tab")).changed, "a line that says the default is not a change");
        let closed = row(&r, herdr("close_tab"));
        assert!(closed.chords.is_empty() && closed.changed, "emptied");
        assert!(!row(&r, herdr("zoom")).changed);
        assert!(row(&r, herdr("switch_tab")).indexed);
        assert!(row(&r, herdr("prefix")).prefix);
    }

    #[test]
    fn a_shepherd_row_shows_its_chords_and_the_herdr_action_that_runs_it() {
        let r = model(&[("NewTab", "super-n")], "");
        let new_tab = row(&r, RowId::Shepherd("NewTab"));
        assert_eq!(new_tab.chords, [normalize("super-n").unwrap()]);
        assert!(new_tab.changed);
        assert_eq!(new_tab.detail.as_deref(), Some("herdr: new_tab"));
        assert!(!new_tab.default.is_empty());
        assert_eq!(row(&r, RowId::Shepherd("Tab4")).detail.as_deref(), Some("herdr: switch_tab"));
        assert_eq!(row(&r, RowId::Shepherd("QuickOpen")).detail, None, "Shepherd's own");
        assert!(!row(&r, RowId::Shepherd("CloseTab")).changed);
    }

    #[test]
    fn a_future_herdr_action_is_listed_and_herdr_only() {
        let mut defaults = builtin_defaults();
        defaults.push(("brand_new".into(), vec!["prefix+9x".into()]));
        let r = rows(&BTreeMap::new(), &defaults, &config("[keys]\nfuture_thing = \"ctrl+alt+f\"\n"));
        for name in ["brand_new", "future_thing"] {
            let x = row(&r, herdr(name));
            assert!(x.herdr_only && x.detail.as_deref() == Some("herdr only: newer than this Shepherd"), "{name}");
        }
        assert_eq!(row(&r, herdr("future_thing")).chords, ["ctrl+alt+f"]);
    }

    // --- display and search ---

    #[test]
    fn labels_read_as_keys() {
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(display_label("prefix+shift+n"), "Prefix+Shift+N");
            assert_eq!(display_label("super+p"), "Super+P");
            assert_eq!(display_label("alt+enter"), "Alt+Enter");
            assert_eq!(display_label("prefix+minus"), "Prefix+-");
            assert_eq!(display_label("prefix+1..9"), "Prefix+1..9");
            assert_eq!(display_label("ctrl+alt+shift+left"), "Ctrl+Alt+Shift+Left");
            assert_eq!(display_label("esc"), "Esc");
        }
        #[cfg(target_os = "macos")]
        assert_eq!(display_label("super+p"), "Cmd+P");
    }

    #[test]
    fn search_finds_a_row_by_label_name_and_chord() {
        let r = model(&[], "[keys]\nnew_tab = \"prefix+t\"\n");
        let found = |q: &str| filter(&r, q, &show).iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert!(found("rename tab").contains(&herdr("rename_tab")), "label");
        assert!(found("RENAME_TAB").contains(&herdr("rename_tab")), "name, any case");
        assert!(found("rename_tab").contains(&RowId::Shepherd("RenameTab")) || found("rename tab").contains(&RowId::Shepherd("RenameTab")), "the other source too");
        assert!(found("prefix+t").contains(&herdr("new_tab")) && found("prefix t").contains(&herdr("new_tab")), "chord as written and as words");
        assert!(found("shift n").contains(&herdr("new_workspace")), "words of a chord");
        assert!(found("zoom shift").is_empty() || !found("zoom shift").contains(&herdr("new_tab")), "every word must match");
        assert_eq!(found("").len(), r.len(), "empty: everything");
        assert!(found("zzzz-no-such").is_empty());
        assert!(found("ctrl shift t").contains(&RowId::Shepherd("NewTab")), "a Shepherd chord by its words");
    }

    // --- conflicts ---

    #[test]
    fn two_herdr_rows_on_one_chord_clash() {
        let r = model(&[], "[keys]\nnew_tab = \"prefix+t\"\nrename_tab = \"prefix+t\"\n");
        let c = conflicts(&r, &["ctrl-b".to_string()]);
        let note = &c[&herdr("new_tab")];
        assert_eq!(note, &["Ctrl+B T is also herdr's \u{201c}Rename tab\u{201d}".to_string()]);
        assert!(c.contains_key(&herdr("rename_tab")));
        assert!(!c.contains_key(&herdr("zoom")));
    }

    #[test]
    fn a_shepherd_chord_on_a_herdr_chord_clashes_and_shepherd_wins() {
        let r = model(&[("NewTab", "ctrl-alt-n")], "[keys]\nnew_workspace = \"ctrl+alt+n\"\n");
        let c = conflicts(&r, &["ctrl-b".to_string()]);
        assert!(c[&herdr("new_workspace")][0].ends_with("is also Shepherd's \u{201c}New tab\u{201d}, which wins in a pane"), "{:?}", c[&herdr("new_workspace")]);
        assert!(c[&RowId::Shepherd("NewTab")][0].ends_with("which loses to it in a pane"), "{:?}", c[&RowId::Shepherd("NewTab")]);
    }

    #[test]
    fn the_same_action_on_the_same_chord_is_no_clash() {
        // herdr's next_tab on Ctrl+Tab runs Shepherd's NextTab, which has Ctrl+Tab itself
        let r = model(&[], "[keys]\nnext_tab = \"ctrl+tab\"\nswitch_tab = \"alt+1..9\"\n");
        let c = conflicts(&r, &["ctrl-b".to_string()]);
        assert!(!c.contains_key(&herdr("next_tab")) && !c.contains_key(&RowId::Shepherd("NextTab")), "{c:?}");
        // but another action on it is one, and so is another digit of an indexed one
        let r = model(&[("Tab4", "alt-3")], "[keys]\nswitch_tab = \"alt+1..9\"\n");
        let c = conflicts(&r, &["ctrl-b".to_string()]);
        assert!(c.contains_key(&herdr("switch_tab")) && c.contains_key(&RowId::Shepherd("Tab4")), "{c:?}");
        let r = model(&[("Tab3", "alt-3")], "[keys]\nswitch_tab = \"alt+1..9\"\n");
        assert!(!conflicts(&r, &["ctrl-b".to_string()]).contains_key(&RowId::Shepherd("Tab3")), "Tab3 on alt-3 is what herdr's alt+3 runs");
    }

    #[test]
    fn a_chord_that_starts_a_longer_one_hides_it() {
        // Shepherd's Ctrl+B swallows herdr's Ctrl+B C
        let r = model(&[("NewTab", "ctrl-b")], "");
        let c = conflicts(&r, &["ctrl-b".to_string()]);
        assert!(c.contains_key(&herdr("new_tab")), "{c:?}");
        assert!(c.contains_key(&RowId::Shepherd("NewTab")));
    }

    #[test]
    fn the_prefix_itself_clashes_only_with_the_same_chord() {
        let r = model(&[], "");
        assert!(conflicts(&r, &["ctrl-b".to_string()]).is_empty(), "defaults clash with nothing: {:?}", conflicts(&r, &["ctrl-b".to_string()]));
        let r = model(&[("NewTab", "ctrl-b")], "");
        assert!(conflicts(&r, &["ctrl-b".to_string()]).contains_key(&herdr("prefix")));
    }

    #[test]
    fn indexed_actions_clash_digit_by_digit() {
        let r = model(&[("Tab3", "alt-3")], "[keys]\nfocus_agent = \"alt+1..9\"\n");
        let c = conflicts(&r, &["ctrl-b".to_string()]);
        assert!(c.contains_key(&herdr("focus_agent")) && c.contains_key(&RowId::Shepherd("Tab3")));
        assert!(!c.contains_key(&RowId::Shepherd("Tab4")));
    }

    #[test]
    fn the_editors_chords_do_not_clash_with_a_panes() {
        // Ctrl+S saves in a file's view only; herdr's own Ctrl+S in a pane is no clash
        let r = model(&[], "[keys]\nzoom = \"ctrl+s\"\n");
        assert!(!conflicts(&r, &["ctrl-b".to_string()]).contains_key(&herdr("zoom")));
    }

    #[test]
    fn a_mode_of_herdrs_screen_or_a_bare_key_clashes_with_nothing() {
        let r = model(&[("NewTab", "ctrl-h")], "[keys]\nnavigate_pane_left = \"ctrl+h\"\nclose_pane = \"h\"\n");
        let c = conflicts(&r, &["ctrl-b".to_string()]);
        assert!(!c.contains_key(&herdr("navigate_pane_left")) && !c.contains_key(&herdr("close_pane")), "{c:?}");
    }

    // --- recording ---

    #[test]
    fn super_chords_record_like_any_other() {
        let prefixes = ["ctrl-b".to_string()];
        let super_k = press("super-k");
        assert!(crate::bindings::recordable(&super_k), "Super+K can be a shortcut");
        let chord = super_k.unparse();
        assert_eq!(record_step(Kind::Shepherd, &super_k, false, &prefixes), Step::Commit(chord.clone()));
        assert_eq!(record_step(Kind::Herdr, &super_k, false, &prefixes), Step::Commit("super+k".into()));
        assert_eq!(record_step(Kind::Prefix, &press("super-space"), false, &prefixes), Step::Commit("super+space".into()));
        assert_eq!(record_step(Kind::Herdr, &press("super-shift-p"), false, &prefixes), Step::Commit("super+shift+p".into()));
        // and it reads back as the chord that was pressed
        assert_eq!(keystroke("super+space"), normalize(&press("super-space").unparse()));
        // Cmd (the Mac's name for it) is the same key
        assert_eq!(record_step(Kind::Herdr, &press("cmd-k"), false, &prefixes), Step::Commit("super+k".into()));
    }

    #[test]
    fn a_bare_key_is_not_a_chord_but_escape_and_backspace_act() {
        let p = ["ctrl-b".to_string()];
        assert_eq!(record_step(Kind::Shepherd, &press("a"), false, &p), Step::Ignore);
        assert_eq!(record_step(Kind::Herdr, &press("a"), false, &p), Step::Ignore);
        assert_eq!(record_step(Kind::Herdr, &press("escape"), false, &p), Step::Cancel);
        assert_eq!(record_step(Kind::Shepherd, &press("backspace"), false, &p), Step::Clear);
        assert_eq!(record_step(Kind::Herdr, &press("f5"), false, &p), Step::Commit("f5".into()));
    }

    #[test]
    fn the_prefix_then_a_key_records_a_prefix_chord() {
        let p = ["ctrl-b".to_string()];
        assert_eq!(record_step(Kind::Herdr, &press("ctrl-b"), false, &p), Step::WaitPrefix);
        assert_eq!(record_step(Kind::Herdr, &press("n"), true, &p), Step::Commit("prefix+n".into()), "a bare key after the prefix");
        assert_eq!(record_step(Kind::Herdr, &press("shift-n"), true, &p), Step::Commit("prefix+shift+n".into()));
        assert_eq!(record_step(Kind::Herdr, &press("backspace"), true, &p), Step::Commit("prefix+backspace".into()));
        assert_eq!(record_step(Kind::Herdr, &press("escape"), true, &p), Step::Cancel);
        assert_eq!(record_step(Kind::Prefix, &press("ctrl-b"), false, &p), Step::Commit("ctrl+b".into()), "a prefix row records the prefix itself");
        assert_eq!(record_step(Kind::Shepherd, &press("ctrl-b"), false, &p), Step::Commit("ctrl-b".into()), "Shepherd's rows have no prefix");
    }

    #[test]
    fn an_indexed_row_takes_a_digit() {
        let p = ["ctrl-b".to_string()];
        assert_eq!(record_step(Kind::HerdrIndexed, &press("alt-3"), false, &p), Step::Commit("alt+1..9".into()));
        assert_eq!(record_step(Kind::HerdrIndexed, &press("5"), true, &p), Step::Commit("prefix+1..9".into()));
        assert_eq!(record_step(Kind::HerdrIndexed, &press("ctrl-!"), false, &p), Step::Commit("ctrl+shift+1..9".into()));
        assert!(matches!(record_step(Kind::HerdrIndexed, &press("alt-x"), false, &p), Step::Reject(_)));
        assert!(matches!(record_step(Kind::HerdrIndexed, &press("alt-0"), false, &p), Step::Reject(_)));
    }

    #[test]
    fn chords_are_added_replaced_and_removed_without_doubles() {
        let l = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(apply_slot(&l(&["a", "b"]), Slot::Replace(1), Some("c".into())), l(&["a", "c"]));
        assert_eq!(apply_slot(&l(&["a", "b"]), Slot::Add, Some("c".into())), l(&["a", "b", "c"]));
        assert_eq!(apply_slot(&l(&["a", "b"]), Slot::Replace(0), None), l(&["b"]));
        assert_eq!(apply_slot(&l(&["a", "b"]), Slot::Add, Some("A".into())), l(&["a", "b"]), "no chord twice");
        assert_eq!(apply_slot(&[], Slot::Add, Some("c".into())), l(&["c"]));
    }

    // --- writes ---

    const CONFIG: &str = "onboarding = false\n\n# herdr configuration\n[keys]\n# the prefix\nprefix = \"ctrl+space\"\n\n# Config and help\nreload_config = \"prefix+q\" # reload\nnew_tab = \"prefix+t\"\n\n[keys.indexed]\ntabs = \"\"\n\n[[keys.command]]\nkey = \"super+p\"\ntype = \"popup\"\ncommand = \"lazygit\" # git\n\n[ui]\n# toasts\nsound = true\n";

    fn write(toml: &str, name: &str, labels: &[&str]) -> String {
        let mut c = config(toml);
        let rows = rows(&BTreeMap::new(), &builtin_defaults(), &c);
        let labels: Vec<String> = labels.iter().map(|s| s.to_string()).collect();
        write_herdr(&mut c, row(&rows, herdr(name)), &labels).unwrap();
        c.to_text()
    }

    #[test]
    fn changing_a_chord_keeps_comments_and_every_other_line() {
        let out = write(CONFIG, "new_tab", &["prefix+y", "alt+t"]);
        assert_eq!(out, CONFIG.replace("new_tab = \"prefix+t\"", "new_tab = [\"prefix+y\", \"alt+t\"]"));
        let out = write(CONFIG, "reload_config", &["prefix+r"]);
        assert_eq!(out, CONFIG.replace("\"prefix+q\" # reload", "\"prefix+r\" # reload"), "a trailing comment stays");
    }

    #[test]
    fn a_new_chord_is_added_under_keys_and_nothing_else_moves() {
        let out = write(CONFIG, "close_pane", &["alt+esc"]);
        let expect = CONFIG.replace("new_tab = \"prefix+t\"\n", "new_tab = \"prefix+t\"\nclose_pane = \"alt+esc\"\n");
        assert_eq!(out, expect);
    }

    #[test]
    fn a_reset_removes_just_that_line() {
        let out = write(CONFIG, "new_tab", &["prefix+c"]);
        assert_eq!(out, CONFIG.replace("new_tab = \"prefix+t\"\n", ""), "the default chord needs no line");
        // and so does a chord given back to the default among several
        let out = write("[keys]\nnew_tab = [\"prefix+c\"]\n", "new_tab", &["prefix+c"]);
        assert_eq!(out, "[keys]\n");
    }

    #[test]
    fn unbinding_writes_an_empty_string_unless_it_was_never_bound() {
        assert_eq!(write(CONFIG, "new_tab", &[]), CONFIG.replace("new_tab = \"prefix+t\"", "new_tab = \"\""));
        let out = write("[keys]\n", "last_pane", &[]);
        assert_eq!(out, "[keys]\n", "unbound by default: an empty line would say nothing");
    }

    #[test]
    fn a_config_with_only_commands_gets_its_keys_table() {
        let toml = "[[keys.command]]\nkey = \"super+p\"\ncommand = \"x\"\n";
        let out = write(toml, "new_tab", &["prefix+t"]);
        let reparsed = config(&out);
        assert_eq!(reparsed.strs("keys.new_tab"), Some(vec!["prefix+t".to_string()]));
        assert_eq!(reparsed.commands().len(), 1, "{out}");
        assert!(out.contains("key = \"super+p\"\ncommand = \"x\""), "{out}");
    }

    #[test]
    fn the_prefix_is_written_like_any_chord() {
        let out = write(CONFIG, "prefix", &["super+space"]);
        assert_eq!(out, CONFIG.replace("\"ctrl+space\"", "\"super+space\""));
        let out = write(CONFIG, "prefix", &["ctrl+b"]);
        assert!(!out.contains("prefix ="), "back to the default: the line goes\n{out}");
    }

    #[test]
    fn a_command_keeps_its_comment_when_rekeyed() {
        let mut c = config(CONFIG);
        c.set_command_labels(0, &["super+g".to_string(), "prefix+g".to_string()]).unwrap();
        assert_eq!(c.to_text(), CONFIG.replace("key = \"super+p\"", "key = [\"super+g\", \"prefix+g\"]"));
        assert!(c.set_command_labels(3, &[]).is_err());
    }

    #[test]
    fn reset_all_removes_only_the_key_lines_the_page_owns() {
        let mut c = config(CONFIG);
        let r = rows(&BTreeMap::new(), &builtin_defaults(), &c);
        reset_herdr_group(&mut c, &r);
        let out = c.to_text();
        for gone in ["prefix =", "reload_config", "new_tab"] {
            assert!(!out.contains(gone), "{gone}\n{out}");
        }
        for kept in ["onboarding = false", "# herdr configuration", "[keys.indexed]", "tabs = \"\"", "[[keys.command]]", "key = \"super+p\"", "command = \"lazygit\" # git", "[ui]", "sound = true", "# toasts"] {
            assert!(out.contains(kept), "{kept}\n{out}");
        }
        assert_eq!(c.commands().len(), 1, "custom commands stay");
    }

    #[test]
    fn a_line_shepherd_does_not_show_is_not_removed() {
        // a key only this config has, shown as a row, goes; a table herdr gained stays
        let mut c = config("[keys]\nfuture_thing = \"ctrl+alt+f\"\nnew_tab = \"prefix+t\"\n\n[keys.extra]\nx = 1\n");
        let r = rows(&BTreeMap::new(), &builtin_defaults(), &c);
        reset_herdr_group(&mut c, &r);
        assert_eq!(c.to_text(), "[keys]\n\n[keys.extra]\nx = 1\n");
    }
}
