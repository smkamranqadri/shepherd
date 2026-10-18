//! One watch per folder for the editors' files: each open editor asks to hear of changes to its file's name in its
//! folder (`subscribe`), and the editors in one folder share one `notify` watcher on it (not its tree), made when the
//! first asks and dropped with the last one's `Subscription`. An event naming a file is handed to the editors of that
//! file; an event naming nothing (a rescan) or an error to every editor in the folder, which then look for themselves.
//!
//! Opens and reads (the editors' own looks among them) are not changes and are dropped; a write's close is one.
//!
//! A folder deleted and made again is not watched again: inotify lets go of the old folder, and the watch hears
//! nothing more. The editors then hear of changes when their tab shows or the window comes to the front, as they do
//! for a folder that can't be watched at all; the next editor to open a file there after the last one closed watches
//! it afresh.
//!
//! The watcher is made and dropped outside the table's lock: notify's watch and drop wait on its event thread, which
//! takes the lock to hand out an event.
use ::notify::event::{AccessKind, AccessMode};
use ::notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// An editor's ear in a folder.
struct Ear {
    id: u64,
    name: OsString,
    tx: UnboundedSender<()>,
}

/// A folder's watcher and its editors.
struct Folder {
    watcher: Option<RecommendedWatcher>,
    ears: Vec<Ear>,
}

type Table = Arc<Mutex<HashMap<PathBuf, Folder>>>;

/// The folders watched, each with its editors.
#[derive(Clone, Default)]
pub struct Watches {
    table: Table,
    ids: Arc<AtomicU64>,
}

/// The process's watches.
pub fn global() -> &'static Watches {
    static GLOBAL: OnceLock<Watches> = OnceLock::new();
    GLOBAL.get_or_init(Watches::default)
}

/// Whether `event` is a change to the file `name` (`None`: to every file of the folder).
fn names(event: &::notify::Result<Event>) -> Option<Vec<&OsStr>> {
    match event {
        // an open or a read changes nothing; a write's close does
        Ok(ev) if matches!(ev.kind, EventKind::Access(AccessKind::Close(AccessMode::Write))) => Some(ev.paths.iter().filter_map(|p| p.file_name()).collect()),
        Ok(ev) if matches!(ev.kind, EventKind::Access(_)) => Some(Vec::new()),
        // a rescan names nothing: everyone looks
        Ok(ev) if ev.paths.is_empty() => None,
        Ok(ev) => Some(ev.paths.iter().filter_map(|p| p.file_name()).collect()),
        Err(_) => None,
    }
}

/// Hands `event` in `dir` to the editors it is for.
fn deliver(table: &Table, dir: &Path, event: &::notify::Result<Event>) {
    let names = names(event);
    let table = table.lock().unwrap_or_else(|e| e.into_inner());
    let Some(folder) = table.get(dir) else { return };
    for ear in &folder.ears {
        if names.as_ref().is_none_or(|n| n.contains(&ear.name.as_os_str())) {
            let _ = ear.tx.send(());
        }
    }
}

/// An editor's place among a folder's: dropping it lets go, and the last one's drop ends the folder's watch.
pub struct Subscription {
    watches: Watches,
    dir: PathBuf,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let gone = {
            let mut table = self.watches.table.lock().unwrap_or_else(|e| e.into_inner());
            let Some(folder) = table.get_mut(&self.dir) else { return };
            folder.ears.retain(|e| e.id != self.id);
            if folder.ears.is_empty() { table.remove(&self.dir) } else { None }
        };
        // the watcher goes outside the lock
        drop(gone);
    }
}

