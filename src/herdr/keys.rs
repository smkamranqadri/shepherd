//! herdr's `[keys]` in Shepherd: its chords for the actions Shepherd has, prefix ones included, and its
//! `[[keys.command]]` (plugin actions, shell commands), so a keymap set up for herdr's TUI works here too.
use super::config::HerdrConfig;
use super::keymap::{Action, Target, bindable, prefixes, targets};

/// A `[[keys.command]]`, run as herdr runs it.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = shepherd, no_json)]
pub struct HerdrCommand {
    pub kind: CommandKind,
    /// A plugin action's id (`plugin.action`, or a bare `action`), or a shell command line.
    pub command: String,
    /// Its keys as the config spells them: how herdr's own list of these commands names it.
    pub keys: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CommandKind {
    Shell,
    Pane,
    Popup,
    PluginAction,
}

/// What a herdr chord runs: a Shepherd action by its name in `bindings::ACTIONS`, a herdr command, a herdr action
/// Shepherd has no equivalent of (caught and said, never typed into the pane), or a chord after the prefix that
/// herdr binds to nothing (swallowed the way herdr's prefix mode does).
#[derive(Clone, Debug, PartialEq)]
pub enum Bind {
    App(&'static str),
    Command(HerdrCommand),
    HerdrOnly(HerdrOnly),
    Unbound(HerdrUnbound),
}

/// A chord of one of herdr's actions that only herdr's own screen can run (`detach`, `copy_mode`, an action a newer
/// herdr has): a pane tells so, instead of the shell getting the prefix and the key typed.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = shepherd, no_json)]
pub struct HerdrOnly {
    /// herdr's name for the action.
    pub name: String,
    /// The chord as the config spells it.
    pub chord: String,
}

/// The key after the prefix that herdr binds to nothing: swallowed, as prefix mode does, with a word about it
/// (none for Esc, which leaves prefix mode on purpose).
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = shepherd, no_json)]
pub struct HerdrUnbound {
    /// The chord pressed, in GPUI's spelling; empty for Esc.
    pub chord: String,
}

/// The prefix chords of `actions` in GPUI's spelling (`ctrl-b`).
pub fn prefix_chords(actions: &[Action]) -> Vec<String> {
    prefixes(actions).iter().filter_map(|p| keystroke(p)).collect()
}

/// The chords a label of the config stands for, in GPUI's spelling: `prefix+n` is the prefix, then `n`, for each
/// prefix (`prefixes`, as [`prefix_chords`] has them); any other label one keystroke. None when it does not parse.
pub fn chords_for(label: &str, prefixes: &[String]) -> Vec<String> {
    match label.trim().strip_prefix("prefix+") {
        Some(rest) => keystroke(rest).map_or(Vec::new(), |k| prefixes.iter().map(|p| format!("{p} {k}")).collect()),
        None => keystroke(label).into_iter().collect(),
    }
}

/// Every chord herdr's config binds to something Shepherd runs, in GPUI's spelling: the actions of `[keys]` as
/// `actions` has them, the `[[keys.command]]`, and, after each prefix, every other key (so herdr's prefix mode is not
/// left half-typed into a shell by a key it does not bind).
pub fn bindings(actions: &[Action], config: &HerdrConfig) -> Vec<(Bind, String)> {
    let prefixes = prefix_chords(actions);
    let chords = |label: &str| chords_for(label, &prefixes);
    let mut out = Vec::new();
    for (target, label) in targets(actions) {
        let chords = chords(&label);
        out.extend(chords.into_iter().map(|c| match &target {
            Target::App(app) => (Bind::App(app), c),
            Target::HerdrOnly(name) => (Bind::HerdrOnly(HerdrOnly { name: name.clone(), chord: label.clone() }), c),
        }));
    }
    for entry in config.commands() {
        let kind = match entry.kind.as_str() {
            "shell" => CommandKind::Shell,
            "pane" => CommandKind::Pane,
            "popup" => CommandKind::Popup,
            "plugin_action" => CommandKind::PluginAction,
            _ => continue,
        };
        if entry.command.is_empty() {
            continue;
        }
        let command = HerdrCommand { kind, command: entry.command, keys: entry.keys.clone() };
        for label in entry.keys.iter().filter(|l| bindable(l)) {
            out.extend(chords(label).into_iter().map(|c| (Bind::Command(command.clone()), c)));
        }
    }
    // a chord given twice (to an action and to a command) keeps its first meaning here, where GPUI would take the later
    let mut seen = std::collections::HashSet::new();
    out.retain(|(_, c)| seen.insert(c.clone()));
    if !prefixes.is_empty() {
        for prefix in &prefixes {
            for key in second_keys() {
                let chord = format!("{prefix} {key}");
                if seen.contains(&chord) || prefixes.contains(&key) {
                    continue;
                }
                let bind = if key == "escape" { HerdrUnbound { chord: String::new() } } else { HerdrUnbound { chord: chord.clone() } };
                out.push((Bind::Unbound(bind), chord));
            }
        }
    }
    out
}

