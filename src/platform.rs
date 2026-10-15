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

/// The command that shows `path` in the file manager: Finder and Explorer select it (`open -R`, `explorer
/// /select,`); Linux has no portable way to select a file (the FileManager1 D-Bus call is not everywhere), so its
/// folder opens.
pub fn reveal_command(path: &std::path::Path) -> (&'static str, Vec<std::ffi::OsString>) {
    if cfg!(target_os = "macos") {
        return ("open", vec!["-R".into(), path.into()]);
    }
    if cfg!(windows) {
        let mut select = std::ffi::OsString::from("/select,");
        select.push(path);
        return (OPEN, vec![select]);
    }
    (OPEN, vec![path.parent().unwrap_or(path).into()])
}

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
}

/// Starts `argv` on its own, as a launcher would: in a session of its own (no terminal, none of Shepherd's
/// signals), with no stdin, stdout or stderr of Shepherd's, and not killed when Shepherd or the handle goes. A
/// program still running after a moment (an app with its window up) is left to run, and a task reaps it when it
/// exits; one that has already failed by then (`xdg-open` with no app for the file) or could not start is the error,
/// in words for a toast. Runs on the tokio runtime.
pub async fn launch(argv: &[std::ffi::OsString]) -> Result<(), String> {
    use std::process::Stdio;
    let Some((program, args)) = argv.split_first() else { return Err("Nothing to run".into()) };
    let name = std::path::Path::new(program).file_name().unwrap_or(program).to_string_lossy().into_owned();
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(false);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe, the only kind of call allowed between fork and exec.
        unsafe {
            cmd.as_std_mut().pre_exec(|| if setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
        }
    }
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("Couldn't find {name}"),
        _ => format!("Couldn't start {name}: {e}"),
    })?;
    match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(match status.code() {
            Some(code) => format!("{name} failed (exit {code})"),
            None => format!("{name} was stopped by a signal"),
        }),
        Ok(Err(e)) => Err(format!("{name}: {e}")),
        // still running (an app with its window up): a task waits on it, so it is reaped the moment it exits
        // rather than whenever tokio's orphan queue next looks
        Err(_) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            Ok(())
        }
    }
}

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

/// The operating system, as far as the terminal's keys care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Mac,
}

impl Os {
    pub const CURRENT: Os = if cfg!(target_os = "macos") { Os::Mac } else { Os::Linux };
}

/// What a pane does with a key before the terminal gets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKey {
    Copy,
    Paste,
    PassThrough,
}

/// The opt-in Ctrl+C / Ctrl+V of a Linux pane: Ctrl+V pastes, and Ctrl+C copies only while something is selected
/// (otherwise it is the shell's ^C). macOS has Cmd for both, so it never takes these.
pub fn ctrl_copy_paste(key: &str, mods: &Modifiers, enabled: bool, has_selection: bool, os: Os) -> PaneKey {
    if os != Os::Linux || !enabled || !mods.control || mods.shift || mods.alt || mods.platform {
        return PaneKey::PassThrough;
    }
    match key {
        "v" => PaneKey::Paste,
        "c" if has_selection => PaneKey::Copy,
        _ => PaneKey::PassThrough,
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
    fn ctrl_copy_paste_decisions() {
        use PaneKey::*;
        let ctrl = m(true, false, false);
        assert_eq!(ctrl_copy_paste("v", &ctrl, true, false, Os::Linux), Paste);
        assert_eq!(ctrl_copy_paste("c", &ctrl, true, true, Os::Linux), Copy);
        assert_eq!(ctrl_copy_paste("c", &ctrl, true, false, Os::Linux), PassThrough, "no selection: ^C");
        assert_eq!(ctrl_copy_paste("v", &ctrl, false, true, Os::Linux), PassThrough, "setting off");
        assert_eq!(ctrl_copy_paste("c", &ctrl, false, true, Os::Linux), PassThrough, "setting off");
        assert_eq!(ctrl_copy_paste("v", &ctrl, true, true, Os::Mac), PassThrough);
        assert_eq!(ctrl_copy_paste("c", &ctrl, true, true, Os::Mac), PassThrough);
        assert_eq!(ctrl_copy_paste("x", &ctrl, true, true, Os::Linux), PassThrough);
        assert_eq!(ctrl_copy_paste("v", &m(false, false, false), true, true, Os::Linux), PassThrough);
        assert_eq!(ctrl_copy_paste("v", &m(true, true, false), true, true, Os::Linux), PassThrough, "Ctrl+Shift+V is the chord's");
        assert_eq!(ctrl_copy_paste("c", &m(true, false, true), true, true, Os::Linux), PassThrough);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn terminal_chords_off_macos() {
        assert_eq!(terminal_chord("c", &m(true, true, false)), Some("c"));
        assert_eq!(terminal_chord("c", &m(true, false, false)), None, "Ctrl+C is the shell's");
        assert_eq!(terminal_chord("v", &m(false, false, true)), None, "Super is the desktop's");
    }

    #[tokio::test]
    async fn a_launch_says_why_it_failed() {
        let argv = |a: &[&str]| a.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert_eq!(launch(&argv(&["/nonexistent/editor", "a"])).await, Err("Couldn't find editor".into()));
        assert_eq!(launch(&argv(&["sh", "-c", "exit 3"])).await, Err("sh failed (exit 3)".into()));
        assert_eq!(launch(&argv(&["true"])).await, Ok(()));
        assert!(launch(&[]).await.is_err());
    }

    #[tokio::test]
    async fn a_launched_program_has_a_session_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ids");
        // setsid makes it the leader of a new session and group: its group's id is its pid (macOS's `ps` has no
        // session id to show)
        let script = format!("echo $$ $(ps -o pgid= -p $$) > '{}'", out.display());
        launch(&["sh".into(), "-c".into(), script.into()]).await.unwrap();
        let ids: Vec<i32> = std::fs::read_to_string(&out).unwrap().split_whitespace().map(|w| w.parse().unwrap()).collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], ids[1]);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn mac_terminal_chords() {
        assert_eq!(terminal_chord("c", &m(false, false, true)), Some("c"));
        assert_eq!(terminal_chord("c", &m(true, true, false)), None);
    }
}
