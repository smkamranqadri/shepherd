//! Saving a text file without losing it: the new text is written beside the file (a temp file in the same folder,
//! made with O_EXCL), synced, checked against what was opened, and renamed over it; then the folder is synced. A
//! crash or a full disk leaves the old file or the new one, never half of either. The file keeps its line endings,
//! its byte order mark and its mode; a symlink stays a link (its target is what gets written); a hard-linked file, one
//! of another owner or group, or one in a folder Shepherd can't write to is refused, and the caller may ask to write
//! it in place instead (`SaveMode::InPlace`), which keeps the inode and so the links and owner.
//!
//! What is not carried over to the new inode by a safe save: extended attributes (macOS Finder tags and quarantine,
//! Linux `user.*`), ACLs, SELinux labels, BSD file flags, and the setuid/setgid bits (as a write by a non-root user
//! clears them anyway). Files that need any of those can be saved in place.
//!
//! Platforms: `File::sync_all` is fsync on Linux and F_FULLFSYNC on macOS (through to the platter, which plain fsync
//! on macOS is not). Syncing the folder makes the rename durable on Linux; on macOS it is best effort (APFS orders
//! the rename with the data already flushed). Modification times are as fine as the file system and the kernel's
//! clock tick make them: nanoseconds on APFS, a jiffy on Linux, a second on HFS+, two on FAT, the server's on NFS.
//! So a stamp is only trusted once it is older than `GRANULARITY_NS`, and a save never trusts one: it always
//! compares the bytes on disk with the baseline, and right before the rename checks that the path is still the file
//! it compared, untouched.
//!
//! Off unix (Windows) a file has no owner, group, mode bits or inode number to keep or compare (`os`): the stamp is
//! the length and modification time, a read-only file is refused, and a deleted file comes back through a hard link
//! or an exclusive create, as on a unix file system without a no-replace rename.
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const BOM: &[u8] = b"\xEF\xBB\xBF";
/// How long after a change a file's stamp may still be the one a further change leaves (a write in the same clock
/// tick, or the same second on HFS+; FAT's two seconds; a remote file system's clock).
const GRANULARITY_NS: i128 = 2_000_000_000;
/// The longest file name the file systems Shepherd runs on take, in bytes.
const NAME_MAX: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
    Cr,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    /// As a status bar shows it.
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::Crlf => "CRLF",
            LineEnding::Cr => "CR",
        }
    }
}

/// How a text ends its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineFormat {
    /// The most common line end (LF on a tie, then CRLF; LF with none), which a save writes everywhere.
    pub ending: LineEnding,
    /// The text ends with a line end.
    pub final_newline: bool,
    /// More than one kind of line end is in it (a save makes them all `ending`).
    pub mixed: bool,
}

/// `text`'s line ends, counted.
pub fn line_format(text: &[u8]) -> LineFormat {
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'\r' if text.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    let ending = if lf >= crlf && lf >= cr {
        LineEnding::Lf
    } else if crlf >= cr {
        LineEnding::Crlf
    } else {
        LineEnding::Cr
    };
    let mixed = [lf, crlf, cr].iter().filter(|&&n| n > 0).count() > 1;
    LineFormat { ending, final_newline: text.ends_with(b"\n") || text.ends_with(b"\r"), mixed }
}

/// Who a file is, as `stat` says: what changes when someone else writes it. An editor's rename swaps the inode; a
/// write in place moves the times or the length; `chmod` and `touch` move the ctime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
    pub ino: u64,
    pub dev: u64,
}

impl Stamp {
    pub fn of(meta: &fs::Metadata) -> Stamp {
        os::stamp(meta)
    }

    /// Whether a change made at `now_ns` could still leave this stamp: its times are within the clock's granularity
    /// of `now_ns`, or after it (a clock that disagrees).
    fn young(&self, now_ns: i128) -> bool {
        now_ns - self.mtime_ns.max(self.ctime_ns) < GRANULARITY_NS
    }
}

fn now_ns() -> i128 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    since.as_nanos() as i128
}

/// The file as it was opened (or last saved): what a save checks the disk against and how it writes the text.
#[derive(Debug, Clone)]
pub struct Baseline {
    /// Exactly the bytes on disk, BOM included.
    pub bytes: Arc<Vec<u8>>,
    pub stamp: Stamp,
    pub ending: LineEnding,
    pub bom: bool,
    pub final_newline: bool,
    /// The file mixed line ends; a save writes `ending` throughout (the header says so).
    pub mixed: bool,
    /// Its permission bits (`0o644` when not read from disk): what a deleted file is recreated with.
    pub mode: u32,
    /// The stamp can be trusted: it was already older than the clock's granularity when a compare found the bytes
    /// unchanged, so any later write leaves another. Shared by clones, which have the same stamp and bytes.
    settled: Arc<AtomicBool>,
}

impl PartialEq for Baseline {
    fn eq(&self, other: &Baseline) -> bool {
        (self.bytes == other.bytes && self.stamp == other.stamp && self.ending == other.ending)
            && (self.bom == other.bom && self.final_newline == other.final_newline && self.mixed == other.mixed)
            && self.mode == other.mode
    }
}

impl Eq for Baseline {}

impl Baseline {
    /// `bytes`, read when the file had `stamp`; its mode is taken to be `0o644` (`read` knows it).
    pub fn new(bytes: Vec<u8>, stamp: Stamp) -> Baseline {
        let bom = bytes.starts_with(BOM);
        let format = line_format(if bom { &bytes[BOM.len()..] } else { &bytes });
        Baseline {
            bytes: Arc::new(bytes),
            settled: Arc::new(AtomicBool::new(false)),
            stamp,
            ending: format.ending,
            bom,
            final_newline: format.final_newline,
            mixed: format.mixed,
            mode: 0o644,
        }
    }

