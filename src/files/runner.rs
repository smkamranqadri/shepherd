//! The one way the right panel runs git and rg: every process has a deadline, a cap on what it may print and dies
//! with the future that waits on it (a new keystroke's search drops the last one, and its rg with it). herdr's own
//! `git`/`ssh` helpers wait as long as the child likes, which a 200k-file repo or a hung NFS mount turns into a
//! frozen panel. Machines (`Host::Ssh`) answer `RemoteUnsupported` for now: the seam is here, the SSH side is not.
use futures::channel::mpsc;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

/// Where a command runs: this machine, or (later) a herdr machine over its SSH target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    Local,
    Ssh { target: String },
}

/// How long a command may take and how much of its stdout is kept.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub timeout: Duration,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Output {
    /// `None` when a signal ended it, as the kill after `truncated` does.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    /// Lossy, and only its first `STDERR_CAP` bytes: it is for error messages.
    pub stderr: String,
    /// stdout went past `max_bytes`: `stdout` holds the first `max_bytes` of it and the process was killed.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The program is not installed (or not on PATH): its name.
    NotFound(String),
    Timeout,
    Io(String),
    RemoteUnsupported,
    /// Only from `stream`, as its last item: it stopped at `max_bytes` (a search shows its results are capped).
    Truncated,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::NotFound(program) => write!(f, "{program} isn't installed"),
            RunError::Timeout => write!(f, "took too long"),
            RunError::Io(e) => write!(f, "{e}"),
            RunError::RemoteUnsupported => write!(f, "files on herdr machines aren't supported yet"),
            RunError::Truncated => write!(f, "output cut at its cap"),
        }
    }
}

impl std::error::Error for RunError {}

#[derive(Debug, Clone, Default)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Taken out of the inherited environment before `env` goes in: `git_cmd` drops every `GIT_*` a pane's shell
    /// (or a git hook that started Shepherd) left, which would point git at another repo.
    pub env_remove: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Written to the child's stdin, which is then closed; `None` gives it /dev/null.
    pub stdin: Option<Vec<u8>>,
}

/// What of stderr is kept: git's and rg's messages are a line or two.
const STDERR_CAP: usize = 64 * 1024;
/// What of a stream's stderr its `Exit` carries: the last 8 KiB, where the reason a search failed is.
const STREAM_STDERR: usize = 8 * 1024;
/// A stream's lines go out at least this often while it prints, and in batches of at most `BATCH_LINES`.
const BATCH_EVERY: Duration = Duration::from_millis(50);
const BATCH_LINES: usize = 200;

/// `git <args>` in `cwd` that cannot page, prompt, take the index lock from a pane's git, or run an fsmonitor, and
/// that prints paths as they are (no octal quoting). Found where `ui::statusbar::tool` looks, as a Dock launch has
/// no Homebrew on PATH.
pub fn git_cmd(cwd: &Path, args: &[&str]) -> Cmd {
    let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
    git_cmd_in(cwd, args, inherited)
}

fn git_cmd_in(cwd: &Path, args: &[&str], inherited: impl IntoIterator<Item = String>) -> Cmd {
    let preamble = ["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.quotepath=false"];
    let env = [("GIT_PAGER", "cat"), ("GIT_TERMINAL_PROMPT", "0"), ("GIT_OPTIONAL_LOCKS", "0"), ("LC_ALL", "C")];
    Cmd {
        program: crate::ui::statusbar::tool("git"),
        args: preamble.iter().chain(args).map(|a| a.to_string()).collect(),
        cwd: cwd.to_path_buf(),
        env_remove: inherited.into_iter().filter(|k| k.starts_with("GIT_")).collect(),
        env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        stdin: None,
    }
}

/// The child, its stdout piped, killed when dropped.
fn spawn(cmd: &Cmd, stderr: bool) -> Result<tokio::process::Child, RunError> {
    spawn_in(cmd, stderr, false)
}

