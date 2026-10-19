//! herdr's `[keys]` as a list of actions: the names and defaults from `herdr --default-config` (so a herdr that
//! gains an action shows it here without a Shepherd release), a built-in table for the labels, for what each action
//! does in Shepherd, and as the fallback when herdr cannot be asked. Pure: no windows, no files but the config
//! it is given.
use super::config::HerdrConfig;
use std::sync::LazyLock;

/// A chord label in herdr's config that stands for the nine digit chords `1`…`9`: `prefix+1..9`.
pub const INDEXED: &str = "1..9";

/// What one of herdr's actions does in Shepherd.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Runs {
    /// Shepherd's own action, by its name in `bindings::ACTIONS`.
    App(&'static str),
    /// An indexed binding (`prefix+1..9`): Shepherd's nine actions, the digit picking one.
    Nine(&'static [&'static str; 9]),
    /// Shepherd has nothing like it (a mode of herdr's own screen). The chord is still caught in a pane, and says
    /// so, instead of being typed into the shell.
    HerdrOnly,
    /// The prefix key itself: chords that start prefix mode, bound to no action.
    Prefix,
    /// Chords herdr's screen reads in a mode of its own (navigate mode's arrows and letters) or only in its remote
    /// client: never bound in a pane, where they are typing.
    Local,
}

/// An action of herdr's `[keys]` Shepherd knows by name.
#[derive(Clone, Copy, Debug)]
pub struct Known {
    pub name: &'static str,
    pub label: &'static str,
    /// herdr's default when it cannot be asked (`""`: unbound).
    pub default: &'static str,
    pub runs: Runs,
}

macro_rules! known {
    ($($name:literal, $label:literal, $default:literal, $runs:expr;)+) => {
        &[$(Known { name: $name, label: $label, default: $default, runs: $runs }),+]
    };
}

