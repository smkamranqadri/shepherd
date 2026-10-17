//! App actions and their key bindings: defaults (Ghostty-like) overlaid by the user's settings.
use gpui::{App, KeyBinding, Keystroke, NoAction};
use std::collections::BTreeMap;

macro_rules! app_actions {
    ($($name:ident: $label:literal),+ $(,)?) => {
        gpui::actions!(shepherd, [$($name),+]);

        /// Every app action as (name in settings, label in the Keys settings).
        pub const ACTIONS: &[(&str, &str)] = &[$((stringify!($name), $label)),+];

        fn binding(action: &str, keystroke: &str, context: Option<&str>) -> Option<KeyBinding> {
            match action {
                $(stringify!($name) => Some(KeyBinding::new(keystroke, $name, context)),)+
                _ => None,
            }
        }
    };
}

app_actions! {
    NewTab: "New tab",
    CloseTab: "Close tab",
    NewWorkspace: "New workspace",
    SplitRight: "Split right",
    SplitDown: "Split down",
    ClosePane: "Close pane",
    FocusLeft: "Focus pane left",
    FocusRight: "Focus pane right",
    FocusUp: "Focus pane up",
    FocusDown: "Focus pane down",
    ToggleZoom: "Zoom pane",
    Tab1: "Go to tab 1",
    Tab2: "Go to tab 2",
    Tab3: "Go to tab 3",
    Tab4: "Go to tab 4",
    Tab5: "Go to tab 5",
    Tab6: "Go to tab 6",
    Tab7: "Go to tab 7",
    Tab8: "Go to tab 8",
    Tab9: "Go to tab 9",
    PreviousTab: "Previous tab",
    NextTab: "Next tab",
    PreviousWorkspace: "Previous workspace",
    NextWorkspace: "Next workspace",
    PreviousSession: "Previous session",
    NextSession: "Next session",
    SwitchSession: "Switch session",
    ToggleSessionCollapsed: "Fold or unfold the session",
    Workspace1: "Go to workspace 1",
    Workspace2: "Go to workspace 2",
    Workspace3: "Go to workspace 3",
    Workspace4: "Go to workspace 4",
    Workspace5: "Go to workspace 5",
    Workspace6: "Go to workspace 6",
    Workspace7: "Go to workspace 7",
    Workspace8: "Go to workspace 8",
    Workspace9: "Go to workspace 9",
    ToggleSidebar: "Toggle sidebar",
    ToggleReview: "Toggle right panel",
    ShowFiles: "Files",
    ShowChanges: "Changes",
    ShowSearch: "Search in files",
    ShowPullRequest: "Pull request",
    QuickOpen: "Go to file",
    RevealActiveFile: "Reveal file in sidebar",
    ChooseRoot: "Choose sidebar folder",
    FontSizeUp: "Bigger text",
    FontSizeDown: "Smaller text",
    FontSizeReset: "Default text size",
    OpenSettings: "Settings",
    ChangeTheme: "Change theme",
    ToggleSecureInput: "Secure keyboard entry",
    CommandPanel: "Command panel",
    AgentsPanel: "Agents",
    NextNeedsYou: "Next agent that needs you",
    WorkspacesPanel: "Workspaces",
    SaveFile: "Save file",
    SaveAllFiles: "Save all files",
    ShowShortcuts: "Keyboard shortcuts",
    Quit: "Quit",
}

/// Where the editor's own chords are bound: its text, inside a file tab's view. A pane's keys are in "Terminal", so
/// Ctrl+Shift+Z and the rest stay the shell's there.
pub const EDITOR: &str = "FileEditor > Input";
/// Where the save chords are bound: anywhere in a file tab's view (its text and a markdown preview alike). A pane's
/// keys are in "Terminal", so Ctrl+S (XOFF) stays the shell's there.
pub const FILE_VIEW: &str = "FileEditor";
/// Where an app chord that is also one of gpui-base's text chords is bound: everywhere but in text. GPUI ranks a
/// binding without a context as deep as the deepest, and Shepherd's are bound after gpui-base's, so bound everywhere
/// it would win in the editor (Cmd+Shift+Up would switch workspace, not select to the start).
pub const NOT_IN_TEXT: &str = "!Input";

/// The key context `action`'s chords are bound in, if it has one of its own: the save actions in a file's view.
pub fn context_of(action: &str) -> Option<&'static str> {
    matches!(action, "SaveFile" | "SaveAllFiles").then_some(FILE_VIEW)
}

/// App actions whose chords are bound in the editor's text as well, over a gpui-base "Input" binding of the same
/// chord (Cmd+Shift+F is its Replace on macOS), so they act the same there as anywhere.
const OVER_THE_EDITOR: &[&str] = &["ShowSearch"];

/// App actions that stay app-wide in the editor's text even over one of its own chords: closing, the sidebar, the
/// command panel, new tab, and moving between tabs (whose hints show while the modifier is held).
const GLOBAL_IN_TEXT: &[&str] = &[
    "ClosePane", "CloseTab", "ToggleSidebar", "CommandPanel", "NewTab", "NextTab", "PreviousTab", "Tab1", "Tab2", "Tab3", "Tab4", "Tab5", "Tab6", "Tab7",
    "Tab8", "Tab9",
];

/// The contexts `action`'s `chord` is bound in, given the chords gpui-base binds in text (`text_chords`).
fn contexts(action: &str, chord: &str, text_chords: &[String]) -> Vec<Option<&'static str>> {
    match context_of(action) {
        Some(context) => vec![Some(context)],
        None if OVER_THE_EDITOR.contains(&action) => vec![None, Some(EDITOR)],
        None if text_chords.iter().any(|k| k == chord) && !GLOBAL_IN_TEXT.contains(&action) => vec![Some(NOT_IN_TEXT)],
        None => vec![None],
    }
}

/// The single-keystroke chords gpui-base binds in its "Input" context (the editor's text, and every other input), in
/// GPUI's spelling.
fn text_chords(cx: &App) -> Vec<String> {
    let input = gpui::KeyBindingContextPredicate::parse("Input").ok();
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap
        .bindings()
        .filter(|b| b.predicate().as_deref() == input.as_ref() && b.keystrokes().len() == 1)
        .map(|b| b.keystrokes()[0].inner().unparse())
        .collect()
}

/// The editor's chords that are not app actions, bound once after gpui-base's: Ctrl+Shift+Z redoes on Linux too.
pub fn bind_editor_keys(cx: &mut App) {
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("ctrl-shift-z", gpui_base::input::Redo, Some(EDITOR))]);
    #[cfg(target_os = "macos")]
    let _ = cx;
}

