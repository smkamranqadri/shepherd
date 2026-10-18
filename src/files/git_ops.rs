//! What the Changes tab asks git to change: stage, unstage, discard and commit, whole files only. Each is an
//! argument list built here from a status row (`files::status`), run under `files::runner::git_cmd`'s preamble with
//! `GIT_LITERAL_PATHSPECS=1`, so a file named `*.rs` or `:(top)x` is that file and nothing else, and every path
//! after a `--`, so one named `-n` is not an option. A commit's message goes in on stdin (`commit -F -`) and git is
//! never left to open an editor. Paths are the status's own, from the repository's top, where the per-file
//! commands run; the "all" commands run in the root, so a root inside a monorepo leaves the rest of it alone.
use crate::files::runner::{self, Cmd, Host, Limits, RunError};
use crate::files::status::{Change, Group};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A file's stage, unstage or discard: the index of a big repository is rewritten in a second or two.
const OP: Limits = Limits { timeout: Duration::from_secs(30), max_bytes: 1 << 20 };
/// Stage All walks the root's whole work tree.
const ALL: Limits = Limits { timeout: Duration::from_secs(60), max_bytes: 1 << 20 };
/// A commit runs its hooks (a test suite, a linter), so it is given a long while. It has no cancel: at its deadline
/// it is stopped as every command here is (`runner::run_stopping`), since SIGKILL mid-hook would leave no lock but
/// orphan the hook, and mid-write would leave `index.lock`.
const COMMIT: Limits = Limits { timeout: Duration::from_secs(5 * 60), max_bytes: 1 << 20 };
/// `rev-parse` and `log -1`: a line or a message.
const QUICK: Limits = Limits { timeout: Duration::from_secs(5), max_bytes: 1 << 20 };
/// How long a command asked to stop at its deadline (SIGTERM to its group, which lets git take `index.lock` away)
/// has before it is killed.
const GRACE: Duration = Duration::from_secs(3);
/// What a busy index's failure adds to git's words.
pub const STALE_LOCK: &str = "If no other git is running, one that crashed left the lock behind: delete that index.lock file, then Retry.";
/// What of a failure's output the tab shows.
const FAILURE_LINES: usize = 20;

/// A discard the user said yes to (`DiscardConfirm::answered`): nothing else makes one, so no discard runs unasked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmed(Change);

/// What the tab asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// A Changes or Untracked row's +, or a conflict's Mark resolved.
    Stage(Change),
    /// A Staged row's −.
    Unstage(Change),
    /// The Changes header's Stage All: every tracked change in the root.
    StageTracked,
    /// The Untracked header's Stage All: these files (from the top), nested repositories left out.
    StageUntracked(Vec<String>),
    /// The Staged header's Unstage All, in the root.
    UnstageAll,
    Discard(Confirmed),
    Commit { message: String, amend: bool },
}

impl Op {
    /// What failed, as the error block's title starts.
    pub fn verb(&self) -> &'static str {
        match self {
            Op::Stage(c) if c.group == Group::Conflicts => "Couldn't mark it resolved",
            Op::Stage(_) | Op::StageTracked | Op::StageUntracked(_) => "Couldn't stage",
            Op::Unstage(_) | Op::UnstageAll => "Couldn't unstage",
            Op::Discard(_) => "Couldn't discard",
            Op::Commit { amend: true, .. } => "Couldn't amend the commit",
            Op::Commit { .. } => "Couldn't commit",
        }
    }
}

/// Where a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// The repository's top: the status's paths are relative to it.
    Top,
    /// The panel's root: `.` is the root and nothing outside it.
    Root,
}

/// One git run: where, its arguments after the preamble, its stdin and limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub at: At,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub env: Vec<(String, String)>,
    limits: (Duration, usize),
}

/// Why the tab offers no such thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// An untracked folder (a nested repository): `git clean` would take a whole repository with it, and `git add`
    /// would embed one.
    Folder,
    /// Only a Changes row (a tracked file's edit, delete or type change) or an Untracked file is discarded; a
    /// staged change is unstaged first, and an intent-to-add file has nothing to go back to.
    NotDiscardable,
    /// The row's group has no such action.
    Group,
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn invocation(at: At, args: Vec<String>, limits: Limits) -> Invocation {
    Invocation { at, args, stdin: None, env: Vec::new(), limits: (limits.timeout, limits.max_bytes) }
}