/// herdr 0.9.5's actions, in the order of its default config, with the copy_mode and swap_pane_* it takes but does not
/// list there, and what each runs in Shepherd.
pub const KNOWN: &[Known] = known! {
    "prefix", "Prefix key", "ctrl+b", Runs::Prefix;
    "extra_prefixes", "Extra prefix keys", "", Runs::Prefix;
    "help", "Help", "prefix+?", Runs::App("ShowShortcuts");
    "settings", "Settings", "prefix+s", Runs::App("OpenSettings");
    "detach", "Detach", "prefix+q", Runs::HerdrOnly;
    "reload_config", "Reload herdr's config", "prefix+shift+r", Runs::App("ReloadHerdrConfig");
    "open_notification_target", "Go to the notification's pane", "prefix+o", Runs::App("NextNeedsYou");
    "workspace_picker", "Workspace picker", "prefix+w", Runs::App("WorkspacesPanel");
    "goto", "Go to", "prefix+g", Runs::App("CommandPanel");
    "new_workspace", "New workspace", "prefix+shift+n", Runs::App("NewWorkspace");
    "new_worktree", "New worktree", "prefix+shift+g", Runs::App("NewWorktree");
    "open_worktree", "Open worktree", "", Runs::App("OpenWorktree");
    "remove_worktree", "Remove worktree", "", Runs::App("RemoveWorktree");
    "rename_workspace", "Rename workspace", "prefix+shift+w", Runs::App("RenameWorkspace");
    "close_workspace", "Close workspace", "prefix+shift+d", Runs::App("CloseWorkspace");
    "previous_workspace", "Previous workspace", "", Runs::App("PreviousWorkspace");
    "next_workspace", "Next workspace", "", Runs::App("NextWorkspace");
    "previous_agent", "Previous agent", "", Runs::App("PreviousAgent");
    "next_agent", "Next agent", "", Runs::App("NextAgent");
    "focus_agent", "Focus agent 1 to 9", "", Runs::Nine(&crate::bindings::AGENT_ACTIONS);
    "remote_image_paste", "Paste an image (herdr --remote)", "ctrl+v", Runs::Local;
    "new_tab", "New tab", "prefix+c", Runs::App("NewTab");
    "rename_tab", "Rename tab", "prefix+shift+t", Runs::App("RenameTab");
    "previous_tab", "Previous tab", "prefix+p", Runs::App("PreviousTab");
    "next_tab", "Next tab", "prefix+n", Runs::App("NextTab");
    "move_tab_previous", "Move tab toward the front", "", Runs::App("MoveTabPrevious");
    "move_tab_next", "Move tab toward the back", "", Runs::App("MoveTabNext");
    "switch_tab", "Go to tab 1 to 9", "prefix+1..9", Runs::Nine(&crate::bindings::TAB_ACTIONS);
    "switch_workspace", "Go to workspace 1 to 9", "", Runs::Nine(&crate::bindings::WORKSPACE_ACTIONS);
    "close_tab", "Close tab", "prefix+shift+x", Runs::App("CloseTab");
    "rename_pane", "Rename pane", "prefix+shift+p", Runs::App("RenamePane");
    "edit_scrollback", "Edit scrollback", "prefix+e", Runs::App("EditScrollback");
    "clear_pane", "Clear pane", "", Runs::App("ClearPane");
    "copy_mode", "Copy mode", "", Runs::HerdrOnly;
    "focus_pane_left", "Focus pane left", "prefix+h", Runs::App("FocusLeft");
    "focus_pane_down", "Focus pane down", "prefix+j", Runs::App("FocusDown");
    "focus_pane_up", "Focus pane up", "prefix+k", Runs::App("FocusUp");
    "focus_pane_right", "Focus pane right", "prefix+l", Runs::App("FocusRight");
    "swap_pane_left", "Swap pane left", "", Runs::App("SwapPaneLeft");
    "swap_pane_down", "Swap pane down", "", Runs::App("SwapPaneDown");
    "swap_pane_up", "Swap pane up", "", Runs::App("SwapPaneUp");
    "swap_pane_right", "Swap pane right", "", Runs::App("SwapPaneRight");
    "cycle_pane_next", "Next pane", "prefix+tab", Runs::App("CyclePaneNext");
    "cycle_pane_previous", "Previous pane", "prefix+shift+tab", Runs::App("CyclePanePrevious");
    "last_pane", "Last pane", "", Runs::HerdrOnly;
    "split_vertical", "Split right", "prefix+v", Runs::App("SplitRight");
    "split_horizontal", "Split down", "prefix+minus", Runs::App("SplitDown");
    "close_pane", "Close pane", "prefix+x", Runs::App("ClosePane");
    "zoom", "Zoom pane", "prefix+z", Runs::App("ToggleZoom");
    "resize_mode", "Resize mode", "prefix+r", Runs::HerdrOnly;
    "resize_pane_left", "Resize pane left", "", Runs::App("ResizePaneLeft");
    "resize_pane_down", "Resize pane down", "", Runs::App("ResizePaneDown");
    "resize_pane_up", "Resize pane up", "", Runs::App("ResizePaneUp");
    "resize_pane_right", "Resize pane right", "", Runs::App("ResizePaneRight");
    "toggle_sidebar", "Toggle sidebar", "prefix+b", Runs::App("ToggleSidebar");
    "navigate_workspace_up", "Navigate mode: workspace up", "up", Runs::Local;
    "navigate_workspace_down", "Navigate mode: workspace down", "down", Runs::Local;
    "navigate_pane_left", "Navigate mode: pane left", "h", Runs::Local;
    "navigate_pane_down", "Navigate mode: pane down", "j", Runs::Local;
    "navigate_pane_up", "Navigate mode: pane up", "k", Runs::Local;
    "navigate_pane_right", "Navigate mode: pane right", "l", Runs::Local;
};

/// What `herdr --default-config` says each action is bound to: (name, chords as it spells them; none when unbound).
pub type Defaults = Vec<(String, Vec<String>)>;

/// The defaults of the built-in table: what herdr 0.9.5 binds.
pub fn builtin_defaults() -> Defaults {
    KNOWN.iter().map(|k| (k.name.to_string(), labels_of(k.default))).collect()
}

fn labels_of(text: &str) -> Vec<String> {
    if text.is_empty() { Vec::new() } else { vec![text.to_string()] }
}

/// What the `[keys]` section of herdr's default config binds. The defaults are commented out there
/// (`# new_tab = "prefix+c"`), so a line of `# name = "chord"` (or a list) is one. The section's other comments,
/// the example of a custom command and the legacy `[keys.indexed]` among them, are skipped.
pub fn parse_default_config(text: &str) -> Defaults {
    // keys of the example `[[keys.command]]` and `[keys.indexed]` that read like an action
    const NOT_ACTIONS: [&str; 8] = ["type", "key", "command", "width", "height", "tabs", "workspaces", "agents"];
    let mut out: Defaults = Vec::new();
    let mut in_keys = false;
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('#') && line.starts_with('[') {
            if in_keys {
                break;
            }
            in_keys = line == "[keys]";
            continue;
        }
        if !in_keys {
            continue;
        }
        let body = line.trim_start_matches('#').trim();
        if body.starts_with('[') {
            break; // the commented example of `[[keys.command]]`, then the legacy tables
        }
        let Some((name, value)) = body.split_once('=') else { continue };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c == '_') || NOT_ACTIONS.contains(&name) {
            continue;
        }
        let Some(labels) = quoted_strings(value) else { continue };
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), labels.into_iter().filter(|l| !l.is_empty()).collect()));
        }
    }
    out
}

