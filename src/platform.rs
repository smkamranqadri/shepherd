//! What differs between macOS, Linux and Windows for the files sidebar: the home folder, what opens a path, and
//! which modifiers the terminal's own shortcuts take. The folders and the modifiers are upstream's (`settings`,
//! `terminal::keys`); this only names them for the sidebar's code.
use gpui::Modifiers;
use std::path::PathBuf;

/// The home folder, as the platform names it.
pub fn home() -> PathBuf {
    crate::settings::home()
}

/// The command that hands a path or URL to its default app: `open` on macOS, `explorer` on Windows, else `xdg-open`.
pub const OPEN: &str = if cfg!(target_os = "macos") {
    "open"
} else if cfg!(windows) {
    "explorer"
} else {
    "xdg-open"
};

/// The terminal's own shortcut for `key` (copy, paste, find, find next): Cmd+key on macOS, Ctrl+Shift+key elsewhere
/// (`terminal::keys::app_shortcut`).
pub fn terminal_chord(key: &str, mods: &Modifiers) -> Option<&'static str> {
    if !crate::terminal::keys::app_shortcut(mods) {
        return None;
    }
    match key {
        "c" => Some("c"),
        "v" => Some("v"),
        "f" => Some("f"),
        "g" => Some("g"),
        _ => None,
    }
}

/// The modifier that turns a link under the pointer into a click target: Cmd on macOS, Ctrl elsewhere.
pub fn link_modifier(mods: &Modifiers) -> bool {
    crate::terminal::keys::link_modifier(mods)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(control: bool, shift: bool, platform: bool) -> Modifiers {
        Modifiers { control, shift, platform, ..Default::default() }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn terminal_chords_off_macos() {
        assert_eq!(terminal_chord("c", &m(true, true, false)), Some("c"));
        assert_eq!(terminal_chord("c", &m(true, false, false)), None, "Ctrl+C is the shell's");
        assert_eq!(terminal_chord("v", &m(false, false, true)), None, "Super is the desktop's");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn mac_terminal_chords() {
        assert_eq!(terminal_chord("c", &m(false, false, true)), Some("c"));
        assert_eq!(terminal_chord("c", &m(true, true, false)), None);
    }
}