/// `change`'s paths for a command that takes both sides of a rename: the source (when it is another path), then the
/// path.
fn paths(change: &Change) -> Vec<String> {
    change.orig.iter().filter(|o| **o != change.path).cloned().chain([change.path.clone()]).collect()
}

/// A Changes or Untracked row staged, or a conflict marked resolved: `git add -- <path>` (a rename both its names, so
/// the source's delete goes in with it).
pub fn stage(change: &Change) -> Result<Invocation, Refusal> {
    if change.group == Group::Staged {
        return Err(Refusal::Group);
    }
    if change.group == Group::Untracked && change.is_folder() {
        return Err(Refusal::Folder);
    }
    let mut args = strings(&["add", "--"]);
    args.extend(paths(change));
    Ok(invocation(At::Top, args, OP))
}

/// A Staged row back out of the index: `git reset -q -- <path>` (a rename both names, so its source comes back).
/// Before the first commit there is no HEAD to reset to: `git rm --cached` takes the file out of the index and
/// leaves it on disk (`-f`, as a file staged then edited again would otherwise be refused).
pub fn unstage(change: &Change, initial: bool) -> Result<Invocation, Refusal> {
    if change.group != Group::Staged {
        return Err(Refusal::Group);
    }
    let mut args = if initial { strings(&["rm", "--cached", "-f", "-q", "--ignore-unmatch", "--"]) } else { strings(&["reset", "-q", "--"]) };
    args.extend(paths(change));
    Ok(invocation(At::Top, args, OP))
}

/// The Changes header's Stage All: `git add -u -- .` in the root, every tracked change under it and nothing outside.
pub fn stage_tracked() -> Invocation {
    invocation(At::Root, strings(&["add", "-u", "--", "."]), ALL)
}

/// The Untracked header's Stage All: the listed files (from the top), fed on stdin so a thousand of them fit, with
/// nested repositories (a path ending in `/`) left out. `None` when that leaves nothing.
pub fn stage_untracked(files: &[String]) -> Option<Invocation> {
    let files: Vec<&String> = files.iter().filter(|f| !f.ends_with('/')).collect();
    if files.is_empty() {
        return None;
    }
    let mut stdin = Vec::new();
    for f in files {
        stdin.extend_from_slice(f.as_bytes());
        stdin.push(0);
    }
    let mut inv = invocation(At::Top, strings(&["add", "--pathspec-from-file=-", "--pathspec-file-nul"]), ALL);
    inv.stdin = Some(stdin);
    Some(inv)
}

/// The Staged header's Unstage All, in the root; before the first commit, the root's files out of the index.
pub fn unstage_all(initial: bool) -> Invocation {
    let args = if initial { strings(&["rm", "--cached", "-r", "-f", "-q", "--ignore-unmatch", "--", "."]) } else { strings(&["reset", "-q", "--", "."]) };
    invocation(At::Root, args, ALL)
}

/// Whether the row offers a discard at all: a tracked file's unstaged edit, delete or type change, or an untracked
/// file. A submodule is staged only.
pub fn discardable(change: &Change) -> Result<(), Refusal> {
    match change.group {
        Group::Untracked if change.is_folder() => Err(Refusal::Folder),
        Group::Untracked => Ok(()),
        Group::Changes if !change.submodule && matches!(change.xy[1], 'M' | 'D' | 'T') => Ok(()),
        Group::Changes => Err(Refusal::NotDiscardable),
        Group::Staged | Group::Conflicts => Err(Refusal::Group),
    }
}

/// A confirmed discard: a tracked file back to the index's version (`git checkout -q -- <path>`), an untracked one
/// deleted (`git clean -f -q -- <path>`, never a folder).
pub fn discard(confirmed: &Confirmed) -> Result<Invocation, Refusal> {
    let change = &confirmed.0;
    discardable(change)?;
    let args = match change.group {
        Group::Untracked => strings(&["clean", "-f", "-q", "--", &change.path]),
        _ => strings(&["checkout", "-q", "--", &change.path]),
    };
    Ok(invocation(At::Top, args, OP))
}

/// What the discard's prompt says, and the discard it lets through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardConfirm {
    pub message: String,
    pub detail: String,
    change: Change,
}