#[cfg(target_os = "macos")]
pub const DEFAULTS: &[(&str, &str)] = &[
    ("NewTab", "cmd-t"),
    ("CloseTab", "cmd-shift-w"),
    ("NewWorkspace", "cmd-shift-n"),
    ("SplitRight", "cmd-d"),
    ("SplitDown", "cmd-shift-d"),
    ("ClosePane", "cmd-w"),
    ("FocusLeft", "cmd-alt-left"),
    ("FocusRight", "cmd-alt-right"),
    ("FocusUp", "cmd-alt-up"),
    ("FocusDown", "cmd-alt-down"),
    ("ToggleZoom", "cmd-shift-enter"),
    ("Tab1", "cmd-1"),
    ("Tab2", "cmd-2"),
    ("Tab3", "cmd-3"),
    ("Tab4", "cmd-4"),
    ("Tab5", "cmd-5"),
    ("Tab6", "cmd-6"),
    ("Tab7", "cmd-7"),
    ("Tab8", "cmd-8"),
    ("Tab9", "cmd-9"),
    ("PreviousTab", "cmd-{"),
    ("NextTab", "cmd-}"),
    // fork-only: Ctrl+Tab steps the tabs as in a browser, from any view (`GLOBAL_IN_TEXT` keeps it over the editor)
    ("PreviousTab", "ctrl-shift-tab"),
    ("NextTab", "ctrl-tab"),
    ("PreviousWorkspace", "cmd-up"),
    ("NextWorkspace", "cmd-down"),
    // the sessions and machines of the sidebar's bar; Cmd+Opt+Up/Down is taken, by focus up and down
    ("PreviousSession", "ctrl-cmd-up"),
    ("NextSession", "ctrl-cmd-down"),
    // fork-only: folds the shown session's workspaces in the sidebar's tree, as a click on its header does
    ("ToggleSessionCollapsed", "cmd-shift-h"),
    // Ctrl, not Cmd+Shift: Cmd+Shift+3, 4 and 5 are macOS's screenshots
    ("Workspace1", "ctrl-1"),
    ("Workspace2", "ctrl-2"),
    ("Workspace3", "ctrl-3"),
    ("Workspace4", "ctrl-4"),
    ("Workspace5", "ctrl-5"),
    ("Workspace6", "ctrl-6"),
    ("Workspace7", "ctrl-7"),
    ("Workspace8", "ctrl-8"),
    ("Workspace9", "ctrl-9"),
    ("ToggleSidebar", "cmd-b"),
    ("ToggleReview", "cmd-alt-b"),
    ("ShowFiles", "cmd-shift-e"),
    // Cmd+Alt, beside the panel's toggle: Cmd+Shift+G is a pane's find previous, and Ctrl+Shift+G a key the pane's
    // app gets
    ("ShowChanges", "cmd-alt-g"),
    ("ShowSearch", "cmd-shift-f"),
    ("QuickOpen", "cmd-p"),
    ("FontSizeUp", "cmd-="),
    ("FontSizeUp", "cmd-+"),
    ("FontSizeDown", "cmd--"),
    ("FontSizeReset", "cmd-0"),
    ("OpenSettings", "cmd-,"),
    ("CommandPanel", "cmd-k"),
    ("CommandPanel", "cmd-shift-p"),
    ("AgentsPanel", "cmd-shift-a"),
    // fork-only: the agent that has waited longest for you, in any session, then the next
    ("NextNeedsYou", "cmd-shift-j"),
    ("SaveFile", "cmd-s"),
    ("SaveAllFiles", "cmd-alt-s"),
    ("Quit", "cmd-q"),
    ("ShowShortcuts", "cmd-/"),
];

/// Off macOS: Ghostty's Linux defaults where it has one. A terminal's Ctrl+letter belongs to the shell, so the app's
/// chords take Ctrl+Shift, and Super belongs to the desktop.
#[cfg(not(target_os = "macos"))]
pub const DEFAULTS: &[(&str, &str)] = &[
    ("NewTab", "ctrl-shift-t"),
    ("CloseTab", "ctrl-alt-w"),
    ("NewWorkspace", "ctrl-shift-n"),
    ("SplitRight", "ctrl-shift-o"),
    ("SplitDown", "ctrl-shift-e"),
    ("ClosePane", "ctrl-shift-w"),
    ("FocusLeft", "ctrl-alt-left"),
    ("FocusRight", "ctrl-alt-right"),
    ("FocusUp", "ctrl-alt-up"),
    ("FocusDown", "ctrl-alt-down"),
    ("ToggleZoom", "ctrl-shift-enter"),
    // fork-only: upstream has the tabs on Alt+1…9; Ctrl+1…9 sit beside the workspaces' Ctrl+Shift+1…9, and holding
    // Ctrl shows the tabs' numbers (`hints`). A terminal's Ctrl+digit is no key a shell or TUI reads (xterm sends the
    // plain digit, or a stray control code for 2–8), and gpui-base's text binds no Ctrl+digit.
    ("Tab1", "ctrl-1"),
    ("Tab2", "ctrl-2"),
    ("Tab3", "ctrl-3"),
    ("Tab4", "ctrl-4"),
    ("Tab5", "ctrl-5"),
    ("Tab6", "ctrl-6"),
    ("Tab7", "ctrl-7"),
    ("Tab8", "ctrl-8"),
    ("Tab9", "ctrl-9"),
    ("PreviousTab", "ctrl-pageup"),
    ("NextTab", "ctrl-pagedown"),
    // fork-only: Ctrl+Tab steps the tabs as in a browser, from a pane, a file, the editor, a diff or a PDF alike
    // (`GLOBAL_IN_TEXT` keeps it over the editor's text, whose Tab indents)
    ("PreviousTab", "ctrl-shift-tab"),
    ("NextTab", "ctrl-tab"),
    // fork-only: upstream has the workspaces on Ctrl+Shift+PageUp/PageDown, which the sessions take here
    ("PreviousWorkspace", "ctrl-shift-up"),
    ("NextWorkspace", "ctrl-shift-down"),
    ("PreviousSession", "ctrl-shift-pageup"),
    ("NextSession", "ctrl-shift-pagedown"),
    // the session switcher, with the next session picked
    ("SwitchSession", "ctrl-shift-s"),
    // fork-only: folds the shown session's workspaces in the sidebar's tree, as a click on its header does. Ctrl+H
    // stays the shell's backspace
    ("ToggleSessionCollapsed", "ctrl-shift-h"),
    // Ctrl+Shift+1…9 as GPUI hears them on Linux: xkb gives the shifted symbol (US layout) and GPUI drops the Shift
    // of a key that has no case, so `ctrl-shift-1` would never match (`shifted_digit` shows them as typed).
    ("Workspace1", "ctrl-!"),
    ("Workspace2", "ctrl-@"),
    ("Workspace3", "ctrl-#"),
    ("Workspace4", "ctrl-$"),
    ("Workspace5", "ctrl-%"),
    ("Workspace6", "ctrl-^"),
    ("Workspace7", "ctrl-&"),
    ("Workspace8", "ctrl-*"),
    ("Workspace9", "ctrl-("),
    ("ToggleSidebar", "ctrl-shift-b"),
    ("ToggleReview", "ctrl-alt-b"),
    // the right panel's chords take Ctrl+Alt, as its toggle does: Ctrl+Shift+E splits down, Ctrl+Shift+F and G are
    // the terminal's find, and Ctrl+letter is the shell's
    ("ShowFiles", "ctrl-alt-e"),
    ("ShowChanges", "ctrl-alt-g"),
    ("ShowSearch", "ctrl-alt-f"),
    // not Ctrl+P: readline's previous line
    ("QuickOpen", "ctrl-alt-p"),
    ("FontSizeUp", "ctrl-="),
    ("FontSizeUp", "ctrl-+"),
    ("FontSizeDown", "ctrl--"),
    ("FontSizeReset", "ctrl-0"),
    ("OpenSettings", "ctrl-,"),
    ("CommandPanel", "ctrl-shift-p"),
    ("AgentsPanel", "ctrl-shift-a"),
    // fork-only: the agent that has waited longest for you, in any session, then the next. Not Ctrl+Shift+U, IBus's
    // Unicode entry
    ("NextNeedsYou", "ctrl-shift-j"),
    // in the editor only (`context_of`): a pane's Ctrl+S is the terminal's
    ("SaveFile", "ctrl-s"),
    ("SaveAllFiles", "ctrl-alt-s"),
    ("Quit", "ctrl-shift-q"),
    // Ctrl+Shift+/ as a US layout reports it: the shifted key, without shift. Ctrl+/ stays the terminal's.
    ("ShowShortcuts", "ctrl-?"),
];

/// The Tab1–9 actions, in order: `TAB_ACTIONS[i]` goes to the strip's `i`th tab.
pub const TAB_ACTIONS: [&str; 9] = ["Tab1", "Tab2", "Tab3", "Tab4", "Tab5", "Tab6", "Tab7", "Tab8", "Tab9"];

/// The Workspace1–9 actions, in order: `WORKSPACE_ACTIONS[i]` goes to the sidebar's `i`th workspace.
pub const WORKSPACE_ACTIONS: [&str; 9] =
    ["Workspace1", "Workspace2", "Workspace3", "Workspace4", "Workspace5", "Workspace6", "Workspace7", "Workspace8", "Workspace9"];