    /// Reads `path` (following links) whole. The stamp is taken before the read, so a write racing it shows as a
    /// changed stamp and the next check compares the bytes.
    pub fn read(path: &Path) -> io::Result<Baseline> {
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a file", path.display())));
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut bytes)?;
        Ok(Baseline { mode: os::mode(&meta), ..Baseline::new(bytes, Stamp::of(&meta)) })
    }

    /// The baseline after writing `bytes` (encoded from this one) to the file now `meta`: same ending and BOM, mixed
    /// no more.
    fn saved(&self, bytes: Vec<u8>, meta: &fs::Metadata) -> Baseline {
        let body = if self.bom { &bytes[BOM.len().min(bytes.len())..] } else { &bytes[..] };
        let final_newline = body.ends_with(b"\n") || body.ends_with(b"\r");
        Baseline {
            bytes: Arc::new(bytes),
            stamp: Stamp::of(meta),
            ending: self.ending,
            bom: self.bom,
            final_newline,
            mixed: false,
            mode: os::mode(meta),
            settled: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// Beside the file and renamed over it, if it is still as opened.
    Safe,
    /// As `Safe`, whatever is on disk now (the user chose to keep theirs); recreates a deleted file.
    Overwrite,
    /// Rewritten where it is, if it is still as opened: for a hard-linked file, another owner's, or a folder
    /// Shepherd can't write to, once the user agreed. A failure midway puts the old bytes back (or says it couldn't:
    /// `Damaged`).
    InPlace,
    /// As `InPlace`, whatever is on disk now; recreates a deleted file.
    OverwriteInPlace,
}

impl SaveMode {
    fn checks(self) -> bool {
        matches!(self, SaveMode::Safe | SaveMode::InPlace)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The file's bytes are not the baseline's any more.
    Conflict,
    Deleted,
    /// It has this many names; a rename would leave the others on the old text.
    HardLinked(u64),
    /// No temp file can be made beside it (permission, a read-only file system).
    FolderNotWritable,
    /// The file is not writable.
    ReadOnly,
    /// A folder, a FIFO, a device.
    NotAFile,
    /// Another user's or group's: the new inode would be Shepherd's.
    OwnerDiffers,
    /// The path leaves the workspace's folder (`..`, or a link to somewhere else).
    OutsideRoot,
    /// A save in place failed midway and the old bytes could not be put back: the file may be half written. The text
    /// is still the editor's. The words name the file and the error.
    Damaged(String),
    /// Anything else, in words naming the file.
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Conflict => f.write_str("The file changed on disk since it was opened"),
            SaveError::Deleted => f.write_str("The file was deleted on disk"),
            SaveError::HardLinked(n) => write!(f, "The file has {n} hard links; saving beside it would split them"),
            SaveError::FolderNotWritable => f.write_str("Can't write to the file's folder"),
            SaveError::ReadOnly => f.write_str("The file is read-only"),
            SaveError::NotAFile => f.write_str("Not a regular file"),
            SaveError::OwnerDiffers => f.write_str("The file belongs to another user or group"),
            SaveError::OutsideRoot => f.write_str("The file is outside the workspace's folder"),
            SaveError::Damaged(why) => write!(f, "The file on disk may be damaged ({why}); the text is still in the editor, save it again"),
            SaveError::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SaveError {}

/// `text` as the file's bytes: every line end (`\n`, `\r\n` or `\r`) made the baseline's, its BOM put back. The
/// final newline is the text's own: what the user left there is what is written.
pub fn encode(text: &str, b: &Baseline) -> Vec<u8> {
    let text = if b.bom { text.strip_prefix('\u{FEFF}').unwrap_or(text) } else { text };
    let (bytes, nl) = (text.as_bytes(), b.ending.as_str().as_bytes());
    let mut out = Vec::with_capacity(bytes.len() + BOM.len() + bytes.len() / 32);
    if b.bom {
        out.extend_from_slice(BOM);
    }
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        if matches!(bytes[i], b'\r' | b'\n') {
            out.extend_from_slice(&bytes[start..i]);
            out.extend_from_slice(nl);
            if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out
}

/// `path`'s stamp (following links).
pub fn stamp(path: &Path) -> io::Result<Stamp> {
    fs::metadata(path).map(|m| Stamp::of(&m))
}

/// Whether `path` no longer holds the baseline's bytes, for polling: an equal stamp old enough to trust is enough,
/// else the bytes are compared (a touched but unchanged file is not changed). A deleted file is `Err(NotFound)`.
pub fn changed_on_disk(path: &Path, b: &Baseline) -> io::Result<bool> {
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Ok(true);
    }
    let now = Stamp::of(&meta);
    if now == b.stamp && b.settled.load(Ordering::Relaxed) {
        return Ok(false);
    }
    if now.len != b.bytes.len() as u64 {
        return Ok(true);
    }
    // the clock before the read: a write after it leaves a later stamp
    let at = now_ns();
    let same = same_content(path, &b.bytes)?;
    if same && now == b.stamp && !now.young(at) {
        b.settled.store(true, Ordering::Relaxed);
    }
    Ok(!same)
}

/// Whether the file at `path` holds exactly `expected`, read in chunks.
fn same_content(path: &Path, expected: &[u8]) -> io::Result<bool> {
    held_if_same(path, expected).map(|f| f.is_some())
}

/// The file at `path`, open, and its stamp as it was opened, if it holds exactly `expected` (a save's last look
/// before replacing it): the caller can check just before the rename that `path` is still this file, untouched
/// since. (The open file keeps its inode from being reused meanwhile.)
fn held_if_same(path: &Path, expected: &[u8]) -> io::Result<Option<(File, Stamp)>> {
    let mut file = retry(|| File::open(path))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() != expected.len() as u64 {
        return Ok(None);
    }
    let mut buf = vec![0u8; 64 << 10];
    let mut at = 0;
    loop {
        let n = retry(|| file.read(&mut buf))?;
        if n == 0 {
            return Ok((at == expected.len()).then_some((file, Stamp::of(&meta))));
        }
        if at + n > expected.len() || buf[..n] != expected[at..at + n] {
            return Ok(None);
        }
        at += n;
    }
}

/// The real file `path` names inside `root`, links followed: `path` is absolute (under `root` as given or as its
/// real path, when `root` is a link) or relative to it, with no `.`, `..` or empty parts; its real path must be inside `root`'s real path, and a regular file when
/// it exists (a missing one is where a deleted file would be recreated).
pub fn contained(root: &Path, path: &Path) -> Result<PathBuf, SaveError> {
    if !os::plain(path) {
        return Err(SaveError::OutsideRoot);
    }
    let real_root = fs::canonicalize(root).map_err(|e| io_error("Couldn't find the workspace folder", root, e))?;
    let joined = if path.is_absolute() {
        if !path.starts_with(root) && !path.starts_with(&real_root) {
            return Err(SaveError::OutsideRoot);
        }
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let root = real_root;
    let real = real_path(&joined)?;
    if real == root || !real.starts_with(&root) {
        return Err(SaveError::OutsideRoot);
    }
    match fs::metadata(&real) {
        Ok(m) if !m.is_file() => Err(SaveError::NotAFile),
        Ok(_) => Ok(real),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(real),
        Err(e) => Err(io_error("Couldn't read", &real, e)),
    }
}

/// `path` with every link followed and its folders made real; a missing last part (a deleted file, or a link to
/// one) is kept as named.
fn real_path(path: &Path) -> Result<PathBuf, SaveError> {
    let mut at = path.to_path_buf();
    for _ in 0..40 {
        match fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => {
                let to = fs::read_link(&at).map_err(|e| io_error("Couldn't read the link", &at, e))?;
                at = at.parent().unwrap_or(Path::new("/")).join(to);
            }
            Ok(_) => return fs::canonicalize(&at).map_err(|e| io_error("Couldn't find", &at, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (at.parent(), at.file_name()) else { return Err(SaveError::NotAFile) };
                let parent = fs::canonicalize(parent).map_err(|e| io_error("Couldn't find the folder of", &at, e))?;
                return Ok(parent.join(name));
            }
            Err(e) => return Err(io_error("Couldn't read", &at, e)),
        }
    }
    Err(SaveError::Io(format!("Too many links to follow from {}", path.display())))
}

/// Whether `name` is one of a save's temp files (`.name.shepherd-PID-N~`), which a file tree should never show.
pub fn is_temp_name(name: &OsStr) -> bool {
    let b = crate::platform::os_bytes(name);
    let b = &b[..];
    let Some(body) = b.strip_prefix(b".").and_then(|b| b.strip_suffix(b"~")) else { return false };
    const TAG: &[u8] = b".shepherd-";
    let Some(at) = body.windows(TAG.len()).rposition(|w| w == TAG) else { return false };
    let mut ids = body[at + TAG.len()..].split(|&c| c == b'-');
    let number = |p: Option<&[u8]>| p.is_some_and(|p| !p.is_empty() && p.iter().all(u8::is_ascii_digit));
    number(ids.next()) && number(ids.next()) && ids.next().is_none()
}

/// The `n`th temp name for the file `name`: `.name.shepherd-PID-N~`, the name cut (at a character, when it is
/// UTF-8, as APFS wants) so the whole fits in `NAME_MAX`.
fn temp_name(name: &OsStr, n: u64) -> OsString {
    let suffix = format!(".shepherd-{}-{n}~", std::process::id());
    let room = NAME_MAX - 1 - suffix.len();
    let whole = crate::platform::os_bytes(name);
    let mut stem = &whole[..];
    if stem.len() > room {
        stem = &stem[..room];
        if let Err(e) = std::str::from_utf8(stem)
            && e.error_len().is_none()
            && std::str::from_utf8(&whole).is_ok()
        {
            stem = &stem[..e.valid_up_to()];
        }
    }
    let mut out = Vec::with_capacity(1 + stem.len() + suffix.len());
    out.push(b'.');
    out.extend_from_slice(stem);
    out.extend_from_slice(suffix.as_bytes());
    crate::platform::os_from_bytes(&out)
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Saves `text` to `path` (inside the workspace folder `root`; a link is followed and its target written) as `b`
/// was encoded, and returns the new baseline. Blocking: run it off the UI thread.
pub fn save(root: &Path, path: &Path, text: &str, b: &Baseline, mode: SaveMode) -> Result<Baseline, SaveError> {
    save_with(root, path, text, b, mode, &mut Hooks::default())
}

/// Faults and races a test makes happen inside a save.
#[derive(Default)]
struct Hooks<'a> {
    /// Writing stops halfway with an error.
    short_write: bool,
    /// Syncing the written file fails.
    fail_sync: bool,
    /// Putting the old bytes back after a failed save in place fails too.
    fail_restore: bool,
    /// The system has no atomic rename-unless-there (`renameat2`/`renamex_np`): the link fallback runs.
    no_noreplace: bool,
    /// Runs right before the bytes on disk are compared (or, recreating, before the file is put in place): another
    /// program writing then.
    before_check: Option<&'a mut dyn FnMut()>,
    /// Runs after the compare, right before the rename.
    before_rename: Option<&'a mut dyn FnMut()>,
}

impl Hooks<'_> {
    fn before_check(&mut self) {
        if let Some(f) = self.before_check.as_mut() {
            f();
        }
    }

    fn before_rename(&mut self) {
        if let Some(f) = self.before_rename.as_mut() {
            f();
        }
    }
}

fn save_with(root: &Path, path: &Path, text: &str, b: &Baseline, mode: SaveMode, hooks: &mut Hooks) -> Result<Baseline, SaveError> {
    let target = contained(root, path)?;
    let bytes = encode(text, b);
    let meta = match fs::metadata(&target) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return if mode.checks() { Err(SaveError::Deleted) } else { recreate(&target, bytes, b, hooks) };
        }
        Err(e) => return Err(io_error("Couldn't read", &target, e)),
    };
    if !meta.is_file() {
        return Err(SaveError::NotAFile);
    }
    let ours = os::ours(&meta);
    if ours && os::read_only(&meta) {
        return Err(SaveError::ReadOnly);
    }
    match mode {
        SaveMode::Safe | SaveMode::Overwrite => {
            let links = os::links(&meta);
            if links > 1 {
                return Err(SaveError::HardLinked(links));
            }
            if !ours {
                return Err(SaveError::OwnerDiffers);
            }
            replace(&target, &meta, bytes, b, mode.checks(), hooks)
        }
        SaveMode::InPlace | SaveMode::OverwriteInPlace => in_place(&target, bytes, b, mode.checks(), hooks),
    }
}

/// The safe save: a temp file beside `target`, written, given the file's group and mode, synced, then (if still as
/// `b`) renamed over it.
fn replace(target: &Path, meta: &fs::Metadata, bytes: Vec<u8>, b: &Baseline, check: bool, hooks: &mut Hooks) -> Result<Baseline, SaveError> {
    let dir = target.parent().ok_or(SaveError::NotAFile)?;
    let name = target.file_name().ok_or(SaveError::NotAFile)?;
    let (temp, mut file) = create_temp(dir, name, target)?;
    // removes the temp file on every way out but the rename (an error, a panic)
    let mut guard = Unlink(Some(temp.clone()));
    let failed = |e| io_error("Couldn't write", target, e);
    write(&mut file, &bytes, hooks).map_err(failed)?;
    // the group first, while the temp is still 0600 (the folder's or the user's group may be one the file's readers
    // aren't in): the file's own if the user is in it, else refuse
    if !os::take_group(&file, meta).map_err(failed)? {
        return Err(SaveError::OwnerDiffers);
    }
    // then exactly the old mode
    os::set_mode(&file, os::mode(meta)).map_err(failed)?;
    sync(&file, hooks).map_err(failed)?;
    hooks.before_check();
    let held = if check {
        match held_if_same(target, &b.bytes) {
            Ok(Some(held)) => Some(held),
            Ok(None) => return Err(SaveError::Conflict),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(SaveError::Deleted),
            Err(e) => return Err(io_error("Couldn't read", target, e)),
        }
    } else {
        None
    };
    hooks.before_rename();
    // the window between the compare and the rename, as narrow as it goes: the path must still be the file compared,
    // untouched since (a rename over it swaps the inode; a write moves the length or the times)
    if let Some((_file, compared)) = &held {
        match fs::metadata(target) {
            Ok(now) if Stamp::of(&now) == *compared => {}
            Ok(_) => return Err(SaveError::Conflict),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(SaveError::Deleted),
            Err(e) => return Err(io_error("Couldn't read", target, e)),
        }
    }
    retry(|| fs::rename(&temp, target)).map_err(failed)?;
    guard.0 = None;
    sync_dir(dir);
    // the inode now at `target`, ctime and all after the rename
    Ok(b.saved(bytes, &file.metadata().map_err(failed)?))
}

/// Rewrites `target` where it is: the new bytes over the old from the start, then the length cut to theirs, then
/// synced (never truncated first, so a failure midway leaves more of the old text, not less). On a failure the old
/// bytes go back the same way; if that fails too, the file is `Damaged`.
fn in_place(target: &Path, bytes: Vec<u8>, b: &Baseline, check: bool, hooks: &mut Hooks) -> Result<Baseline, SaveError> {
    let mut file = retry(|| OpenOptions::new().read(true).write(true).open(target)).map_err(|e| match e.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => SaveError::ReadOnly,
        io::ErrorKind::NotFound => SaveError::Deleted,
        _ => io_error("Couldn't open", target, e),
    })?;
    hooks.before_check();
    let mut old = Vec::new();
    file.read_to_end(&mut old).map_err(|e| io_error("Couldn't read", target, e))?;
    if check && old != *b.bytes {
        return Err(SaveError::Conflict);
    }
    let rewrite = |file: &mut File, bytes: &[u8], hooks: &Hooks| -> io::Result<()> {
        file.rewind()?;
        write(file, bytes, hooks)?;
        file.set_len(bytes.len() as u64)?;
        sync(file, hooks)
    };
    if let Err(e) = rewrite(&mut file, &bytes, hooks) {
        let restored = if hooks.fail_restore { Err(io::Error::other("injected restore failure")) } else { rewrite(&mut file, &old, &Hooks::default()) };
        return Err(match restored {
            Ok(()) => io_error("Couldn't write", target, e),
            Err(again) => SaveError::Damaged(format!("{}: {e}, then {again}", target.display())),
        });
    }
    Ok(b.saved(bytes, &file.metadata().map_err(|e| io_error("Couldn't read", target, e))?))
}