/// The prompt's answers: the first discards.
pub const DISCARD_ANSWERS: [&str; 2] = ["Discard", "Cancel"];

impl DiscardConfirm {
    /// The prompt's answer (an index into `DISCARD_ANSWERS`): the discard on Discard, nothing on Cancel or a closed
    /// prompt.
    pub fn answered(self, answer: usize) -> Option<Op> {
        (answer == 0).then_some(Op::Discard(Confirmed(self.change)))
    }
}

/// The prompt before discarding `change` (`rel` its path in the root); `unsaved` when its file is open in a tab with
/// edits not yet saved, which the discard does not touch but will no longer match.
pub fn confirm_discard(change: &Change, rel: &str, unsaved: bool) -> Result<DiscardConfirm, Refusal> {
    discardable(change)?;
    let message = if change.group == Group::Untracked { format!("Delete the untracked file {rel}?") } else { format!("Discard changes to {rel}?") };
    let mut detail = if change.group == Group::Untracked { "git doesn't have it, so this can't be undone.".to_string() } else { "This can't be undone.".to_string() };
    if unsaved {
        detail.push_str("\nIt's open with unsaved changes, which stay in its tab.");
    }
    Ok(DiscardConfirm { message, detail, change: change.clone() })
}

/// `git commit -F -` with `message` on stdin (`--amend` to replace the last commit). `GIT_EDITOR=false` makes sure
/// git never waits on an editor nobody can see: should it want one, the commit fails instead.
pub fn commit(message: &str, amend: bool) -> Invocation {
    let mut args = strings(&["commit", "-F", "-"]);
    if amend {
        args.push("--amend".into());
    }
    let mut inv = invocation(At::Top, args, COMMIT);
    inv.stdin = Some(message.as_bytes().to_vec());
    inv.env.push(("GIT_EDITOR".into(), "false".into()));
    inv
}

/// What `op` runs; `initial` before the first commit (no HEAD).
pub fn plan(op: &Op, initial: bool) -> Result<Option<Invocation>, Refusal> {
    Ok(Some(match op {
        Op::Stage(c) => stage(c)?,
        Op::Unstage(c) => unstage(c, initial)?,
        Op::StageTracked => stage_tracked(),
        Op::StageUntracked(files) => return Ok(stage_untracked(files)),
        Op::UnstageAll => unstage_all(initial),
        Op::Discard(c) => discard(c)?,
        Op::Commit { message, amend } => commit(message, *amend),
    }))
}

/// `inv` as the runner's command, in `top` or `root`, its pathspecs literal.
pub fn command(inv: &Invocation, top: &Path, root: &Path) -> Cmd {
    let cwd = match inv.at {
        At::Top => top,
        At::Root => root,
    };
    let args: Vec<&str> = inv.args.iter().map(String::as_str).collect();
    let mut cmd = runner::git_cmd(cwd, &args);
    cmd.env.push(("GIT_LITERAL_PATHSPECS".into(), "1".into()));
    cmd.env.extend(inv.env.iter().cloned());
    cmd.stdin = inv.stdin.clone();
    cmd
}

/// Why a command failed, as the tab titles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NothingStaged,
    /// A pre-commit or commit-msg hook said no; its own output says why.
    HookFailed,
    /// Nothing was left of the message once git stripped it (`commit.cleanup=strip` takes `#` lines).
    EmptyMessage,
    /// Another git (a pane's, an editor's) holds `index.lock`: try again.
    IndexLocked,
    /// No user.name or user.email.
    IdentityUnknown,
    /// gpg or ssh signing failed: no TTY for pinentry, a missing key.
    SigningFailed,
    Other,
}

/// A failure: its kind and git's words, at most `FAILURE_LINES` of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: Kind,
    pub text: String,
}

impl Failure {
    fn other(text: impl Into<String>) -> Failure {
        Failure { kind: Kind::Other, text: text.into() }
    }

    /// The error block's title for a failure of `op`.
    pub fn title(&self, op: &Op) -> String {
        match self.kind {
            Kind::NothingStaged => "Nothing staged to commit".into(),
            Kind::HookFailed => "A git hook stopped the commit".into(),
            Kind::EmptyMessage => "The message is empty once git strips its comments".into(),
            Kind::IndexLocked => "Git is busy: another git command holds the index".into(),
            Kind::IdentityUnknown => "Set user.name and user.email".into(),
            Kind::SigningFailed => "Signing the commit failed".into(),
            Kind::Other => op.verb().into(),
        }
    }
}

