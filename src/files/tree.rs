//! The Files tab's tree, without gpui: one folder's listing at a time (a folder is read when it is opened, never
//! ahead), sorted as file managers do, and flattened into the rows the panel's list draws. Reading a folder is
//! blocking file-system work, run off the main thread by the panel; everything else here is plain data.
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// A folder lists at most this many entries: a `node_modules/.cache` or a mail spool with 300k files is not read
/// to its end, nor drawn (the list is told it holds more).
pub const MAX_ENTRIES: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Dir,
    /// `dir`: it points at a folder (one that exists). `outside`: what it points at is not under the root, so the
    /// tree does not open it (a link to `/` or to home would list everything under it).
    Symlink { dir: bool, outside: bool },
    /// A link to nothing: shown, but not opened (an editor handed it would create the file it names).
    Dangling,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name as shown (lossy where it is not UTF-8).
    pub name: String,
    /// The name as the file system has it: the path is built from this, so a non-UTF-8 name still opens.
    pub os_name: OsString,
    pub kind: EntryKind,
    pub size: u64,
    /// git ignores it (`git check-ignore`), set after the listing by the panel; the tree hides it unless asked not to.
    pub ignored: bool,
}

impl Entry {
    /// A folder the tree may open: a real one, or a link to one under the root.
    pub fn expandable(&self) -> bool {
        matches!(self.kind, EntryKind::Dir | EntryKind::Symlink { dir: true, outside: false })
    }

    /// Sorted with the folders, and drawn with a folder's icon.
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir | EntryKind::Symlink { dir: true, .. })
    }

    /// A dotfile: hidden unless the panel shows hidden files. `.git` is hidden always, as no one browses it.
    pub fn hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// One folder's entries, sorted; `truncated` when it held more than `MAX_ENTRIES`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

impl Listing {
    /// A digest of its names, kinds and sizes: a poll that finds the same one leaves the tree alone, and the poll
    /// is handed this rather than the listing (what git ignores is not counted: it is asked only when names change).
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.truncated.hash(&mut h);
        for e in &self.entries {
            (&e.os_name, e.kind, e.size).hash(&mut h);
        }
        h.finish()
    }
}

/// Reads `dir` (blocking; call it off the main thread), at most `MAX_ENTRIES` of it, sorted. A symlink is followed
/// to tell a folder from a file and whether it leaves `root`; one that points nowhere is a file. `.git` is left out,
/// and so are a save's temp files (`.name.shepherd-PID-N~`), which a save in flight or one cut short leaves.
pub fn list(dir: &Path, root: &Path) -> io::Result<Listing> {
    // a root that is itself a link (`~/src` → `/data/src`) still holds its own folders
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut listing = Listing::default();
    for item in std::fs::read_dir(dir)? {
        if listing.entries.len() == MAX_ENTRIES {
            listing.truncated = true;
            break;
        }
        // an entry gone between the read and the stat (a build's temp file) is just not listed
        let Ok(item) = item else { continue };
        let os_name = item.file_name();
        if os_name == ".git" || crate::files::save::is_temp_name(&os_name) {
            continue;
        }
        let Ok(file_type) = item.file_type() else { continue };
        let (kind, size) = if file_type.is_symlink() {
            let path = item.path();
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let outside = !path.canonicalize().is_ok_and(|t| t.starts_with(&root));
                    (EntryKind::Symlink { dir: meta.is_dir(), outside }, if meta.is_dir() { 0 } else { meta.len() })
                }
                Err(_) => (EntryKind::Dangling, 0),
            }
        } else if file_type.is_dir() {
            (EntryKind::Dir, 0)
        } else {
            (EntryKind::File, item.metadata().map(|m| m.len()).unwrap_or(0))
        };
        listing.entries.push(Entry { name: os_name.to_string_lossy().into_owned(), os_name, kind, size, ignored: false });
    }
    sort(&mut listing.entries);
    Ok(listing)
}

/// Folders first, then files; each by name, case not counting, and digits by their number (`file2` before
/// `file10`), as Finder and Nautilus sort.
pub fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| b.is_dir().cmp(&a.is_dir()).then_with(|| natural(&a.name, &b.name)).then_with(|| a.os_name.cmp(&b.os_name)));
}