/// `spawn`; with `group`, the child leads a process group of its own (on unix), which its children (a git hook, a
/// clean filter) join, so one signal reaches them all.
fn spawn_in(cmd: &Cmd, stderr: bool, group: bool) -> Result<tokio::process::Child, RunError> {
    let mut command = tokio::process::Command::new(&cmd.program);
    command.args(&cmd.args).current_dir(&cmd.cwd).kill_on_drop(true);
    #[cfg(unix)]
    if group {
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = group;
    // git and rg are console programs: on Windows they'd each open a console window (`crate::process::command`)
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    for key in &cmd.env_remove {
        command.env_remove(key);
    }
    command.envs(cmd.env.iter().map(|(k, v)| (k, v)));
    command.stdin(if cmd.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    command.stdout(Stdio::piped());
    command.stderr(if stderr { Stdio::piped() } else { Stdio::null() });
    command.spawn().map_err(|e| match e.kind() {
        // a folder deleted under the panel fails the same way as a missing program
        std::io::ErrorKind::NotFound if !cmd.cwd.is_dir() => RunError::Io(format!("{} doesn't exist", cmd.cwd.display())),
        std::io::ErrorKind::NotFound => RunError::NotFound(cmd.program.clone()),
        _ => RunError::Io(format!("{}: {e}", cmd.program)),
    })
}

/// Feeds `data` to the child's stdin and closes it, beside the reads (a child that prints before it has read all
/// its input would otherwise block on a full pipe while we block on its stdin).
async fn feed(stdin: Option<tokio::process::ChildStdin>, data: Option<Vec<u8>>) {
    if let (Some(mut w), Some(data)) = (stdin, data) {
        // a child that exits without reading it all closes the pipe: not ours to report
        let _ = w.write_all(&data).await;
        let _ = w.shutdown().await;
    }
}

/// Reads all of `r`, keeping the first `cap` bytes; the rest is drained so the child never blocks on it.
async fn read_capped(r: Option<impl AsyncRead + Unpin>, cap: usize) -> Vec<u8> {
    let (Some(mut r), mut kept) = (r, Vec::new()) else { return Vec::new() };
    let mut buf = [0u8; 8192];
    while let Ok(n) = r.read(&mut buf).await {
        if n == 0 {
            break;
        }
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
    kept
}

/// Runs `cmd` to its end, its deadline or its stdout cap, whichever comes first. A timeout (or the caller dropping
/// this future) kills the child.
pub async fn run(host: &Host, cmd: Cmd, limits: Limits) -> Result<Output, RunError> {
    if let Host::Ssh { .. } = host {
        return Err(RunError::RemoteUnsupported);
    }
    let mut child = spawn(&cmd, true)?;
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let job = async {
        let out = async {
            let mut kept = Vec::new();
            if let Some(stdout) = stdout {
                // one byte past the cap says there was more
                let _ = stdout.take(limits.max_bytes as u64 + 1).read_to_end(&mut kept).await;
            }
            kept
        };
        let (mut stdout, stderr, ()) = tokio::join!(out, read_capped(stderr, STDERR_CAP), feed(stdin, cmd.stdin.clone()));
        let truncated = stdout.len() > limits.max_bytes;
        if truncated {
            stdout.truncate(limits.max_bytes);
            // the rest is not wanted: stop it rather than wait for it
            let _ = child.start_kill();
        }
        let status = child.wait().await.map_err(|e| RunError::Io(e.to_string()))?;
        Ok(Output { code: status.code(), stdout, stderr: String::from_utf8_lossy(&stderr).into_owned(), truncated })
    };
    // ponytail: stderr is read beside stdout, so a child that keeps stderr open past a truncation is waited on
    // until the deadline; git and rg close both together
    tokio::time::timeout(limits.timeout, job).await.map_err(|_| RunError::Timeout)?
}

/// Sends `signal` to the process group `pid` leads; nothing once it is gone.
#[cfg(unix)]
fn signal_group(pid: u32, signal: i32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill(2) takes plain integers; a group already gone answers ESRCH
        unsafe { libc::kill(-pid, signal) };
    }
}

/// A group `run_stopping` started, killed if the future waiting on it is dropped before it ends.
#[cfg(unix)]
struct GroupGuard(Option<u32>);

#[cfg(unix)]
impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            signal_group(pid, libc::SIGKILL);
        }
    }
}

