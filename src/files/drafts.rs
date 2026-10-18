//! Drafts: the unsaved text of an editor's buffer, kept on disk a moment after typing stops (and at once when its
//! tab goes out of sight or Shepherd quits), so a crash or a quit too short to save loses nothing. The next time
//! the file opens its draft comes back over it, as an unsaved change.
//!
//! They live beside the settings file, in `drafts/` (so `SHEPHERD_SETTINGS_FILE` moves them too), the folder 0700
//! and each file 0600: they hold the text as typed, in plain text. A draft is `<key>.draft`, the key the FNV-1a
//! digest of the file's path (the editor's path is already its real one, links resolved): one JSON header line
//! (`Header`), then the text with `\n` line ends. Each is written beside itself and renamed into place
//! (`write_atomic`), so a crash leaves the old draft or the new one, and a temp file that a crash left is swept.
//!
//! A draft knows the version of the file it was typed over (`base_len`, `base_hash`), so the file found changed
//! since (`Restore::Stale`) or gone (`Restore::Deleted`) says so when it comes back. Drafts no file opens again are
//! not listed anywhere; they and unreadable ones go after `MAX_AGE` (`sweep`).
//!
//! A draft is a plain copy of the text in the user's own folder, so a home backup takes it, unencrypted, even for a
//! file from an encrypted or removable volume. Files that look like secrets (`secret`) get none.
//!
//! Two windows or two Shepherds editing one file write one draft: the last writer wins.
use crate::files::save::{self, Baseline};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a draft is kept unopened.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// A temp file older than this is a crash's leftover, never a write under way.
const TEMP_AGE: Duration = Duration::from_secs(10 * 60);
const VERSION: u32 = 1;
const EXTENSION: &str = "draft";

/// The drafts' folder: beside the settings file.
pub fn dir() -> PathBuf {
    crate::settings::Settings::path().with_file_name("drafts")
}

/// A draft's file name for the file at `path`: its path's digest.
pub fn key(path: &Path) -> String {
    format!("{:016x}.{EXTENSION}", save::fnv1a64(&crate::platform::os_bytes(path.as_os_str())))
}

/// What a draft says about itself, its first line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub v: u32,
    /// The file it is for, as text (a path that is not UTF-8 lossily): the key is what finds it.
    pub path: String,
    /// The version of the file the text was typed over: its length and digest, and its modification time.
    pub base_len: u64,
    pub base_hash: u64,
    pub base_mtime_ns: i128,
    /// When it was written, in seconds since the epoch.
    pub saved_at: u64,
}

impl Header {
    /// A header for a draft of `path` typed over `base`, written at `now` (seconds since the epoch).
    pub fn new(path: &Path, base: &Baseline, now: u64) -> Header {
        let seen = save::Seen::of(&base.bytes);
        Header { v: VERSION, path: path.to_string_lossy().into_owned(), base_len: seen.len, base_hash: seen.hash, base_mtime_ns: base.stamp.mtime_ns, saved_at: now }
    }
}

/// A draft as read: its header and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub header: Header,
    pub text: String,
}

/// A draft's bytes: the header's line, then the text.
pub fn encode(header: &Header, text: &str) -> Vec<u8> {
    // JSON escapes a newline in the path: the header stays one line
    let mut out = serde_json::to_vec(header).unwrap_or_default();
    out.push(b'\n');
    out.extend_from_slice(text.as_bytes());
    out
}

/// A draft from its bytes; `None` when it is not one this version reads.
pub fn decode(bytes: &[u8]) -> Option<Draft> {
    let at = bytes.iter().position(|&b| b == b'\n')?;
    let header: Header = serde_json::from_slice(&bytes[..at]).ok()?;
    if header.v != VERSION {
        return None;
    }
    let text = String::from_utf8(bytes[at + 1..].to_vec()).ok()?;
    Some(Draft { header, text })
}

/// How a draft stands against the file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restore {
    /// The file is the version it was typed over.
    Fresh,
    /// The file changed since.
    Stale,
    /// No file there now.
    Deleted,
}

/// `header`'s draft against the file on disk (`None`: there is none).
pub fn classify(header: &Header, disk: Option<&Baseline>) -> Restore {
    match disk {
        None => Restore::Deleted,
        Some(b) if save::Seen::of(&b.bytes) == (save::Seen { len: header.base_len, hash: header.base_hash }) => Restore::Fresh,
        Some(_) => Restore::Stale,
    }
}

