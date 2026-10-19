//! herdr's own settings, `config.toml`, edited in place: comments, key order and the blocks other tools generate
//! survive, and a symlinked file (dotfiles) is written through to its target. herdr rereads it on
//! `server.reload_config`.
use anyhow::{Context, anyhow};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

/// A string or a list of strings; anything else is none.
pub(super) fn str_list(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.value().clone()],
        Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

/// Where herdr reads it: `HERDR_CONFIG_PATH`, else in [`dir`].
pub fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HERDR_CONFIG_PATH") {
        return path.into();
    }
    dir().join("config.toml")
}

/// herdr's folder, with its config and its sessions' sockets: `$XDG_CONFIG_HOME/herdr`, else `%APPDATA%\herdr` on
/// Windows, else `~/.config/herdr`.
pub(super) fn dir() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("APPDATA").filter(|_| cfg!(windows)).map(PathBuf::from));
    config.unwrap_or_else(|| crate::settings::home().join(".config")).join("herdr")
}

/// A `[[keys.command]]` of the config.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandEntry {
    pub index: usize,
    /// `shell` (herdr's default), `pane`, `popup` or `plugin_action`, as the file spells it.
    pub kind: String,
    pub command: String,
    /// Its chords as the file spells them.
    pub keys: Vec<String>,
}

/// Chords as herdr takes them: a string for one (or none, empty), a list for several.
fn labels_value(labels: &[String]) -> Value {
    match labels {
        [] => "".into(),
        [one] => one.as_str().into(),
        many => Value::Array(many.iter().map(|l| Value::from(l.as_str())).collect()),
    }
}

/// Copies the config next to itself as `config.toml.shepherd-bak`, once: before Shepherd's first write, so what the
/// user had stays one copy away. A later write leaves the copy alone (it holds the user's own version, not ours).
/// `true` when it made the copy; a missing config has nothing to keep.
pub fn backup_once(path: &Path) -> anyhow::Result<bool> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Some(name) = target.file_name() else { return Ok(false) };
    let copy = target.with_file_name(format!("{}.shepherd-bak", name.to_string_lossy()));
    if copy.exists() || !target.is_file() {
        return Ok(false);
    }
    std::fs::copy(&target, &copy).with_context(|| format!("cannot back up {}", target.display()))?;
    Ok(true)
}

/// The file as herdr reads it; a missing one is empty, so every key reads as herdr's default.
#[derive(Debug, Clone, Default)]
pub struct HerdrConfig(pub(super) DocumentMut);

