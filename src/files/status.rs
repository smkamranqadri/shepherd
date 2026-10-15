//! What `git status --porcelain=v2 -z --branch` says of a repository, read for the Changes tab: the branch, and each
//! changed path sorted into the group VS Code's Source Control shows it in. The panel's root may be a folder inside
//! the repository; the parser keeps what is under it and counts the rest, so the tab can say there is more elsewhere
//! without listing a monorepo's every change under one package's folder.

/// The group a change is listed in, in the order the tab shows them: conflicts first, as they block a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Conflicts,
    Staged,
    Changes,
    Untracked,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Conflicts, Group::Staged, Group::Changes, Group::Untracked];

    pub fn label(self) -> &'static str {
        match self {
            Group::Conflicts => "Merge Changes",
            Group::Staged => "Staged Changes",
            Group::Changes => "Changes",
            Group::Untracked => "Untracked",
        }
    }
}

/// One row of the tab: a path in one group. A file staged and then edited again is two changes, one in Staged and one
/// in Changes, as git (and VS Code) count it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub group: Group,
    /// Relative to the repository's top, as git prints it (`core.quotepath=false`: as it is, not octal-quoted). An
    /// untracked folder git does not look into (a nested repository) ends in `/`.
    pub path: String,
    /// A rename's or copy's source, also from the top; it may lie outside the root.
    pub orig: Option<String>,
    /// git's letter for it in its group: `M`, `A`, `D`, `R`, `C`, `T`; `U` for an untracked file and `!` for a
    /// conflict (whose own two letters, `UU`, `AA`, `DU`…, say who changed what, which the row does not need).
    pub code: char,
    /// git's own two letters, index then work tree (`.M`, `R.`, `UU`, `DD`; `??` for untracked): what the row's
    /// single letter leaves out, such as whether the work tree still has the file.
    pub xy: [char; 2],
    /// A submodule's folder, not a file: there is nothing to open.
    pub submodule: bool,
    /// The row stands for a rename's source, under the root, whose destination is outside it (`path` is the source
    /// and this the destination): listed as a delete, as the root sees it.
    pub moved_to: Option<String>,
}

impl Change {
    /// The working tree has no file at `path`: deleted from it, both sides of a conflict deleted it, or it was
    /// renamed out of the root. (A staged delete, `D.`, may still have its file: `git rm --cached` keeps it. A
    /// conflict's `UD` or `DU` is one side's delete against the other's edit, and git leaves the edited file.)
    pub fn absent(&self) -> bool {
        let deleted = match self.group {
            Group::Conflicts => self.xy == ['D', 'D'],
            _ => self.xy[1] == 'D',
        };
        deleted || self.moved_to.is_some()
    }

    /// A folder rather than a file: a submodule, or an untracked nested repository.
    pub fn is_folder(&self) -> bool {
        self.submodule || self.path.ends_with('/')
    }

    /// There is a file in the working tree to open.
    pub fn openable(&self) -> bool {
        !self.absent() && !self.is_folder()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The checked-out branch; `None` on a detached HEAD, where `detached` names the commit.
    pub branch: Option<String>,
    /// A detached HEAD's commit, abbreviated; `None` on a branch or before the first commit.
    pub detached: Option<String>,
    /// Commits ahead of and behind the upstream, when the branch has one.
    pub ahead_behind: Option<(u32, u32)>,
    /// The changes under the root, each group's sorted by path.
    pub changes: Vec<Change>,
    /// Changes in the repository outside the root: counted, not listed.
    pub outside_root: usize,
}

/// Why there is no list to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    GitMissing,
    FolderMissing,
    NotRepo,
    /// git refuses a repository another user owns until it is marked safe; the repository's path, as git names it.
    DubiousOwnership(String),
    Timeout,
    /// The status ran past its output cap: more changes than the panel will hold.
    TooLarge,
    /// Anything else, as git's first line of complaint says it.
    Failed(String),
}