/// Makes a deleted file again with the baseline's mode: written to a temp file beside it, synced, then renamed into
/// place only if nothing is there (`renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS, a
/// hard link where the file system has neither, else O_CREAT|O_EXCL): a file that appeared meanwhile is a conflict,
/// never replaced.
fn recreate(target: &Path, bytes: Vec<u8>, b: &Baseline, hooks: &mut Hooks) -> Result<Baseline, SaveError> {
    let dir = target.parent().ok_or(SaveError::NotAFile)?;
    let name = target.file_name().ok_or(SaveError::NotAFile)?;
    let (temp, mut file) = create_temp(dir, name, target)?;
    let mut guard = Unlink(Some(temp.clone()));
    let failed = |e| io_error("Couldn't write", target, e);
    write(&mut file, &bytes, hooks).map_err(failed)?;
    os::set_mode(&file, b.mode).map_err(failed)?;
    sync(&file, hooks).map_err(failed)?;
    hooks.before_check();
    match rename_noreplace(&temp, target, hooks.no_noreplace) {
        Ok(()) => guard.0 = None,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(SaveError::Conflict),
        Err(e) if unsupported(&e) => {
            // no atomic way on this file system: create it exclusively and write it there
            drop(guard);
            return recreate_exclusive(target, bytes, b, hooks);
        }
        Err(e) => return Err(failed(e)),
    }
    sync_dir(dir);
    Ok(b.saved(bytes, &file.metadata().map_err(failed)?))
}

