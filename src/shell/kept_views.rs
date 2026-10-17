//! The session views kept for the next run, in `session-views.json` beside `open-files.json`: per session the
//! keyboard's pane of each tab, per workspace (`roots::root_key`) the Files tree's root and open folders. Only those:
//! the rest of a view (its tab, its terminals) is herdr's to say on the next run. Kept apart from the settings, as the
//! open files are, since it changes with every click; read off the main thread, written a second after a change
//! through a file renamed over.
use super::session_views::KeyboardMemory;
use super::*;
use crate::files::open_files::{self, Fingerprinted};
use futures::FutureExt;
use crate::files::roots;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct KeptViews {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keyboard: KeyboardMemory,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, KeptFiles>,
}

/// A workspace's Files tree as kept: its root and the folders open in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct KeptFiles {
    /// `roots::fingerprint` of the workspace when kept: herdr gives a closed workspace's id to a new one later.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
    pub root: String,
    #[serde(default)]
    pub expanded: Vec<String>,
}

impl Fingerprinted for KeptFiles {
    fn fingerprint(&mut self) -> &mut String {
        &mut self.fingerprint
    }
}

pub(super) fn path() -> PathBuf {
    open_files::path().with_file_name("session-views.json")
}

/// What was kept; nothing when the file is missing or unreadable (the next change writes it whole).
pub(super) fn load(path: &Path) -> KeptViews {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub(super) fn encode(kept: &KeptViews) -> String {
    serde_json::to_string_pretty(kept).unwrap_or_default()
}

/// The Shell's side: the keyboard memory lives on the Shell (`Shell::keyboard`), the Files trees' in the side panel;
/// this holds what was read and the writes.
#[derive(Default)]
pub(super) struct KeptState {
    /// The file has been read: until then nothing is written (it would be written without the rest).
    pub loaded: bool,
    /// The Files entries as last written or read, for their fingerprints.
    pub files: BTreeMap<String, KeptFiles>,
    /// Workspaces of the shown place checked against their Files entry this run (`open_files::reconcile`).
    pub seen: HashSet<String>,
    writes: open_files::Writes,
    /// The write under way, which a quit waits for before it writes the latest.
    in_flight: Option<open_files::InFlight>,
    save: Option<gpui::Task<()>>,
}

/// The Files entries for `now` (the side panel's memory: key, root, open folders), each with the fingerprint it had
/// (`kept`), or the shown place's workspace's now (`fingerprint`) when it has none yet.
fn files_entries(now: Vec<(String, PathBuf, Vec<PathBuf>)>, kept: &BTreeMap<String, KeptFiles>, fingerprint: impl Fn(&str) -> Option<String>) -> BTreeMap<String, KeptFiles> {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    now.into_iter()
        .map(|(key, root, expanded)| {
            let fingerprint = kept.get(&key).map(|k| k.fingerprint.clone()).filter(|f| !f.is_empty()).or_else(|| fingerprint(&key)).unwrap_or_default();
            let entry = KeptFiles { fingerprint, root: text(&root), expanded: expanded.iter().map(|p| text(p)).collect() };
            (key, entry)
        })
        .collect()
}

impl Shell {
    /// Reads what was kept off the main thread at launch: the keyboard's panes join those noted since, the Files
    /// trees' open folders go to the side panel.
    pub(super) fn load_kept_views(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let path = path();
            let kept = cx.background_spawn(async move { load(&path) }).await;
            let _ = this.update(cx, |s, cx| {
                s.kept_views.writes.read(encode(&kept));
                for (session, tabs) in kept.keyboard {
                    let now = s.keyboard.entry(session).or_default();
                    for (tab, pane) in tabs {
                        now.entry(tab).or_insert(pane);
                    }
                }
                let files = kept.files.iter().map(|(k, f)| (k.clone(), PathBuf::from(&f.root), f.expanded.iter().map(PathBuf::from).collect())).collect();
                s.kept_views.files = kept.files;
                s.kept_views.loaded = true;
                s.side_panel.update(cx, |p, cx| p.seed_files_memory(files, cx));
                // the shown tab's keyboard pane, read just now, is given back as on a switch to it
                s.keyboard_tab = None;
                s.prune_kept_views(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// On each snapshot of the shown place: its tabs' keyboard panes that no longer fit go, and the Files entries
    /// of its workspaces that closed (or whose id herdr gave to another), from the file and the side panel.
    pub(super) fn prune_kept_views(&mut self, cx: &mut Context<Self>) {
        let (Some(key), Some(snapshot)) = (self.shown_key(), self.snapshot.as_ref()) else { return };
        let mut changed = self.keyboard.get_mut(&key.0).is_some_and(|m| session_views::prune_keyboard(m, snapshot));
        if let Some(place) = self.shown_place().filter(|_| self.kept_views.loaded && !snapshot.workspaces.is_empty()) {
            let live: Vec<(&str, String)> = snapshot
                .workspaces
                .iter()
                .map(|w| (w.workspace_id.as_str(), roots::fingerprint(&w.label, w.worktree.as_ref().map(|t| t.checkout_path.as_str()))))
                .collect();
            let before: Vec<String> = self.kept_views.files.keys().cloned().collect();
            if open_files::reconcile(&mut self.kept_views.files, &place, &live, &mut self.kept_views.seen) {
                let gone: Vec<String> = before.into_iter().filter(|k| !self.kept_views.files.contains_key(k)).collect();
                // a closed workspace the side panel remembers too (left this run), though the file never had it
                let prefix = format!("{place}\n");
                let closed = |k: &str| k.strip_prefix(&prefix).is_some_and(|ws| !live.iter().any(|(id, _)| *id == ws));
                let mut forget = gone;
                forget.extend(self.side_panel.read(cx).files_memory_now().into_iter().map(|(k, _, _)| k).filter(|k| closed(k)));
                self.side_panel.update(cx, |p, _| p.forget_files(&forget));
                changed = true;
            }
        }
        if changed {
            self.save_kept_views(cx);
        }
    }

    /// Writes what is kept `SAVE_AFTER` from now (a later change moves it on).
    pub(super) fn save_kept_views(&mut self, cx: &mut Context<Self>) {
        if !self.kept_views.loaded {
            return;
        }
        self.kept_views.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(file_tabs::SAVE_AFTER).await;
            let _ = this.update(cx, |s, cx| s.write_kept_views(cx).map(gpui::Task::detach));
        }));
    }

    /// Starts the write on the background executor when it differs from what the file holds; quitting waits for it.
    pub(super) fn write_kept_views(&mut self, cx: &mut Context<Self>) -> Option<gpui::Task<()>> {
        let text = self.kept_views_text(cx)?;
        if !self.kept_views.writes.start(&text) {
            return None;
        }
        let write = cx
            .background_spawn(async move {
                let saved = open_files::save(&path(), &text);
                if let Err(e) = &saved {
                    eprintln!("shepherd: cannot save session views: {e}");
                }
                (text, saved.is_ok())
            })
            .shared();
        self.kept_views.in_flight = Some(write.clone());
        Some(cx.spawn(async move |this, cx| {
            let (text, ok) = write.await;
            let _ = this.update(cx, |s, cx| {
                s.kept_views.in_flight = None;
                if s.kept_views.writes.finish(text, ok) {
                    s.save_kept_views(cx);
                }
            });
        }))
    }

    /// The file's text for what is kept now (the side panel's open folders with their fingerprints); none before the
    /// file was read.
    fn kept_views_text(&mut self, cx: &mut Context<Self>) -> Option<String> {
        if !self.kept_views.loaded {
            return None;
        }
        let now = self.side_panel.read(cx).files_memory_now();
        let place = self.shown_place();
        let fingerprint = |key: &str| {
            let (p, ws) = key.split_once('\n')?;
            let w = self.snapshot.as_ref()?.workspaces.iter().find(|w| w.workspace_id == ws).filter(|_| place.as_deref() == Some(p))?;
            Some(roots::fingerprint(&w.label, w.worktree.as_ref().map(|t| t.checkout_path.as_str())))
        };
        self.kept_views.files = files_entries(now, &self.kept_views.files, fingerprint);
        let kept = KeptViews { keyboard: self.keyboard.iter().filter(|(_, m)| !m.is_empty()).map(|(k, m)| (k.clone(), m.clone())).collect(), files: self.kept_views.files.clone() };
        Some(encode(&kept))
    }

    /// Quitting: what changed since the last write is written now; a write under way ends first, then the latest is
    /// written after it.
    pub(super) fn flush_kept_views(&mut self, cx: &mut Context<Self>) -> Option<gpui::Task<()>> {
        drop(self.kept_views.save.take());
        if let Some(in_flight) = self.kept_views.in_flight.clone() {
            let text = self.kept_views_text(cx)?;
            return Some(cx.background_spawn(async move {
                if let Err(e) = open_files::write_after(in_flight, path(), text).await {
                    eprintln!("shepherd: cannot save session views: {e}");
                }
            }));
        }
        self.write_kept_views(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::session_views::KeyPane;

    #[test]
    fn kept_views_round_trip_and_a_broken_file_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/session-views.json");
        assert_eq!(load(&file), KeptViews::default(), "no file yet");
        let mut kept = KeptViews::default();
        kept.keyboard.entry("/tmp/a.sock".into()).or_default().insert("w1:t1".into(), KeyPane { pane: "w1:p2".into(), terminal: "term_1".into() });
        kept.files.insert("/tmp/a.sock\nw1".into(), KeptFiles { fingerprint: "label:api".into(), root: "/r".into(), expanded: vec!["/r/src".into()] });
        open_files::save(&file, &encode(&kept)).unwrap();
        assert_eq!(load(&file), kept);
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(load(&file), KeptViews::default());
        // an older or hand-written file without a part reads with it empty
        std::fs::write(&file, r#"{"files": {"k": {"root": "/r"}}}"#).unwrap();
        assert_eq!(load(&file).files["k"].expanded, Vec::<String>::new());
    }

    #[test]
    fn files_entries_keep_their_fingerprint_and_reconcile_drops_a_reused_id() {
        let mut kept = BTreeMap::new();
        kept.insert("p\nw1".to_string(), KeptFiles { fingerprint: "label:api".into(), root: "/r".into(), expanded: Vec::new() });
        let now = vec![("p\nw1".to_string(), PathBuf::from("/r"), vec![PathBuf::from("/r/src")]), ("p\nw2".to_string(), PathBuf::from("/q"), Vec::new())];
        let files = files_entries(now, &kept, |k| (k == "p\nw2").then(|| "label:web".to_string()));
        assert_eq!(files["p\nw1"], KeptFiles { fingerprint: "label:api".into(), root: "/r".into(), expanded: vec!["/r/src".into()] });
        assert_eq!(files["p\nw2"].fingerprint, "label:web");
        // next run: w1's id now names another workspace, w2 closed
        let mut files = files;
        let mut seen = Default::default();
        assert!(open_files::reconcile(&mut files, "p", &[("w1", "label:other".to_string())], &mut seen));
        assert!(files.is_empty());
    }
}
