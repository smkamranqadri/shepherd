//! herdr's sessions folder watched, so a session started or stopped elsewhere (a terminal's `herdr --session`, a
//! server that quit) shows in the sidebar within a moment rather than at the next 15 s read of herdr's list. herdr
//! keeps the default session's socket in its folder (`herdr.sock`) and each named one's in `sessions/<name>/`; a
//! server makes its socket when it starts and removes it when it stops. Only those folders are watched, each by
//! itself (`notify`, non-recursive): the logs, client sockets, `session.json` and snapshots in them change often and
//! say nothing about which sessions run (`relevant`). What changed is not read here: a change only asks for herdr's
//! list again, which stays the one source of truth (and the 15 s read stays as the fallback).
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};

/// A server's socket: the file whose coming and going is a session starting and stopping.
const SOCKET: &str = "herdr.sock";
/// The named sessions' folder in herdr's.
const SESSIONS: &str = "sessions";

/// What an event says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// A session may have started or stopped: herdr's list is to be read again.
    Changed,
    /// A folder appeared that holds sessions or a session's socket (`sessions/`, or `sessions/<name>/`): it is to be
    /// watched too, and the list read again (its socket may be there before the watch is).
    NewDir(PathBuf),
    Ignore,
}

/// The entry at the path came (made, or moved in).
fn appeared(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Any | EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Any)))
}

/// The entry at the path went (deleted, or moved away).
fn departed(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Remove(_) | EventKind::Any | EventKind::Modify(ModifyKind::Name(RenameMode::From | RenameMode::Any | RenameMode::Both)))
}

/// What one event's path means for herdr's folder `root`. Only a socket named `herdr.sock` coming or going (in
/// `root` or in `sessions/<name>/`), and a session's folder coming or going, matter; writes never do (a log line, a
/// snapshot), nor `herdr-client.sock`, which every attached terminal makes and removes.
pub fn relevant(kind: &EventKind, path: &Path, root: &Path) -> Seen {
    let (came, went) = (appeared(kind), departed(kind));
    if !came && !went {
        return Seen::Ignore;
    }
    let sessions = root.join(SESSIONS);
    if path == root.join(SOCKET) {
        return Seen::Changed;
    }
    if path == sessions {
        return if came { Seen::NewDir(path.to_path_buf()) } else { Seen::Changed };
    }
    let Ok(rel) = path.strip_prefix(&sessions) else {
        return Seen::Ignore;
    };
    let mut parts = rel.components();
    match (parts.next(), parts.next(), parts.next()) {
        (Some(_), None, None) if came => Seen::NewDir(path.to_path_buf()),
        (Some(_), None, None) => Seen::Changed,
        (Some(_), Some(file), None) if file.as_os_str() == SOCKET => Seen::Changed,
        _ => Seen::Ignore,
    }
}

/// What each of an event's paths says, the ignored left out; lost events (the kernel's queue overflowed, a rescan)
/// are a change, since a socket may have come or gone among them.
pub fn seen(event: &Event, root: &Path) -> Vec<Seen> {
    if event.need_rescan() {
        return vec![Seen::Changed];
    }
    event.paths.iter().map(|p| relevant(&event.kind, p, root)).filter(|s| *s != Seen::Ignore).collect()
}

/// The folders to watch under `root` as it is now: itself, its `sessions/` when there, and each folder in that.
/// Empty when `root` is not there (herdr never ran): nothing to watch, and the 15 s read covers a first start.
pub fn folders(root: &Path) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut dirs = vec![root.to_path_buf()];
    let sessions = root.join(SESSIONS);
    if let Ok(entries) = std::fs::read_dir(&sessions) {
        dirs.push(sessions);
        let mut named: Vec<PathBuf> = entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
        named.sort();
        dirs.extend(named);
    }
    dirs
}