/// `recreate` where nothing atomic works: O_CREAT|O_EXCL on the file itself, removed again if the write fails.
fn recreate_exclusive(target: &Path, bytes: Vec<u8>, b: &Baseline, hooks: &mut Hooks) -> Result<Baseline, SaveError> {
    let open = || os::private(OpenOptions::new().write(true).create_new(true)).open(target);
    let mut file = retry(open).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => SaveError::Conflict,
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => SaveError::FolderNotWritable,
        _ => io_error("Couldn't create", target, e),
    })?;
    let mut guard = Unlink(Some(target.to_path_buf()));
    let failed = |e| io_error("Couldn't write", target, e);
    write(&mut file, &bytes, hooks).map_err(failed)?;
    os::set_mode(&file, b.mode).map_err(failed)?;
    sync(&file, hooks).map_err(failed)?;
    guard.0 = None;
    if let Some(dir) = target.parent() {
        sync_dir(dir);
    }
    Ok(b.saved(bytes, &file.metadata().map_err(failed)?))
}

/// An error saying the system or file system can't do the call at all.
fn unsupported(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput)
}

/// Renames `from` to `to` unless `to` exists (`AlreadyExists`), atomically: the system's no-replace rename, else
/// (`fallback`, or a file system without it) a hard link and the unlink of `from`.
fn rename_noreplace(from: &Path, to: &Path, fallback: bool) -> io::Result<()> {
    let atomic = if fallback { Err(io::Error::from(io::ErrorKind::Unsupported)) } else { os::rename_noreplace(from, to) };
    match atomic {
        Err(e) if unsupported(&e) => {
            retry(|| fs::hard_link(from, to))?;
            let _ = fs::remove_file(from);
            Ok(())
        }
        r => r,
    }
}

