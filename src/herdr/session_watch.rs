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

/// Each folder watched by itself (inotify: a watch per folder, from a per-user budget), rather than herdr's folder
/// whole by one stream (FSEvents on macOS, and Windows), as `files::watch` does: there `add` has nothing to do.
const PER_FOLDER: bool = cfg!(target_os = "linux");

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

/// What the watcher hands over: an event's `seen`, or for an error (the kernel's queue overflowing, a watch lost) a
/// change, so the list is read to be sure.
pub fn heard(event: notify::Result<Event>, root: &Path) -> Vec<Seen> {
    event.map(|e| seen(&e, root)).unwrap_or_else(|_| vec![Seen::Changed])
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

/// What a burst of `Seen` asks: whether to read the list, and whether folders came that are to be watched
/// (`catch_up`).
pub fn gather(burst: impl IntoIterator<Item = Seen>) -> (bool, bool) {
    burst.into_iter().fold((false, false), |(read, new), seen| match seen {
        Seen::Changed => (true, new),
        Seen::NewDir(_) => (true, true),
        Seen::Ignore => (read, new),
    })
}

/// Why there is no watch.
#[derive(Debug, PartialEq, Eq)]
pub enum NoWatch {
    /// herdr's folder is not there yet (herdr never ran): worth trying again once it may be.
    NotYet,
    /// The watcher could not start (the system's limit on watches, say): the 15 s read is all there is.
    Failed,
}

/// The live watch: dropping it stops the events.
pub struct SessionWatch {
    watcher: RecommendedWatcher,
    root: PathBuf,
}

impl SessionWatch {
    /// Watches herdr's folder; its `Seen`s arrive on the receiver. Without a watch the list is only read every 15 s,
    /// as before.
    pub fn start() -> Result<(SessionWatch, UnboundedReceiver<Seen>), NoWatch> {
        Self::start_at(super::config::dir())
    }

    fn start_at(root: PathBuf) -> Result<(SessionWatch, UnboundedReceiver<Seen>), NoWatch> {
        if !root.is_dir() {
            return Err(NoWatch::NotYet);
        }
        // FSEvents names paths with links resolved (`/var` is `/private/var`), and a dotfiles setup may link herdr's
        // folder: the events' paths are matched against the folder as resolved
        let root = root.canonicalize().unwrap_or(root);
        let (tx, rx): (UnboundedSender<Seen>, _) = unbounded();
        let events_root = root.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            for s in heard(event, &events_root) {
                let _ = tx.unbounded_send(s);
            }
        })
        .map_err(|_| NoWatch::Failed)?;
        let mut watch = SessionWatch { watcher, root };
        // herdr's own folder must be watched (the whole of it, where one stream watches a tree)
        let mode = if PER_FOLDER { RecursiveMode::NonRecursive } else { RecursiveMode::Recursive };
        watch.watcher.watch(&watch.root, mode).map_err(|_| NoWatch::Failed)?;
        watch.catch_up();
        Ok((watch, rx))
    }

    /// Watches every folder there is to watch now (inotify), after folders came: `sessions/` before the folders in
    /// it are listed, so one made meanwhile is heard of. A burst can make `sessions/` and `sessions/<name>/` before
    /// the first is watched, so the folders in it are not left to their own events. Watching a folder again does no
    /// harm, and one that went meanwhile is let be. Not called from the watcher's callback: notify's watch waits on
    /// the thread that runs it.
    pub fn catch_up(&mut self) {
        if !PER_FOLDER {
            return;
        }
        let _ = self.watcher.watch(&self.root.join(SESSIONS), RecursiveMode::NonRecursive);
        for dir in folders(&self.root).iter().skip(1) {
            let _ = self.watcher.watch(dir, RecursiveMode::NonRecursive);
        }
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
    fn a_burst_reads_once_and_catches_up_on_new_folders() {
        let pets = root().join("sessions/pets");
        assert_eq!(gather([Seen::Changed, Seen::Changed]), (true, false));
        assert_eq!(gather([Seen::Changed, Seen::NewDir(pets.clone()), Seen::Changed]), (true, true));
        assert_eq!(gather([Seen::NewDir(pets)]), (true, true), "a new folder alone is a read too");
        assert_eq!(gather([]), (false, false));
    }

    #[test]
    fn lost_events_and_errors_are_a_change() {
        let lost = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert_eq!(seen(&lost, &root()), [Seen::Changed], "the kernel's queue overflowed");
        assert_eq!(heard(Ok(lost), &root()), [Seen::Changed]);
        assert_eq!(heard(Err(notify::Error::generic("a watch was lost")), &root()), [Seen::Changed]);
        let log = Event::new(WRITTEN).add_path(root().join("sessions/pets/herdr-server.log"));
        assert!(heard(Ok(log), &root()).is_empty());
        let both = Event::new(MADE).add_path(root().join("herdr.sock")).add_path(root().join("herdr-client.sock"));
        assert_eq!(heard(Ok(both), &root()), [Seen::Changed], "each path judged, the ignored left out");
    }

    /// Waits a few seconds at most for a `Seen` that `wanted` picks, passing over the others: FSEvents batches its
    /// events, may merge or repeat them, and can deliver ones from just before the watch began.
    fn wait_for(rx: &mut UnboundedReceiver<Seen>, wanted: impl Fn(&Seen) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(seen) if wanted(&seen) => return true,
                Ok(_) => {}
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        false
    }

    fn temp_root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("herdr");
        (tmp, root)
    }

    #[test]
    fn a_watch_hears_a_session_folder_and_then_its_socket() {
        let (_tmp, root) = temp_root();
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        let (mut watch, mut rx) = SessionWatch::start_at(root.clone()).unwrap();
        let pets = root.join("sessions/pets");
        std::fs::create_dir(&pets).unwrap();
        assert!(wait_for(&mut rx, |s| *s == Seen::NewDir(pets.clone())), "the new session's folder");
        watch.catch_up();
        std::fs::write(pets.join("herdr-server.log"), "started").unwrap();
        std::fs::write(pets.join("herdr.sock"), "").unwrap();
        assert!(wait_for(&mut rx, |s| *s == Seen::Changed), "its socket made");
        std::fs::remove_file(pets.join("herdr.sock")).unwrap();
        assert!(wait_for(&mut rx, |s| *s == Seen::Changed), "its socket gone");
    }

    #[test]
    fn folders_made_before_their_parent_is_watched_are_caught_up_on() {
        let (_tmp, root) = temp_root();
        std::fs::create_dir(&root).unwrap();
        let (mut watch, mut rx) = SessionWatch::start_at(root.clone()).unwrap();
        // herdr's first named session: sessions/ and sessions/pets back to back, the second before sessions/ is watched
        let pets = root.join("sessions/pets");
        std::fs::create_dir_all(&pets).unwrap();
        assert!(wait_for(&mut rx, |s| *s == Seen::NewDir(root.join("sessions"))));
        watch.catch_up();
        // whatever else the burst brought is passed over, so a socket heard now was heard in pets/
        while rx.try_recv().is_ok() {}
        std::fs::write(pets.join("herdr.sock"), "").unwrap();
        assert!(wait_for(&mut rx, |s| *s == Seen::Changed), "pets/ is watched though no event named it");
    }

    #[test]
    fn no_herdr_folder_is_no_watch_yet() {
        let (_tmp, root) = temp_root();
        assert_eq!(SessionWatch::start_at(root.clone()).err(), Some(NoWatch::NotYet));
        std::fs::create_dir(&root).unwrap();
        assert!(SessionWatch::start_at(root).is_ok(), "once herdr made it");
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