/// Parses `git status --porcelain=v2 -z --branch` output, keeping the changes whose path is under `prefix` (the root's
/// own path inside the repository, `git rev-parse --show-prefix`: empty at the top, else ending in `/`).
pub fn parse_status_v2(out: &[u8], prefix: &str) -> Status {
    let mut status = Status::default();
    let mut oid = None;
    let mut fields = out.split(|&b| b == 0).map(|f| String::from_utf8_lossy(f));
    let under = |path: &str| path.starts_with(prefix);
    let push = |status: &mut Status, change: Change| {
        if under(&change.path) {
            status.changes.push(change);
        } else {
            status.outside_root += 1;
        }
    };
    while let Some(record) = fields.next() {
        if let Some(header) = record.strip_prefix("# ") {
            match header.split_once(' ') {
                Some(("branch.oid", v)) if v != "(initial)" => oid = Some(v.chars().take(7).collect::<String>()),
                Some(("branch.head", v)) if v != "(detached)" => status.branch = Some(v.to_string()),
                Some(("branch.ab", v)) => {
                    let mut n = v.split(' ').map(|n| n.trim_start_matches(['+', '-']).parse::<u32>().ok());
                    if let (Some(Some(a)), Some(Some(b))) = (n.next(), n.next()) {
                        status.ahead_behind = Some((a, b));
                    }
                }
                _ => {}
            }
            continue;
        }
        // `1 XY sub mH mI mW hH hI path`, `2 XY sub mH mI mW hH hI Xscore path` then the source as its own field,
        // `u XY sub m1 m2 m3 mW h1 h2 h3 path`, `? path`; `!` (ignored) is not asked for. The path is the record's
        // last field and may hold spaces, so the split stops before it.
        let kind = record.as_bytes().first().copied();
        let (n, ordinary) = match kind {
            Some(b'1') => (9, true),
            Some(b'2') => (10, true),
            Some(b'u') => (11, false),
            Some(b'?') => {
                if let Some(path) = record.strip_prefix("? ") {
                    let change = Change { group: Group::Untracked, path: path.into(), orig: None, code: 'U', xy: ['?', '?'], submodule: false, moved_to: None };
                    push(&mut status, change);
                }
                continue;
            }
            _ => continue,
        };
        let parts: Vec<&str> = record.splitn(n, ' ').collect();
        if parts.len() != n {
            continue;
        }
        let (sub, path) = (parts[2], parts[n - 1]);
        let mut letters = parts[1].chars();
        let (Some(x), Some(y)) = (letters.next(), letters.next()) else { continue };
        let change = |group: Group, code: char| Change { group, path: path.into(), orig: None, code, xy: [x, y], submodule: sub.starts_with('S'), moved_to: None };
        // a rename's source is the next field, whether or not the change is kept
        let orig = (kind == Some(b'2')).then(|| fields.next().map(|f| f.into_owned())).flatten();
        if !ordinary {
            push(&mut status, change(Group::Conflicts, '!'));
            continue;
        }
        if x != '.' {
            match orig.as_deref() {
                // renamed out of the root: what the root sees is its file gone
                Some(from) if x == 'R' && !under(path) && under(from) => {
                    status.changes.push(Change { path: from.into(), code: 'D', moved_to: Some(path.into()), ..change(Group::Staged, 'D') });
                }
                _ => push(&mut status, Change { orig: orig.clone(), ..change(Group::Staged, x) }),
            }
        }
        if y != '.' {
            // the rename is the index's: the work tree's edit is to the file under its new name
            push(&mut status, change(Group::Changes, y));
        }
    }
    if status.branch.is_none() {
        status.detached = oid;
    }
    status.changes.sort_by(|a, b| (a.group, &a.path).cmp(&(b.group, &b.path)));
    status
}