impl Watches {
    /// Hears of changes to `name` in `dir` until the subscription is dropped (blocking: making a watch can hang on a
    /// dead mount). A folder that can't be watched still subscribes, and hears nothing.
    pub fn subscribe(&self, dir: &Path, name: &OsStr) -> (Subscription, UnboundedReceiver<()>) {
        let (tx, rx) = unbounded_channel();
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        let ear = Ear { id, name: name.to_os_string(), tx };
        let subscription = Subscription { watches: self.clone(), dir: dir.to_path_buf(), id };
        {
            let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(folder) = table.get_mut(dir) {
                folder.ears.push(ear);
                return (subscription, rx);
            }
        }
        let watcher = self.watcher(dir);
        let spare = {
            let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
            match table.get_mut(dir) {
                // another editor made one meanwhile: that one serves both
                Some(folder) => {
                    folder.ears.push(ear);
                    watcher
                }
                None => {
                    table.insert(dir.to_path_buf(), Folder { watcher, ears: vec![ear] });
                    None
                }
            }
        };
        drop(spare);
        (subscription, rx)
    }

    fn watcher(&self, dir: &Path) -> Option<RecommendedWatcher> {
        let (table, at) = (self.table.clone(), dir.to_path_buf());
        let mut watcher = ::notify::recommended_watcher(move |event: ::notify::Result<Event>| deliver(&table, &at, &event)).ok()?;
        watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
        Some(watcher)
    }

    /// The folders watched now, and whether each has a working watcher.
    pub fn folders(&self) -> Vec<(PathBuf, bool, usize)> {
        let table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        table.iter().map(|(dir, f)| (dir.clone(), f.watcher.is_some(), f.ears.len())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Waits a while for one event on `rx`.
    fn heard(rx: &mut UnboundedReceiver<()>) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if rx.try_recv().is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn drain(rx: &mut UnboundedReceiver<()>) {
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
    }

    #[test]
    fn editors_in_one_folder_share_a_watch_that_goes_with_the_last() {
        let watches = Watches::default();
        let dir = tempfile::tempdir().unwrap();
        let (a, mut a_rx) = watches.subscribe(dir.path(), OsStr::new("a.txt"));
        let (b, mut b_rx) = watches.subscribe(dir.path(), OsStr::new("b.txt"));
        let (a2, mut a2_rx) = watches.subscribe(dir.path(), OsStr::new("a.txt"));
        assert_eq!(watches.folders(), [(dir.path().to_path_buf(), true, 3)], "one watch for the folder");
        // a change to a.txt reaches its two editors, not b.txt's
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        assert!(heard(&mut a_rx) && heard(&mut a2_rx));
        drain(&mut b_rx);
        std::fs::write(dir.path().join("c.txt"), "x").unwrap();
        drain(&mut a_rx);
        std::fs::write(dir.path().join("b.txt"), "x").unwrap();
        assert!(heard(&mut b_rx));
        drop(a);
        drop(b);
        assert_eq!(watches.folders().len(), 1, "a2 still listens");
        drop(a2);
        assert!(watches.folders().is_empty(), "the last one gone, the watch goes");
        // asked again, the folder is watched afresh
        let (_c, mut c_rx) = watches.subscribe(dir.path(), OsStr::new("a.txt"));
        std::fs::write(dir.path().join("a.txt"), "y").unwrap();
        assert!(heard(&mut c_rx));
    }

    #[test]
    fn reads_are_not_changes_and_a_rescan_reaches_everyone() {
        let ev = |kind| Ok(Event::new(kind).add_path(PathBuf::from("/d/a.txt")));
        assert_eq!(names(&ev(EventKind::Access(AccessKind::Open(AccessMode::Any)))), Some(Vec::new()));
        assert_eq!(names(&ev(EventKind::Access(AccessKind::Close(AccessMode::Write)))), Some(vec![OsStr::new("a.txt")]));
        assert_eq!(names(&ev(EventKind::Any)), Some(vec![OsStr::new("a.txt")]));
        assert_eq!(names(&Ok(Event::new(EventKind::Any))), None);
        assert_eq!(names(&Err(::notify::Error::generic("x"))), None);
    }

    #[test]
    fn a_folder_that_cant_be_watched_still_subscribes() {
        let watches = Watches::default();
        let (s, _rx) = watches.subscribe(Path::new("/nonexistent/shepherd-test-folder"), OsStr::new("a"));
        assert_eq!(watches.folders(), [(PathBuf::from("/nonexistent/shepherd-test-folder"), false, 1)]);
        drop(s);
        assert!(watches.folders().is_empty());
    }
}