/// A string or a list of strings at the start of `value` (text after it, a comment, is ignored); `None` for anything
/// else.
fn quoted_strings(value: &str) -> Option<Vec<String>> {
    let (list, rest) = match value.strip_prefix('[') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let mut out = Vec::new();
    let mut rest = rest.trim_start();
    loop {
        let after = rest.strip_prefix('"')?;
        let end = after.find('"')?;
        out.push(after[..end].to_string());
        rest = after[end + 1..].trim_start();
        if !list {
            return Some(out);
        }
        match rest.strip_prefix(',') {
            Some(more) => rest = more.trim_start(),
            None if rest.starts_with(']') => return Some(out),
            None => return None,
        }
        if rest.starts_with(']') {
            return Some(out);
        }
    }
}

/// herdr's defaults, asked of the herdr binary once (`herdr --default-config`, given two seconds), else the
/// built-in table. A herdr that lists nothing it can parse is no herdr to ask.
pub fn defaults() -> &'static Defaults {
    static DEFAULTS: LazyLock<Defaults> = LazyLock::new(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let output = std::process::Command::new(super::session::binary())
                .arg("--default-config")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            let _ = tx.send(output);
        });
        let asked = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .ok()
            .and_then(Result::ok)
            .filter(|o| o.status.success())
            .map(|o| parse_default_config(&String::from_utf8_lossy(&o.stdout)))
            .filter(|d| !d.is_empty());
        asked.unwrap_or_else(builtin_defaults)
    });
    &DEFAULTS
}

/// One action of `[keys]`, with the chords in force.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub name: String,
    pub label: String,
    pub runs: Runs,
    /// The chords now: the config's, else the default.
    pub labels: Vec<String>,
    pub default: Vec<String>,
    /// The config has a line for it.
    pub set: bool,
    /// Shepherd's table knows it; the others are herdr's own, newer than this table.
    pub known: bool,
}

