//! The file tabs kept for the next run, in `open-files.json` beside the settings: per workspace (`roots::root_key`)
//! its pinned file tabs in order and the one shown, never previews. Kept apart from the settings, which are the
//! user's to edit and are written only on a change of theirs; this changes with every tab opened or closed.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One workspace's file tabs as kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    /// `roots::fingerprint` of the workspace when they were kept: herdr gives a closed workspace's id to a new one
    /// later, which must not open the old one's files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
    #[serde(default)]
    pub files: Vec<RememberedFile>,
    /// The tab shown, by its path; none for the terminals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedFile {
    pub path: String,
    /// Always true today (previews are not kept); there so a later kind of kept tab needs no new format.
    #[serde(default = "yes")]
    pub pinned: bool,
}

fn yes() -> bool {
    true
}

impl Remembered {
    pub fn new(fingerprint: String, files: Vec<PathBuf>, active: Option<PathBuf>) -> Remembered {
        let text = |p: PathBuf| p.to_string_lossy().into_owned();
        Remembered { fingerprint, files: files.into_iter().map(|p| RememberedFile { path: text(p), pinned: true }).collect(), active: active.map(text) }
    }

    /// The kept tabs, if they were kept for the workspace that now has `fingerprint` (an empty one fits any).
    pub fn fitting(&self, fingerprint: &str) -> Option<(Vec<PathBuf>, Option<PathBuf>)> {
        (self.fingerprint.is_empty() || self.fingerprint == fingerprint)
            .then(|| (self.files.iter().filter(|f| f.pinned).map(|f| PathBuf::from(&f.path)).collect(), self.active.as_ref().map(PathBuf::from)))
    }
}

/// Where they are kept: beside the settings (`SHEPHERD_SETTINGS_FILE` moves both).
pub fn path() -> PathBuf {
    crate::settings::Settings::path().with_file_name("open-files.json")
}

/// The kept tabs; none when the file is missing or unreadable (a broken file is overwritten by the next change).
pub fn load(path: &Path) -> BTreeMap<String, Remembered> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// The file's text for `map`, to compare with what was written last before writing it again.
pub fn encode(map: &BTreeMap<String, Remembered>) -> String {
    serde_json::to_string_pretty(map).unwrap_or_default()
}