/// What a burst of `Seen` asks: whether to read the list, and the new folders to watch.
pub fn gather(burst: impl IntoIterator<Item = Seen>) -> (bool, Vec<PathBuf>) {
    let (mut read, mut dirs) = (false, Vec::new());
    for seen in burst {
        match seen {
            Seen::Changed => read = true,
            Seen::NewDir(dir) => {
                read = true;
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
            Seen::Ignore => {}
        }
    }
    (read, dirs)
}

/// The live watch: dropping it stops the events.
pub struct SessionWatch {
    watcher: RecommendedWatcher,
}

impl SessionWatch {
    /// Watches herdr's folder; its `Seen`s arrive on the receiver. `None` when there is nothing to watch or the
    /// watcher cannot start: the list is then only read every 15 s, as before.
    pub fn start() -> Option<(SessionWatch, UnboundedReceiver<Seen>)> {
        Self::start_at(super::config::dir())
    }

    fn start_at(root: PathBuf) -> Option<(SessionWatch, UnboundedReceiver<Seen>)> {
        // FSEvents names paths with links resolved (`/var` is `/private/var`), and a dotfiles setup may link herdr's
        // folder: the events' paths are matched against the folder as resolved
        let root = root.canonicalize().unwrap_or(root);
        let dirs = folders(&root);
        if dirs.is_empty() {
            return None;
        }
        let (tx, rx): (UnboundedSender<Seen>, _) = unbounded();
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            // an error is the kernel's queue overflowing or a watch lost: read the list to be sure
            let seen = event.map(|e| seen(&e, &root)).unwrap_or_else(|_| vec![Seen::Changed]);
            for s in seen {
                let _ = tx.unbounded_send(s);
            }
        })
        .ok()?;
        let mut watch = SessionWatch { watcher };
        // herdr's own folder must be watched; a session folder that went meanwhile is let be
        watch.watcher.watch(&dirs[0], RecursiveMode::NonRecursive).ok()?;
        for dir in &dirs[1..] {
            watch.add(dir);
        }
        Some((watch, rx))
    }

    /// Watches a folder that appeared. Not called from the watcher's callback: notify's watch waits on the thread
    /// that runs it.
    pub fn add(&mut self, dir: &Path) {
        let _ = self.watcher.watch(dir, RecursiveMode::NonRecursive);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, RemoveKind};

    fn root() -> PathBuf {
        PathBuf::from("/home/sam/.config/herdr")
    }

    const MADE: EventKind = EventKind::Create(CreateKind::File);
    const GONE: EventKind = EventKind::Remove(RemoveKind::File);
    const WRITTEN: EventKind = EventKind::Modify(ModifyKind::Data(DataChange::Content));

    #[test]
    fn a_sessions_socket_coming_or_going_is_a_change() {
        let sock = root().join("sessions/pets/herdr.sock");
        assert_eq!(relevant(&MADE, &sock, &root()), Seen::Changed);
        assert_eq!(relevant(&GONE, &sock, &root()), Seen::Changed);
        assert_eq!(relevant(&EventKind::Modify(ModifyKind::Name(RenameMode::To)), &sock, &root()), Seen::Changed);
        assert_eq!(relevant(&MADE, &root().join("herdr.sock"), &root()), Seen::Changed, "the default session");
        assert_eq!(relevant(&GONE, &root().join("herdr.sock"), &root()), Seen::Changed);
    }

    #[test]
    fn logs_snapshots_and_client_sockets_are_ignored() {
        let pets = root().join("sessions/pets");
        assert_eq!(relevant(&WRITTEN, &pets.join("herdr-server.log"), &root()), Seen::Ignore);
        assert_eq!(relevant(&MADE, &pets.join("herdr-server.log"), &root()), Seen::Ignore);
        assert_eq!(relevant(&MADE, &pets.join("herdr-client.sock"), &root()), Seen::Ignore);
        assert_eq!(relevant(&GONE, &pets.join("herdr-client.sock"), &root()), Seen::Ignore);
        assert_eq!(relevant(&MADE, &root().join("herdr-client.sock"), &root()), Seen::Ignore);
        assert_eq!(relevant(&MADE, &pets.join("session.json"), &root()), Seen::Ignore);
        assert_eq!(relevant(&MADE, &pets.join("session-snapshots"), &root()), Seen::Ignore);
        assert_eq!(relevant(&MADE, &pets.join("session-snapshots/1.json"), &root()), Seen::Ignore);
        assert_eq!(relevant(&WRITTEN, &root().join("herdr.sock"), &root()), Seen::Ignore, "a write is no start");
        assert_eq!(relevant(&MADE, &root().join("config.toml"), &root()), Seen::Ignore);
        assert_eq!(relevant(&MADE, Path::new("/tmp/herdr.sock"), &root()), Seen::Ignore);
    }

    #[test]
    fn a_new_session_folder_is_watched_and_a_gone_one_is_a_change() {
        let pets = root().join("sessions/pets");
        assert_eq!(relevant(&EventKind::Create(CreateKind::Folder), &pets, &root()), Seen::NewDir(pets.clone()));
        assert_eq!(relevant(&EventKind::Remove(RemoveKind::Folder), &pets, &root()), Seen::Changed);
        let sessions = root().join("sessions");
        assert_eq!(relevant(&EventKind::Create(CreateKind::Folder), &sessions, &root()), Seen::NewDir(sessions), "herdr's first named session");
    }

    #[test]
    fn a_burst_reads_once_and_watches_each_new_folder_once() {
        let pets = root().join("sessions/pets");
        assert_eq!(gather([Seen::Changed, Seen::Changed]), (true, vec![]));
        assert_eq!(gather([Seen::NewDir(pets.clone()), Seen::Changed, Seen::NewDir(pets.clone())]), (true, vec![pets]));
        assert_eq!(gather([]), (false, vec![]));
    }

    /// The next `Seen` the watch sends, waiting at most a few seconds (FSEvents batches its events).
    fn next(rx: &mut UnboundedReceiver<Seen>) -> Option<Seen> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(seen) = rx.try_recv() {
                return Some(seen);
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn a_watch_hears_a_session_folder_and_then_its_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("herdr");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        let (mut watch, mut rx) = SessionWatch::start_at(root.clone()).unwrap();
        let pets = root.join("sessions/pets");
        std::fs::create_dir(&pets).unwrap();
        assert_eq!(next(&mut rx), Some(Seen::NewDir(pets.clone())));
        watch.add(&pets);
        std::fs::write(pets.join("herdr-server.log"), "started").unwrap();
        std::fs::write(pets.join("herdr.sock"), "").unwrap();
        assert_eq!(next(&mut rx), Some(Seen::Changed), "the log is not heard, the socket is");
        std::fs::remove_file(pets.join("herdr.sock")).unwrap();
        assert_eq!(next(&mut rx), Some(Seen::Changed));
    }

    #[test]
    fn the_folders_watched_are_herdrs_its_sessions_and_each_session() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("herdr");
        assert!(folders(&root).is_empty(), "herdr never ran");
        std::fs::create_dir(&root).unwrap();
        assert_eq!(folders(&root), vec![root.clone()], "no named session yet");
        std::fs::create_dir_all(root.join("sessions/pets/session-snapshots")).unwrap();
        std::fs::create_dir(root.join("sessions/alpha")).unwrap();
        std::fs::write(root.join("sessions/notes.txt"), "").unwrap();
        let want = vec![root.clone(), root.join("sessions"), root.join("sessions/alpha"), root.join("sessions/pets")];
        assert_eq!(folders(&root), want, "not the snapshots inside a session, nor a file");
    }
}