/// The digit a US layout types `key` with Shift: Linux hears Ctrl+Shift+1 as `ctrl-!` (see the defaults), which
/// reads better as the keys pressed.
pub fn shifted_digit(key: &str) -> Option<char> {
    let at = ["!", "@", "#", "$", "%", "^", "&", "*", "(", ")"].iter().position(|k| *k == key)?;
    char::from_digit((at as u32 + 1) % 10, 10)
}

/// The chord bound to `action` now, if any.
pub fn chord_of(cx: &App, action: &str) -> Option<String> {
    cx.try_global::<ActiveBindings>()?.0.iter().find(|(a, _)| a == action).map(|(_, k)| k.clone())
}

fn known(action: &str) -> bool {
    ACTIONS.iter().any(|(name, _)| *name == action)
}

/// The chord in GPUI's own spelling (`alt-cmd-left`), so the same keys written two ways compare equal.
pub fn normalize(chord: &str) -> Option<String> {
    let keys: Option<Vec<String>> = chord.split_whitespace().map(|k| Keystroke::parse(k).ok().map(|k| k.unparse())).collect();
    keys.filter(|k| !k.is_empty()).map(|k| k.join(" "))
}

/// A keystroke worth binding: with cmd, ctrl, alt or fn, or a function key. A bare letter would be taken from
/// every terminal.
pub fn recordable(keystroke: &Keystroke) -> bool {
    let m = &keystroke.modifiers;
    m.platform || m.control || m.alt || m.function || (keystroke.key.starts_with('f') && keystroke.key[1..].parse::<u8>().is_ok())
}

/// A chord GPUI can bind: space-separated keystrokes that all parse.
pub fn valid_chord(chord: &str) -> bool {
    !chord.trim().is_empty() && chord.split_whitespace().all(|k| Keystroke::parse(k).is_ok())
}

/// The bindings in force: the user's chords first, then defaults for actions the user did not set, never two
/// actions on one chord (the user's wins). An empty user chord unbinds its action.
pub fn effective(user: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (action, chord) in user {
        if let (true, Some(chord)) = (known(action), normalize(chord)) {
            out.retain(|(_, k)| *k != chord);
            out.push((action.clone(), chord));
        }
    }
    for (action, chord) in DEFAULTS {
        let chord = normalize(chord).expect("defaults parse");
        if !user.contains_key(*action) && !out.iter().any(|(_, k)| *k == chord) {
            out.push((action.to_string(), chord));
        }
    }
    out
}

/// The bindings currently applied, so a settings change can unbind what it replaces.
pub struct ActiveBindings(pub Vec<(String, String)>);

impl gpui::Global for ActiveBindings {}

/// Applies the bindings `user` asks for, replacing the ones in force.
pub fn rebind(cx: &mut App, user: &BTreeMap<String, String>) {
    let previous = cx.try_global::<ActiveBindings>().map(|a| a.0.clone()).unwrap_or_default();
    let next = effective(user);
    apply(cx, &previous, &next);
    rebind_herdr(cx, &next);
    cx.set_global(ActiveBindings(next));
    // macOS copies menu shortcuts when the menu is set: set it again so it shows (and only fires) the new ones.
    set_menus(cx);
}

/// Sets the menu bar again, with Secure Keyboard Entry's check as the settings have it.
pub fn set_menus(cx: &mut App) {
    let secure = cx.try_global::<crate::settings::SettingsGlobal>().is_some_and(|s| s.settings.secure_input);
    cx.set_menus(menus(secure));
}

// Window-management items macOS apps carry; not rebindable, so outside `app_actions!`.
gpui::actions!(shepherd, [Hide, HideOthers, ShowAll, Minimize, Zoom, ToggleFullScreen]);

// A PDF tab's zoom, in its view only (`PDF_VIEW`); not rebindable, so outside `app_actions!`.
gpui::actions!(shepherd, [PdfZoomIn, PdfZoomOut, PdfZoomReset]);

/// Where a PDF tab's zoom chords are bound: its pages' view, which has the keyboard while the tab shows.
pub const PDF_VIEW: &str = "PdfView";

/// A PDF tab's zoom: Cmd/Ctrl with +, − and 0, the text size's chords everywhere else. Bound after the app's
/// (`apply`), so in the PDF view, where both match as deep, these win; anywhere else the app's stay.
fn pdf_keys() -> Vec<KeyBinding> {
    let primary = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    let chord = |key: &str| format!("{primary}-{key}");
    vec![
        KeyBinding::new(&chord("="), PdfZoomIn, Some(PDF_VIEW)),
        KeyBinding::new(&chord("+"), PdfZoomIn, Some(PDF_VIEW)),
        KeyBinding::new(&chord("-"), PdfZoomOut, Some(PDF_VIEW)),
        KeyBinding::new(&chord("0"), PdfZoomReset, Some(PDF_VIEW)),
    ]
}

fn menus(secure_input: bool) -> Vec<gpui::Menu> {
    use gpui::{Menu, MenuItem as I, SystemMenuType};
    vec![
        Menu::new("Shepherd").items([
            I::action("Settings…", OpenSettings),
            I::action("Secure Keyboard Entry", ToggleSecureInput).checked(secure_input),
            I::separator(),
            I::os_submenu("Services", SystemMenuType::Services),
            I::separator(),
            I::action("Hide Shepherd", Hide),
            I::action("Hide Others", HideOthers),
            I::action("Show All", ShowAll),
            I::separator(),
            I::action("Quit Shepherd", Quit),
        ]),
        Menu::new("Shell").items([
            I::action("New Tab", NewTab),
            I::action("New Workspace", NewWorkspace),
            I::separator(),
            I::action("Split Right", SplitRight),
            I::action("Split Down", SplitDown),
            I::separator(),
            I::action("Close Pane", ClosePane),
            I::action("Close Tab", CloseTab),
        ]),
        Menu::new("View").items([
            I::action("Toggle Sidebar", ToggleSidebar),
            I::action("Toggle Right Panel", ToggleReview),
            I::action("Zoom Pane", ToggleZoom),
            I::separator(),
            I::action("Files", ShowFiles),
            I::action("Changes", ShowChanges),
            I::action("Search in Files", ShowSearch),
            I::action("Go to File", QuickOpen),
            I::separator(),
            I::action("Command Panel", CommandPanel),
            I::action("Agents", AgentsPanel),
            I::action("Workspaces", WorkspacesPanel),
            I::action("Keyboard Shortcuts", ShowShortcuts),
            I::separator(),
            I::action("Enter Full Screen", ToggleFullScreen),
        ]),
        Menu::new("Window").items([
            I::action("Minimize", Minimize),
            I::action("Zoom", Zoom),
            I::separator(),
            I::action("Previous Tab", PreviousTab),
            I::action("Next Tab", NextTab),
            I::action("Previous Workspace", PreviousWorkspace),
            I::action("Next Workspace", NextWorkspace),
            I::action("Previous Session", PreviousSession),
            I::action("Next Session", NextSession),
            I::action("Switch Session…", SwitchSession),
            I::action("Fold Session", ToggleSessionCollapsed),
            I::action("Next Agent That Needs You", NextNeedsYou),
            I::submenu(Menu::new("Go to Workspace").items([
                I::action("Workspace 1", Workspace1),
                I::action("Workspace 2", Workspace2),
                I::action("Workspace 3", Workspace3),
                I::action("Workspace 4", Workspace4),
                I::action("Workspace 5", Workspace5),
                I::action("Workspace 6", Workspace6),
                I::action("Workspace 7", Workspace7),
                I::action("Workspace 8", Workspace8),
                I::action("Workspace 9", Workspace9),
            ])),
            I::separator(),
            I::action("Focus Pane Left", FocusLeft),
            I::action("Focus Pane Right", FocusRight),
            I::action("Focus Pane Up", FocusUp),
            I::action("Focus Pane Down", FocusDown),
        ]),
    ]
}

/// Handlers for the window-management menu items.
pub fn init_menu_actions(cx: &mut App) {
    fn window(cx: &mut App, f: fn(&mut gpui::Window)) {
        if let Some(w) = cx.active_window() {
            let _ = w.update(cx, |_, window, _| f(window));
        }
    }
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| window(cx, |w| w.toggle_fullscreen()));
}