/// Every key that can follow the prefix, with the modifiers a terminal tells apart, in GPUI's spelling.
fn second_keys() -> Vec<String> {
    const MODS: [&str; 8] = ["", "shift-", "ctrl-", "alt-", "ctrl-shift-", "ctrl-alt-", "alt-shift-", "ctrl-alt-shift-"];
    let letters = ('a'..='z').map(String::from);
    let digits = ('0'..='9').map(String::from);
    let named = ["space", "tab", "enter", "escape", "backspace", "left", "right", "up", "down", "home", "end", "pageup", "pagedown", "delete", "insert"]
        .into_iter()
        .map(String::from)
        .chain((1..=12).map(|n| format!("f{n}")));
    let mut out = Vec::new();
    for key in letters.chain(named) {
        out.extend(MODS.iter().map(|m| format!("{m}{key}")));
    }
    // a digit or a punctuation key: shift makes another character, which is a key of its own below
    for key in digits.chain("-=[]\\;',./`".chars().map(String::from)) {
        out.extend(MODS.iter().filter(|m| !m.contains("shift")).map(|m| format!("{m}{key}")));
    }
    for key in "!@#$%^&*()_+{}|:\"<>?~".chars() {
        out.extend(["", "ctrl-", "alt-", "ctrl-alt-"].iter().map(|m| format!("{m}{key}")));
    }
    out.into_iter().filter_map(|k| crate::bindings::normalize(&k)).collect()
}

/// What shift makes of a key on a US layout: GPUI matches the typed character, so `shift+1` is `!`.
const SHIFTED: &[(char, char)] = &[
    ('1', '!'), ('2', '@'), ('3', '#'), ('4', '$'), ('5', '%'), ('6', '^'), ('7', '&'), ('8', '*'), ('9', '('),
    ('0', ')'), ('-', '_'), ('=', '+'), ('[', '{'), (']', '}'), (';', ':'), ('\'', '"'), (',', '<'), ('.', '>'),
    ('/', '?'), ('\\', '|'), ('`', '~'),
];

/// One herdr key (`ctrl+shift+k`, `minus`, `%`) in GPUI's spelling; `None` for an empty or unknown one.
pub(crate) fn keystroke(label: &str) -> Option<String> {
    let (mut mods, mut shift, mut key) = (Vec::new(), false, None);
    for part in label.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.push("ctrl"),
            "alt" | "option" | "meta" => mods.push("alt"),
            "cmd" | "command" | "super" => mods.push("cmd"),
            "shift" => shift = true,
            _ if part.is_empty() || key.is_some() => return None,
            _ => key = Some(part),
        }
    }
    let key = key?;
    let named = match key.to_ascii_lowercase().as_str() {
        "space" | " " => "space",
        "enter" | "return" => "enter",
        "esc" | "escape" => "escape",
        "bs" | "backspace" => "backspace",
        "minus" => "-",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "backslash" => "\\",
        "quote" => "'",
        "double_quote" | "double-quote" => "\"",
        "semicolon" => ";",
        "colon" => ":",
        "percent" => "%",
        "ampersand" => "&",
        "backtick" => "`",
        "plus" => "+",
        _ => "",
    };
    let mut chars = key.chars();
    let key = match (named, chars.next(), chars.next()) {
        ("", Some(c), None) if c.is_ascii_uppercase() => {
            shift = true;
            c.to_ascii_lowercase().to_string()
        }
        ("", Some(c), None) => c.to_string(),
        ("", ..) => key.to_ascii_lowercase(), // tab, left, f5
        (named, ..) => named.to_string(),
    };
    let mut chars = key.chars();
    let key = match (chars.next(), chars.next()) {
        // a shifted digit or punctuation is the character it types; a letter or named key keeps shift
        (Some(c), None) if shift && !c.is_ascii_alphabetic() => {
            shift = false;
            SHIFTED.iter().find(|(k, _)| *k == c).map_or(c, |(_, s)| *s).to_string()
        }
        _ => key,
    };
    if shift {
        mods.push("shift");
    }
    mods.push(&key);
    crate::bindings::normalize(&mods.join("-"))
}