/// The draft of the file at `path` in `dir`, if there is one it can read and it is that file's.
pub fn read(dir: &Path, path: &Path) -> Option<Draft> {
    let draft = decode(&fs::read(dir.join(key(path))).ok()?)?;
    (draft.header.path == path.to_string_lossy()).then_some(draft)
}

/// Whether the file at `path` looks like it holds a secret (keys, certificates, an environment's settings, anything
/// in `.ssh` or `.gnupg`): its unsaved text is never copied into a draft.
pub fn secret(path: &Path) -> bool {
    let in_secret_folder = path.components().any(|c| matches!(c.as_os_str().to_str(), Some(".ssh" | ".gnupg")));
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return in_secret_folder };
    let lower = name.to_ascii_lowercase();
    in_secret_folder
        || lower == ".env"
        || lower.starts_with(".env.")
        || lower.starts_with("id_")
        || [".pem", ".key", ".p12"].iter().any(|ext| lower.ends_with(ext))
}

/// The folder, made 0700 from the start when missing (and made so if it is not).
fn ensure_dir(dir: &Path) -> io::Result<()> {
    os::make_private_dir(dir)?;
    os::private_dir(dir)
}

/// Writes `bytes` as `dir/name`: to a temp file beside it (O_EXCL, 0600), synced, then renamed over it.
pub fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    ensure_dir(dir)?;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let temp = dir.join(format!(".{name}.{}-{}.tmp", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let mut file = os::private_file(OpenOptions::new().write(true).create_new(true)).open(&temp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all()).and_then(|()| fs::rename(&temp, dir.join(name)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Removes the draft of the file at `path`; none there is no error.
pub fn remove(dir: &Path, path: &Path) -> io::Result<()> {
    match fs::remove_file(dir.join(key(path))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Removes from `dir` the drafts older than `max_age` at `now` (by the time they say they were written), the ones
/// it cannot read, and the temp files a crash left; what else is there is not its own and is left. The count
/// removed.
pub fn sweep(dir: &Path, now: std::time::SystemTime, max_age: Duration) -> usize {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    let now_secs = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let old = |age: Duration| entry.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok()).is_some_and(|d| d > age);
        let gone = if name.starts_with('.') && name.ends_with(".tmp") {
            old(TEMP_AGE)
        } else if name.ends_with(&format!(".{EXTENSION}")) {
            match fs::read(&path).ok().as_deref().and_then(decode) {
                Some(d) => now_secs.saturating_sub(d.header.saved_at) > max_age.as_secs(),
                // a half-written or older draft
                None => true,
            }
        } else {
            false
        };
        if gone && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The seconds since the epoch at `now`.
pub fn epoch_secs(now: std::time::SystemTime) -> u64 {
    now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// How long before `now` a draft written at `then` was, as a banner says it: "just now", "5 minutes ago".
pub fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    let plural = |n: u64, unit: &str| if n == 1 { format!("1 {unit} ago") } else { format!("{n} {unit}s ago") };
    match secs {
        0..60 => "just now".into(),
        60..3600 => plural(secs / 60, "minute"),
        3600..86_400 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86_400, "day"),
    }
}

/// One editor's draft, written and removed off the main thread in the order asked: each write or removal takes a
/// ticket when asked for, and does its work only if no later one was asked for meanwhile, under a lock held through
/// the rename. So a write still running when the buffer comes back clean never leaves a draft behind the removal.
#[derive(Clone)]
pub struct Slot {
    dir: PathBuf,
    path: PathBuf,
    asked: Arc<AtomicU64>,
    lock: Arc<Mutex<()>>,
}

impl Slot {
    pub fn new(dir: PathBuf, path: PathBuf) -> Slot {
        Slot { dir, path, asked: Arc::default(), lock: Arc::default() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where this file's draft is.
    pub fn file(&self) -> PathBuf {
        self.dir.join(key(&self.path))
    }

    /// A ticket for the next write or removal: any asked for before it no longer does anything.
    pub fn ticket(&self) -> u64 {
        self.asked.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Runs `f` if `ticket` is still the latest asked for, under the lock (blocking); false when it was not.
    fn latest(&self, ticket: u64, f: impl FnOnce() -> io::Result<()>) -> io::Result<bool> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.asked.load(Ordering::SeqCst) != ticket {
            return Ok(false);
        }
        f().map(|()| true)
    }

    /// Writes the draft (blocking), if `ticket` is still the latest.
    pub fn write(&self, ticket: u64, header: &Header, text: &str) -> io::Result<bool> {
        self.latest(ticket, || write_atomic(&self.dir, &key(&self.path), &encode(header, text)))
    }

    /// Removes the draft (blocking), if `ticket` is still the latest.
    pub fn remove(&self, ticket: u64) -> io::Result<bool> {
        self.latest(ticket, || remove(&self.dir, &self.path))
    }
}

#[cfg(unix)]
mod os {
    use std::fs::{self, DirBuilder, OpenOptions, Permissions};
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    /// Made 0700 (its missing parents too), never open to others for a moment.
    pub(super) fn make_private_dir(dir: &Path) -> io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }

    pub(super) fn private_dir(dir: &Path) -> io::Result<()> {
        if fs::metadata(dir)?.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(dir, Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub(super) fn private_file(options: &mut OpenOptions) -> &mut OpenOptions {
        options.mode(0o600)
    }
}

/// Off unix the folder is the user's profile's, as private as the system makes it.
#[cfg(not(unix))]
mod os {
    use std::fs::OpenOptions;
    use std::io;
    use std::path::Path;

    pub(super) fn make_private_dir(dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)
    }

    pub(super) fn private_dir(_: &Path) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn private_file(options: &mut OpenOptions) -> &mut OpenOptions {
        options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline(bytes: &[u8]) -> Baseline {
        Baseline::new(bytes.to_vec(), save::Stamp { len: bytes.len() as u64, mtime_ns: 7, ctime_ns: 7, ino: 1, dev: 1 })
    }

    #[test]
    fn a_header_round_trips_on_one_line() {
        let path = Path::new("/w/odd\nname \"q\".rs");
        let header = Header::new(path, &baseline(b"fn a() {}\n"), 1_700_000_000);
        let bytes = encode(&header, "fn b() {}\nlast");
        let line = bytes.iter().position(|&b| b == b'\n').unwrap();
        assert!(bytes[line + 1..].starts_with(b"fn b() {}"), "the header is one line");
        let back = decode(&bytes).unwrap();
        assert_eq!(back.header, header);
        assert_eq!(back.header.path, "/w/odd\nname \"q\".rs");
        assert_eq!(back.text, "fn b() {}\nlast");
        // an empty text, another version, a broken header
        assert_eq!(decode(&encode(&header, "")).unwrap().text, "");
        let other = Header { v: 2, ..header.clone() };
        assert_eq!(decode(&encode(&other, "x")), None);
        assert_eq!(decode(b"{not json\nx"), None);
        assert_eq!(decode(b"no header line"), None);
    }

    #[test]
    fn a_draft_is_fresh_stale_or_deleted_against_the_disk() {
        let header = Header::new(Path::new("/w/a.rs"), &baseline(b"one\n"), 0);
        assert_eq!(classify(&header, Some(&baseline(b"one\n"))), Restore::Fresh);
        assert_eq!(classify(&header, Some(&baseline(b"two\n"))), Restore::Stale, "same length, other bytes");
        assert_eq!(classify(&header, Some(&baseline(b"one\nmore\n"))), Restore::Stale);
        assert_eq!(classify(&header, None), Restore::Deleted);
    }

    #[test]
    fn the_key_is_the_path_s_digest() {
        // golden: the same file has the same draft on every run and platform
        assert_eq!(key(Path::new("/w/a.rs")), format!("{:016x}.draft", save::fnv1a64(b"/w/a.rs")));
        assert_eq!(key(Path::new("")), "cbf29ce484222325.draft");
        assert_ne!(key(Path::new("/w/a.rs")), key(Path::new("/w/b.rs")));
    }

    #[cfg(unix)]
    #[test]
    fn a_draft_is_written_whole_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("drafts");
        let path = Path::new("/w/a.rs");
        let header = Header::new(path, &baseline(b"one\n"), 5);
        let slot = Slot::new(dir.clone(), path.to_path_buf());
        assert!(slot.write(slot.ticket(), &header, "mine\n").unwrap());
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&slot.file()), 0o600);
        // nothing but the draft: no temp left
        let names: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, [key(path)]);
        assert_eq!(read(&dir, path).unwrap().text, "mine\n");
        // another file's path under the same key is not this file's
        assert_eq!(read(&dir, Path::new("/w/b.rs")), None);
        // a folder made by someone else, too open: made private
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        write_atomic(&dir, "x.draft", b"x").unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn a_write_or_removal_asked_for_before_another_does_nothing() {
        let home = tempfile::tempdir().unwrap();
        let path = Path::new("/w/a.rs");
        let header = Header::new(path, &baseline(b"one\n"), 5);
        let slot = Slot::new(home.path().join("drafts"), path.to_path_buf());
        let (first, second) = (slot.ticket(), slot.ticket());
        assert!(slot.write(second, &header, "newer\n").unwrap());
        // the older write landing late leaves the newer one
        assert!(!slot.write(first, &header, "older\n").unwrap());
        assert_eq!(read(slot.dir(), path).unwrap().text, "newer\n");
        // a removal asked for after a write still to run: the write does nothing, the removal does
        let (write, removal) = (slot.ticket(), slot.ticket());
        assert!(slot.remove(removal).unwrap());
        assert!(!slot.write(write, &header, "late\n").unwrap());
        assert!(!slot.file().exists());
        assert!(slot.remove(slot.ticket()).unwrap(), "removing none is fine");
    }

    #[test]
    fn the_sweep_takes_old_and_unreadable_drafts_and_crash_leftovers() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("drafts");
        let now = std::time::SystemTime::now();
        let secs = epoch_secs(now);
        let write = |path: &str, saved_at: u64| write_atomic(&dir, &key(Path::new(path)), &encode(&Header::new(Path::new(path), &baseline(b""), saved_at), "x")).unwrap();
        write("/w/new.rs", secs - 60);
        write("/w/month.rs", secs - MAX_AGE.as_secs() + 3600);
        write("/w/old.rs", secs - MAX_AGE.as_secs() - 1);
        fs::write(dir.join("0000000000000000.draft"), b"half a he").unwrap();
        fs::write(dir.join(".x.draft.1-2.tmp"), b"x").unwrap();
        fs::write(dir.join("notes.txt"), b"not ours").unwrap();
        // a temp file just made is a write under way: kept
        assert_eq!(sweep(&dir, now, MAX_AGE), 2);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        let mut want = vec![key(Path::new("/w/new.rs")), key(Path::new("/w/month.rs")), ".x.draft.1-2.tmp".into(), "notes.txt".into()];
        want.sort();
        assert_eq!(left, want);
        // later, the temp file is a crash's
        assert_eq!(sweep(&dir, now + TEMP_AGE + Duration::from_secs(1), MAX_AGE), 1);
        assert!(!dir.join(".x.draft.1-2.tmp").exists());
        assert_eq!(sweep(&home.path().join("none"), now, MAX_AGE), 0, "no folder yet");
    }

    #[test]
    fn secret_looking_files_get_no_draft() {
        for path in ["/w/.env", "/w/.env.local", "/w/.ENV.production", "/w/cert.pem", "/w/server.key", "/w/store.p12", "/h/.ssh/config", "/h/.ssh/known_hosts", "/h/.gnupg/gpg.conf", "/h/id_ed25519", "/h/id_rsa.pub"] {
            assert!(secret(Path::new(path)), "{path}");
        }
        for path in ["/w/a.rs", "/w/env.rs", "/w/.envrc", "/w/keys.rs", "/w/README.md", "/w/ssh/config", "/w/pem.txt", "/w/valid_id.rs"] {
            assert!(!secret(Path::new(path)), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_folder_is_private_from_its_making() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("config/drafts");
        os::make_private_dir(&dir).unwrap();
        // checked before anything could have changed it afterwards
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn a_draft_s_age_reads_as_words() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100, 200), "just now", "a clock that went back");
        assert_eq!(ago(160, 100), "1 minute ago");
        assert_eq!(ago(100 + 5 * 60 + 30, 100), "5 minutes ago");
        assert_eq!(ago(2 * 3600, 0), "2 hours ago");
        assert_eq!(ago(86_400 * 3, 0), "3 days ago");
    }
}