/// `run` for a command that must not be killed outright: the child leads its own process group, and its deadline
/// asks the whole group to stop (SIGTERM), so git takes its `index.lock` away (its tempfile handler) and its hooks
/// and filters end with it; whatever is left after `grace` is killed. A future dropped early kills the group. Other
/// platforms are `run`.
pub async fn run_stopping(host: &Host, cmd: Cmd, limits: Limits, grace: Duration) -> Result<Output, RunError> {
    #[cfg(not(unix))]
    {
        let _ = grace;
        return run(host, cmd, limits).await;
    }
    #[cfg(unix)]
    {
        if let Host::Ssh { .. } = host {
            return Err(RunError::RemoteUnsupported);
        }
        let mut child = spawn_in(&cmd, true, true)?;
        let pid = child.id();
        let mut guard = GroupGuard(pid);
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let job = async {
            let out = async {
                let mut kept = Vec::new();
                if let Some(stdout) = stdout {
                    let _ = stdout.take(limits.max_bytes as u64 + 1).read_to_end(&mut kept).await;
                }
                kept
            };
            let (mut stdout, stderr, ()) = tokio::join!(out, read_capped(stderr, STDERR_CAP), feed(stdin, cmd.stdin.clone()));
            let truncated = stdout.len() > limits.max_bytes;
            if truncated {
                stdout.truncate(limits.max_bytes);
                let _ = child.start_kill();
            }
            let status = child.wait().await.map_err(|e| RunError::Io(e.to_string()))?;
            Ok(Output { code: status.code(), stdout, stderr: String::from_utf8_lossy(&stderr).into_owned(), truncated })
        };
        tokio::pin!(job);
        let result = tokio::select! {
            done = &mut job => done,
            _ = tokio::time::sleep(limits.timeout) => {
                if let Some(pid) = pid {
                    signal_group(pid, libc::SIGTERM);
                    if tokio::time::timeout(grace, &mut job).await.is_err() {
                        signal_group(pid, libc::SIGKILL);
                        let _ = tokio::time::timeout(Duration::from_secs(2), &mut job).await;
                    }
                }
                Err(RunError::Timeout)
            }
        };
        // ended, or stopped above: the group's id is not ours to signal any more
        guard.0 = None;
        result
    }
}

/// What a stream sends: its lines in batches, then how the child ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Lines(Vec<Vec<u8>>),
    /// Only from `stream_lines`, before its end: this many lines were longer than its cap and were dropped.
    Skipped(usize),
    /// The last item of a child that ran to its end: its exit code (`None` when a signal ended it) and the last
    /// `STREAM_STDERR` bytes of its stderr, lossy. rg's exit 2 with its complaints is not "no results" (exit 1).
    Exit { code: Option<i32>, stderr: String },
}

/// A stream's items; its last is `Exit`, or an error (`NotFound`, `Timeout`, `Truncated`, …) when it did not end
/// on its own.
pub type Batches = mpsc::UnboundedReceiver<Result<Item, RunError>>;

/// `cmd`'s stdout as lines (split on `\n`, which is dropped), sent in batches every `BATCH_EVERY` or `BATCH_LINES`
/// until it ends (then `Exit` comes last), prints `max_bytes` (then `Truncated`), reaches its deadline (`Timeout`)
/// or the receiver is dropped. Aborting the handle, the deadline and a dropped receiver all kill the child. The channel is unbounded:
/// `max_bytes` bounds what it can hold, and a receiver that stops reading cannot hold the deadline off.
pub fn stream(host: &Host, cmd: Cmd, limits: Limits, rt: &Handle) -> (Batches, AbortHandle) {
    stream_lines(host, cmd, limits, None, rt)
}

/// `stream`, with lines longer than `max_line` bytes dropped as they are read: never held, not counted toward
/// `max_bytes` (one minified bundle's line would otherwise fill a search's whole cap), and counted in a `Skipped`
/// before the end.
pub fn stream_lines(host: &Host, cmd: Cmd, limits: Limits, max_line: Option<usize>, rt: &Handle) -> (Batches, AbortHandle) {
    let (tx, rx) = mpsc::unbounded();
    let host = host.clone();
    let task = rt.spawn(async move {
        if let Err(e) = pump(&host, cmd, limits, max_line.unwrap_or(usize::MAX), &tx).await {
            let _ = tx.unbounded_send(Err(e));
        }
    });
    (rx, task.abort_handle())
}