impl HerdrConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self(text.parse().with_context(|| format!("{} is not valid TOML", path.display()))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// The config as text, parsed; for tests and previews.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(Self(text.parse().context("not valid TOML")?))
    }

    /// The file as it would be written.
    pub fn to_text(&self) -> String {
        self.0.to_string()
    }

    /// A dotted key: `ui.toast.delivery`.
    fn get(&self, key: &str) -> Option<&Value> {
        key.split('.').try_fold(self.0.as_item(), |item, part| item.get(part))?.as_value()
    }

    pub fn bool(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(default)
    }

    pub fn int(&self, key: &str, default: i64) -> i64 {
        self.get(key).and_then(Value::as_integer).unwrap_or(default)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// A key herdr takes as a string or a list of them (`next_tab = ["prefix+n", "ctrl+alt+]"]`); `None` when
    /// missing, so herdr's default applies.
    pub fn strs(&self, key: &str) -> Option<Vec<String>> {
        self.get(key).map(str_list)
    }

    /// Sets a dotted key, making missing tables; `None` removes it, so herdr's default applies. A value already
    /// there keeps its trailing comment.
    pub fn set(&mut self, key: &str, value: Option<Value>) -> anyhow::Result<()> {
        let (tables, last) = key.rsplit_once('.').unwrap_or(("", key));
        let mut table = self.0.as_table_mut();
        for part in tables.split('.').filter(|p| !p.is_empty()) {
            table = table
                .entry(part)
                .or_insert_with(|| Item::Table(Table::new()))
                .as_table_mut()
                .ok_or_else(|| anyhow!("`{part}` in herdr's config is not a table"))?;
        }
        match (value, table.get_mut(last)) {
            (None, _) => {
                table.remove(last);
            }
            (Some(mut value), Some(Item::Value(old))) => {
                *value.decor_mut() = old.decor().clone();
                *old = value;
            }
            (Some(value), _) => {
                table.insert(last, Item::Value(value));
            }
        }
        Ok(())
    }

    /// The names under `[keys]` that hold a string or a list of strings (an action's chords), in file order. The
    /// `command` entries and `indexed` table are no such names.
    pub fn key_names(&self) -> Vec<String> {
        let Some(keys) = self.0.get("keys").and_then(Item::as_table_like) else { return Vec::new() };
        keys.iter().filter(|(_, v)| v.as_value().is_some_and(|v| matches!(v, Value::String(_) | Value::Array(_)))).map(|(k, _)| k.to_string()).collect()
    }

    /// Puts an action's chords under `[keys]`: none writes an empty string (unbound), one a string, several a list;
    /// `None` removes the line, so herdr's default applies again. Comments and the rest of the file stay as they are.
    pub fn set_key_labels(&mut self, name: &str, labels: Option<&[String]>) -> anyhow::Result<()> {
        self.set(&format!("keys.{name}"), labels.map(labels_value))
    }

    /// Removes these `[keys]` lines, only these: custom commands, `[keys.indexed]` and every other line stay.
    pub fn remove_key_lines(&mut self, names: &[&str]) {
        if let Some(keys) = self.0.get_mut("keys").and_then(Item::as_table_like_mut) {
            for name in names {
                keys.remove(name);
            }
        }
    }

    /// The `[[keys.command]]` entries, in file order.
    pub fn commands(&self) -> Vec<CommandEntry> {
        let tables = self.0.get("keys").and_then(|k| k.get("command")).and_then(Item::as_array_of_tables);
        tables
            .into_iter()
            .flat_map(|a| a.iter())
            .enumerate()
            .map(|(index, t)| {
                let text = |key: &str| t.get(key).and_then(Item::as_str).map(str::to_string);
                CommandEntry {
                    index,
                    kind: text("type").unwrap_or_else(|| "shell".into()),
                    command: text("command").unwrap_or_default(),
                    keys: t.get("key").and_then(Item::as_value).map(str_list).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Changes the chords of `[[keys.command]]` number `index`.
    pub fn set_command_labels(&mut self, index: usize, labels: &[String]) -> anyhow::Result<()> {
        let table = self
            .0
            .get_mut("keys")
            .and_then(|k| k.get_mut("command"))
            .and_then(Item::as_array_of_tables_mut)
            .and_then(|a| a.get_mut(index))
            .ok_or_else(|| anyhow!("herdr's config has no [[keys.command]] number {}", index + 1))?;
        match (labels_value(labels), table.get_mut("key")) {
            (mut value, Some(Item::Value(old))) => {
                *value.decor_mut() = old.decor().clone();
                *old = value;
            }
            (value, _) => {
                table.insert("key", Item::Value(value));
            }
        }
        Ok(())
    }

    /// Writes next to the real file, then renames over it: a crash never leaves half a config, and a symlink stays
    /// a symlink.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = target.with_extension("toml.shepherd");
        std::fs::write(&tmp, self.0.to_string())?;
        std::fs::rename(&tmp, &target).with_context(|| format!("cannot write {}", target.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"onboarding = false
# herdr configuration

[ui]
# Ask for a tab name before creating a new tab.
prompt_new_tab_name = false

[ui.toast]
# off = disable pop-up notifications
delivery = "terminal" # where pop-ups go

[ui.sound]
enabled = false
"#;

    #[test]
    fn reads_dotted_keys_with_defaults() {
        let config: HerdrConfig = HerdrConfig(CONFIG.parse().unwrap());
        assert_eq!(config.str("ui.toast.delivery"), Some("terminal"));
        assert!(!config.bool("ui.sound.enabled", true));
        assert!(config.bool("session.resume_agents_on_restore", true), "missing reads as the default");
        assert_eq!(config.int("ui.toast.delay_seconds", 1), 1);
    }

    #[test]
    fn edits_keep_comments_and_order() {
        let mut config = HerdrConfig(CONFIG.parse().unwrap());
        config.set("ui.toast.delivery", Some("off".into())).unwrap();
        config.set("ui.sound.enabled", Some(true.into())).unwrap();
        assert_eq!(config.0.to_string(), CONFIG.replace(r#"= "terminal""#, r#"= "off""#).replace("enabled = false", "enabled = true"));
    }

    #[test]
    fn missing_tables_are_made_and_none_removes() {
        let mut config = HerdrConfig(CONFIG.parse().unwrap());
        config.set("session.resume_agents_on_restore", Some(false.into())).unwrap();
        assert!(config.0.to_string().ends_with("[session]\nresume_agents_on_restore = false\n"));
        config.set("ui.prompt_new_tab_name", None).unwrap();
        assert!(!config.0.to_string().contains("prompt_new_tab_name"));
        assert!(config.set("onboarding.x", Some(1.into())).is_err(), "a value is not a table");
    }

    const KEYS: &str = "[keys]\nprefix = \"ctrl+space\"\nnew_tab = [\"prefix+t\", \"alt+t\"]\n# a note\nzoom = \"prefix+z\" # keep\n\n[keys.indexed]\ntabs = \"\"\n\n[[keys.command]]\nkey = \"prefix+g\"\ntype = \"popup\"\ncommand = \"lazygit\"\n\n[[keys.command]]\ncommand = \"say hi\"\n";

    #[test]
    fn the_key_names_are_the_actions_chords_not_the_tables() {
        let config = HerdrConfig::parse(KEYS).unwrap();
        assert_eq!(config.key_names(), ["prefix", "new_tab", "zoom"], "no `indexed` table, no `command` entries");
        assert!(HerdrConfig::parse("[ui]\nx = 1\n").unwrap().key_names().is_empty());
        assert!(HerdrConfig::default().key_names().is_empty());
    }

    #[test]
    fn the_command_entries_are_listed_in_file_order() {
        let c = HerdrConfig::parse(KEYS).unwrap().commands();
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].index, c[0].kind.as_str(), c[0].command.as_str(), c[0].keys.clone()), (0, "popup", "lazygit", vec!["prefix+g".to_string()]));
        assert_eq!((c[1].kind.as_str(), c[1].command.as_str(), c[1].keys.is_empty()), ("shell", "say hi", true), "type defaults to shell; no key");
    }

    #[test]
    fn a_commands_chords_are_set_where_its_key_was_or_added() {
        let mut config = HerdrConfig::parse(KEYS).unwrap();
        config.set_command_labels(1, &["alt+h".to_string()]).unwrap();
        assert_eq!(config.commands()[1].keys, ["alt+h"]);
        config.set_command_labels(0, &[]).unwrap();
        assert!(config.to_text().contains("key = \"\"\ntype = \"popup\""), "unbound: an empty key, in place");
        assert!(config.set_command_labels(2, &[]).is_err());
    }

    #[test]
    fn key_lines_are_set_listed_and_removed_in_place() {
        let mut config = HerdrConfig::parse(KEYS).unwrap();
        config.set_key_labels("zoom", Some(&["prefix+f".to_string()])).unwrap();
        config.set_key_labels("new_tab", Some(&[])).unwrap();
        config.set_key_labels("close_pane", Some(&["alt+x".to_string(), "alt+y".to_string()])).unwrap();
        let out = config.to_text();
        assert!(out.contains("zoom = \"prefix+f\" # keep\n"), "the comment stays: {out}");
        assert!(out.contains("new_tab = \"\"\n") && out.contains("close_pane = [\"alt+x\", \"alt+y\"]\n"), "{out}");
        assert!(out.contains("# a note\n") && out.contains("[keys.indexed]\ntabs = \"\"\n") && out.contains("command = \"lazygit\""), "{out}");
        config.remove_key_lines(&["zoom", "close_pane", "nothing"]);
        assert!(!config.to_text().contains("zoom") && !config.to_text().contains("close_pane"));
        assert_eq!(config.commands().len(), 2, "removing key lines leaves the commands");
    }

    #[test]
    fn the_first_write_keeps_a_copy_and_a_later_one_leaves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, KEYS).unwrap();
        assert!(backup_once(&path).unwrap(), "the first time");
        let copy = dir.path().join("config.toml.shepherd-bak");
        assert_eq!(std::fs::read(&copy).unwrap(), KEYS.as_bytes(), "byte for byte");
        std::fs::write(&path, "changed = true\n").unwrap();
        assert!(!backup_once(&path).unwrap(), "once");
        assert_eq!(std::fs::read(&copy).unwrap(), KEYS.as_bytes(), "the user's own file, not our edit");
        assert!(!backup_once(&dir.path().join("none.toml")).unwrap(), "a missing config has nothing to keep");
        assert!(!dir.path().join("none.toml.shepherd-bak").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_is_kept_beside_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let (real, link) = (dir.path().join("dotfiles.toml"), dir.path().join("config.toml"));
        std::fs::write(&real, KEYS).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(backup_once(&link).unwrap());
        assert_eq!(std::fs::read(dir.path().join("dotfiles.toml.shepherd-bak")).unwrap(), KEYS.as_bytes());
    }

    #[cfg(unix)] // a symlink on Windows needs developer mode or admin
    #[test]
    fn saving_writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let (real, link) = (dir.path().join("dotfiles.toml"), dir.path().join("config.toml"));
        std::fs::write(&real, CONFIG).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut config = HerdrConfig::load(&link).unwrap();
        config.set("ui.sound.enabled", Some(true.into())).unwrap();
        config.save(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(std::fs::read_to_string(&real).unwrap().contains("enabled = true"));
    }

    #[test]
    fn a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = HerdrConfig::load(&dir.path().join("none.toml")).unwrap();
        assert_eq!(config.str("ui.toast.delivery"), None);
    }
}