/// Writes `text` through a temporary file renamed over, so a crash never leaves half a file.
pub fn save(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The writes of the file: what was written last, whether one is under way, and whether a change came in meanwhile.
/// One write at a time (they share the temporary file); a change during one is written once it ends.
#[derive(Debug, Default)]
pub struct Writes {
    written: Option<String>,
    in_flight: bool,
    again: bool,
}

impl Writes {
    /// Read from the file at launch: the same text is not written back.
    pub fn read(&mut self, text: String) {
        self.written = Some(text);
    }

    /// Whether to write `text` now: not when the file holds it already, nor while a write is under way (it is
    /// written again after that one).
    pub fn start(&mut self, text: &str) -> bool {
        if self.in_flight {
            self.again = true;
            return false;
        }
        if self.written.as_deref() == Some(text) {
            return false;
        }
        self.in_flight = true;
        true
    }

    /// The write of `text` ended: the file holds it only if it worked. True when a change came in during it (write
    /// again).
    pub fn finish(&mut self, text: String, ok: bool) -> bool {
        self.in_flight = false;
        if ok {
            self.written = Some(text);
        }
        std::mem::take(&mut self.again)
    }
}

/// An entry kept per workspace, made for the workspace that had this `roots::fingerprint`.
pub trait Fingerprinted {
    fn fingerprint(&mut self) -> &mut String;
}

impl Fingerprinted for Remembered {
    fn fingerprint(&mut self) -> &mut String {
        &mut self.fingerprint
    }
}

/// On a snapshot of `place` (its live workspaces and their `roots::fingerprint`s): the kept entries of its workspaces
/// that closed go. A workspace's entry is checked against its fingerprint the first time the workspace is seen this
/// run (`seen`): a mismatch then is an id herdr gave to another workspace while Shepherd was not looking (or a rename
/// meanwhile, which cannot be told apart, as for `workspace_roots`), and the entry goes. Once seen, the workspace is
/// watched without a gap (a closed one leaves the snapshot, and this map, before herdr can give its id again), so a
/// later change of fingerprint is a rename, and the entry follows it. True when the map changed.
pub fn reconcile<T: Fingerprinted>(map: &mut BTreeMap<String, T>, place: &str, live: &[(&str, String)], seen: &mut std::collections::HashSet<String>) -> bool {
    let prefix = format!("{place}\n");
    let is_live = |k: &str| k.strip_prefix(&prefix).is_none_or(|ws| live.iter().any(|(id, _)| *id == ws));
    let mut changed = false;
    map.retain(|k, _| {
        let keep = is_live(k);
        changed |= !keep;
        keep
    });
    seen.retain(|k| is_live(k));
    for (ws, fingerprint) in live {
        let key = format!("{prefix}{ws}");
        let first = seen.insert(key.clone());
        let Some(entry) = map.get_mut(&key).map(Fingerprinted::fingerprint) else { continue };
        if *entry == *fingerprint {
            continue;
        }
        changed = true;
        if first && !entry.is_empty() {
            map.remove(&key);
        } else {
            *entry = fingerprint.clone();
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_write_at_a_time_and_written_only_once_it_worked() {
        let mut w = Writes::default();
        w.read("a".into());
        assert!(!w.start("a"), "what the file holds");
        assert!(w.start("b"));
        assert!(!w.start("c"), "under way: c waits");
        assert!(w.finish("b".into(), false), "c came in meanwhile");
        assert!(w.start("b"), "b failed, so it is not what the file holds");
        assert!(!w.finish("b".into(), true));
        assert!(!w.start("b"));
    }

    #[test]
    fn a_rename_is_followed_and_a_reused_id_drops_its_entry() {
        let mut map = BTreeMap::new();
        let entry = |fp: &str| Remembered::new(fp.into(), vec!["/a".into()], None);
        map.insert("p\nw1".to_string(), entry("label:api"));
        map.insert("p\nw2".to_string(), entry("label:old"));
        map.insert("p\nw3".to_string(), entry("label:gone"));
        map.insert("q\nw3".to_string(), entry("label:elsewhere"));
        let mut seen = Default::default();
        let live = [("w1", "label:api".to_string()), ("w2", "label:new".to_string())];
        assert!(reconcile(&mut map, "p", &live, &mut seen));
        // w2's id now names another workspace, w3 closed, the other place's are not known here
        assert_eq!(map.keys().collect::<Vec<_>>(), ["p\nw1", "q\nw3"]);
        // seen since: renamed, its entry follows
        assert!(reconcile(&mut map, "p", &[("w1", "label:api2".to_string())], &mut seen));
        assert_eq!(map["p\nw1"].fingerprint, "label:api2");
        assert!(!reconcile(&mut map, "p", &[("w1", "label:api2".to_string())], &mut seen));
    }

    #[test]
    fn kept_tabs_round_trip_and_fit_their_workspace_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/open-files.json");
        assert!(load(&file).is_empty(), "no file yet");
        let mut map = BTreeMap::new();
        map.insert("sock\nw1".to_string(), Remembered::new("label:api".into(), vec!["/r/a.rs".into(), "/r/b.rs".into()], Some("/r/b.rs".into())));
        save(&file, &encode(&map)).unwrap();
        let back = load(&file);
        assert_eq!(back, map);
        let kept = &back["sock\nw1"];
        assert_eq!(kept.fitting("label:api"), Some((vec![PathBuf::from("/r/a.rs"), PathBuf::from("/r/b.rs")], Some(PathBuf::from("/r/b.rs")))));
        assert_eq!(kept.fitting("label:web"), None, "another workspace that got the id later");
        std::fs::write(&file, "{ not json").unwrap();
        assert!(load(&file).is_empty());
    }

    #[test]
    fn a_hand_written_entry_reads_with_defaults() {
        let map: BTreeMap<String, Remembered> = serde_json::from_str(r#"{"k": {"files": [{"path": "/a"}]}}"#).unwrap();
        assert_eq!(map["k"].fitting("anything"), Some((vec![PathBuf::from("/a")], None)));
    }
}