/// A recorded keystroke (GPUI's spelling, `super-space`) as herdr's config spells it (`super+space`): the other way
/// from [`keystroke`]. `None` for more than one keystroke, or a key herdr has no word for. The super key is `super`
/// on every system (herdr takes it for the Cmd key on a Mac), the keys that would break the syntax by name
/// (`minus`, `comma`, `plus`, `ampersand`, `backtick`) and the rest as they are.
pub fn label_of(chord: &str) -> Option<String> {
    let mut parts = chord.split_whitespace();
    let (first, rest) = (parts.next()?, parts.next());
    let k = gpui::Keystroke::parse(first).ok().filter(|_| rest.is_none())?;
    let mut m = k.modifiers;
    // Linux hears Ctrl+Shift+1 as `ctrl-!`: pressed as shift and the digit
    let mut key = k.key.clone();
    if let Some(digit) = crate::bindings::shifted_digit(&key) {
        key = digit.to_string();
        m.shift = true;
    }
    let key = match key.as_str() {
        "-" => "minus",
        "," => "comma",
        "+" => "plus",
        "&" => "ampersand",
        "`" => "backtick",
        "escape" => "esc",
        other => other,
    };
    let mut label: Vec<&str> = Vec::new();
    for (on, name) in [(m.control, "ctrl"), (m.alt, "alt"), (m.platform, "super"), (m.shift, "shift")] {
        if on {
            label.push(name);
        }
    }
    label.push(key);
    Some(label.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::keymap::{actions, builtin_defaults};

    fn config(toml: &str) -> HerdrConfig {
        HerdrConfig(toml.parse().unwrap())
    }

    fn binds(toml: &str) -> Vec<(Bind, String)> {
        let config = config(toml);
        bindings(&actions(&builtin_defaults(), &config), &config)
    }

    fn chord(binds: &[(Bind, String)], bind: Bind) -> Vec<&str> {
        binds.iter().filter(|(b, _)| *b == bind).map(|(_, c)| c.as_str()).collect()
    }

    fn unbound(binds: &[(Bind, String)]) -> Vec<&str> {
        binds.iter().filter(|(b, _)| matches!(b, Bind::Unbound(_))).map(|(_, c)| c.as_str()).collect()
    }

    #[test]
    fn herdr_keys_in_gpui_spelling() {
        let k = |s| keystroke(s);
        assert_eq!(k("ctrl+b").as_deref(), Some("ctrl-b"));
        assert_eq!(k("ctrl+alt+n"), crate::bindings::normalize("ctrl-alt-n"));
        assert_eq!(k("shift+k").as_deref(), Some("shift-k"));
        assert_eq!(k("K").as_deref(), Some("shift-k"));
        assert_eq!(k("minus").as_deref(), Some("-"));
        assert_eq!(k("%").as_deref(), Some("%"));
        assert_eq!(k("shift+5").as_deref(), Some("%"));
        assert_eq!(k("cmd+shift+}"), crate::bindings::normalize("cmd-}"));
        assert_eq!(k("shift+tab").as_deref(), Some("shift-tab"));
        assert_eq!(k("f5").as_deref(), Some("f5"));
        assert_eq!(k(""), None);
        assert_eq!(k("ctrl+a+b"), None);
    }

    #[test]
    fn super_and_cmd_are_the_platform_key() {
        let platform = |label: &str| keystroke(label).unwrap();
        assert_eq!(platform("super+space"), platform("cmd+space"));
        assert_eq!(platform("super+space"), platform("command+space"));
        assert_eq!(platform("super+space"), crate::bindings::normalize("cmd-space").unwrap());
        #[cfg(not(target_os = "macos"))]
        assert_eq!(platform("super+space"), "super-space");
        assert!(gpui::Keystroke::parse(&platform("super+shift+k")).unwrap().modifiers.platform);
    }

    #[test]
    fn a_recorded_chord_is_written_the_way_herdr_reads_it() {
        let label = |chord: &str| label_of(&crate::bindings::normalize(chord).unwrap());
        assert_eq!(label("super-space").as_deref(), Some("super+space"));
        assert_eq!(label("cmd-space").as_deref(), Some("super+space"), "a Mac's Cmd is herdr's super");
        assert_eq!(label("ctrl-alt-shift-left").as_deref(), Some("ctrl+alt+shift+left"));
        assert_eq!(label("ctrl-space").as_deref(), Some("ctrl+space"));
        assert_eq!(label("escape").as_deref(), Some("esc"));
        assert_eq!(label("alt--").as_deref(), Some("alt+minus"));
        assert_eq!(label("ctrl-,").as_deref(), Some("ctrl+comma"));
        assert_eq!(label("ctrl-!").as_deref(), Some("ctrl+shift+1"), "Linux hears Ctrl+Shift+1 as ctrl-!");
        assert_eq!(label("ctrl-b c"), None, "one keystroke");
        // and herdr's spelling reads back as the same chord
        for chord in ["super-space", "ctrl-alt-shift-left", "alt--", "ctrl-,", "ctrl-!", "f12", "alt-enter", "super-shift-k", "ctrl-+", "ctrl-alt-&", "ctrl-`", "alt-tab", "ctrl-/"] {
            let chord = crate::bindings::normalize(chord).unwrap();
            let there = label_of(&chord).unwrap_or_else(|| panic!("{chord}"));
            assert_eq!(keystroke(&there).as_deref(), Some(chord.as_str()), "{chord} -> {there}");
        }
    }

    #[test]
    fn defaults_hang_off_the_prefix() {
        let binds = binds("");
        assert_eq!(chord(&binds, Bind::App("NewTab")), ["ctrl-b c"]);
        assert_eq!(chord(&binds, Bind::App("SplitDown")), ["ctrl-b -"]);
        assert_eq!(chord(&binds, Bind::App("Tab3")), ["ctrl-b 3"]);
        assert_eq!(chord(&binds, Bind::App("WorkspacesPanel")), ["ctrl-b w"]);
        assert_eq!(chord(&binds, Bind::App("RenameTab")), ["ctrl-b shift-t"]);
        assert!(chord(&binds, Bind::App("NextWorkspace")).is_empty(), "unset by default");
    }

    #[test]
    fn the_users_keys_replace_the_defaults() {
        let binds = binds(
            r#"
[keys]
prefix = ["ctrl+a", "f12"]
split_vertical = "prefix+%"
next_tab = ["prefix+n", "ctrl+alt+]"]
close_pane = ""
toggle_sidebar = "cmd+b"
"#,
        );
        assert_eq!(chord(&binds, Bind::App("SplitRight")), ["ctrl-a %", "f12 %"]);
        assert_eq!(chord(&binds, Bind::App("NextTab")).len(), 3);
        assert!(chord(&binds, Bind::App("ClosePane")).is_empty(), "empty unbinds");
        assert_eq!(chord(&binds, Bind::App("ToggleSidebar")), [crate::bindings::normalize("cmd-b").unwrap()]);
    }

    #[test]
    fn a_super_prefix_binds_like_any_other() {
        let binds = binds("[keys]\nprefix = \"super+space\"\nnew_tab = \"prefix+t\"\ndetach = \"super+q\"\n");
        let prefix = crate::bindings::normalize("cmd-space").unwrap();
        assert_eq!(chord(&binds, Bind::App("NewTab")), [format!("{prefix} t")]);
        assert_eq!(chord(&binds, Bind::App("SplitRight")), [format!("{prefix} v")]);
        assert!(chord(&binds, Bind::App("NewTab")).iter().all(|c| gpui::Keystroke::parse(c.split(' ').next().unwrap()).unwrap().modifiers.platform));
        let detach = binds.iter().find(|(b, _)| matches!(b, Bind::HerdrOnly(h) if h.name == "detach")).unwrap();
        assert_eq!(detach.1, crate::bindings::normalize("cmd-q").unwrap());
    }

    #[test]
    fn indexed_bindings_take_each_digit() {
        let binds = binds("[keys]\nswitch_tab = [\"prefix+1..9\", \"alt+1..9\"]\nfocus_agent = \"prefix+alt+1..9\"\n");
        assert_eq!(chord(&binds, Bind::App("Tab7")), ["ctrl-b 7", "alt-7"]);
        assert_eq!(chord(&binds, Bind::App("Agent4")), ["ctrl-b alt-4"]);
    }

    #[test]
    fn an_action_shepherd_lacks_is_caught_not_typed() {
        let binds = binds("[keys]\ndetach = \"prefix+q\"\nfuture_thing = \"prefix+shift+f\"\ncopy_mode = \"prefix+[\"\n");
        let only = |name: &str| binds.iter().find(|(b, _)| matches!(b, Bind::HerdrOnly(h) if h.name == name));
        assert_eq!(only("detach").map(|(_, c)| c.as_str()), Some("ctrl-b q"));
        assert_eq!(only("future_thing").map(|(_, c)| c.as_str()), Some("ctrl-b shift-f"));
        assert_eq!(only("copy_mode").map(|(_, c)| c.as_str()), Some("ctrl-b ["));
    }

    #[test]
    fn a_key_herdr_binds_to_nothing_after_the_prefix_is_swallowed() {
        let binds = binds("");
        let un = unbound(&binds);
        // letters, digits, punctuation, named keys and modified ones; none the prefix itself, none a bound chord
        for c in ["ctrl-b a", "ctrl-b shift-q", "ctrl-b ctrl-x", "ctrl-b alt-f", "ctrl-b f9", "ctrl-b left", "ctrl-b ,", "ctrl-b ctrl-alt-up", "ctrl-b ~"] {
            assert!(un.contains(&c), "{c}");
        }
        assert!(!un.contains(&"ctrl-b c"), "a bound chord keeps its action");
        assert!(!un.contains(&"ctrl-b ctrl-b"), "the prefix twice is left as typed");
        let esc = binds.iter().find(|(_, c)| c == "ctrl-b escape").unwrap();
        assert_eq!(esc.0, Bind::Unbound(HerdrUnbound { chord: String::new() }), "Esc leaves prefix mode quietly");
        let a = binds.iter().find(|(_, c)| c == "ctrl-b a").unwrap();
        assert_eq!(a.0, Bind::Unbound(HerdrUnbound { chord: "ctrl-b a".into() }));
        // every chord is bound once, and parses
        let mut seen = std::collections::HashSet::new();
        for (_, c) in &binds {
            assert!(seen.insert(c.clone()), "{c} twice");
            assert!(crate::bindings::valid_chord(c), "{c}");
        }
    }

    #[test]
    fn an_emptied_prefix_is_the_default_prefix() {
        let binds = binds("[keys]\nprefix = \"\"\n");
        assert_eq!(chord(&binds, Bind::App("NewTab")), ["ctrl-b c"]);
    }

    #[test]
    fn a_bare_key_is_never_bound() {
        let binds = binds("[keys]\nnew_tab = \"t\"\nnavigate_pane_left = \"h\"\nzoom = \"ctrl+z\"\n");
        assert!(!binds.iter().any(|(_, c)| c == "t" || c == "h"));
        assert_eq!(chord(&binds, Bind::App("ToggleZoom")), ["ctrl-z"]);
    }

    #[test]
    fn commands_run_plugin_actions_and_shell_lines() {
        let binds = binds(
            r#"
[[keys.command]]
key = "ctrl+h"
type = "plugin_action"
command = "vim-herdr-navigation.left"

[[keys.command]]
key = "prefix+t"
command = "say hi"

[[keys.command]]
key = "prefix+y"
type = "teleport"
command = "x"
"#,
        );
        let plugin = HerdrCommand { kind: CommandKind::PluginAction, command: "vim-herdr-navigation.left".into(), keys: vec!["ctrl+h".into()] };
        assert_eq!(chord(&binds, Bind::Command(plugin)), ["ctrl-h"]);
        let shell = HerdrCommand { kind: CommandKind::Shell, command: "say hi".into(), keys: vec!["prefix+t".into()] };
        assert_eq!(chord(&binds, Bind::Command(shell)), ["ctrl-b t"]);
        assert_eq!(binds.iter().filter(|(b, _)| matches!(b, Bind::Command(_))).count(), 2, "unknown types are skipped");
        assert!(unbound(&binds).iter().all(|c| *c != "ctrl-b t"), "a command's chord is not swallowed");
    }
}