/// Each binding of `bound` as (chord, context, action), in every context its action binds in.
fn placed<'a>(bound: &'a [(String, String)], text_chords: &[String]) -> Vec<(&'a str, Option<&'static str>, &'a str)> {
    bound.iter().flat_map(|(action, chord)| contexts(action, chord, text_chords).into_iter().map(move |c| (chord.as_str(), c, action.as_str()))).collect()
}

/// Binds `next`; chords of `previous` that `next` no longer binds in the same context are bound to `NoAction` there
/// (a later binding wins in GPUI), so other bindings, such as gpui-base's input keys, are left alone.
pub fn apply(cx: &mut App, previous: &[(String, String)], next: &[(String, String)]) {
    let text = text_chords(cx);
    let (before, after) = (placed(previous, &text), placed(next, &text));
    let mut bindings: Vec<KeyBinding> = before
        .iter()
        .filter(|(chord, context, _)| !after.iter().any(|(k, c, _)| k == chord && c == context))
        .map(|(chord, context, _)| KeyBinding::new(chord, NoAction, *context))
        .collect();
    bindings.extend(after.iter().filter_map(|(chord, context, action)| binding(action, chord, *context)));
    bindings.extend(pdf_keys());
    cx.bind_keys(bindings);
}

/// The chords of herdr's own keymap bound now, so the next rebind can unbind what it drops.
struct HerdrBindings(Vec<String>);

impl gpui::Global for HerdrBindings {}

/// The key context herdr's chords are bound in: herdr's keymap is for its panes, and a prefix waiting for its
/// second key would get in the way of typing elsewhere.
const TERMINAL: &str = "Terminal";