/// `a` against `b` with runs of digits compared as numbers and letters without case.
fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut run = String::new();
                    while let Some(c) = it.next_if(|c| c.is_ascii_digit()) {
                        run.push(c);
                    }
                    run
                };
                let (x, y) = (take(&mut a), take(&mut b));
                let (tx, ty) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                // more digits is a bigger number; as many, the digits decide; then fewer leading zeros first
                let order = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty)).then_with(|| x.len().cmp(&y.len()));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// A line of the list: an entry, or a note under an open folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Entry {
        path: PathBuf,
        depth: usize,
        entry: Entry,
        expanded: bool,
        /// git ignores it or a folder above it (shown only while the panel shows ignored files, dimmed).
        ignored: bool,
    },
    /// Under the folder at `path`: why it could not be read, or that it holds more than is listed.
    Note { path: PathBuf, depth: usize, text: String },
}

impl Row {
    pub fn path(&self) -> &Path {
        match self {
            Row::Entry { path, .. } | Row::Note { path, .. } => path,
        }
    }
}

/// What the tree holds of each read folder, by its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Listed(Listing),
    Failed(String),
}

/// The rows the tree shows: `root`'s entries, and under each open folder that has been read, its own, deeper by
/// one. Dotfiles are left out without `show_hidden`, and what git ignores (or sits in an ignored folder) without
/// `show_ignored`.
pub fn visible_rows(root: &Path, reads: &HashMap<PathBuf, Read>, expanded: &HashSet<PathBuf>, show_hidden: bool, show_ignored: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    push_rows(&mut rows, root, 0, false, reads, expanded, show_hidden, show_ignored);
    rows
}

#[allow(clippy::too_many_arguments)]
fn push_rows(rows: &mut Vec<Row>, dir: &Path, depth: usize, ignored_above: bool, reads: &HashMap<PathBuf, Read>, expanded: &HashSet<PathBuf>, show_hidden: bool, show_ignored: bool) {
    let listing = match reads.get(dir) {
        Some(Read::Listed(listing)) => listing,
        Some(Read::Failed(error)) => return rows.push(Row::Note { path: dir.to_path_buf(), depth, text: error.clone() }),
        // still being read: nothing yet, rather than a "Loading…" that flashes for a local folder
        None => return,
    };
    for entry in &listing.entries {
        let ignored = ignored_above || entry.ignored;
        if (entry.hidden() && !show_hidden) || (ignored && !show_ignored) {
            continue;
        }
        let path = dir.join(&entry.os_name);
        let open = entry.expandable() && expanded.contains(&path);
        rows.push(Row::Entry { path: path.clone(), depth, entry: entry.clone(), expanded: open, ignored });
        if open {
            push_rows(rows, &path, depth + 1, ignored, reads, expanded, show_hidden, show_ignored);
        }
    }
    if listing.truncated {
        rows.push(Row::Note { path: dir.to_path_buf(), depth, text: format!("Only the first {} entries are listed", group_digits(MAX_ENTRIES)) });
    }
}

fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `git check-ignore -z --stdin`'s answer: the ignored paths it was given, NUL-separated, in the raw bytes they
/// were sent in (a name that is not UTF-8 comes back as it went).
pub fn parse_check_ignore(out: &[u8]) -> Vec<Vec<u8>> {
    out.split(|b| *b == 0).filter(|p| !p.is_empty()).map(<[u8]>::to_vec).collect()
}

/// What `git check-ignore -z --stdin` is fed for `listing` of `rel` (the folder's path under the root, empty for
/// the root): each entry's path under the root in the file system's own bytes, NUL-terminated. A folder goes with
/// a trailing `/`, so a `build/` pattern matches it; a link does not (git takes a link as a file, and refuses a
/// path through one).
pub fn check_ignore_input(rel: &Path, listing: &Listing) -> Vec<u8> {
    let mut input = Vec::new();
    for entry in &listing.entries {
        input.extend_from_slice(&crate::platform::git_path(&rel.join(&entry.os_name)));
        if entry.kind == EntryKind::Dir {
            input.push(b'/');
        }
        input.push(0);
    }
    input
}