/// The lines a failure shows: stderr, or stdout when stderr is empty (`nothing to commit` comes on stdout), its
/// first `FAILURE_LINES`, blank ends trimmed.
pub fn failure_text(stdout: &str, stderr: &str) -> String {
    let text = if stderr.trim().is_empty() { stdout } else { stderr };
    let lines: Vec<&str> = text.trim().lines().take(FAILURE_LINES).collect();
    lines.join("\n").trim_end().to_string()
}

fn index_locked(text: &str) -> bool {
    text.contains("index.lock") && (text.contains("File exists") || text.contains("Unable to create"))
}

/// A commit's failure from its exit code and output (git's own words are `LC_ALL=C`, from the preamble). A hook's
/// output is its own, so a failure git did not word (no `fatal:` or `error:` line) after an exit of 1 is the hook's.
pub fn classify_commit(code: Option<i32>, stdout: &str, stderr: &str) -> Kind {
    let all = format!("{stdout}\n{stderr}");
    let gits = all.lines().any(|l| l.starts_with("fatal:") || l.starts_with("error:"));
    if index_locked(&all) {
        Kind::IndexLocked
    } else if all.contains("Please tell me who you are") || all.contains("unable to auto-detect email address") || all.contains("empty ident name") {
        Kind::IdentityUnknown
    } else if all.contains("gpg failed") || all.contains("failed to sign") || (all.contains("failed to write commit object") && all.contains("sign")) {
        Kind::SigningFailed
    } else if all.contains("nothing to commit") || all.contains("no changes added to commit") || all.contains("nothing added to commit") {
        Kind::NothingStaged
    } else if all.contains("Aborting commit due to empty commit message") {
        Kind::EmptyMessage
    } else if code == Some(1) && !gits {
        Kind::HookFailed
    } else {
        Kind::Other
    }
}

/// Any other command's failure: busy, or git's words.
fn classify_op(stderr: &str) -> Kind {
    if index_locked(stderr) { Kind::IndexLocked } else { Kind::Other }
}

/// A run's output, `Ok` on exit 0; stopped, not killed, at its deadline.
async fn git(cmd: Cmd, limits: Limits, commit: bool) -> Result<String, Failure> {
    match runner::run_stopping(&Host::Local, cmd, limits, GRACE).await {
        Ok(out) if out.code == Some(0) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let kind = if commit { classify_commit(out.code, &stdout, &out.stderr) } else { classify_op(&out.stderr) };
            let mut text = failure_text(&stdout, &out.stderr);
            if text.is_empty() {
                text = format!("git exited with status {:?}", out.code);
            }
            if kind == Kind::IndexLocked {
                text = format!("{text}\n{STALE_LOCK}");
            }
            Err(Failure { kind, text })
        }
        Err(RunError::NotFound(_)) => Err(Failure::other("Git isn't installed")),
        Err(RunError::Timeout) => Err(Failure::other(format!("git didn't finish within {} seconds and was stopped", limits.timeout.as_secs()))),
        Err(e) => Err(Failure::other(e.to_string())),
    }
}

/// `root`'s repository's top.
pub async fn top(root: &Path) -> Result<PathBuf, Failure> {
    let out = git(runner::git_cmd(root, &["rev-parse", "--show-toplevel"]), QUICK, false).await?;
    Ok(PathBuf::from(out.trim_end_matches(['\n', '\r'])))
}

/// The repository has no commit yet.
async fn initial(root: &Path) -> Result<bool, Failure> {
    match runner::run(&Host::Local, runner::git_cmd(root, &["rev-parse", "-q", "--verify", "HEAD^{commit}"]), QUICK).await {
        Ok(out) => Ok(out.code != Some(0)),
        Err(e) => Err(Failure::other(e.to_string())),
    }
}

/// Runs `op` in `root`'s repository. Whether it has a first commit is asked here, as the unstage commands differ.
pub async fn run(root: PathBuf, op: Op) -> Result<(), Failure> {
    run_within(root, op, None).await
}