#[cfg(target_os = "linux")]
fn noreplace(from: &std::ffi::CStr, to: &std::ffi::CStr) -> io::Result<()> {
    unsafe extern "C" {
        fn renameat2(olddirfd: i32, oldpath: *const std::ffi::c_char, newdirfd: i32, newpath: *const std::ffi::c_char, flags: u32) -> i32;
    }
    const AT_FDCWD: i32 = -100;
    const RENAME_NOREPLACE: u32 = 1;
    // SAFETY: both paths are NUL-terminated and live through the call.
    if unsafe { renameat2(AT_FDCWD, from.as_ptr(), AT_FDCWD, to.as_ptr(), RENAME_NOREPLACE) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(target_os = "macos")]
fn noreplace(from: &std::ffi::CStr, to: &std::ffi::CStr) -> io::Result<()> {
    unsafe extern "C" {
        fn renamex_np(from: *const std::ffi::c_char, to: *const std::ffi::c_char, flags: u32) -> i32;
    }
    const RENAME_EXCL: u32 = 0x4;
    // SAFETY: both paths are NUL-terminated and live through the call.
    if unsafe { renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn noreplace(_: &std::ffi::CStr, _: &std::ffi::CStr) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// A new temp file beside `name` in `dir`, O_EXCL (so never someone else's file, nor a link planted there), 0600 until
/// it has the file's group and mode.
fn create_temp(dir: &Path, name: &OsStr, target: &Path) -> Result<(PathBuf, File), SaveError> {
    for _ in 0..100 {
        let temp = dir.join(temp_name(name, TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)));
        match os::private(OpenOptions::new().write(true).create_new(true)).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(e) if matches!(e.kind(), io::ErrorKind::AlreadyExists | io::ErrorKind::Interrupted) => continue,
            Err(e) if matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem) => {
                return Err(SaveError::FolderNotWritable);
            }
            Err(e) => return Err(io_error("Couldn't save", target, e)),
        }
    }
    Err(SaveError::Io(format!("Couldn't save {}: no free temp name beside it", target.display())))
}

fn write(file: &mut File, bytes: &[u8], hooks: &Hooks) -> io::Result<()> {
    if hooks.short_write {
        file.write_all(&bytes[..bytes.len() / 2])?;
        return Err(io::Error::new(io::ErrorKind::WriteZero, "injected short write"));
    }
    // write_all retries EINTR itself
    file.write_all(bytes)
}

fn sync(file: &File, hooks: &Hooks) -> io::Result<()> {
    if hooks.fail_sync {
        return Err(io::Error::other("injected fsync failure"));
    }
    retry(|| file.sync_all())
}

/// Makes a rename in `dir` durable. Best effort: the save has happened, and macOS may refuse a full sync of a
/// folder.
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = retry(|| d.sync_all());
    }
}

fn retry<T>(mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match f() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            r => return r,
        }
    }
}

/// An error in words that name the user's file, never the temp one.
fn io_error(what: &str, path: &Path, e: io::Error) -> SaveError {
    SaveError::Io(format!("{what} {}: {e}", path.display()))
}

/// Removes its path when dropped, unless taken.
struct Unlink(Option<PathBuf>);

impl Drop for Unlink {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// What a save needs of the system: owners, groups, modes and links, as unix keeps them.
#[cfg(unix)]
mod os {
    use super::{Stamp, noreplace, retry};
    use std::fs::{self, File, OpenOptions, Permissions};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    unsafe extern "C" {
        pub(super) fn geteuid() -> u32;
    }

    pub(super) fn stamp(meta: &fs::Metadata) -> Stamp {
        Stamp {
            len: meta.size(),
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
            ctime_ns: i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
            ino: meta.ino(),
            dev: meta.dev(),
        }
    }

    /// The permission bits.
    pub(super) fn mode(meta: &fs::Metadata) -> u32 {
        meta.mode() & 0o777
    }

    /// The file is this user's.
    pub(super) fn ours(meta: &fs::Metadata) -> bool {
        // SAFETY: geteuid takes nothing and cannot fail.
        meta.uid() == unsafe { geteuid() }
    }

    /// Its owner may not write it.
    pub(super) fn read_only(meta: &fs::Metadata) -> bool {
        meta.mode() & 0o200 == 0
    }

    pub(super) fn links(meta: &fs::Metadata) -> u64 {
        meta.nlink()
    }

    /// Gives `file` the group of the file `meta` is; false when this user can't.
    pub(super) fn take_group(file: &File, meta: &fs::Metadata) -> io::Result<bool> {
        Ok(file.metadata()?.gid() == meta.gid() || std::os::unix::fs::fchown(file, None, Some(meta.gid())).is_ok())
    }

    pub(super) fn set_mode(file: &File, mode: u32) -> io::Result<()> {
        file.set_permissions(Permissions::from_mode(mode & 0o777))
    }

    /// Opens 0600: the temp is no one else's to read until it has the file's group and mode.
    pub(super) fn private(options: &mut OpenOptions) -> &mut OpenOptions {
        options.mode(0o600)
    }

    /// `path` has no `.`, `..` or empty parts, and is not the root alone.
    pub(super) fn plain(path: &Path) -> bool {
        let raw = path.as_os_str().as_bytes();
        let parts = if path.is_absolute() { &raw[1..] } else { raw };
        !parts.is_empty() && !parts.split(|&c| c == b'/').any(|p| p.is_empty() || p == b"." || p == b"..")
    }

    pub(super) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
        let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput));
        let (from, to) = (c(from)?, c(to)?);
        retry(|| noreplace(&from, &to))
    }
}

/// Off unix (Windows): no owner, group, mode bits, inode or link count to keep; the read-only flag is what refuses a
/// save.
#[cfg(not(unix))]
mod os {
    use super::Stamp;
    use std::fs::{self, File, OpenOptions};
    use std::io;
    use std::path::{Component, Path};