/// Marks the entries of `listing` (of `rel` under the root) that `ignored` (`parse_check_ignore`'s answer) names.
pub fn mark_ignored(listing: &mut Listing, rel: &Path, ignored: &[Vec<u8>]) {
    let ignored: HashSet<&[u8]> = ignored.iter().map(|p| p.strip_suffix(b"/").unwrap_or(p)).collect();
    for entry in &mut listing.entries {
        entry.ignored = ignored.contains(&*crate::platform::git_path(&rel.join(&entry.os_name)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry { name: name.into(), os_name: name.into(), kind, size: 0, ignored: false }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn folders_come_first_then_names_without_case_and_digits_by_number() {
        let mut entries = vec![
            entry("file10.txt", EntryKind::File),
            entry("b.rs", EntryKind::File),
            entry("src", EntryKind::Dir),
            entry("file2.txt", EntryKind::File),
            entry("A.md", EntryKind::File),
            entry("link", EntryKind::Symlink { dir: true, outside: true }),
            entry("Docs", EntryKind::Dir),
            entry("file02.txt", EntryKind::File),
        ];
        sort(&mut entries);
        assert_eq!(names(&entries), ["Docs", "link", "src", "A.md", "b.rs", "file2.txt", "file02.txt", "file10.txt"]);
    }

    fn listed(entries: Vec<Entry>) -> Read {
        Read::Listed(Listing { entries, truncated: false })
    }

    fn row_names(rows: &[Row]) -> Vec<(String, usize)> {
        rows.iter()
            .map(|r| match r {
                Row::Entry { entry, depth, .. } => (entry.name.clone(), *depth),
                Row::Note { text, depth, .. } => (format!("note: {text}"), *depth),
            })
            .collect()
    }

    #[test]
    fn rows_follow_open_folders_and_leave_out_hidden_and_ignored_ones() {
        let root = PathBuf::from("/r");
        let mut target = entry("target", EntryKind::Dir);
        target.ignored = true;
        let mut reads = HashMap::new();
        reads.insert(root.clone(), listed(vec![entry("src", EntryKind::Dir), target, entry(".env", EntryKind::File), entry("Cargo.toml", EntryKind::File)]));
        reads.insert(root.join("src"), listed(vec![entry("ui", EntryKind::Dir), entry("main.rs", EntryKind::File)]));
        // read once, then folded: its entries are kept but not shown
        reads.insert(root.join("src/ui"), listed(vec![entry("mod.rs", EntryKind::File)]));
        reads.insert(root.join("target"), listed(vec![entry("debug", EntryKind::Dir)]));
        let mut expanded = HashSet::from([root.join("src"), root.join("target")]);

        let rows = visible_rows(&root, &reads, &expanded, false, false);
        assert_eq!(row_names(&rows), [("src".into(), 0), ("ui".into(), 1), ("main.rs".into(), 1), ("Cargo.toml".into(), 0)]);
        assert!(matches!(&rows[0], Row::Entry { expanded: true, path, .. } if path == &root.join("src")));
        assert!(matches!(&rows[1], Row::Entry { expanded: false, .. }));

        let rows = visible_rows(&root, &reads, &expanded, true, true);
        let shown: Vec<_> = row_names(&rows).into_iter().map(|(n, _)| n).collect();
        assert_eq!(shown, ["src", "ui", "main.rs", "target", "debug", ".env", "Cargo.toml"]);
        // inside an ignored folder is ignored too
        assert!(rows.iter().any(|r| matches!(r, Row::Entry { entry, ignored: true, .. } if entry.name == "debug")));

        expanded.insert(root.join("src/ui"));
        let rows = visible_rows(&root, &reads, &expanded, false, false);
        assert_eq!(row_names(&rows)[2], ("mod.rs".into(), 2));
    }

    #[test]
    fn a_folder_that_failed_or_holds_too_much_says_so_under_it() {
        let root = PathBuf::from("/r");
        let mut reads = HashMap::new();
        reads.insert(root.clone(), listed(vec![entry("locked", EntryKind::Dir), entry("big", EntryKind::Dir), entry("slow", EntryKind::Dir)]));
        reads.insert(root.join("locked"), Read::Failed("Permission denied".into()));
        reads.insert(root.join("big"), Read::Listed(Listing { entries: vec![entry("a", EntryKind::File)], truncated: true }));
        let expanded = HashSet::from([root.join("locked"), root.join("big"), root.join("slow")]);
        let rows = visible_rows(&root, &reads, &expanded, false, false);
        assert_eq!(
            row_names(&rows),
            [
                ("locked".into(), 0),
                ("note: Permission denied".into(), 1),
                ("big".into(), 0),
                ("a".into(), 1),
                ("note: Only the first 20,000 entries are listed".into(), 1),
                // not read yet: no rows under it
                ("slow".into(), 0),
            ]
        );
    }

    #[test]
    fn check_ignore_names_come_back_split_on_nul_and_mark_their_entries() {
        assert_eq!(parse_check_ignore(b"target/\0a b.log\0"), [b"target/".to_vec(), b"a b.log".to_vec()]);
        assert!(parse_check_ignore(b"").is_empty());
        let mut listing = Listing { entries: vec![entry("target", EntryKind::Dir), entry("main.rs", EntryKind::File), entry("x.log", EntryKind::File)], truncated: false };
        let rel = Path::new("crate");
        assert_eq!(check_ignore_input(rel, &listing), b"crate/target/\0crate/main.rs\0crate/x.log\0");
        mark_ignored(&mut listing, rel, &parse_check_ignore(b"crate/target/\0crate/x.log\0"));
        assert_eq!(listing.entries.iter().map(|e| e.ignored).collect::<Vec<_>>(), [true, false, true]);
        assert_eq!(check_ignore_input(Path::new(""), &listing)[..7], *b"target/");
    }

    /// A name that is not UTF-8 goes to git, and is matched in its answer, byte for byte: a lossy `�` would match
    /// nothing (or the wrong file).
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_goes_to_git_as_its_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let raw = b"caf\xe9.log".to_vec();
        let odd = Entry { name: String::from_utf8_lossy(&raw).into_owned(), os_name: OsString::from_vec(raw.clone()), kind: EntryKind::File, size: 0, ignored: false };
        let mut listing = Listing { entries: vec![odd, entry("cafe.log", EntryKind::File)], truncated: false };
        assert_eq!(check_ignore_input(Path::new(""), &listing), [raw.as_slice(), b"\0cafe.log\0"].concat());
        mark_ignored(&mut listing, Path::new(""), &parse_check_ignore(&[raw.as_slice(), b"\0"].concat()));
        assert_eq!(listing.entries.iter().map(|e| e.ignored).collect::<Vec<_>>(), [true, false]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_root_is_not_opened_and_one_inside_is() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/a.rs"), "fn main() {}").unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("away")).unwrap();
        std::os::unix::fs::symlink(root.path().join("src"), root.path().join("near")).unwrap();
        std::os::unix::fs::symlink(root.path().join("gone"), root.path().join("dangling")).unwrap();
        let listing = list(root.path(), root.path()).unwrap();
        assert_eq!(names(&listing.entries), ["away", "near", "src", "dangling"]);
        let kind = |name: &str| listing.entries.iter().find(|e| e.name == name).unwrap().clone();
        assert_eq!(kind("away").kind, EntryKind::Symlink { dir: true, outside: true });
        assert!(!kind("away").expandable() && kind("away").is_dir());
        assert_eq!(kind("near").kind, EntryKind::Symlink { dir: true, outside: false });
        assert!(kind("near").expandable());
        assert_eq!(kind("dangling").kind, EntryKind::Dangling);
        let src = list(&root.path().join("src"), root.path()).unwrap();
        assert_eq!((src.entries[0].kind, src.entries[0].size), (EntryKind::File, 12));
        assert!(list(&root.path().join("missing"), root.path()).is_err());
    }

    #[test]
    fn a_saves_temp_files_are_never_listed() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a.rs", ".a.rs.shepherd-123-4~", ".shepherd-1-2~x", "b.shepherd-1-2~"] {
            std::fs::write(root.path().join(name), "").unwrap();
        }
        let listing = list(root.path(), root.path()).unwrap();
        assert_eq!(names(&listing.entries), [".shepherd-1-2~x", "a.rs", "b.shepherd-1-2~"]);
    }

    #[test]
    fn a_poll_compares_names_kinds_and_sizes_not_what_git_ignores() {
        let a = Listing { entries: vec![entry("a", EntryKind::File)], truncated: false };
        let mut b = a.clone();
        b.entries[0].ignored = true;
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.entries[0].size = 3;
        assert_ne!(a.fingerprint(), b.fingerprint());
    }
}