/// `run`, its deadline `timeout` instead of the op's own when given (a test's slow filter).
pub(crate) async fn run_within(root: PathBuf, op: Op, timeout: Option<Duration>) -> Result<(), Failure> {
    let top = top(&root).await?;
    let first = match op {
        Op::Unstage(_) | Op::UnstageAll => initial(&root).await?,
        _ => false,
    };
    let inv = match plan(&op, first) {
        Ok(Some(inv)) => inv,
        Ok(None) => return Ok(()),
        Err(refused) => return Err(Failure::other(format!("{refused:?}"))),
    };
    let limits = Limits { timeout: timeout.unwrap_or(inv.limits.0), max_bytes: inv.limits.1 };
    git(command(&inv, &top, &root), limits, matches!(op, Op::Commit { .. })).await.map(|_| ())
}

/// The last commit's message, for Amend to start from.
pub async fn last_message(root: PathBuf) -> Result<String, Failure> {
    let out = git(runner::git_cmd(&root, &["log", "-1", "--format=%B"]), QUICK, false).await?;
    Ok(out.trim_end().to_string())
}

/// `MERGE_MSG` as the box starts from: git's commented lines (the conflicts it lists) left out, as its editor would.
pub fn merge_message(text: &str) -> String {
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    kept.join("\n").trim().to_string()
}

/// `text` still holds a conflict's markers: a line opening `<<<<<<<` or closing `>>>>>>>`.
pub fn has_conflict_markers(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>"))
}