/// Every action of herdr's `[keys]`: Shepherd's table first (in its order), then the ones `defaults` has that it
/// lacks, then the config's own that neither has (a herdr newer than both). New ones are `HerdrOnly`: their chords
/// are caught and say so.
pub fn actions(defaults: &Defaults, config: &HerdrConfig) -> Vec<Action> {
    let default_of = |name: &str| defaults.iter().find(|(n, _)| n == name).map(|(_, l)| l.clone());
    let mut names: Vec<(String, Option<&'static Known>)> = KNOWN.iter().map(|k| (k.name.to_string(), Some(k))).collect();
    for name in defaults.iter().map(|(n, _)| n.clone()).chain(config.key_names()) {
        if !names.iter().any(|(n, _)| *n == name) {
            names.push((name, None));
        }
    }
    names
        .into_iter()
        .map(|(name, known)| {
            let default = default_of(&name).or_else(|| known.map(|k| labels_of(k.default))).unwrap_or_default();
            let own = config.strs(&format!("keys.{name}"));
            Action {
                label: known.map_or_else(|| sentence(&name), |k| k.label.to_string()),
                runs: known.map_or(Runs::HerdrOnly, |k| k.runs),
                set: own.is_some(),
                labels: own.map_or_else(|| default.clone(), |l| l.into_iter().filter(|l| !l.is_empty()).collect()),
                default,
                known: known.is_some(),
                name,
            }
        })
        .collect()
}

/// `rename_tab` → `Rename tab`.
fn sentence(name: &str) -> String {
    let mut words = name.replace('_', " ");
    if let Some(first) = words.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    words
}

/// The chords that start prefix mode: `prefix` (herdr's `ctrl+b` unless set) and `extra_prefixes`.
pub fn prefixes(actions: &[Action]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for action in actions.iter().filter(|a| a.runs == Runs::Prefix) {
        // an emptied `prefix` is herdr's default again: prefix mode has to be entered somehow
        let labels = if action.name == "prefix" && action.labels.is_empty() { &action.default } else { &action.labels };
        for label in labels {
            if !out.contains(label) {
                out.push(label.clone());
            }
        }
    }
    out
}

/// What a chord of an action runs in a pane: Shepherd's action, or herdr's action Shepherd lacks (caught, said).
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    App(&'static str),
    HerdrOnly(String),
}

/// Every chord label of the actions that is bound in a pane, with what it runs; an indexed binding is one label
/// for each digit. A label herdr reads only in its own modes, and a bare key (typing) is none.
pub fn targets(actions: &[Action]) -> Vec<(Target, String)> {
    let mut out = Vec::new();
    for action in actions {
        for label in &action.labels {
            match action.runs {
                Runs::Prefix | Runs::Local => {}
                _ if !bindable(label) => {}
                Runs::App(app) => out.push((Target::App(app), label.clone())),
                Runs::HerdrOnly => out.push((Target::HerdrOnly(action.name.clone()), label.clone())),
                Runs::Nine(apps) => {
                    if label.contains(INDEXED) {
                        for (i, app) in apps.iter().enumerate() {
                            out.push((Target::App(app), label.replace(INDEXED, &(i + 1).to_string())));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Whether a label can be bound in a pane: after the prefix any key, else it needs a modifier or to be a function
/// key (a bare letter is typing).
pub fn bindable(label: &str) -> bool {
    let label = label.trim();
    if label.starts_with("prefix+") {
        return true;
    }
    let mut parts: Vec<&str> = label.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    // a key that is `+` itself is spelled `plus`; "ctrl++" splits to an empty last part
    let key = if key.is_empty() { "+" } else { key };
    let modified = parts.iter().any(|m| {
        matches!(m.to_ascii_lowercase().as_str(), "ctrl" | "control" | "alt" | "option" | "meta" | "cmd" | "command" | "super")
    });
    modified || (key.len() > 1 && key.starts_with(['f', 'F']) && key[1..].parse::<u8>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../../tests/fixtures/herdr-0.9.5-default-keys.toml");

    fn config(toml: &str) -> HerdrConfig {
        HerdrConfig(toml.parse().unwrap())
    }

    fn find<'a>(actions: &'a [Action], name: &str) -> &'a Action {
        actions.iter().find(|a| a.name == name).unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn the_real_default_config_parses_to_every_action() {
        let defaults = parse_default_config(REAL);
        let get = |name: &str| defaults.iter().find(|(n, _)| n == name).map(|(_, l)| l.clone());
        assert_eq!(get("prefix"), Some(vec!["ctrl+b".into()]));
        assert_eq!(get("new_tab"), Some(vec!["prefix+c".into()]));
        assert_eq!(get("split_horizontal"), Some(vec!["prefix+minus".into()]));
        assert_eq!(get("switch_tab"), Some(vec!["prefix+1..9".into()]));
        assert_eq!(get("navigate_pane_left"), Some(vec!["h".into()]));
        assert_eq!(get("open_worktree"), Some(vec![]), "unset by default: present, no chord");
        assert_eq!(get("clear_pane"), Some(vec![]));
        assert_eq!(get("cycle_pane_previous"), Some(vec!["prefix+shift+tab".into()]));
        // 0.9.5 lists these 55 names (the prefix, the actions, the navigate keys); none of the example command's keys
        assert_eq!(defaults.len(), 55, "{:?}", defaults.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>());
        for skipped in ["type", "key", "command", "width", "height", "tabs", "workspaces", "agents"] {
            assert!(get(skipped).is_none(), "{skipped} belongs to the example command");
        }
    }

    #[test]
    fn every_action_in_the_real_default_config_is_in_the_table() {
        let missing: Vec<_> = parse_default_config(REAL).into_iter().map(|(n, _)| n).filter(|n| !KNOWN.iter().any(|k| k.name == n)).collect();
        assert_eq!(missing, Vec::<String>::new(), "a herdr action the table has no label or Shepherd action for");
    }

    #[test]
    fn the_built_in_defaults_are_the_real_ones() {
        let real = parse_default_config(REAL);
        for (name, labels) in &real {
            let built = builtin_defaults().into_iter().find(|(n, _)| n == name).unwrap().1;
            assert_eq!(&built, labels, "{name}");
        }
    }

    #[test]
    fn a_list_and_a_trailing_comment_are_read() {
        let text = "[keys]\n# a = [\"x\", \"y+z\"]  # note\n# b = \"\"   # optional\n# c = 5\n# Action bindings use explicit syntax: \"prefix+n\" requires\n[server]\n# d = \"q\"\n";
        assert_eq!(parse_default_config(text), vec![("a".to_string(), vec!["x".to_string(), "y+z".to_string()]), ("b".to_string(), vec![])]);
        assert_eq!(parse_default_config("[ui]\n# a = \"x\"\n"), Vec::new(), "only [keys]");
        assert_eq!(parse_default_config("[keys]\na = \"x\"\n"), vec![("a".to_string(), vec!["x".to_string()])], "an uncommented line too");
    }

    #[test]
    fn the_commented_example_tables_end_the_actions() {
        let text = "[keys]\n# a = \"x\"\n# [[keys.command]]\n# zzz = \"y\"\n# [keys.indexed]\n# yyy = \"q\"\n";
        assert_eq!(parse_default_config(text), vec![("a".to_string(), vec!["x".to_string()])]);
    }

    #[test]
    fn the_configs_chords_replace_the_defaults_and_empty_unbinds() {
        let defaults = builtin_defaults();
        let a = actions(&defaults, &config("[keys]\nnew_tab = [\"prefix+t\", \"alt+t\"]\nclose_tab = \"\"\n"));
        assert_eq!(find(&a, "new_tab").labels, ["prefix+t", "alt+t"]);
        assert!(find(&a, "new_tab").set);
        assert_eq!(find(&a, "new_tab").default, ["prefix+c"]);
        assert!(find(&a, "close_tab").labels.is_empty() && find(&a, "close_tab").set, "empty unbinds");
        assert_eq!(find(&a, "next_tab").labels, ["prefix+n"]);
        assert!(!find(&a, "next_tab").set);
    }

    #[test]
    fn an_action_only_the_config_or_a_newer_herdr_has_is_herdr_only() {
        let mut defaults = builtin_defaults();
        defaults.push(("brand_new".into(), vec!["prefix+9x".into()]));
        let a = actions(&defaults, &config("[keys]\nfuture_thing = \"ctrl+alt+f\"\n\n[[keys.command]]\nkey = \"prefix+t\"\ncommand = \"x\"\n"));
        let new = find(&a, "brand_new");
        assert_eq!((new.runs, new.known, new.label.as_str()), (Runs::HerdrOnly, false, "Brand new"));
        assert_eq!(find(&a, "future_thing").labels, ["ctrl+alt+f"]);
        assert!(a.iter().all(|x| x.name != "command"), "a custom command is no action");
    }

    #[test]
    fn prefix_and_extra_prefixes_are_the_prefixes() {
        let a = actions(&builtin_defaults(), &config(""));
        assert_eq!(prefixes(&a), ["ctrl+b"]);
        let a = actions(&builtin_defaults(), &config("[keys]\nprefix = \"super+space\"\nextra_prefixes = [\"f12\", \"super+space\"]\n"));
        assert_eq!(prefixes(&a), ["super+space", "f12"]);
    }

    #[test]
    fn targets_expand_digits_and_leave_modes_alone() {
        let a = actions(&builtin_defaults(), &config("[keys]\nswitch_tab = [\"prefix+1..9\", \"alt+1..9\"]\ndetach = \"super+q\"\nfocus_agent = \"prefix+alt+1..9\"\nnavigate_pane_left = \"ctrl+h\"\nnew_tab = \"t\"\n"));
        let t = targets(&a);
        let has = |target: Target, label: &str| t.iter().any(|(x, l)| *x == target && l == label);
        assert!(has(Target::App("Tab3"), "prefix+3") && has(Target::App("Tab9"), "alt+9"));
        assert!(has(Target::App("Agent2"), "prefix+alt+2"));
        assert!(has(Target::HerdrOnly("detach".into()), "super+q"));
        assert!(!t.iter().any(|(_, l)| l == "ctrl+h"), "navigate mode's keys are not bound in a pane");
        assert!(!t.iter().any(|(_, l)| l == "t"), "a bare key is typing");
        assert!(!t.iter().any(|(_, l)| l.contains("1..9")), "no unexpanded range");
        assert!(has(Target::App("NewWorkspace"), "prefix+shift+n"), "defaults stay");
    }

    #[test]
    fn a_label_is_bindable_with_a_modifier_a_prefix_or_as_a_function_key() {
        for ok in ["prefix+c", "ctrl+b", "alt+enter", "super+space", "cmd+k", "f5", "F12", "ctrl++", "ctrl+alt+shift+left"] {
            assert!(bindable(ok), "{ok}");
        }
        for typing in ["c", "h", "up", "shift+k", "enter", "tab", ""] {
            assert!(!bindable(typing), "{typing}");
        }
    }
}