type Sender = mpsc::UnboundedSender<Result<Item, RunError>>;

/// Adds `data` to `tail`, which keeps (at least) the last `cap` bytes, in amortised constant time.
fn keep_tail(tail: &mut Vec<u8>, data: &[u8], cap: usize) {
    tail.extend_from_slice(&data[data.len().saturating_sub(cap)..]);
    if tail.len() > cap * 2 {
        tail.drain(..tail.len() - cap);
    }
}

/// Reads all of `r`, keeping its last `cap` bytes; the child never blocks on a full stderr pipe.
async fn read_tail(r: Option<impl AsyncRead + Unpin>, cap: usize) -> Vec<u8> {
    let (Some(mut r), mut tail) = (r, Vec::new()) else { return Vec::new() };
    let mut buf = [0u8; 8192];
    while let Ok(n) = r.read(&mut buf).await {
        if n == 0 {
            break;
        }
        keep_tail(&mut tail, &buf[..n], cap);
    }
    let cut = tail.len().saturating_sub(cap);
    tail.drain(..cut);
    tail
}

struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How much of a line being dropped (`stream_lines`) is read at a time.
const SKIP_CHUNK: u64 = 64 * 1024;

async fn pump(host: &Host, cmd: Cmd, limits: Limits, max_line: usize, tx: &Sender) -> Result<(), RunError> {
    if let Host::Ssh { .. } = host {
        return Err(RunError::RemoteUnsupported);
    }
    let mut child = spawn(&cmd, true)?;
    tokio::spawn(feed(child.stdin.take(), cmd.stdin.clone()));
    // read beside stdout, and stopped with this task however it ends (a grandchild may hold the pipe open)
    let mut stderr = tokio::spawn(read_tail(child.stderr.take(), STREAM_STDERR));
    let _stop_stderr = AbortOnDrop(stderr.abort_handle());
    let Some(stdout) = child.stdout.take() else { return Ok(()) };
    let mut reader = BufReader::new(stdout);
    let deadline = tokio::time::sleep(limits.timeout);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(BATCH_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `total` counts the lines sent; `line` the one being read, never past one byte over the cap (or `max_line`);
    // `skipping` while the rest of a line over `max_line` is read into `skipped` and thrown away
    let (mut batch, mut line, mut total) = (Vec::new(), Vec::new(), 0usize);
    let (mut skipping, mut skipped, mut dropped) = (false, Vec::new(), 0usize);
    let send = |batch: &mut Vec<Vec<u8>>| batch.is_empty() || tx.unbounded_send(Ok(Item::Lines(std::mem::take(batch)))).is_ok();
    let end = loop {
        let budget = if skipping {
            SKIP_CHUNK
        } else {
            // room for `max_line` bytes and their newline; one more byte without one says the line is longer
            let line_room = max_line.saturating_add(2).saturating_sub(line.len());
            (limits.max_bytes + 1).saturating_sub(total + line.len()).min(line_room) as u64
        };
        let mut limited = (&mut reader).take(budget);
        let into = if skipping { &mut skipped } else { &mut line };
        tokio::select! {
            // read_until keeps what it read in `line` when another branch wins, and goes on from there next time;
            // `take` stops a minified file's one huge line at the cap rather than buffering all of it
            read = limited.read_until(b'\n', into) => match read {
                Ok(n) if skipping => {
                    let ended = n == 0 || skipped.last() == Some(&b'\n');
                    skipped.clear();
                    if n == 0 {
                        break Ok(());
                    }
                    skipping = !ended;
                }
                Ok(_) if total + line.len() > limits.max_bytes => break Err(RunError::Truncated),
                Ok(_) if line.len() - usize::from(line.last() == Some(&b'\n')) > max_line => {
                    // the whole line, or the start of one longer still
                    dropped += 1;
                    skipping = line.last() != Some(&b'\n');
                    line.clear();
                }
                Ok(0) => {
                    if !line.is_empty() {
                        batch.push(std::mem::take(&mut line));
                    }
                    break Ok(());
                }
                Ok(_) if line.last() == Some(&b'\n') => {
                    total += line.len();
                    line.pop();
                    batch.push(std::mem::take(&mut line));
                    if batch.len() >= BATCH_LINES && !send(&mut batch) {
                        return Ok(());
                    }
                }
                // the last line, without its newline: the next read says the end
                Ok(_) => {}
                Err(e) => break Err(RunError::Io(e.to_string())),
            },
            _ = tick.tick() => {
                // a dropped receiver is noticed here too, while the child prints nothing
                if tx.is_closed() || !send(&mut batch) {
                    return Ok(());
                }
            }
            _ = &mut deadline => break Err(RunError::Timeout),
        }
    };
    send(&mut batch);
    if dropped > 0 {
        let _ = tx.unbounded_send(Ok(Item::Skipped(dropped)));
    }
    if let Err(e) = end {
        // at the cap or the deadline the child may still be running
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(e);
    }
    // stdout closed: the child is ending; it, and a grandchild holding stderr open, still have the deadline
    let ended = async {
        let status = child.wait().await.map_err(|e| RunError::Io(e.to_string()))?;
        let stderr = (&mut stderr).await.unwrap_or_default();
        Ok::<_, RunError>(Item::Exit { code: status.code(), stderr: String::from_utf8_lossy(&stderr).into_owned() })
    };
    let exit = tokio::select! {
        exit = ended => Some(exit),
        _ = &mut deadline => None,
    };
    let _ = tx.unbounded_send(Ok(exit.ok_or(RunError::Timeout)??));
    Ok(())
}

// The tests run sh and ps.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn sh(script: &str) -> Cmd {
        Cmd { program: "sh".into(), args: vec!["-c".into(), script.into()], cwd: std::env::temp_dir(), ..Default::default() }
    }

    fn limits(ms: u64, max_bytes: usize) -> Limits {
        Limits { timeout: Duration::from_millis(ms), max_bytes }
    }

    /// The process `pid` is gone, or a zombie no one has reaped yet (it holds nothing).
    fn gone(pid: &str) -> bool {
        let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", pid]).output().unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        stat.trim().is_empty() || stat.trim().starts_with('Z')
    }

    #[tokio::test]
    async fn a_command_past_its_deadline_times_out_and_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 5", pid_file.display());
        let started = std::time::Instant::now();
        assert_eq!(run(&Host::Local, sh(&script), limits(300, 1000)).await.unwrap_err(), RunError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        for _ in 0..50 {
            if gone(pid.trim()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("sleep {} outlived its timeout", pid.trim());
    }

    #[tokio::test]
    async fn output_past_the_cap_is_cut_and_flagged() {
        let out = run(&Host::Local, sh("yes | head -c 100000"), limits(5000, 1000)).await.unwrap();
        assert!(out.truncated);
        assert_eq!(out.stdout.len(), 1000);
        let whole = run(&Host::Local, sh("printf 'a\\nb\\n'; echo oops >&2; exit 3"), limits(5000, 1000)).await.unwrap();
        assert_eq!((whole.stdout.as_slice(), whole.stderr.trim(), whole.code, whole.truncated), (&b"a\nb\n"[..], "oops", Some(3), false));
    }

    #[tokio::test]
    async fn stdin_reaches_the_child() {
        let cmd = Cmd { stdin: Some(b"one\0two\0".to_vec()), ..sh("tr '\\0' '\\n'") };
        let out = run(&Host::Local, cmd, limits(5000, 1000)).await.unwrap();
        assert_eq!(out.stdout, b"one\ntwo\n");
    }

    #[tokio::test]
    async fn a_missing_program_is_not_found() {
        let cmd = Cmd { program: "shepherd-no-such-tool".into(), cwd: std::env::temp_dir(), ..Default::default() };
        assert_eq!(run(&Host::Local, cmd, limits(1000, 10)).await.unwrap_err(), RunError::NotFound("shepherd-no-such-tool".into()));
    }

    #[tokio::test]
    async fn a_missing_folder_is_not_a_missing_program() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("deleted");
        let cmd = Cmd { cwd: gone.clone(), ..sh("true") };
        assert_eq!(run(&Host::Local, cmd, limits(1000, 10)).await.unwrap_err(), RunError::Io(format!("{} doesn't exist", gone.display())));
    }

    #[tokio::test]
    async fn machines_are_not_supported_yet() {
        let host = Host::Ssh { target: "box".into() };
        assert_eq!(run(&host, sh("true"), limits(1000, 10)).await.unwrap_err(), RunError::RemoteUnsupported);
        let (mut rx, _) = stream(&host, sh("true"), limits(1000, 10), &Handle::current());
        assert_eq!(rx.next().await, Some(Err(RunError::RemoteUnsupported)));
    }

    #[tokio::test]
    async fn the_git_preamble_strips_the_inherited_git_variables() {
        let cmd = git_cmd_in(Path::new("/r"), &["status"], ["GIT_DIR".to_string(), "PATH".into(), "GIT_WORK_TREE".into()]);
        assert_eq!(cmd.env_remove, ["GIT_DIR", "GIT_WORK_TREE"]);
        assert_eq!(cmd.args[..5], ["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.quotepath=false"]);
        assert_eq!(cmd.args.last().map(String::as_str), Some("status"));
        assert!(cmd.env.contains(&("GIT_TERMINAL_PROMPT".into(), "0".into())));
        // and the removal reaches the child
        let probe = Cmd { env_remove: vec!["HOME".into()], ..sh("echo ${HOME-unset}") };
        let out = run(&Host::Local, probe, limits(5000, 100)).await.unwrap();
        assert_eq!(out.stdout, b"unset\n");
    }

    /// The lines of `items`, which must all be `Lines`.
    fn lines(items: Vec<Result<Item, RunError>>) -> Vec<Vec<u8>> {
        items
            .into_iter()
            .flat_map(|i| match i {
                Ok(Item::Lines(lines)) => lines,
                other => panic!("not lines: {other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_stream_sends_every_line_then_ends() {
        let (rx, _) = stream(&Host::Local, sh("seq 1 450; printf last"), limits(5000, 1 << 20), &Handle::current());
        let mut got: Vec<_> = rx.collect().await;
        assert_eq!(got.pop(), Some(Ok(Item::Exit { code: Some(0), stderr: String::new() })), "no Truncated under the cap");
        let lines = lines(got);
        assert_eq!(lines.len(), 451);
        assert_eq!((lines[0].as_slice(), lines[449].as_slice(), lines[450].as_slice()), (&b"1"[..], &b"450"[..], &b"last"[..]));
    }

    #[tokio::test]
    async fn a_stream_stops_at_its_cap_and_its_deadline() {
        let (rx, _) = stream(&Host::Local, sh("yes"), limits(5000, 1000), &Handle::current());
        let mut got: Vec<_> = rx.collect().await;
        assert_eq!(got.pop(), Some(Err(RunError::Truncated)));
        assert_eq!(lines(got).len(), 500, "two bytes a line");
        let (rx, _) = stream(&Host::Local, sh("echo one; exec sleep 5"), limits(300, 1000), &Handle::current());
        let got: Vec<_> = rx.collect().await;
        assert_eq!(got, vec![Ok(Item::Lines(vec![b"one".to_vec()])), Err(RunError::Timeout)]);
    }

    #[tokio::test]
    async fn one_long_line_stops_at_the_cap_unbuffered() {
        // 50 MB without a newline: only the cap's worth is ever held
        let (rx, _) = stream(&Host::Local, sh("head -c 50000000 /dev/zero"), limits(10_000, 1000), &Handle::current());
        let got: Vec<_> = rx.collect().await;
        assert_eq!(got, vec![Err(RunError::Truncated)]);
        let (rx, _) = stream(&Host::Local, sh("printf 'abc'"), limits(5000, 3), &Handle::current());
        let got: Vec<_> = rx.collect().await;
        assert_eq!(got[0], Ok(Item::Lines(vec![b"abc".to_vec()])), "exactly the cap is not cut");
        assert!(matches!(got[1], Ok(Item::Exit { code: Some(0), .. })));
    }

    #[tokio::test]
    async fn a_stream_ends_with_the_exit_code_and_the_tail_of_stderr() {
        // rg's "problems": exit 2 after some results, its complaints on stderr
        let (rx, _) = stream(&Host::Local, sh("echo found; echo 'a: Permission denied' >&2; exit 2"), limits(5000, 1000), &Handle::current());
        let got: Vec<_> = rx.collect().await;
        assert_eq!(got, vec![Ok(Item::Lines(vec![b"found".to_vec()])), Ok(Item::Exit { code: Some(2), stderr: "a: Permission denied\n".into() })]);
        // a lot of stderr: only its last 8 KiB, where the end is
        let (rx, _) = stream(&Host::Local, sh("head -c 100000 /dev/zero | tr '\\0' x >&2; printf END >&2; exit 1"), limits(5000, 1000), &Handle::current());
        let got: Vec<_> = rx.collect().await;
        let [Ok(Item::Exit { code: Some(1), stderr })] = &got[..] else { panic!("{got:?}") };
        assert_eq!(stderr.len(), STREAM_STDERR);
        assert!(stderr.ends_with("xEND"));
    }

    #[test]
    fn a_tail_keeps_the_last_bytes() {
        let mut tail = Vec::new();
        for chunk in [&b"abcdef"[..], b"gh", b"ijklmnopqrstuvwxyz"] {
            keep_tail(&mut tail, chunk, 4);
            assert!(tail.len() <= 8);
        }
        assert!(tail.ends_with(b"wxyz"));
    }

    #[tokio::test]
    async fn lines_over_the_line_cap_are_dropped_and_not_counted() {
        // a 200 KB line between two short ones, under a 1000-byte cap it would fill on its own
        let script = "printf 'short\\n'; head -c 200000 /dev/zero | tr '\\0' x; printf '\\nafter\\n'; head -c 50 /dev/zero | tr '\\0' y; printf '\\nend'";
        let (rx, _) = stream_lines(&Host::Local, sh(script), limits(5000, 1000), Some(40), &Handle::current());
        let mut got: Vec<_> = rx.collect().await;
        assert!(matches!(got.pop(), Some(Ok(Item::Exit { code: Some(0), .. }))), "{got:?}");
        assert_eq!(got.pop(), Some(Ok(Item::Skipped(2))));
        assert_eq!(lines(got), [b"short".to_vec(), b"after".to_vec(), b"end".to_vec()]);
        // a line exactly at the cap stays
        let (rx, _) = stream_lines(&Host::Local, sh("printf 'abcd\\nabcde\\n'"), limits(5000, 1000), Some(4), &Handle::current());
        let got: Vec<_> = rx.collect().await;
        assert_eq!(got[0], Ok(Item::Lines(vec![b"abcd".to_vec()])));
        assert_eq!(got[1], Ok(Item::Skipped(1)));
    }

    async fn wait_gone(pid_file: &Path) -> bool {
        for _ in 0..100 {
            let pid = std::fs::read_to_string(pid_file).unwrap_or_default();
            if !pid.trim().is_empty() && gone(pid.trim()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_receiver_that_never_reads_cannot_hold_the_deadline_off() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > '{}'; seq 1 100000; exec sleep 30", pid_file.display());
        let (rx, _) = stream(&Host::Local, sh(&script), limits(300, 1 << 30), &Handle::current());
        assert!(wait_gone(&pid_file).await, "killed at the deadline");
        let mut got: Vec<_> = rx.collect().await;
        assert_eq!(got.pop(), Some(Err(RunError::Timeout)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_receiver_kills_a_silent_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 30", pid_file.display());
        let (rx, _) = stream(&Host::Local, sh(&script), limits(60_000, 1000), &Handle::current());
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(rx);
        assert!(wait_gone(&pid_file).await, "killed once nobody listens");
    }
}