/// `git status --porcelain=v2 --branch` says the branch has no commit yet.
pub fn initial_in_status(out: &[u8]) -> bool {
    out.split(|&b| b == 0 || b == b'\n').any(|r| r == b"# branch.oid (initial)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(group: Group, path: &str) -> Change {
        let (code, xy) = match group {
            Group::Untracked => ('U', ['?', '?']),
            Group::Staged => ('M', ['M', '.']),
            Group::Conflicts => ('!', ['U', 'U']),
            Group::Changes => ('M', ['.', 'M']),
        };
        Change { group, path: path.into(), orig: None, code, xy, submodule: false, moved_to: None }
    }

    fn args(inv: &Invocation) -> Vec<&str> {
        inv.args.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_stage_takes_both_names_of_a_rename_after_the_double_dash() {
        assert_eq!(args(&stage(&change(Group::Changes, "-n.txt")).unwrap()), ["add", "--", "-n.txt"]);
        let mut moved = change(Group::Changes, "new name.txt");
        moved.orig = Some("old name.txt".into());
        assert_eq!(args(&stage(&moved).unwrap()), ["add", "--", "old name.txt", "new name.txt"]);
        // a conflict's Mark resolved is the same add
        assert_eq!(args(&stage(&change(Group::Conflicts, "f.txt")).unwrap()), ["add", "--", "f.txt"]);
        assert_eq!(stage(&change(Group::Staged, "f.txt")), Err(Refusal::Group));
        assert_eq!(stage(&change(Group::Untracked, "nested/")), Err(Refusal::Folder));
        assert_eq!(stage(&change(Group::Untracked, "a.txt")).unwrap().at, At::Top);
    }

    #[test]
    fn an_unstage_resets_or_before_the_first_commit_takes_it_out_of_the_index() {
        let mut moved = change(Group::Staged, "b.txt");
        moved.orig = Some("a.txt".into());
        assert_eq!(args(&unstage(&moved, false).unwrap()), ["reset", "-q", "--", "a.txt", "b.txt"]);
        assert_eq!(args(&unstage(&change(Group::Staged, "*.txt"), true).unwrap()), ["rm", "--cached", "-f", "-q", "--ignore-unmatch", "--", "*.txt"]);
        assert_eq!(unstage(&change(Group::Changes, "a.txt"), false), Err(Refusal::Group));
        assert_eq!(args(&unstage_all(false)), ["reset", "-q", "--", "."]);
        assert_eq!(args(&unstage_all(true)), ["rm", "--cached", "-r", "-f", "-q", "--ignore-unmatch", "--", "."]);
        assert_eq!(unstage_all(true).at, At::Root);
    }

    #[test]
    fn stage_all_keeps_to_the_root_and_feeds_untracked_files_on_stdin() {
        let tracked = stage_tracked();
        assert_eq!((tracked.at, args(&tracked)), (At::Root, vec!["add", "-u", "--", "."]));
        let files = ["a b.txt".to_string(), "nested/".to_string(), "c/d.txt".to_string()];
        let inv = stage_untracked(&files).unwrap();
        assert_eq!(args(&inv), ["add", "--pathspec-from-file=-", "--pathspec-file-nul"]);
        assert_eq!(inv.stdin.as_deref(), Some(&b"a b.txt\0c/d.txt\0"[..]));
        assert_eq!(stage_untracked(&["nested/".into()]), None);
    }

    #[test]
    fn discard_offers_tracked_edits_and_untracked_files_and_refuses_a_folder() {
        assert_eq!(discardable(&change(Group::Changes, "a.txt")), Ok(()));
        assert_eq!(discardable(&change(Group::Untracked, "a.txt")), Ok(()));
        assert_eq!(discardable(&change(Group::Untracked, "nested/")), Err(Refusal::Folder));
        assert_eq!(discardable(&change(Group::Staged, "a.txt")), Err(Refusal::Group));
        assert_eq!(discardable(&change(Group::Conflicts, "a.txt")), Err(Refusal::Group));
        let mut intent = change(Group::Changes, "a.txt");
        intent.xy = ['.', 'A'];
        assert_eq!(discardable(&intent), Err(Refusal::NotDiscardable));
        let mut sub = change(Group::Changes, "lib");
        sub.submodule = true;
        assert_eq!(discardable(&sub), Err(Refusal::NotDiscardable));
        assert_eq!(confirm_discard(&change(Group::Untracked, "nested/"), "nested", false), Err(Refusal::Folder));
        // the only way to a discard is through its prompt
        let tracked = confirm_discard(&change(Group::Changes, "-x.txt"), "-x.txt", false).unwrap().answered(0).unwrap();
        assert_eq!(args(&plan(&tracked, false).unwrap().unwrap()), ["checkout", "-q", "--", "-x.txt"]);
        let untracked = confirm_discard(&change(Group::Untracked, "n.txt"), "n.txt", false).unwrap().answered(0).unwrap();
        assert_eq!(args(&plan(&untracked, false).unwrap().unwrap()), ["clean", "-f", "-q", "--", "n.txt"]);
    }

    #[test]
    fn the_discard_prompt_says_it_cannot_be_undone_and_cancel_discards_nothing() {
        let c = confirm_discard(&change(Group::Changes, "src/a.rs"), "src/a.rs", true).unwrap();
        assert_eq!(c.message, "Discard changes to src/a.rs?");
        assert_eq!(c.detail, "This can't be undone.\nIt's open with unsaved changes, which stay in its tab.");
        assert_eq!(c.clone().answered(1), None);
        assert_eq!(c.clone().answered(2), None);
        assert!(matches!(c.answered(0), Some(Op::Discard(_))));
        let u = confirm_discard(&change(Group::Untracked, "notes.txt"), "notes.txt", false).unwrap();
        assert_eq!((u.message.as_str(), u.detail.as_str()), ("Delete the untracked file notes.txt?", "git doesn't have it, so this can't be undone."));
        assert_eq!(DISCARD_ANSWERS[0], "Discard");
    }

    #[test]
    fn a_commit_reads_its_message_from_stdin_and_cannot_open_an_editor() {
        let inv = commit("Fix the widget\n\nBody", false);
        assert_eq!(args(&inv), ["commit", "-F", "-"]);
        assert_eq!(inv.stdin.as_deref(), Some(&b"Fix the widget\n\nBody"[..]));
        assert_eq!(inv.env, [("GIT_EDITOR".to_string(), "false".to_string())]);
        assert_eq!(args(&commit("x", true)), ["commit", "-F", "-", "--amend"]);
    }

    #[test]
    fn every_command_has_literal_pathspecs_and_runs_where_its_paths_are() {
        let (top, root) = (Path::new("/tmp/fictional-repo"), Path::new("/tmp/fictional-repo/app"));
        let cmd = command(&stage(&change(Group::Changes, "app/*.rs")).unwrap(), top, root);
        assert_eq!(cmd.cwd, top);
        assert_eq!(cmd.env.iter().filter(|(k, v)| k == "GIT_LITERAL_PATHSPECS" && v == "1").count(), 1);
        assert_eq!(cmd.args.iter().rev().take(2).collect::<Vec<_>>(), ["app/*.rs", "--"]);
        assert_eq!(command(&unstage_all(false), top, root).cwd, root);
        let commit = command(&commit("m", false), top, root);
        assert_eq!(commit.stdin.as_deref(), Some(&b"m"[..]));
        assert!(commit.env.contains(&("GIT_EDITOR".into(), "false".into())));
    }

    #[test]
    fn commit_failures_are_told_apart() {
        let none = "On branch main\nChanges not staged for commit:\n\tmodified:   a.txt\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n";
        assert_eq!(classify_commit(Some(1), none, ""), Kind::NothingStaged);
        assert_eq!(classify_commit(Some(1), "On branch main\nnothing to commit, working tree clean\n", ""), Kind::NothingStaged);
        assert_eq!(classify_commit(Some(1), "", "lint: 2 problems in a.rs\n"), Kind::HookFailed);
        assert_eq!(classify_commit(Some(1), "", ""), Kind::HookFailed);
        let locked = "fatal: Unable to create '/tmp/fictional-repo/.git/index.lock': File exists.\n\nAnother git process seems to be running";
        assert_eq!(classify_commit(Some(128), "", locked), Kind::IndexLocked);
        let who = "Author identity unknown\n\n*** Please tell me who you are.\n\nfatal: unable to auto-detect email address (got 'x@y.(none)')\n";
        assert_eq!(classify_commit(Some(128), "", who), Kind::IdentityUnknown);
        assert_eq!(classify_commit(Some(128), "", "fatal: empty ident name (for <>) not allowed\n"), Kind::IdentityUnknown);
        let gpg = "error: gpg failed to sign the data:\n[GNUPG:] KEY_CONSIDERED\nfatal: failed to write commit object\n";
        assert_eq!(classify_commit(Some(128), "", gpg), Kind::SigningFailed);
        assert_eq!(classify_commit(Some(128), "", "error: Couldn't load public key: failed to sign\nfatal: failed to write commit object\n"), Kind::SigningFailed);
        assert_eq!(classify_commit(Some(128), "", "fatal: could not parse HEAD\n"), Kind::Other);
        assert_eq!(classify_commit(Some(1), "", "error: something git said\n"), Kind::Other);
        assert_eq!(classify_commit(None, "", ""), Kind::Other);
        // git words this one without `fatal:`; it is not a hook's
        assert_eq!(classify_commit(Some(1), "", "Aborting commit due to empty commit message.\n"), Kind::EmptyMessage);
        assert_eq!(classify_op("fatal: Unable to create '/r/.git/index.lock': File exists."), Kind::IndexLocked);
        assert_eq!(classify_op("fatal: pathspec 'x' did not match any files"), Kind::Other);
    }

    #[test]
    fn a_failure_shows_stderr_or_else_stdout_up_to_twenty_lines() {
        assert_eq!(failure_text("nothing to commit\n", ""), "nothing to commit");
        assert_eq!(failure_text("ignored\n", "  \nfatal: no\n\n"), "fatal: no");
        let many: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        let text = failure_text("", &many);
        assert_eq!(text.lines().count(), 20);
        assert_eq!(text.lines().last(), Some("line 20"));
        let f = Failure { kind: Kind::IdentityUnknown, text: String::new() };
        assert_eq!(f.title(&Op::UnstageAll), "Set user.name and user.email");
        assert_eq!(Failure::other("x").title(&Op::Commit { message: String::new(), amend: true }), "Couldn't amend the commit");
    }

    #[test]
    fn a_merge_message_drops_its_comments_and_markers_are_found_at_line_starts() {
        let msg = "Merge branch 'other'\n\n# Conflicts:\n#\tf.txt\n";
        assert_eq!(merge_message(msg), "Merge branch 'other'");
        assert!(has_conflict_markers("a\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> other\n"));
        assert!(!has_conflict_markers("a <<<<<<< b\n"));
        assert!(initial_in_status(b"# branch.oid (initial)\0# branch.head main\0"));
        assert!(!initial_in_status(b"# branch.oid 0123abc\0# branch.head main\0"));
    }
}