/// What a failed git run's stderr means for the tab.
pub fn classify(stderr: &str) -> GitError {
    if stderr.contains("not a git repository") {
        return GitError::NotRepo;
    }
    // Since git 2.37.1 (and 2.36.2, 2.35.4): "fatal: detected dubious ownership in repository at '/path'"; 2.35.2
    // to those: "fatal: unsafe repository ('/path' is owned by someone else)". The path is the rest of the line
    // less the closing quote: it may hold quotes of its own.
    for (start, end) in [("dubious ownership in repository at '", "'"), ("unsafe repository ('", "' is owned by someone else)")] {
        if let Some(at) = stderr.find(start) {
            let line = stderr[at + start.len()..].lines().next().unwrap_or_default().trim_end();
            return GitError::DubiousOwnership(line.strip_suffix(end).unwrap_or(line).to_string());
        }
    }
    let line = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("git failed");
    GitError::Failed(line.strip_prefix("fatal: ").or_else(|| line.strip_prefix("error: ")).unwrap_or(line).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nul(records: &[&str]) -> Vec<u8> {
        records.iter().flat_map(|r| r.bytes().chain([0])).collect()
    }

    const H: &str = "100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn rows(s: &Status) -> Vec<(Group, &str, char)> {
        s.changes.iter().map(|c| (c.group, c.path.as_str(), c.code)).collect()
    }

    fn find<'a>(s: &'a Status, group: Group, path: &str) -> &'a Change {
        s.changes.iter().find(|c| c.group == group && c.path == path).unwrap()
    }

    #[test]
    fn ordinary_renamed_unmerged_and_untracked_records_land_in_their_groups() {
        let out = nul(&[
            "# branch.oid 0725a805caa63ada89cb3741e38e85da80995693",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            &format!("1 .M N... {H} src/a b.rs"),
            &format!("1 MM N... {H} src/both.rs"),
            &format!("1 D. N... {H} gone.rs"),
            &format!("2 R. N... {H} R100 src/new name.rs"),
            "old name.rs",
            "u UU N... 100644 100644 100644 100644 a b c fight.rs",
            "? notes/todo.md",
        ]);
        let s = parse_status_v2(&out, "");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.detached, None);
        assert_eq!(s.ahead_behind, Some((2, 1)));
        assert_eq!(s.outside_root, 0);
        assert_eq!(
            rows(&s),
            [
                (Group::Conflicts, "fight.rs", '!'),
                (Group::Staged, "gone.rs", 'D'),
                (Group::Staged, "src/both.rs", 'M'),
                (Group::Staged, "src/new name.rs", 'R'),
                (Group::Changes, "src/a b.rs", 'M'),
                (Group::Changes, "src/both.rs", 'M'),
                (Group::Untracked, "notes/todo.md", 'U'),
            ]
        );
        let renamed = find(&s, Group::Staged, "src/new name.rs");
        assert_eq!((renamed.orig.as_deref(), renamed.xy), (Some("old name.rs"), ['R', '.']));
        assert_eq!(find(&s, Group::Conflicts, "fight.rs").xy, ['U', 'U']);
        assert_eq!(find(&s, Group::Untracked, "notes/todo.md").xy, ['?', '?']);
    }

    /// Only a file the work tree lacks, or a folder, has nothing to open.
    #[test]
    fn what_can_be_opened() {
        let out = nul(&[
            &format!("1 .D N... {H} unstaged-delete"),
            &format!("1 D. N... {H} staged-delete"),
            "u DD N... 100644 100644 100644 100644 a b c both-deleted",
            "u UD N... 100644 100644 100644 100644 a b c they-deleted",
            &format!("1 .M SC.. {H} vendor/lib"),
            "? nested/",
            "? file",
        ]);
        let s = parse_status_v2(&out, "");
        let openable = |group, path| find(&s, group, path).openable();
        assert!(!openable(Group::Changes, "unstaged-delete"));
        assert!(openable(Group::Staged, "staged-delete"));
        assert!(!openable(Group::Conflicts, "both-deleted"));
        assert!(openable(Group::Conflicts, "they-deleted"));
        assert!(!openable(Group::Changes, "vendor/lib"));
        assert!(find(&s, Group::Untracked, "nested/").is_folder() && !openable(Group::Untracked, "nested/"));
        assert!(openable(Group::Untracked, "file"));
    }

    #[test]
    fn a_prefix_keeps_what_is_under_it_and_counts_the_rest() {
        let out = nul(&[
            "# branch.head main",
            &format!("1 .M N... {H} app/x.rs"),
            &format!("1 .M N... {H} lib/y.rs"),
            // renamed into the root from outside it: kept, and its source does not count as a record
            &format!("2 R. N... {H} R90 app/moved.rs"),
            "lib/moved.rs",
            // renamed out of it, and edited there since: the root sees its file gone; the edit is elsewhere
            &format!("2 RM N... {H} R90 lib/out.rs"),
            "app/out.rs",
            // copied out: the source is still there, nothing under the root changed
            &format!("2 C. N... {H} C90 lib/copy.rs"),
            "app/x.rs",
            "? app/new.txt",
            "? application.txt",
        ]);
        let s = parse_status_v2(&out, "app/");
        assert_eq!(
            rows(&s),
            [(Group::Staged, "app/moved.rs", 'R'), (Group::Staged, "app/out.rs", 'D'), (Group::Changes, "app/x.rs", 'M'), (Group::Untracked, "app/new.txt", 'U')]
        );
        let out_row = find(&s, Group::Staged, "app/out.rs");
        assert_eq!(out_row.moved_to.as_deref(), Some("lib/out.rs"));
        assert!(out_row.absent() && !out_row.openable());
        // lib/y.rs, lib/out.rs's edit, the copy, application.txt
        assert_eq!(s.outside_root, 4);
    }

    #[test]
    fn a_detached_head_names_its_commit_and_a_new_repo_has_neither() {
        let s = parse_status_v2(&nul(&["# branch.oid 0725a805caa63ada89cb3741e38e85da80995693", "# branch.head (detached)"]), "");
        assert_eq!((s.branch, s.detached.as_deref()), (None, Some("0725a80")));
        let s = parse_status_v2(&nul(&["# branch.oid (initial)", "# branch.head main", "? a"]), "");
        assert_eq!((s.branch.as_deref(), s.detached.as_deref(), s.ahead_behind), (Some("main"), None, None));
        assert_eq!(rows(&s), [(Group::Untracked, "a", 'U')]);
    }

    /// A rename's source is a field of its own, whatever it looks like: a file named `? x` or `1 .M …` is not read
    /// as a record. Ignored (`!`) and unknown records are skipped, not miscounted as changes outside the root.
    #[test]
    fn a_source_that_looks_like_a_record_unknown_records_and_ignored_ones_are_not_changes() {
        let out = nul(&[
            &format!("2 R. N... {H} R100 new.rs"),
            "? looks untracked",
            &format!("2 C. N... {H} C75 copy.rs"),
            &format!("1 .M N... {H} fake"),
            "! target/debug",
            "z some future record",
            "# branch.future x",
            "? real",
        ]);
        let s = parse_status_v2(&out, "");
        assert_eq!(rows(&s), [(Group::Staged, "copy.rs", 'C'), (Group::Staged, "new.rs", 'R'), (Group::Untracked, "real", 'U')]);
        assert_eq!(s.changes[0].orig.as_deref(), Some(format!("1 .M N... {H} fake").as_str()));
        assert_eq!(s.changes[1].orig.as_deref(), Some("? looks untracked"));
        assert_eq!(s.outside_root, 0);
        // the same under a prefix: the sources do not count as outside
        let s = parse_status_v2(&out, "sub/");
        assert_eq!(s.outside_root, 3);
    }

    #[test]
    fn a_submodule_is_marked_and_a_cut_record_is_skipped() {
        let out = nul(&[&format!("1 .M SC.. {H} vendor/lib"), "1 .M N... 100644 cut"]);
        let s = parse_status_v2(&out, "");
        assert_eq!(s.changes.len(), 1);
        assert!(s.changes[0].submodule);
        assert_eq!(parse_status_v2(b"", ""), Status::default());
    }

    #[test]
    fn stderr_tells_not_a_repo_from_dubious_ownership_from_the_rest() {
        assert_eq!(classify("fatal: not a git repository (or any of the parent directories): .git\n"), GitError::NotRepo);
        let dubious = "fatal: detected dubious ownership in repository at '/srv/x y'\nTo add an exception for this directory, call:\n\n\tgit config --global --add safe.directory '/srv/x y'\n";
        assert_eq!(classify(dubious), GitError::DubiousOwnership("/srv/x y".into()));
        assert_eq!(classify("\nfatal: index file corrupt\n"), GitError::Failed("index file corrupt".into()));
        assert_eq!(classify(""), GitError::Failed("git failed".into()));
    }

    /// A path with quotes of its own is kept whole; git 2.35.2–2.37's wording says the same thing differently.
    #[test]
    fn dubious_ownership_keeps_quotes_in_the_path_and_reads_the_older_wording() {
        let dubious = "fatal: detected dubious ownership in repository at '/srv/it's here'\n";
        assert_eq!(classify(dubious), GitError::DubiousOwnership("/srv/it's here".into()));
        let crlf = "fatal: detected dubious ownership in repository at 'C:/x'\r\n";
        assert_eq!(classify(crlf), GitError::DubiousOwnership("C:/x".into()));
        let older = "fatal: unsafe repository ('/srv/o'neil' is owned by someone else)\nTo add an exception for this directory, call:\n";
        assert_eq!(classify(older), GitError::DubiousOwnership("/srv/o'neil".into()));
    }

    /// The real thing, in a scratch repository: paths come from the top even when git runs in a folder under it.
    #[test]
    fn git_status_in_a_subfolder_parses_from_the_top() {
        if std::process::Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let config = ["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"];
            let ok = std::process::Command::new("git").args(config).args(args).current_dir(dir.path()).status().unwrap();
            assert!(ok.success());
        };
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/x"), "a").unwrap();
        std::fs::write(dir.path().join("top"), "b").unwrap();
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "i"]);
        std::fs::write(dir.path().join("sub/x"), "c").unwrap();
        std::fs::write(dir.path().join("top"), "d").unwrap();
        let out = std::process::Command::new("git")
            .args(["-c", "status.renames=true", "status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"])
            .current_dir(dir.path().join("sub"))
            .output()
            .unwrap();
        let s = parse_status_v2(&out.stdout, "sub/");
        assert_eq!(rows(&s), [(Group::Changes, "sub/x", 'M')]);
        assert_eq!(s.outside_root, 1);
    }
}