/// Binds herdr's keymap from its config.toml (`[keys]`, `[[keys.command]]`) in the panes, under Shepherd's
/// own: a chord `shepherd` uses stays Shepherd's.
fn rebind_herdr(cx: &mut App, shepherd: &[(String, String)]) {
    use crate::herdr::keys::Bind;
    // ponytail: read on rebind (start, Settings), not watched; a config.toml edit takes a restart
    let config = crate::herdr::HerdrConfig::load(&crate::herdr::config_path()).unwrap_or_default();
    let next: Vec<_> = crate::herdr::keys::bindings(&config)
        .into_iter()
        .filter(|(_, chord)| !shepherd.iter().any(|(_, k)| k == chord))
        .collect();
    let previous = cx.try_global::<HerdrBindings>().map(|b| b.0.clone()).unwrap_or_default();
    let mut bindings: Vec<KeyBinding> = previous
        .iter()
        .filter(|chord| !next.iter().any(|(_, k)| k == *chord))
        .map(|chord| KeyBinding::new(chord, NoAction, Some(TERMINAL)))
        .collect();
    bindings.extend(next.iter().filter_map(|(bind, chord)| match bind {
        Bind::App(action) => binding(action, chord, Some(TERMINAL)),
        Bind::Command(command) => Some(KeyBinding::new(chord, command.clone(), Some(TERMINAL))),
    }));
    cx.bind_keys(bindings);
    cx.set_global(HerdrBindings(next.into_iter().map(|(_, chord)| chord).collect()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn user(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(a, k)| (a.to_string(), k.to_string())).collect()
    }

    fn chord<'a>(eff: &'a [(String, String)], action: &str) -> Option<&'a str> {
        eff.iter().find(|(a, _)| a == action).map(|(_, k)| k.as_str())
    }

    /// Upstream's defaults (freethinkel/shepherd v1.3.0), which the sidebar's actions never change: only its own
    /// actions gain chords.
    #[cfg(target_os = "macos")]
    const UPSTREAM: &[(&str, &str)] = &[
        ("NewTab", "cmd-t"),
        ("CloseTab", "cmd-shift-w"),
        ("NewWorkspace", "cmd-shift-n"),
        ("SplitRight", "cmd-d"),
        ("SplitDown", "cmd-shift-d"),
        ("ClosePane", "cmd-w"),
        ("FocusLeft", "cmd-alt-left"),
        ("FocusRight", "cmd-alt-right"),
        ("FocusUp", "cmd-alt-up"),
        ("FocusDown", "cmd-alt-down"),
        ("ToggleZoom", "cmd-shift-enter"),
        ("Tab1", "cmd-1"),
        ("Tab2", "cmd-2"),
        ("Tab3", "cmd-3"),
        ("Tab4", "cmd-4"),
        ("Tab5", "cmd-5"),
        ("Tab6", "cmd-6"),
        ("Tab7", "cmd-7"),
        ("Tab8", "cmd-8"),
        ("Tab9", "cmd-9"),
        ("PreviousTab", "cmd-{"),
        ("NextTab", "cmd-}"),
        ("PreviousWorkspace", "cmd-up"),
        ("NextWorkspace", "cmd-down"),
        ("ToggleSidebar", "cmd-b"),
        ("ToggleReview", "cmd-alt-b"),
        ("FontSizeUp", "cmd-="),
        ("FontSizeUp", "cmd-+"),
        ("FontSizeDown", "cmd--"),
        ("FontSizeReset", "cmd-0"),
        ("OpenSettings", "cmd-,"),
        ("CommandPanel", "cmd-k"),
        ("CommandPanel", "cmd-shift-p"),
        ("AgentsPanel", "cmd-shift-a"),
        ("Quit", "cmd-q"),
    ];
    #[cfg(not(target_os = "macos"))]
    const UPSTREAM: &[(&str, &str)] = &[
        ("NewTab", "ctrl-shift-t"),
        ("CloseTab", "ctrl-alt-w"),
        ("NewWorkspace", "ctrl-shift-n"),
        ("SplitRight", "ctrl-shift-o"),
        ("SplitDown", "ctrl-shift-e"),
        ("ClosePane", "ctrl-shift-w"),
        ("FocusLeft", "ctrl-alt-left"),
        ("FocusRight", "ctrl-alt-right"),
        ("FocusUp", "ctrl-alt-up"),
        ("FocusDown", "ctrl-alt-down"),
        ("ToggleZoom", "ctrl-shift-enter"),
        ("Tab1", "alt-1"),
        ("Tab2", "alt-2"),
        ("Tab3", "alt-3"),
        ("Tab4", "alt-4"),
        ("Tab5", "alt-5"),
        ("Tab6", "alt-6"),
        ("Tab7", "alt-7"),
        ("Tab8", "alt-8"),
        ("Tab9", "alt-9"),
        ("PreviousTab", "ctrl-pageup"),
        ("NextTab", "ctrl-pagedown"),
        ("PreviousWorkspace", "ctrl-shift-pageup"),
        ("NextWorkspace", "ctrl-shift-pagedown"),
        ("ToggleSidebar", "ctrl-shift-b"),
        ("ToggleReview", "ctrl-alt-b"),
        ("FontSizeUp", "ctrl-="),
        ("FontSizeUp", "ctrl-+"),
        ("FontSizeDown", "ctrl--"),
        ("FontSizeReset", "ctrl-0"),
        ("OpenSettings", "ctrl-,"),
        ("CommandPanel", "ctrl-shift-p"),
        ("AgentsPanel", "ctrl-shift-a"),
        ("Quit", "ctrl-shift-q"),
    ];

    #[test]
    fn upstream_actions_keep_upstreams_chords() {
        let upstream = |action: &str| UPSTREAM.iter().any(|(a, _)| *a == action);
        let ours: Vec<_> = DEFAULTS.iter().filter(|d| upstream(d.0) && !FORK_ADDED.contains(d)).collect();
        assert_eq!(ours.len(), UPSTREAM.len(), "the same number of upstream chords");
        for added in FORK_ADDED {
            assert!(DEFAULTS.contains(added), "{added:?}");
        }
        for (action, chord) in UPSTREAM {
            // but for the fork's deliberate overrides
            let chord = FORK_OVERRIDES.iter().find(|(a, _)| a == action).map_or(chord, |(_, c)| c);
            assert!(DEFAULTS.contains(&(*action, *chord)), "{action} keeps {chord}");
        }
        // and every upstream action is still an action
        for (action, _) in UPSTREAM {
            assert!(known(action), "{action}");
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn linux_defaults_leave_the_terminal_its_chords() {
        let eff = effective(&user(&[]));
        for key in ["c", "v", "f", "g"] {
            let taken = normalize(&format!("ctrl-shift-{key}")).unwrap();
            assert!(!eff.iter().any(|(_, k)| *k == taken), "ctrl-shift-{key} is the terminal's");
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn linux_defaults_leave_readline_its_ctrl_letters() {
        let eff = effective(&user(&[]));
        // every Ctrl+letter is the shell's (readline's, ^C, ^D, ^W…); an editor-only chord never reaches a pane
        // (`editor_chords_never_fire_in_a_pane`)
        for key in 'a'..='z' {
            let taken = normalize(&format!("ctrl-{key}")).unwrap();
            assert!(!eff.iter().any(|(a, k)| *k == taken && context_of(a).is_none()), "ctrl-{key} is the shell's");
        }
        assert_eq!(chord(&eff, "QuickOpen"), normalize("ctrl-alt-p").as_deref());
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn linux_right_panel_chords_are_ctrl_alt_beside_its_toggle() {
        let eff = effective(&user(&[]));
        for (action, keys) in [
            ("ToggleReview", "ctrl-alt-b"),
            ("ShowFiles", "ctrl-alt-e"),
            ("ShowChanges", "ctrl-alt-g"),
            ("ShowSearch", "ctrl-alt-f"),
            ("QuickOpen", "ctrl-alt-p"),
        ] {
            assert_eq!(chord(&eff, action), normalize(keys).as_deref(), "{action}");
        }
        // Ghostty's own beside them: new tab, split down, close pane, and the sidebar's toggle
        for (action, keys) in [("NewTab", "ctrl-shift-t"), ("SplitDown", "ctrl-shift-e"), ("ClosePane", "ctrl-shift-w"), ("ToggleSidebar", "ctrl-shift-b")] {
            assert_eq!(chord(&eff, action), normalize(keys).as_deref(), "{action}");
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn linux_tabs_are_ctrl_digits_and_page_keys_and_ctrl_tab() {
        let eff = effective(&user(&[]));
        for (i, action) in TAB_ACTIONS.iter().enumerate() {
            assert_eq!(chord(&eff, action), normalize(&format!("ctrl-{}", i + 1)).as_deref(), "{action}");
            // nothing else on the digit: not a workspace, not the text size's Ctrl+0
            let taken = normalize(&format!("ctrl-{}", i + 1)).unwrap();
            assert_eq!(eff.iter().filter(|(_, k)| *k == taken).count(), 1, "ctrl-{}", i + 1);
        }
        let bound = |action: &str, keys: &str| eff.iter().any(|(a, k)| a == action && Some(k.as_str()) == normalize(keys).as_deref());
        assert!(bound("NextTab", "ctrl-pagedown") && bound("PreviousTab", "ctrl-pageup"));
        assert!(bound("NextTab", "ctrl-tab") && bound("PreviousTab", "ctrl-shift-tab"));
        // Alt+digit is free again (upstream's tabs): nothing binds it
        for i in 1..=9 {
            let alt = normalize(&format!("alt-{i}")).unwrap();
            assert!(!eff.iter().any(|(_, k)| *k == alt), "alt-{i}");
        }
        // Ctrl+1 as GPUI's Linux backend reports it fires the first tab, and Ctrl+Shift+1 the first workspace
        let ctrl_1 = gpui::Keystroke { modifiers: gpui::Modifiers::control(), key: "1".into(), key_char: Some("1".into()) };
        assert_eq!(fired(&ctrl_1), ["Tab1"]);
        assert_eq!(fired(&from_xkb("!", true, Some("!"))), ["Workspace1"]);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_tabs_step_on_ctrl_tab_too() {
        let eff = effective(&user(&[]));
        let bound = |action: &str, keys: &str| eff.iter().any(|(a, k)| a == action && Some(k.as_str()) == normalize(keys).as_deref());
        assert!(bound("NextTab", "cmd-}") && bound("PreviousTab", "cmd-{"));
        assert!(bound("NextTab", "ctrl-tab") && bound("PreviousTab", "ctrl-shift-tab"));
    }

    /// The tab chords fire in every view a tab can show: a pane, a file viewer, the editor's text, a diff and a PDF.
    /// A keymap in `apply`'s order, over gpui-base's Input bindings (its Tab and Shift+Tab indent and outdent).
    #[test]
    fn the_tab_chords_fire_from_every_view() {
        let mut text = text_chords_here();
        text.extend(["tab", "shift-tab"].map(|c| normalize(c).unwrap()));
        let eff = effective(&user(&[]));
        let mut keymap = gpui::Keymap::new(text.iter().map(|c| KeyBinding::new(c, gpui_base::input::SelectToStart, Some("Input"))).collect());
        keymap.add_bindings(placed(&eff, &text).into_iter().filter_map(|(chord, context, action)| binding(action, chord, context)));
        keymap.add_bindings(pdf_keys());
        let views = [
            stack(&["Workspace", "Terminal"]),
            stack(&["Workspace", "FileEditor", "FileViewer"]),
            stack(&["Workspace", "FileEditor", "Input"]),
            stack(&["Workspace", "DiffView"]),
            stack(&["Workspace", "FileEditor", "FileViewer", PDF_VIEW]),
        ];
        let mut chords = vec![("NextTab", "ctrl-tab".to_string()), ("PreviousTab", "ctrl-shift-tab".to_string())];
        chords.extend(TAB_ACTIONS.iter().map(|a| (*a, chord(&eff, a).unwrap().to_string())));
        for (action, keys) in chords {
            let typed = gpui::Keystroke::parse(&keys).unwrap();
            for at in &views {
                let fired = keymap.bindings_for_input(std::slice::from_ref(&typed), at).0.first().map(|b| b.action().name().to_string());
                assert_eq!(fired.as_deref(), Some(format!("shepherd::{action}").as_str()), "{keys} in {at:?}");
            }
        }
        // and the editor's Tab still indents
        let tab = gpui::Keystroke::parse("tab").unwrap();
        let fired = keymap.bindings_for_input(&[tab], &views[2]).0.first().map(|b| b.action().name().to_string());
        assert_eq!(fired.as_deref(), Some("input::SelectToStart"));
    }

    fn stack(names: &[&str]) -> Vec<gpui::KeyContext> {
        names.iter().map(|n| gpui::KeyContext::parse(n).unwrap()).collect()
    }

    #[test]
    fn editor_actions_bind_in_the_editor_only() {
        for (action, _) in ACTIONS {
            let editor = matches!(*action, "SaveFile" | "SaveAllFiles");
            assert_eq!(context_of(action), editor.then_some(FILE_VIEW), "{action}");
        }
        let view = gpui::KeyBindingContextPredicate::parse(FILE_VIEW).unwrap();
        assert!(view.depth_of(&stack(&["FileEditor", "Input"])).is_some(), "the editor's text");
        assert!(view.depth_of(&stack(&["FileEditor"])).is_some(), "a markdown preview");
        assert!(view.depth_of(&stack(&["Terminal"])).is_none(), "a pane");
        assert!(view.depth_of(&stack(&["Input"])).is_none(), "another input (a rename, the command panel)");
        let editor = gpui::KeyBindingContextPredicate::parse(EDITOR).unwrap();
        assert!(editor.eval(&stack(&["FileEditor", "Input"])));
        assert!(!editor.eval(&stack(&["FileEditor"])));
        // the default chords exist and parse
        let eff = effective(&user(&[]));
        assert!(chord(&eff, "SaveFile").is_some() && chord(&eff, "SaveAllFiles").is_some());
        // ShowSearch is bound over the editor's own chord as well as everywhere
        let text = text_chords_here();
        assert_eq!(contexts("ShowSearch", "x", &text), [None, Some(EDITOR)]);
        assert_eq!(contexts("SaveFile", "x", &text), [Some(FILE_VIEW)]);
        assert_eq!(contexts("NewTab", "x", &text), [None]);
    }

    /// gpui-base's text chords (its `input::init`) that Shepherd's defaults could meet, as GPUI spells them.
    fn text_chords_here() -> Vec<String> {
        #[cfg(target_os = "macos")]
        let chords = ["cmd-a", "cmd-c", "cmd-x", "cmd-v", "cmd-z", "cmd-shift-z", "cmd-f", "cmd-shift-f", "cmd-left", "cmd-right", "cmd-up", "cmd-down", "cmd-shift-up", "cmd-shift-down", "cmd-[", "cmd-]", "alt-left", "alt-right", "cmd-."];
        #[cfg(not(target_os = "macos"))]
        let chords = ["ctrl-a", "ctrl-c", "ctrl-x", "ctrl-v", "ctrl-z", "ctrl-y", "ctrl-f", "ctrl-h", "ctrl-left", "ctrl-right", "ctrl-shift-left", "ctrl-shift-right", "ctrl-[", "ctrl-]", "ctrl-.", "ctrl-backspace", "ctrl-delete"];
        chords.iter().map(|c| normalize(c).unwrap()).collect()
    }

    /// In the editor's text its own chords win over the app's, but for the deliberate app-wide ones; outside text the
    /// app's fire. A keymap as GPUI keeps it: gpui-base's text bindings first, then Shepherd's (`apply`'s order).
    #[test]
    fn the_editor_s_chords_win_in_its_text_but_for_the_app_wide_ones() {
        let mut text = text_chords_here();
        // pretend gpui-base took the deliberate app-wide chords too: they must stay the app's
        let closes = if cfg!(target_os = "macos") { "cmd-w" } else { "ctrl-shift-w" };
        text.extend([normalize(closes).unwrap(), normalize("ctrl-tab").unwrap()]);
        // and an app chord that is not app-wide: the editor's own wins in its text (off macOS none of the defaults
        // meets gpui-base's text chords, Ctrl+Shift+Left/Right and the Ctrl+letters, so one is made up)
        #[cfg(not(target_os = "macos"))]
        text.push(normalize("ctrl-alt-g").unwrap());
        let eff = effective(&user(&[]));
        let mut keymap = gpui::Keymap::new(text.iter().map(|c| KeyBinding::new(c, gpui_base::input::SelectToStart, Some("Input"))).collect());
        keymap.add_bindings(placed(&eff, &text).into_iter().filter_map(|(chord, context, action)| binding(action, chord, context)));
        let editor = stack(&["Workspace", "FileEditor", "Input"]);
        let pane = stack(&["Workspace", "Terminal"]);
        let fired = |chord: &str, at: &[gpui::KeyContext]| {
            let typed = gpui::Keystroke::parse(chord).unwrap();
            keymap.bindings_for_input(&[typed], at).0.first().map(|b| b.action().name().to_string())
        };
        let mut met = 0;
        for chord in &text {
            let Some((action, _)) = eff.iter().find(|(_, k)| k == chord) else { continue };
            met += 1;
            let wanted = if GLOBAL_IN_TEXT.contains(&action.as_str()) || OVER_THE_EDITOR.contains(&action.as_str()) { format!("shepherd::{action}") } else { "input::SelectToStart".into() };
            assert_eq!(fired(chord, &editor).as_deref(), Some(wanted.as_str()), "{chord} ({action}) in the editor");
            assert_eq!(fired(chord, &pane).as_deref(), Some(format!("shepherd::{action}").as_str()), "{chord} ({action}) in a pane");
        }
        assert!(met >= 2, "the defaults meet the editor's chords: {met}");
        #[cfg(target_os = "macos")]
        {
            assert_eq!(fired("cmd-up", &editor).as_deref(), Some("input::SelectToStart"), "not the previous workspace");
            assert_eq!(fired("cmd-up", &pane).as_deref(), Some("shepherd::PreviousWorkspace"));
        }
    }

    /// In a PDF tab the text size's chords zoom its pages; everywhere else they are the app's, as before. A keymap in
    /// `apply`'s order: the app's bindings, then the PDF view's.
    #[test]
    fn the_zoom_chords_zoom_a_pdf_only_in_its_view() {
        let text = text_chords_here();
        let eff = effective(&user(&[]));
        let mut keymap = gpui::Keymap::new(placed(&eff, &text).into_iter().filter_map(|(chord, context, action)| binding(action, chord, context)).collect());
        keymap.add_bindings(pdf_keys());
        let fired = |chord: &str, at: &[gpui::KeyContext]| {
            let typed = gpui::Keystroke::parse(chord).unwrap();
            keymap.bindings_for_input(&[typed], at).0.first().map(|b| b.action().name().to_string())
        };
        let pdf = stack(&["Workspace", "FileEditor", "FileViewer", PDF_VIEW]);
        let viewer = stack(&["Workspace", "FileEditor", "FileViewer"]);
        let pane = stack(&["Workspace", "Terminal"]);
        let primary = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
        for (key, zoom, app) in [("=", "PdfZoomIn", "FontSizeUp"), ("+", "PdfZoomIn", "FontSizeUp"), ("-", "PdfZoomOut", "FontSizeDown"), ("0", "PdfZoomReset", "FontSizeReset")] {
            let chord = format!("{primary}-{key}");
            assert_eq!(fired(&chord, &pdf).as_deref(), Some(format!("shepherd::{zoom}").as_str()), "{chord} in a PDF");
            // the same keys as the text size's: theirs outside the PDF view
            assert!(eff.iter().any(|(a, k)| a == app && Some(k) == normalize(&chord).as_ref()), "{chord} is {app}'s");
            assert_eq!(fired(&chord, &viewer).as_deref(), Some(format!("shepherd::{app}").as_str()), "{chord} in the viewer");
            assert_eq!(fired(&chord, &pane).as_deref(), Some(format!("shepherd::{app}").as_str()), "{chord} in a pane");
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn editor_chords_never_fire_in_a_pane() {
        let eff = effective(&user(&[]));
        let pane = stack(&["Terminal"]);
        for key in ["s", "z", "y", "a", "c", "v", "x"] {
            let typed = normalize(&format!("ctrl-{key}")).unwrap();
            for (chord, context, action) in placed(&eff, &text_chords_here()) {
                if chord != typed {
                    continue;
                }
                let context = context.unwrap_or_else(|| panic!("ctrl-{key} ({action}) is bound everywhere"));
                assert!(gpui::KeyBindingContextPredicate::parse(context).unwrap().depth_of(&pane).is_none(), "ctrl-{key} reaches a pane");
            }
        }
        assert_eq!(chord(&eff, "SaveFile"), normalize("ctrl-s").as_deref());
        // the panes' own chords stay theirs: none of the editor's takes Ctrl+Shift+C, V, F or G
        for key in ["c", "v", "f", "g"] {
            let taken = normalize(&format!("ctrl-shift-{key}")).unwrap();
            assert!(!eff.iter().any(|(_, k)| *k == taken));
        }
    }

    #[test]
    fn the_global_chords_still_work_in_the_editor() {
        // bound everywhere, so an editor without a binding of its own for them hears them too
        let text = text_chords_here();
        for action in ["ClosePane", "CloseTab", "ToggleSidebar", "CommandPanel", "NewTab", "NextTab", "PreviousTab"].iter().chain(&TAB_ACTIONS) {
            let keys = chord(&effective(&user(&[])), action).unwrap().to_string();
            assert_eq!(contexts(action, &keys, &text), [None], "{action}");
        }
        // even were gpui-base to bind the tab chords in its text, they stay app-wide there
        for (action, keys) in [("NextTab", "ctrl-tab"), ("PreviousTab", "ctrl-shift-tab"), ("Tab1", "ctrl-1"), ("Tab9", "ctrl-9")] {
            let keys = normalize(keys).unwrap();
            assert_eq!(contexts(action, &keys, std::slice::from_ref(&keys)), [None], "{action}");
        }
    }

    /// Whether a keystroke typed as GPUI's Linux backend reports it (`keystroke_from_xkb`) fires `chord`.
    #[cfg(not(target_os = "macos"))]
    fn fires(typed: gpui::Keystroke, chord: &str) -> bool {
        let target = gpui::KeybindingKeystroke::from_keystroke(gpui::Keystroke::parse(chord).unwrap());
        typed.should_match(&target)
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn linux_workspaces_are_ctrl_shift_digits_as_xkb_types_them() {
        let eff = effective(&user(&[]));
        assert_eq!(chord(&eff, "PreviousWorkspace"), normalize("ctrl-shift-up").as_deref());
        assert_eq!(chord(&eff, "NextWorkspace"), normalize("ctrl-shift-down").as_deref());
        for (i, action) in WORKSPACE_ACTIONS.iter().enumerate() {
            let bound = chord(&eff, action).unwrap_or_else(|| panic!("{action} is bound"));
            let key = Keystroke::parse(bound).unwrap().key;
            assert_eq!(shifted_digit(&key), char::from_digit(i as u32 + 1, 10), "{action} is Ctrl+Shift+{}", i + 1);
            // Ctrl+Shift+digit on a US layout: xkb's shifted symbol, and no Shift (GPUI drops it on a caseless key)
            let typed = gpui::Keystroke { modifiers: gpui::Modifiers::control(), key: key.clone(), key_char: Some(key.clone()) };
            assert!(fires(typed.clone(), bound), "{action}");
            assert!(!fires(typed, &format!("ctrl-shift-{}", i + 1)), "GPUI never hears ctrl-shift-{} on Linux", i + 1);
        }
        let ctrl_1 = gpui::Keystroke { modifiers: gpui::Modifiers::control(), key: "1".into(), key_char: Some("1".into()) };
        assert!(fires(ctrl_1.clone(), chord(&eff, "Tab1").unwrap()));
        assert!(!fires(ctrl_1, chord(&eff, "Workspace1").unwrap()), "Ctrl+1 is a tab, not a workspace");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn linux_defaults_leave_word_moves_their_ctrl_arrows() {
        let eff = effective(&user(&[]));
        for key in ["ctrl-up", "ctrl-down", "ctrl-left", "ctrl-right"] {
            let taken = normalize(key).unwrap();
            assert!(!eff.iter().any(|(_, k)| *k == taken), "{key} is the terminal's");
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_workspaces_are_ctrl_digits_off_the_screenshot_chords() {
        let eff = effective(&user(&[]));
        assert_eq!(chord(&eff, "PreviousWorkspace"), normalize("cmd-up").as_deref());
        assert_eq!(chord(&eff, "NextWorkspace"), normalize("cmd-down").as_deref());
        for (i, action) in WORKSPACE_ACTIONS.iter().enumerate() {
            assert_eq!(chord(&eff, action), normalize(&format!("ctrl-{}", i + 1)).as_deref(), "{action}");
        }
        for key in ["3", "4", "5"] {
            let shot = normalize(&format!("cmd-shift-{key}")).unwrap();
            assert!(!eff.iter().any(|(_, k)| *k == shot), "cmd-shift-{key} is a screenshot");
        }
    }

    #[test]
    fn every_numbered_action_has_a_default() {
        for action in TAB_ACTIONS.iter().chain(&WORKSPACE_ACTIONS) {
            assert!(known(action), "{action}");
            assert!(DEFAULTS.iter().any(|(a, _)| a == action), "{action} has a default");
        }
    }

    #[test]
    fn shifted_digits_read_as_their_digit() {
        assert_eq!(shifted_digit("!"), Some('1'));
        assert_eq!(shifted_digit("("), Some('9'));
        assert_eq!(shifted_digit(")"), Some('0'));
        assert_eq!(shifted_digit("1"), None);
        assert_eq!(shifted_digit("{"), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_defaults_leave_a_panes_find_previous() {
        let eff = effective(&user(&[]));
        let previous = normalize("cmd-shift-g").unwrap();
        assert!(!eff.iter().any(|(_, k)| *k == previous), "cmd-shift-g is a pane's find previous");
        assert_eq!(chord(&eff, "ShowChanges"), normalize("cmd-alt-g").as_deref());
        let bell = normalize("ctrl-shift-g").unwrap();
        assert!(!eff.iter().any(|(_, k)| *k == bell), "ctrl-shift-g is the pane app's");
        assert_eq!(chord(&eff, "ShowSearch"), normalize("cmd-shift-f").as_deref());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn defaults_when_the_user_set_nothing() {
        let eff = effective(&user(&[]));
        assert_eq!(chord(&eff, "SplitRight"), Some("cmd-d"));
        assert_eq!(chord(&eff, "NewTab"), Some("cmd-t"));
        assert_eq!(chord(&eff, "OpenSettings"), Some("cmd-,"));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn defaults_when_the_user_set_nothing() {
        let eff = effective(&user(&[]));
        assert_eq!(chord(&eff, "SplitRight"), Some("ctrl-shift-o"));
        assert_eq!(chord(&eff, "NewTab"), Some("ctrl-shift-t"));
        assert_eq!(chord(&eff, "OpenSettings"), Some("ctrl-,"));
    }

    #[test]
    fn a_user_chord_replaces_the_default() {
        let eff = effective(&user(&[("SplitRight", "cmd-shift-right")]));
        assert_eq!(chord(&eff, "SplitRight"), normalize("cmd-shift-right").as_deref());
    }

    #[test]
    fn rebinding_takes_the_chord_from_the_old_action() {
        let split = DEFAULTS.iter().find(|(a, _)| *a == "SplitRight").unwrap().1;
        let eff = effective(&user(&[("NewTab", split)]));
        assert_eq!(chord(&eff, "NewTab"), normalize(split).as_deref());
        assert_eq!(chord(&eff, "SplitRight"), None, "two actions on one chord");
    }

    #[test]
    fn an_empty_chord_unbinds() {
        let eff = effective(&user(&[("ClosePane", "")]));
        assert_eq!(chord(&eff, "ClosePane"), None);
    }

    #[test]
    fn unknown_actions_are_ignored() {
        let eff = effective(&user(&[("Teleport", "cmd-j")]));
        assert_eq!(chord(&eff, "Teleport"), None);
    }

    #[test]
    fn chords_written_differently_collide() {
        // the default for FocusLeft (cmd-alt-left, or ctrl-alt-left) with its modifiers the other way round
        let default = DEFAULTS.iter().find(|(a, _)| *a == "FocusLeft").unwrap().1;
        let mut parts: Vec<&str> = default.split('-').collect();
        let key = parts.pop().unwrap();
        parts.reverse();
        let swapped = format!("{}-{key}", parts.join("-"));
        assert_ne!(swapped, default);
        let eff = effective(&user(&[("NewTab", swapped.as_str())]));
        assert_eq!(chord(&eff, "FocusLeft"), None, "two actions on one chord");
        assert!(chord(&eff, "NewTab").is_some());
    }

    #[test]
    fn normalize_orders_modifiers_one_way() {
        assert_eq!(normalize("shift-cmd-d"), normalize("cmd-shift-d"));
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn only_modified_or_function_keys_are_recordable() {
        let k = |s: &str| gpui::Keystroke::parse(s).unwrap();
        assert!(recordable(&k("cmd-d")));
        assert!(recordable(&k("ctrl-a")));
        assert!(recordable(&k("f5")));
        assert!(!recordable(&k("l")), "a bare letter would steal typing in every pane");
        assert!(!recordable(&k("shift-l")));
    }

    #[test]
    fn every_default_names_a_known_action() {
        for (action, chord) in DEFAULTS {
            assert!(ACTIONS.iter().any(|(name, _)| name == action), "{action}");
            assert!(normalize(chord).is_some(), "{chord}");
        }
        for action in ["ShowFiles", "ShowChanges", "ShowSearch", "QuickOpen"] {
            assert!(DEFAULTS.iter().any(|(a, _)| *a == action), "{action} has a default");
        }
    }

    #[test]
    fn no_two_defaults_share_a_chord() {
        let mut seen = std::collections::HashSet::new();
        for (action, chord) in DEFAULTS {
            assert!(seen.insert(normalize(chord).unwrap()), "{action}'s {chord} is taken");
        }
    }

    /// Upstream defaults this fork deliberately changes (Linux): the workspaces move to Ctrl+Shift+Up/Down (the
    /// sessions take Ctrl+Shift+PageUp/PageDown), and the tabs to Ctrl+1…9, beside the workspaces' Ctrl+Shift+1…9, with
    /// Ctrl held showing their numbers.
    #[cfg(target_os = "macos")]
    const FORK_OVERRIDES: &[(&str, &str)] = &[];
    #[cfg(not(target_os = "macos"))]
    const FORK_OVERRIDES: &[(&str, &str)] = &[
        ("PreviousWorkspace", "ctrl-shift-up"),
        ("NextWorkspace", "ctrl-shift-down"),
        ("Tab1", "ctrl-1"),
        ("Tab2", "ctrl-2"),
        ("Tab3", "ctrl-3"),
        ("Tab4", "ctrl-4"),
        ("Tab5", "ctrl-5"),
        ("Tab6", "ctrl-6"),
        ("Tab7", "ctrl-7"),
        ("Tab8", "ctrl-8"),
        ("Tab9", "ctrl-9"),
    ];

    /// Chords the fork binds to upstream actions beside upstream's own: Ctrl+Tab and Ctrl+Shift+Tab step the tabs, as
    /// in a browser, from any view.
    const FORK_ADDED: &[(&str, &str)] = &[("PreviousTab", "ctrl-shift-tab"), ("NextTab", "ctrl-tab")];

    /// gpui-base's Input bindings near the chords added here, as its input state binds them on each OS.
    #[cfg(target_os = "macos")]
    const INPUT: &[&str] = &["cmd-up", "cmd-down", "cmd-shift-up", "cmd-shift-down", "ctrl-cmd-space", "cmd-left", "cmd-right", "alt-left", "alt-right", "cmd-enter"];

    #[cfg(not(target_os = "macos"))]
    const INPUT: &[&str] = &["pageup", "pagedown", "ctrl-left", "ctrl-right", "ctrl-shift-left", "ctrl-shift-right", "ctrl-enter"];

    /// The defaults added to upstream's.
    fn added() -> Vec<(&'static str, &'static str)> {
        DEFAULTS.iter().filter(|d| !UPSTREAM.contains(d)).copied().collect()
    }

    #[test]
    fn upstream_defaults_stay_as_they_are() {
        let eff = effective(&user(&[]));
        for (action, chord) in UPSTREAM {
            // fork-only overrides: the one place the defaults depart from upstream's
            let chord = FORK_OVERRIDES.iter().find(|(a, _)| a == action).map_or(chord, |(_, c)| c);
            assert!(DEFAULTS.contains(&(action, chord)), "{action} {chord}");
            let chord = normalize(chord).unwrap();
            assert!(eff.iter().any(|(a, k)| a == action && *k == chord), "{action} lost {chord}");
        }
    }

    #[test]
    fn added_defaults_miss_the_input_keys() {
        let input: Vec<String> = INPUT.iter().map(|k| normalize(k).unwrap()).collect();
        let added = added();
        assert!(!added.is_empty());
        for (action, chord) in added {
            assert!(!input.contains(&normalize(chord).unwrap()), "{action}'s {chord} is an input key");
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn sessions_step_on_ctrl_cmd_up_and_down() {
        let eff = effective(&user(&[]));
        assert_eq!(chord(&eff, "NextSession"), normalize("ctrl-cmd-down").as_deref());
        assert_eq!(chord(&eff, "PreviousSession"), normalize("ctrl-cmd-up").as_deref());
        // Cmd+Opt+Down, the chord first thought of, is focus down's
        assert_eq!(chord(&eff, "FocusDown"), normalize("alt-cmd-down").as_deref());
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn sessions_take_ctrl_shift_page_keys_and_the_switcher_ctrl_shift_s() {
        let eff = effective(&user(&[]));
        assert_eq!(chord(&eff, "SwitchSession"), normalize("ctrl-shift-s").as_deref());
        assert_eq!(chord(&eff, "NextSession"), normalize("ctrl-shift-pagedown").as_deref());
        assert_eq!(chord(&eff, "PreviousSession"), normalize("ctrl-shift-pageup").as_deref());
        assert_eq!(chord(&eff, "NextWorkspace"), normalize("ctrl-shift-down").as_deref());
        assert_eq!(chord(&eff, "PreviousWorkspace"), normalize("ctrl-shift-up").as_deref());
        // a letter keeps shift on Linux: Ctrl+Shift+S arrives as ctrl-shift-s, and only the switcher fires
        assert_eq!(fired(&from_xkb("s", true, Some("S"))), ["SwitchSession"]);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn ctrl_shift_j_goes_to_the_next_agent_that_needs_you_and_ctrl_j_stays_the_shells() {
        assert_eq!(chord(&effective(&user(&[])), "NextNeedsYou"), normalize("ctrl-shift-j").as_deref());
        assert_eq!(fired(&from_xkb("j", true, Some("J"))), ["NextNeedsYou"]);
        assert!(fired(&from_xkb("j", false, None)).is_empty(), "Ctrl+J is a newline to the pane");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn ctrl_shift_h_folds_the_session_and_ctrl_h_stays_the_shells() {
        assert_eq!(chord(&effective(&user(&[])), "ToggleSessionCollapsed"), normalize("ctrl-shift-h").as_deref());
        assert_eq!(fired(&from_xkb("h", true, Some("H"))), ["ToggleSessionCollapsed"]);
        assert!(fired(&from_xkb("h", false, None)).is_empty(), "Ctrl+H is a backspace to the pane");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_session_folds_on_shift_cmd_h() {
        assert_eq!(chord(&effective(&user(&[])), "ToggleSessionCollapsed"), normalize("cmd-shift-h").as_deref());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_next_agent_that_needs_you_is_on_shift_cmd_j() {
        assert_eq!(chord(&effective(&user(&[])), "NextNeedsYou"), normalize("cmd-shift-j").as_deref());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_shortcuts_panel_is_on_cmd_slash() {
        assert_eq!(chord(&effective(&user(&[])), "ShowShortcuts"), Some("cmd-/"));
    }

    /// What gpui_linux's `keystroke_from_xkb` makes of a key on a US layout: the keysym's name (`question` for
    /// Shift+/), shift dropped for a symbol, and the char typed unless it is a control code (Ctrl+/ types 0x1f).
    #[cfg(not(target_os = "macos"))]
    fn from_xkb(key: &str, shift: bool, key_char: Option<&str>) -> Keystroke {
        let modifiers = gpui::Modifiers { control: true, shift, ..Default::default() };
        let symbol = key.chars().count() == 1 && key.to_lowercase() == key.to_uppercase();
        Keystroke { modifiers: gpui::Modifiers { shift: shift && !symbol, ..modifiers }, key: key.into(), key_char: key_char.map(str::to_string) }
    }

    /// The actions a typed keystroke fires with no user chords, as GPUI matches it against the bindings.
    #[cfg(not(target_os = "macos"))]
    fn fired(typed: &Keystroke) -> Vec<String> {
        effective(&user(&[]))
            .into_iter()
            .filter(|(_, chord)| typed.should_match(&gpui::KeybindingKeystroke::from_keystroke(Keystroke::parse(chord).unwrap())))
            .map(|(action, _)| action)
            .collect()
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn ctrl_shift_slash_opens_the_shortcuts_and_ctrl_slash_stays_the_terminal_s() {
        let ctrl_shift_slash = from_xkb("?", true, Some("?"));
        assert_eq!(ctrl_shift_slash.unparse(), "ctrl-?", "as Settings records it too");
        assert_eq!(fired(&ctrl_shift_slash), ["ShowShortcuts"]);
        assert!(fired(&from_xkb("/", false, None)).is_empty(), "Ctrl+/ goes to the pane");
        // Ctrl+Shift+1 arrives the same way, as ctrl-!
        assert_eq!(from_xkb("!", true, Some("!")).unparse(), "ctrl-!");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_session_switcher_has_no_default_chord() {
        assert_eq!(chord(&effective(&user(&[])), "SwitchSession"), None);
    }
}