    pub(super) fn stamp(meta: &fs::Metadata) -> Stamp {
        let ns = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i128);
        Stamp { len: meta.len(), mtime_ns: ns, ctime_ns: ns, ino: 0, dev: 0 }
    }

    /// What the file's mode would be: read-only or not.
    pub(super) fn mode(meta: &fs::Metadata) -> u32 {
        if meta.permissions().readonly() { 0o444 } else { 0o644 }
    }

    pub(super) fn ours(_: &fs::Metadata) -> bool {
        true
    }

    pub(super) fn read_only(meta: &fs::Metadata) -> bool {
        meta.permissions().readonly()
    }

    pub(super) fn links(_: &fs::Metadata) -> u64 {
        1
    }

    pub(super) fn take_group(_: &File, _: &fs::Metadata) -> io::Result<bool> {
        Ok(true)
    }

    pub(super) fn set_mode(file: &File, mode: u32) -> io::Result<()> {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(mode & 0o200 == 0);
        file.set_permissions(permissions)
    }

    pub(super) fn private(options: &mut OpenOptions) -> &mut OpenOptions {
        options
    }

    pub(super) fn plain(path: &Path) -> bool {
        path.components().any(|c| matches!(c, Component::Normal(_)))
            && !path.components().any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    }

    /// No atomic rename-unless-there: the hard link fallback.
    pub(super) fn rename_noreplace(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

// The tests make links, modes and other users' files, as unix has them.
#[cfg(all(test, unix))]
mod tests {
    use super::os::geteuid;
    use super::*;
    use std::fs::Permissions;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn no_temps(dir: &Path) {
        for e in fs::read_dir(dir).unwrap() {
            let name = e.unwrap().file_name();
            assert!(!is_temp_name(&name), "temp left: {name:?}");
        }
    }

    /// The text an editor would hold for `b`: as decoded for the viewer.
    fn text_of(b: &Baseline) -> String {
        crate::files::content::decode(&b.bytes).unwrap().text
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().mode() & 0o7777
    }

    #[test]
    fn line_ends_bom_and_final_newline_are_kept_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let cases: [&[u8]; 8] = [b"\xEF\xBB\xBFa\r\nb\r\n", b"a\r\nb", b"a\rb\r", b"a\nb\n", b"a\nb", b"", b"\xEF\xBB\xBF", b"\r\n"];
        for case in cases {
            fs::write(&path, case).unwrap();
            let b = Baseline::read(&path).unwrap();
            assert!(!b.mixed);
            let saved = save(dir.path(), &path, &text_of(&b), &b, SaveMode::Safe).unwrap();
            assert_eq!(fs::read(&path).unwrap(), case, "{case:?}");
            assert_eq!(*saved.bytes, case);
            assert!(!changed_on_disk(&path, &saved).unwrap());
        }
        // an edit typed with `\n` is written with the file's own ends, and its BOM
        fs::write(&path, b"\xEF\xBB\xBFa\r\nb\r\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        assert_eq!((b.ending, b.bom, b.final_newline), (LineEnding::Crlf, true, true));
        save(dir.path(), &path, "a\nB\nc", &b, SaveMode::Safe).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\xEF\xBB\xBFa\r\nB\r\nc");
        // mixed: the majority everywhere
        fs::write(&path, b"a\r\nb\r\nc\nd").unwrap();
        let b = Baseline::read(&path).unwrap();
        assert_eq!((b.ending, b.mixed, b.final_newline), (LineEnding::Crlf, true, false));
        let saved = save(dir.path(), &path, &text_of(&b), &b, SaveMode::Safe).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"a\r\nb\r\nc\r\nd");
        assert!(!saved.mixed);
        no_temps(dir.path());
    }

    #[test]
    fn encode_makes_every_line_end_the_baselines() {
        let b = |ending, bom| Baseline { ending, bom, ..Baseline::new(Vec::new(), stamp(Path::new("/")).unwrap()) };
        assert_eq!(encode("a\nb\r\nc\rd\n", &b(LineEnding::Lf, false)), b"a\nb\nc\nd\n");
        assert_eq!(encode("a\nb\r\nc\rd", &b(LineEnding::Crlf, false)), b"a\r\nb\r\nc\r\nd");
        assert_eq!(encode("a\n\n", &b(LineEnding::Cr, false)), b"a\r\r");
        assert_eq!(encode("\u{FEFF}x", &b(LineEnding::Lf, true)), b"\xEF\xBB\xBFx");
        assert_eq!(line_format(b"a\rb\rc\n"), LineFormat { ending: LineEnding::Cr, final_newline: true, mixed: true });
        assert_eq!(line_format(b"a\r\nb\n"), LineFormat { ending: LineEnding::Lf, final_newline: true, mixed: true });
        assert_eq!(line_format(b"abc"), LineFormat::default());
    }

    #[test]
    fn the_mode_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        for mode in [0o755, 0o600, 0o640] {
            fs::write(&path, "echo\n").unwrap();
            fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            let b = Baseline::read(&path).unwrap();
            assert_eq!(b.mode, mode);
            let saved = save(dir.path(), &path, "echo hi\n", &b, SaveMode::Safe).unwrap();
            assert_eq!(mode_of(&path), mode);
            assert_eq!(saved.mode, mode);
            assert_eq!(fs::read_to_string(&path).unwrap(), "echo hi\n");
        }
    }

    #[test]
    fn a_link_inside_the_root_stays_a_link_and_its_target_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        fs::write(&real, "old\n").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let link = dir.path().join("sub/link.txt");
        symlink("../real.txt", &link).unwrap();
        let chain = dir.path().join("chain.txt");
        symlink(&link, &chain).unwrap();
        let b = Baseline::read(&chain).unwrap();
        let saved = save(dir.path(), &chain, "new\n", &b, SaveMode::Safe).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(&chain).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("../real.txt"));
        assert_eq!(fs::read_to_string(&real).unwrap(), "new\n");
        assert!(!changed_on_disk(&chain, &saved).unwrap());
        // relative to the root works too
        save(dir.path(), Path::new("sub/link.txt"), "newer\n", &saved, SaveMode::Safe).unwrap();
        assert_eq!(fs::read_to_string(&real).unwrap(), "newer\n");
        no_temps(dir.path());
        no_temps(&dir.path().join("sub"));
    }

    #[test]
    fn a_root_that_is_a_link_takes_paths_through_either_name() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let root = dir.path().join("root");
        symlink(&real, &root).unwrap();
        let file = real.join("a.txt");
        fs::write(&file, "one\n").unwrap();
        let canonical = fs::canonicalize(&file).unwrap();
        assert_eq!(contained(&root, &root.join("a.txt")), Ok(canonical.clone()));
        assert_eq!(contained(&root, &canonical), Ok(canonical.clone()));
        assert_eq!(contained(&root, Path::new("a.txt")), Ok(canonical.clone()));
        let b = Baseline::read(&file).unwrap();
        save(&root, &canonical, "two\n", &b, SaveMode::Safe).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "two\n");
        assert!(fs::symlink_metadata(&root).unwrap().file_type().is_symlink());
        assert_eq!(contained(&root, &dir.path().join("elsewhere.txt")), Err(SaveError::OutsideRoot));
    }

    #[test]
    fn a_path_leaving_the_root_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, "keep\n").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("l.txt");
        symlink(&secret, &link).unwrap();
        let b = Baseline::read(&link).unwrap();
        assert_eq!(save(dir.path(), &link, "x", &b, SaveMode::Overwrite), Err(SaveError::OutsideRoot));
        assert_eq!(save(dir.path(), &link, "x", &b, SaveMode::OverwriteInPlace), Err(SaveError::OutsideRoot));
        assert_eq!(fs::read_to_string(&secret).unwrap(), "keep\n");
        let root = dir.path();
        for bad in ["../x", "a/../b", "./a", "a//b", "a/", ""] {
            assert_eq!(contained(root, Path::new(bad)), Err(SaveError::OutsideRoot), "{bad}");
        }
        assert_eq!(contained(root, &root.join("a/../l.txt")), Err(SaveError::OutsideRoot));
        assert_eq!(contained(root, &secret), Err(SaveError::OutsideRoot));
        assert_eq!(contained(root, root), Err(SaveError::OutsideRoot));
        fs::create_dir(root.join("d")).unwrap();
        assert_eq!(contained(root, Path::new("d")), Err(SaveError::NotAFile));
        // a missing file in the root is where a deleted one is recreated
        assert_eq!(contained(root, Path::new("new.txt")), Ok(fs::canonicalize(root).unwrap().join("new.txt")));
    }

    #[test]
    fn a_change_on_disk_is_a_conflict_until_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        // same size, same second: only the bytes tell
        fs::write(&path, "two\n").unwrap();
        assert!(changed_on_disk(&path, &b).unwrap());
        assert_eq!(save(dir.path(), &path, "mine\n", &b, SaveMode::Safe), Err(SaveError::Conflict));
        assert_eq!(save(dir.path(), &path, "mine\n", &b, SaveMode::InPlace), Err(SaveError::Conflict));
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        no_temps(dir.path());
        let saved = save(dir.path(), &path, "mine\n", &b, SaveMode::Overwrite).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "mine\n");
        assert!(!changed_on_disk(&path, &saved).unwrap());
    }

    #[test]
    fn a_save_compares_the_bytes_even_when_the_stamp_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "two\n").unwrap();
        // a baseline whose stamp is the disk's exactly (a rewrite the clock didn't see) but whose bytes are not
        let b = Baseline::new(b"one\n".to_vec(), stamp(&path).unwrap());
        assert!(changed_on_disk(&path, &b).unwrap(), "a young stamp is not trusted");
        assert_eq!(save(dir.path(), &path, "mine\n", &b, SaveMode::Safe), Err(SaveError::Conflict));
        assert_eq!(save(dir.path(), &path, "mine\n", &b, SaveMode::InPlace), Err(SaveError::Conflict));
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        // (the touch moved the ctime, so this stamp is young and the bytes are compared)
        let b = Baseline::read(&path).unwrap();
        assert!(!changed_on_disk(&path, &b).unwrap());
        // touched with the same bytes is not a change
        File::options().write(true).open(&path).unwrap().set_modified(std::time::SystemTime::now()).unwrap();
        assert!(!changed_on_disk(&path, &b).unwrap());
        save(dir.path(), &path, "mine\n", &b, SaveMode::Safe).unwrap();
    }

    #[test]
    fn another_program_writing_before_the_rename_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        let p = path.clone();
        let mut theirs = move || fs::write(&p, "two\n").unwrap();
        let mut hooks = Hooks { before_check: Some(&mut theirs), ..Hooks::default() };
        assert_eq!(save_with(dir.path(), &path, "mine\n", &b, SaveMode::Safe, &mut hooks), Err(SaveError::Conflict));
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        assert_eq!(names(dir.path()), ["a.txt"]);
    }

    #[test]
    fn a_write_between_the_compare_and_the_rename_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        let p = path.clone();
        let mut theirs = move || {
            let mut f = OpenOptions::new().append(true).open(&p).unwrap();
            f.write_all(b"two\n").unwrap();
        };
        let mut hooks = Hooks { before_rename: Some(&mut theirs), ..Hooks::default() };
        assert_eq!(save_with(dir.path(), &path, "mine\n", &b, SaveMode::Safe, &mut hooks), Err(SaveError::Conflict));
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        // a rename over it swaps the inode, even with the same bytes
        let b = Baseline::read(&path).unwrap();
        let (p, other) = (path.clone(), dir.path().join("other"));
        let mut swap = move || {
            fs::write(&other, "one\ntwo\n").unwrap();
            fs::rename(&other, &p).unwrap();
        };
        let mut hooks = Hooks { before_rename: Some(&mut swap), ..Hooks::default() };
        assert_eq!(save_with(dir.path(), &path, "mine\n", &b, SaveMode::Safe, &mut hooks), Err(SaveError::Conflict));
        assert_eq!(names(dir.path()), ["a.txt"]);
    }

    #[test]
    fn a_stamp_is_trusted_only_once_older_than_the_clock_granularity() {
        let s = Stamp { len: 1, mtime_ns: 10_000_000_000, ctime_ns: 11_000_000_000, ino: 1, dev: 1 };
        assert!(s.young(11_500_000_000));
        assert!(s.young(9_000_000_000), "a time in the future is not trusted");
        assert!(s.young(12_999_999_999), "the later of mtime and ctime counts");
        assert!(!s.young(13_000_000_000));
    }

    #[test]
    fn a_hard_linked_file_is_refused_unless_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let other = dir.path().join("b.txt");
        fs::write(&path, "one\n").unwrap();
        fs::hard_link(&path, &other).unwrap();
        let ino = fs::metadata(&path).unwrap().ino();
        let b = Baseline::read(&path).unwrap();
        assert_eq!(save(dir.path(), &path, "two\n", &b, SaveMode::Safe), Err(SaveError::HardLinked(2)));
        assert_eq!(save(dir.path(), &path, "two\n", &b, SaveMode::Overwrite), Err(SaveError::HardLinked(2)));
        no_temps(dir.path());
        let saved = save(dir.path(), &path, "two, longer\n", &b, SaveMode::InPlace).unwrap();
        assert_eq!(fs::read_to_string(&other).unwrap(), "two, longer\n");
        assert_eq!(fs::metadata(&path).unwrap().ino(), ino);
        assert!(!changed_on_disk(&path, &saved).unwrap());
        save(dir.path(), &path, "3\n", &saved, SaveMode::InPlace).unwrap();
        assert_eq!(fs::read_to_string(&other).unwrap(), "3\n", "truncated, not overwritten over the longer text");
    }

    #[test]
    fn a_deleted_file_is_said_so_and_recreated_on_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(changed_on_disk(&path, &b).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(save(dir.path(), &path, "x\n", &b, SaveMode::Safe), Err(SaveError::Deleted));
        assert_eq!(save(dir.path(), &path, "x\n", &b, SaveMode::InPlace), Err(SaveError::Deleted));
        assert!(!path.exists());
        // one that appears while recreating is someone else's
        let p = path.clone();
        let mut theirs = move || fs::write(&p, "theirs\n").unwrap();
        let mut hooks = Hooks { before_check: Some(&mut theirs), ..Hooks::default() };
        assert_eq!(save_with(dir.path(), &path, "x\n", &b, SaveMode::Overwrite, &mut hooks), Err(SaveError::Conflict));
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
        assert_eq!(names(dir.path()), ["a.txt"]);
        // the same where the file system has no atomic rename-unless-there: the hard link refuses it too
        fs::remove_file(&path).unwrap();
        let p = path.clone();
        let mut theirs = move || fs::write(&p, "theirs\n").unwrap();
        let mut hooks = Hooks { before_check: Some(&mut theirs), no_noreplace: true, ..Hooks::default() };
        assert_eq!(save_with(dir.path(), &path, "x\n", &b, SaveMode::Overwrite, &mut hooks), Err(SaveError::Conflict));
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
        assert_eq!(names(dir.path()), ["a.txt"]);
        fs::remove_file(&path).unwrap();
        let mut hooks = Hooks { no_noreplace: true, ..Hooks::default() };
        save_with(dir.path(), &path, "linked\n", &b, SaveMode::Overwrite, &mut hooks).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "linked\n");
        assert_eq!(fs::metadata(&path).unwrap().nlink(), 1);
        assert_eq!(names(dir.path()), ["a.txt"]);
        fs::remove_file(&path).unwrap();
        // recreated with the mode it had
        let b = Baseline { mode: 0o750, ..b };
        let saved = save(dir.path(), &path, "x\n", &b, SaveMode::Overwrite).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "x\n");
        assert_eq!(mode_of(&path), 0o750);
        assert_eq!(saved.mode, 0o750);
        assert!(!changed_on_disk(&path, &saved).unwrap());
        // a link to a deleted file inside the root recreates its target
        let link = dir.path().join("l.txt");
        symlink("gone.txt", &link).unwrap();
        save(dir.path(), &link, "back\n", &b, SaveMode::OverwriteInPlace).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("gone.txt")).unwrap(), "back\n");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    }

    fn is_root() -> bool {
        // SAFETY: as in save_with
        unsafe { geteuid() == 0 }
    }

    #[test]
    fn a_read_only_folder_is_refused_unless_written_in_place() {
        if is_root() {
            return; // root writes anywhere
        }
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ro");
        fs::create_dir(&sub).unwrap();
        let path = sub.join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        fs::set_permissions(&sub, Permissions::from_mode(0o555)).unwrap();
        let safe = save(dir.path(), &path, "two\n", &b, SaveMode::Safe);
        let in_place = save(dir.path(), &path, "two\n", &b, SaveMode::InPlace);
        fs::set_permissions(&sub, Permissions::from_mode(0o755)).unwrap();
        assert_eq!(safe, Err(SaveError::FolderNotWritable));
        assert!(in_place.is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        assert_eq!(names(&sub), ["a.txt"]);
    }

    #[test]
    fn a_read_only_file_is_refused() {
        if is_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one\n").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o444)).unwrap();
        let b = Baseline::read(&path).unwrap();
        for mode in [SaveMode::Safe, SaveMode::Overwrite, SaveMode::InPlace, SaveMode::OverwriteInPlace] {
            assert_eq!(save(dir.path(), &path, "two\n", &b, mode), Err(SaveError::ReadOnly), "{mode:?}");
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\n");
        assert_eq!(names(dir.path()), ["a.txt"]);
    }

    #[test]
    fn not_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("d")).unwrap();
        let b = Baseline::new(Vec::new(), stamp(dir.path()).unwrap());
        assert_eq!(save(dir.path(), &dir.path().join("d"), "x", &b, SaveMode::Overwrite), Err(SaveError::NotAFile));
    }

    #[test]
    fn a_failed_restore_in_place_says_the_file_may_be_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one\n".repeat(100)).unwrap();
        let b = Baseline::read(&path).unwrap();
        let mut hooks = Hooks { short_write: true, fail_restore: true, ..Hooks::default() };
        let got = save_with(dir.path(), &path, &"two\n".repeat(80), &b, SaveMode::InPlace, &mut hooks);
        let Err(SaveError::Damaged(why)) = &got else { panic!("{got:?}") };
        assert!(why.contains("a.txt"), "{why}");
        assert!(got.unwrap_err().to_string().contains("still in the editor"));
        // written over from the start, never truncated first: the old text's tail is still there
        let now = fs::read_to_string(&path).unwrap();
        assert_eq!(now.len(), 400);
        assert!(now.starts_with("two\n") && now.ends_with("one\n"));
    }

    #[test]
    fn a_failed_write_or_sync_leaves_the_file_whole_and_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let one = "one\n".repeat(1000);
        fs::write(&path, &one).unwrap();
        let b = Baseline::read(&path).unwrap();
        let faults = [Hooks { short_write: true, ..Hooks::default() }, Hooks { fail_sync: true, ..Hooks::default() }];
        for mut hooks in faults {
            for mode in [SaveMode::Safe, SaveMode::Overwrite, SaveMode::InPlace, SaveMode::OverwriteInPlace] {
                let got = save_with(dir.path(), &path, &"two\n".repeat(900), &b, mode, &mut hooks);
                let Err(SaveError::Io(why)) = got else { panic!("{mode:?}: {got:?}") };
                // the error names the file, never the temp
                assert!(why.contains("a.txt") && !why.contains("shepherd-"), "{why}");
                assert_eq!(fs::read_to_string(&path).unwrap(), one, "{mode:?}");
                assert_eq!(names(dir.path()), ["a.txt"]);
            }
            // recreating: no half file left
            let gone = dir.path().join("gone.txt");
            assert!(matches!(save_with(dir.path(), &gone, "x", &b, SaveMode::Overwrite, &mut hooks), Err(SaveError::Io(_))));
            assert!(!gone.exists());
        }
    }

    #[test]
    fn temp_names_fit_and_are_known() {
        let long = "é".repeat(126); // 252 bytes
        let temp = temp_name(OsStr::new(&long), 7);
        assert!(temp.len() <= NAME_MAX);
        assert!(temp.to_str().is_some(), "cut at a character");
        assert!(is_temp_name(&temp));
        assert!(is_temp_name(&temp_name(OsStr::new("a.txt"), 0)));
        assert!(is_temp_name(OsStr::new(".a.txt.shepherd-12-3~")));
        for not in [".a.txt~", "a.txt.shepherd-1-2~", ".a.shepherd-1~", ".a.shepherd-x-2~", ".a.shepherd-1-2", ".a.shepherd-1-2-3~"] {
            assert!(!is_temp_name(OsStr::new(not)), "{not}");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(&long);
        fs::write(&path, "one\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        save(dir.path(), &path, "two\n", &b, SaveMode::Safe).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        no_temps(dir.path());
    }
}
