//! herdr's saved SSH machines (`herdr machine`). Shepherd reaches one through two local sockets: each connection to
//! them runs herdr's own stdio bridge on the host over SSH (`remote-api-bridge`, `remote-client-bridge`), so the rest
//! of Shepherd talks to it as to a local session.
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

/// A saved machine, as `herdr machine list --json` lists it.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Machine {
    pub id: String,
    pub label: String,
    pub target: String,
    pub session: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

/// herdr's saved machines.
pub fn machines() -> anyhow::Result<Vec<Machine>> {
    Ok(serde_json::from_slice(&herdr(&["machine", "list", "--json"])?)?)
}

/// How long saving a machine may take: herdr reaches the host, checks its herdr (it may copy one over) and starts
/// the session's server first.
pub const ADD_TIMEOUT: Duration = Duration::from_secs(300);

/// Why a machine was not added.
#[derive(Debug)]
pub enum AddError {
    /// `herdr machine add` failed, as it said.
    Failed(String),
    /// It took past its deadline and was ended.
    TimedOut,
    /// It succeeded, yet herdr's list of machines has none for the session.
    Unsaved,
}

impl std::fmt::Display for AddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddError::Failed(why) => f.write_str(why),
            AddError::TimedOut => write!(f, "herdr machine add took over {} min and was stopped", ADD_TIMEOUT.as_secs() / 60),
            AddError::Unsaved => f.write_str("herdr added the machine but does not list it yet"),
        }
    }
}

impl std::error::Error for AddError {}

/// Saves `target` as a machine running the session `session`: herdr checks the host's herdr and starts that
/// session's server first, within [`ADD_TIMEOUT`]. Setup that needs approval (installing or replacing herdr) only
/// runs in a terminal.
pub async fn add_machine(target: &str, session: &str) -> Result<Machine, AddError> {
    add_machine_with(std::ffi::OsStr::new(&super::session::binary()), target, session, ADD_TIMEOUT).await
}

/// [`add_machine`] with the `herdr` to run and its deadline, for a test's stand-in.
async fn add_machine_with(herdr: &std::ffi::OsStr, target: &str, session: &str, timeout: Duration) -> Result<Machine, AddError> {
    let run = |args: Vec<String>| async move {
        let child = tokio::process::Command::from(crate::process::command(herdr))
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // past the deadline the child is dropped, and herdr ended with it
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(timeout, child).await {
            Err(_) => Err(AddError::TimedOut),
            Ok(Err(e)) => Err(AddError::Failed(format!("herdr: {e}"))),
            Ok(Ok(out)) if !out.status.success() => Err(AddError::Failed(format!("herdr {}: {}", args[..2].join(" "), String::from_utf8_lossy(&out.stderr).trim()))),
            Ok(Ok(out)) => Ok(out.stdout),
        }
    };
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    run(words(&["machine", "add", target, "--remote-session", session])).await.map_err(|e| match e {
        AddError::Failed(why) => {
            AddError::Failed(format!("{why}\nIf the host needs herdr set up, run `herdr machine add {target} --remote-session {session}` in a terminal."))
        }
        e => e,
    })?;
    let list = run(words(&["machine", "list", "--json"])).await?;
    let saved: Vec<Machine> = serde_json::from_slice(&list).map_err(|e| AddError::Failed(format!("herdr's list of machines was not readable: {e}")))?;
    saved.into_iter().find(|m| m.target == target && m.session == session).ok_or(AddError::Unsaved)
}

/// Renames the machine's label.
pub fn rename_machine(id: &str, label: &str) -> anyhow::Result<()> {
    herdr(&["machine", "rename", id, "--label", label]).map(|_| ())
}

/// Forgets the machine; its sessions keep running on the host.
pub fn remove_machine(id: &str) -> anyhow::Result<()> {
    herdr(&["machine", "remove", id]).map(|_| ())
}

fn herdr(args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let out = crate::process::command(super::session::binary()).args(args).stdin(Stdio::null()).output()?;
    anyhow::ensure!(
        out.status.success(),
        "herdr {}: {}",
        args[..2.min(args.len())].join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out.stdout)
}

/// The SSH target of the machine shown; `None` while a local session is.
static SHOWN: Mutex<Option<String>> = Mutex::new(None);

/// Marks a located directory as a machine's: `MARK target MARK path`.
const MARK: char = '\u{1}';

/// The machine shown from now on, by its SSH target; `None` for a local session.
pub fn show_machine(target: Option<String>) {
    *SHOWN.lock().unwrap() = target;
}

/// A directory of the place shown, as the rest of Shepherd keeps it: a machine's path is that machine's, and git
/// reaches it over SSH.
pub fn locate(path: &str) -> String {
    locate_on(SHOWN.lock().unwrap().as_deref(), path)
}

/// A directory of the machine at `target` (`None` this one), as [`locate`] keeps it, whichever place is shown.
pub fn locate_on(target: Option<&str>, path: &str) -> String {
    match target {
        Some(target) => format!("{MARK}{target}{MARK}{path}"),
        None => path.to_string(),
    }
}

/// A located directory's machine, by its SSH target (`None` here), and its path there.
pub fn split(dir: &str) -> (Option<&str>, &str) {
    dir.strip_prefix(MARK).and_then(|rest| rest.split_once(MARK)).map_or((None, dir), |(target, path)| (Some(target), path))
}

/// `git -C <path> <args>` for a located directory: here, or on its machine over the shared SSH connection.
pub fn git(dir: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    match split(dir) {
        (None, path) => crate::process::command("git").arg("-C").arg(path).args(args).stdin(Stdio::null()).output(),
        (Some(target), path) => {
            let words: Vec<String> = ["git", "-C", path].iter().chain(args).map(|w| quote(w)).collect();
            ssh(target, &words.join(" "))
        }
    }
}

/// `script` run by `sh` on the machine at `target` (its login shell may be fish), over the shared SSH connection.
pub fn ssh(target: &str, script: &str) -> std::io::Result<std::process::Output> {
    crate::process::command("ssh").args(ssh_options()).arg("--").arg(target).arg(format!("sh -c {}", quote(script))).stdin(Stdio::null()).output()
}

/// `word` for the remote shell, as one argument.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The relays running, by machine id and session, to their API socket.
static RELAYS: Mutex<Option<HashMap<(String, String), PathBuf>>> = Mutex::new(None);

// ponytail: a relay lives until Shepherd quits; stop it on a switch away if idle SSH masters ever matter.
/// The local API socket that reaches `machine`, starting its relay on first use. Its client socket is the
/// `herdr-client.sock` next to it, where Shepherd looks for one.
pub fn relay(machine: &Machine, rt: &tokio::runtime::Handle) -> anyhow::Result<PathBuf> {
    static PRUNED: std::sync::Once = std::sync::Once::new();
    PRUNED.call_once(|| prune_old_relays(&cache_dir().join("r")));
    let mut relays = RELAYS.lock().unwrap();
    let relays = relays.get_or_insert_with(HashMap::new);
    let key = (machine.id.clone(), machine.session.clone());
    if let Some(api) = relays.get(&key) {
        return Ok(api.clone());
    }
    let dir = cache_dir().join("r").join(relay_dir(&machine.id, &machine.session));
    std::fs::create_dir_all(&dir)?;
    let api = dir.join("herdr.sock");
    for (path, bridge) in [(api.clone(), "remote-api-bridge"), (dir.join("herdr-client.sock"), "remote-client-bridge")] {
        let (target, command) = (machine.target.clone(), bridge_command(&machine.session, bridge));
        super::socket::listen(&path, rt, move |stream| {
            let (target, command) = (target.clone(), command.clone());
            async move { pipe(stream, &target, &command).await }
        })?;
    }
    relays.insert(key, api.clone());
    Ok(api)
}

/// One local connection, carried over SSH to the bridge.
async fn pipe(stream: super::socket::AsyncStream, target: &str, command: &str) {
    let Ok(mut ssh) = tokio::process::Command::from(crate::process::command("ssh"))
        .args(ssh_options())
        .arg("--")
        .arg(target)
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        return;
    };
    let (Some(mut to_ssh), Some(mut from_ssh)) = (ssh.stdin.take(), ssh.stdout.take()) else { return };
    let (mut read, mut write) = tokio::io::split(stream);
    let up = async {
        let _ = tokio::io::copy(&mut read, &mut to_ssh).await;
        drop(to_ssh); // the bridge sees EOF and ends
    };
    let down = async {
        let _ = tokio::io::copy(&mut from_ssh, &mut write).await;
    };
    tokio::select! {
        _ = async { tokio::join!(up, down) } => {}
        _ = ssh.wait() => {}
    }
}

/// Never prompts (there is no terminal); one shared SSH connection per host, as herdr's own machines keep, where
/// OpenSSH shares them (not on Windows).
fn ssh_options() -> Vec<String> {
    let control = cache_dir().join("cm-%C");
    let shared = [
        "-o".into(), "ControlMaster=auto".into(),
        "-o".into(), "ControlPersist=600".into(),
        "-o".into(), format!("ControlPath={}", control.display()),
    ];
    [
        "-C",
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=10",
        "-o", "ServerAliveInterval=15",
        "-o", "ServerAliveCountMax=4",
    ]
    .into_iter()
    .map(String::from)
    .chain(shared.into_iter().filter(|_| !cfg!(windows)))
    .collect()
}

/// Where herdr may be on a host, for `sh`, in the order tried: on the PATH first, then where its installers put it
/// (a non-interactive SSH shell has a bare PATH): cargo, Homebrew on Linux and macOS, Nix's profiles.
const HERDR_CANDIDATES: [&str; 10] = [
    "herdr",
    "\"$HOME/.local/bin/herdr\"",
    "\"$HOME/.cargo/bin/herdr\"",
    "/home/linuxbrew/.linuxbrew/bin/herdr",
    "/opt/homebrew/bin/herdr",
    "/usr/local/bin/herdr",
    "\"$HOME/.nix-profile/bin/herdr\"",
    "\"/etc/profiles/per-user/$USER/bin/herdr\"",
    "/nix/var/nix/profiles/default/bin/herdr",
    "/run/current-system/sw/bin/herdr",
];

/// For `sh`: runs `action` with `$h` the first herdr found on the host, or says none is, exiting 127.
fn with_herdr(action: &str) -> String {
    format!(
        "for h in {}; do command -v \"$h\" >/dev/null 2>&1 && {action}; done; \
         echo \"herdr is not installed on this host\" >&2; exit 127",
        HERDR_CANDIDATES.join(" ")
    )
}

/// What SSH asks the host's login shell to run: `script` handed to `sh`, as one single-quoted word. The login shell
/// may be fish or nushell, which parse the script differently or not at all, but each takes a single-quoted word
/// with no quote or backslash in it literally: the scripts here never have one (`the_scripts_quote_plainly`).
/// `sh -s` with the script on stdin would sidestep the login shell too, but a bridge's stdin is its byte stream.
fn remote_command(script: &str) -> String {
    format!("sh -c {}", quote(script))
}

/// The bridge's script: `bridge` for `session`. Its output is the bridge's protocol from the first byte, so it never
/// prints [`OUTPUT_MARKER`].
fn bridge_script(session: &str, bridge: &str) -> String {
    // the session's name is herdr's ASCII letters, digits, '.', '_' and '-', so it needs no quoting
    with_herdr(&format!("exec \"$h\" --session {session} {bridge}"))
}

/// The command that runs `bridge` for `session` on the host.
fn bridge_command(session: &str, bridge: &str) -> String {
    remote_command(&bridge_script(session, bridge))
}

/// Printed on its own line just before herdr runs, so what a login shell prints first (a banner, a motd from
/// `.bashrc`) is told from herdr's answer: [`after_marker`].
const OUTPUT_MARKER: &str = "SHEPHERD_OUT";

/// The script that lists the host's herdr sessions as JSON. A session or socket the host's shell sets (it may run
/// inside a herdr pane there) would point herdr at one session; the list is of them all.
fn list_script() -> String {
    format!(
        "unset HERDR_SESSION HERDR_SOCKET_PATH; {}",
        with_herdr(&format!("{{ echo {OUTPUT_MARKER}; exec \"$h\" session list --json; }}"))
    )
}

/// What came after the marker's line: herdr's own output. `None` when the marker never came (herdr never ran).
fn after_marker(out: &str) -> Option<&str> {
    out.match_indices(OUTPUT_MARKER).find_map(|(at, _)| {
        let rest = &out[at + OUTPUT_MARKER.len()..];
        let line_start = at == 0 || out[..at].ends_with('\n');
        let line_end = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n')).or(rest.is_empty().then_some(rest));
        line_end.filter(|_| line_start)
    })
}

/// How long listing a host's sessions may take, the SSH connection made first included.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// The most of herdr's answer read: a list of sessions is a few kB.
const LIST_CAP: usize = 1 << 20;

/// Why a host's sessions could not be listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListError {
    /// No herdr on the host, in any place its installers put it.
    NotInstalled,
    /// SSH cannot sign in without a prompt (a key not loaded, a host key not trusted yet): `herdr machine
    /// reconnect` in a terminal signs in, and the shared connection then carries the list.
    SignIn,
    /// Anything else, as SSH or herdr said it.
    Failed(String),
}

impl std::fmt::Display for ListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListError::NotInstalled => f.write_str("herdr is not installed on this host"),
            ListError::SignIn => f.write_str("SSH needs to sign in: run `herdr machine reconnect` for this host in a terminal"),
            ListError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for ListError {}

/// What a failed `ssh` run says: 127 before the marker (`herdr_ran` false) is the script's own exit when no herdr
/// was found, and after it herdr's or something it ran; 255 is SSH's, and its complaint tells a sign-in it could
/// not make without a prompt (BatchMode) from a host out of reach.
fn ssh_failure(code: Option<i32>, stderr: &str, herdr_ran: bool) -> ListError {
    const SIGN_IN: [&str; 3] = ["Permission denied", "Host key verification failed", "REMOTE HOST IDENTIFICATION HAS CHANGED"];
    let said = stderr.trim();
    match code {
        Some(127) if !herdr_ran => ListError::NotInstalled,
        Some(255) if SIGN_IN.iter().any(|s| said.contains(s)) => ListError::SignIn,
        _ if !said.is_empty() => ListError::Failed(said.to_string()),
        Some(code) => ListError::Failed(format!("ssh exited with {code}")),
        None => ListError::Failed("ssh was killed".into()),
    }
}

/// The herdr sessions on the host at `target`, saved as machines or not, in herdr's order: `herdr session list`
/// is a command of herdr's own, not of its API, so it runs over SSH (the shared connection, when there is one)
/// rather than through a machine's relay.
pub async fn remote_sessions(target: &str) -> Result<Vec<super::SessionInfo>, ListError> {
    sessions_over(std::ffi::OsStr::new("ssh"), target, LIST_TIMEOUT).await
}

/// [`remote_sessions`] with the `ssh` to run, for a test's stand-in.
pub(crate) async fn sessions_over(ssh: &std::ffi::OsStr, target: &str, timeout: Duration) -> Result<Vec<super::SessionInfo>, ListError> {
    use tokio::io::AsyncReadExt;
    let failed = |e: std::io::Error| ListError::Failed(format!("ssh: {e}"));
    let mut child = tokio::process::Command::from(crate::process::command(ssh))
        .args(ssh_options())
        .arg("--")
        .arg(target)
        .arg(remote_command(&list_script()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // a timeout or an answer past the cap drops the child: SSH is ended with it
        .kill_on_drop(true)
        .spawn()
        .map_err(failed)?;
    let (Some(mut out), Some(mut err)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(ListError::Failed("ssh gave no output".into()));
    };
    let run = async {
        let stdout = async {
            let mut v = Vec::new();
            (&mut out).take(LIST_CAP as u64 + 1).read_to_end(&mut v).await?;
            if v.len() > LIST_CAP {
                return Err(std::io::Error::other("herdr's answer was over 1 MB"));
            }
            Ok(v)
        };
        let stderr = async {
            let mut v = Vec::new();
            (&mut err).take(64 * 1024).read_to_end(&mut v).await?;
            // the rest is drained, not left in the pipe: a host that fills it would stall before herdr answers
            tokio::io::copy(&mut err, &mut tokio::io::sink()).await?;
            Ok(v)
        };
        let (stdout, stderr) = tokio::try_join!(stdout, stderr)?;
        Ok::<_, std::io::Error>((child.wait().await?, stdout, stderr))
    };
    let (status, stdout, stderr) = match tokio::time::timeout(timeout, run).await {
        Err(_) => return Err(ListError::Failed(format!("no answer from {target} in {} s", timeout.as_secs()))),
        Ok(ran) => ran.map_err(failed)?,
    };
    let stdout = String::from_utf8_lossy(&stdout);
    if !status.success() {
        return Err(ssh_failure(status.code(), &String::from_utf8_lossy(&stderr), after_marker(&stdout).is_some()));
    }
    let answer = after_marker(&stdout).ok_or_else(|| ListError::Failed("herdr's answer over SSH was not readable".into()))?;
    super::session::parse_sessions(answer.as_bytes()).map_err(|e| ListError::Failed(format!("herdr's session list was not readable: {e}")))
}

/// The sessions `found` on the host at `target` that no machine in `saved` (turned on or off) is for: the ones a
/// machine could be added for. The default session first, then those running, each in herdr's order. Only the
/// exact target counts: an alias of the same host is another target to herdr too.
pub fn unsaved(found: &[super::SessionInfo], saved: &[Machine], target: &str) -> Vec<super::SessionInfo> {
    let mut left: Vec<super::SessionInfo> =
        found.iter().filter(|s| !saved.iter().any(|m| m.target == target && m.session == s.name)).cloned().collect();
    left.sort_by_key(|s| (!s.default, !s.running));
    left
}

use crate::settings::cache_dir;

/// A relay folder's name as [`relay_dir`] makes it.
fn is_relay_dir(name: &str) -> bool {
    name.len() == 12 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Relay folders under `root` from before their names were hashes (an id's first 12 letters and digits): removed,
/// once a run and best effort, when all they hold is sockets nothing listens on. Anything else in one, or a socket
/// that answers (an older Shepherd still running), keeps it.
#[cfg(unix)]
fn prune_old_relays(root: &std::path::Path) {
    use std::os::unix::fs::FileTypeExt;
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_str().is_some_and(is_relay_dir) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(inside) = std::fs::read_dir(entry.path()).and_then(|d| d.collect::<std::io::Result<Vec<_>>>()) else { continue };
        let sockets = inside.iter().all(|f| f.file_type().is_ok_and(|t| t.is_socket()));
        if !sockets || inside.iter().any(|f| std::os::unix::net::UnixStream::connect(f.path()).is_ok()) {
            continue;
        }
        for socket in &inside {
            let _ = std::fs::remove_file(socket.path());
        }
        // not remove_dir_all: anything that came meanwhile stays, and the folder with it
        let _ = std::fs::remove_dir(entry.path());
    }
}

#[cfg(not(unix))]
fn prune_old_relays(_root: &std::path::Path) {}

/// A relay's directory name: 12 hex digits of a hash of the machine's id and session, short enough for a socket
/// path (macOS allows 104 bytes) and distinct where ids share a prefix.
fn relay_dir(id: &str, session: &str) -> String {
    // FNV-1a, 64 bits: std's hasher may change between Rust releases, and the name must not
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes().chain([0]).chain(session.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")[..12].to_string()
}

/// A stand-in host and `ssh` for tests here and in the sidebar's discovery: no real host is reached.
#[cfg(all(test, unix))]
pub(crate) mod fake {
    /// An executable `sh` script at `path`.
    pub(crate) fn script(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A host for the tests, in a folder of its own: `bin` holds `sh` and, given its body, a stand-in `herdr`, and is
    /// all its PATH has, so no herdr of this machine's is found by name.
    pub(crate) fn host(herdr: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink("/bin/sh", bin.join("sh")).unwrap();
        if let Some(body) = herdr {
            script(&bin.join("herdr"), body);
        }
        dir
    }

    /// A stand-in for `ssh` to `host`: it checks its last words are `-- you@workbox <command>`, runs `login` (a
    /// banner, a refusal), then hands the command to a shell, as sshd hands it to the login shell. No real host is
    /// reached.
    pub(crate) fn fake_ssh(host: &tempfile::TempDir, login: &str) -> std::path::PathBuf {
        let path = host.path().join("ssh");
        let body = r#"n=$#
eval "dash=\${$((n - 2))} target=\${$((n - 1))} command=\${$n}"
[ "$dash" = -- ] && [ "$target" = you@workbox ] || { echo "ssh got: $*" >&2; exit 98; }
LOGIN
PATH='HOST/bin' HOME='HOST' exec /bin/sh -c "$command""#;
        script(&path, &body.replace("LOGIN", login).replace("HOST", &host.path().to_string_lossy()));
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use super::fake::{fake_ssh, host};

    #[test]
    fn machines_from_herdrs_list() {
        let json = br#"[{"id":"m-1a2b","label":"workbox/agents","target":"you@workbox","session":"agents","enabled":true,"selected":false}]"#;
        let list: Vec<Machine> = serde_json::from_slice(json).unwrap();
        assert_eq!(list[0], Machine { id: "m-1a2b".into(), label: "workbox/agents".into(), target: "you@workbox".into(), session: "agents".into(), enabled: true });
    }

    #[test]
    fn the_bridge_runs_for_the_session() {
        let command = bridge_command("agents", "remote-client-bridge");
        assert!(command.contains("--session agents remote-client-bridge"));
        assert!(command.contains("\"$HOME/.local/bin/herdr\""));
    }

    #[test]
    fn a_directory_located_on_a_machine_splits_back() {
        let dir = locate_on(Some("you@workbox"), "/srv/app");
        assert_eq!(split(&dir), (Some("you@workbox"), "/srv/app"));
        assert_eq!(locate_on(None, "/srv/app"), "/srv/app");
        assert_eq!(split(&locate_on(None, "/srv/app")), (None, "/srv/app"));
    }

    #[test]
    fn relay_paths_fit_a_socket() {
        let dir = relay_dir("0123456789abcdef-0123", "a-session-with-a-long-name");
        let api = cache_dir().join("r").join(&dir).join("herdr-client.sock");
        assert!(api.as_os_str().len() < 104, "{}", api.display());
        // macOS's cache for a 32-character user name, the longest its accounts allow
        let mac = std::path::Path::new("/Users").join("a".repeat(32)).join("Library/Caches/Shepherd/r").join(&dir).join("herdr-client.sock");
        assert!(mac.as_os_str().len() < 104, "{} is {}", mac.display(), mac.as_os_str().len());
    }

    #[cfg(unix)]
    #[test]
    fn old_relay_folders_go_when_only_dead_sockets_are_left() {
        let root = tempfile::tempdir().unwrap();
        let folder = |name: &str| {
            let p = root.path().join(name);
            std::fs::create_dir(&p).unwrap();
            p
        };
        // a socket nothing listens on any more: bound, then let go, as a relay leaves it when Shepherd quits
        let dead = |p: &std::path::Path| drop(std::os::unix::net::UnixListener::bind(p).unwrap());
        let old = folder("m1a2bworkbox");
        dead(&old.join("herdr.sock"));
        dead(&old.join("herdr-client.sock"));
        let kept_file = folder("m9z8yotherbox");
        dead(&kept_file.join("herdr.sock"));
        std::fs::write(kept_file.join("notes"), "").unwrap();
        let live = folder("m7x6wlivebox");
        let _listening = std::os::unix::net::UnixListener::bind(live.join("herdr.sock")).unwrap();
        let new = folder(&relay_dir("m-1a2b", "agents"));
        dead(&new.join("herdr.sock"));
        std::fs::write(root.path().join("stray"), "").unwrap();
        prune_old_relays(root.path());
        assert!(!old.exists(), "only dead sockets: gone");
        assert!(kept_file.join("notes").exists() && kept_file.join("herdr.sock").exists(), "something else in it: kept whole");
        assert!(live.join("herdr.sock").exists(), "a socket that answers: kept");
        assert!(new.join("herdr.sock").exists(), "a relay folder of today: kept");
        assert!(root.path().join("stray").exists());
        prune_old_relays(&root.path().join("missing"));
    }

    #[test]
    fn relay_dirs_are_stable_and_tell_shared_prefixes_apart() {
        assert_eq!(relay_dir("m-1a2b", "agents"), "a3a934ee189f", "FNV-1a of `m-1a2b\\0agents`");
        assert_eq!(relay_dir("m-1a2b", "agents").len(), 12);
        // the old names took the id's first 12 letters and digits: these two shared one
        assert_ne!(relay_dir("0123456789abcdef-0123", "default"), relay_dir("0123456789abXYZ", "default"));
        assert_ne!(relay_dir("m-1a2b", "agents"), relay_dir("m-1a2b", "default"), "one machine id, two sessions");
        assert_ne!(relay_dir("m-1a2ba", "gents"), relay_dir("m-1a2b", "agents"), "the id ends where the session starts");
    }

    #[test]
    fn the_bridge_never_prints_the_marker() {
        for bridge in ["remote-api-bridge", "remote-client-bridge"] {
            let command = bridge_command("default", bridge);
            assert!(!command.contains(OUTPUT_MARKER), "{command}");
            assert!(command.contains(&format!("exec \"$h\" --session default {bridge}; done;")), "{command}");
            assert_eq!(command, format!("sh -c '{}'", bridge_script("default", bridge)), "handed to sh, whatever the login shell");
            assert!(command.ends_with("exit 127'"), "{command}");
        }
    }

    /// fish and nushell take a single-quoted word literally only when it holds no quote or backslash.
    #[test]
    fn the_scripts_quote_plainly() {
        for script in [list_script(), bridge_script("agents", "remote-api-bridge"), bridge_script("default", "remote-client-bridge")] {
            assert!(!script.contains('\'') && !script.contains('\\'), "{script}");
            assert_eq!(remote_command(&script), format!("sh -c '{script}'"));
        }
    }

    #[test]
    fn the_list_runs_herdr_wherever_it_is_after_the_marker() {
        let script = list_script();
        assert!(script.starts_with("unset HERDR_SESSION HERDR_SOCKET_PATH; for h in herdr "), "{script}");
        assert!(script.contains("{ echo SHEPHERD_OUT; exec \"$h\" session list --json; }; done;"), "{script}");
        for candidate in HERDR_CANDIDATES {
            assert!(script.contains(candidate) && bridge_command("default", "remote-api-bridge").contains(candidate), "{candidate}");
        }
        assert_eq!(script.matches(OUTPUT_MARKER).count(), 1);
    }

    #[test]
    fn herdrs_answer_is_what_follows_the_marker() {
        let json = r#"{"sessions":[]}"#;
        assert_eq!(after_marker(&format!("SHEPHERD_OUT\n{json}")), Some(json));
        assert_eq!(after_marker(&format!("Welcome to workbox\nlast login: never\nSHEPHERD_OUT\n{json}")), Some(json), "a banner");
        assert_eq!(after_marker(&format!("banner\r\nSHEPHERD_OUT\r\n{json}")), Some(json));
        assert_eq!(after_marker("Welcome to workbox\n"), None, "herdr never ran");
        assert_eq!(after_marker(""), None);
        assert_eq!(after_marker("banner\nSHEPHERD_OUT"), Some(""), "the marker the last line, herdr said nothing");
        assert_eq!(after_marker(&format!("say SHEPHERD_OUT twice\nSHEPHERD_OUT\n{json}")), Some(json), "only a line of its own");
        assert_eq!(after_marker(&format!("motd: SHEPHERD_OUT\nnews\nSHEPHERD_OUT\n{json}")), Some(json), "from the line's start");
        assert_eq!(after_marker(&format!("SHEPHERD_OUTPUT\nSHEPHERD_OUT\n{json}")), Some(json));
    }

    #[test]
    fn a_failed_ssh_says_what_to_do() {
        assert_eq!(ssh_failure(Some(127), "herdr is not installed on this host\n", false), ListError::NotInstalled);
        assert_eq!(ssh_failure(Some(127), "herdr: git: command not found\n", true), ListError::Failed("herdr: git: command not found".into()), "herdr ran");
        assert_eq!(ssh_failure(Some(255), "you@workbox: Permission denied (publickey).\r\n", false), ListError::SignIn);
        assert_eq!(ssh_failure(Some(255), "Host key verification failed.\n", false), ListError::SignIn);
        assert_eq!(
            ssh_failure(Some(255), "ssh: Could not resolve hostname workbox: Name or service not known\n", false),
            ListError::Failed("ssh: Could not resolve hostname workbox: Name or service not known".into())
        );
        assert_eq!(ssh_failure(Some(2), "error: unexpected argument '--json' found\n", true), ListError::Failed("error: unexpected argument '--json' found".into()), "an old herdr");
        assert_eq!(ssh_failure(Some(1), "", false), ListError::Failed("ssh exited with 1".into()));
        assert_eq!(ssh_failure(Some(1), "Permission denied", false), ListError::Failed("Permission denied".into()), "SSH's own failures are 255");
    }

    fn info(name: &str, running: bool) -> crate::herdr::SessionInfo {
        crate::herdr::SessionInfo { name: name.into(), running, socket_path: format!("/home/you/.config/herdr/sessions/{name}/herdr.sock"), default: name == "default" }
    }

    fn saved(target: &str, session: &str, enabled: bool) -> Machine {
        Machine { id: format!("m-{session}"), label: format!("workbox/{session}"), target: target.into(), session: session.into(), enabled }
    }

    #[test]
    fn the_sessions_left_to_add_skip_the_saved_ones() {
        let found = [info("api", false), info("agents", true), info("default", true), info("docs", true), info("old", false)];
        let names = |list: Vec<crate::herdr::SessionInfo>| list.into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names(unsaved(&found, &[], "you@workbox")), ["default", "agents", "docs", "api", "old"], "default, running, herdr's order");
        let machines = [saved("you@workbox", "agents", true), saved("you@workbox", "old", false), saved("you@otherbox", "docs", true)];
        assert_eq!(names(unsaved(&found, &machines, "you@workbox")), ["default", "docs", "api"], "a machine turned off is saved too");
        assert_eq!(names(unsaved(&found, &machines, "workbox")).len(), 5, "only the exact target");
    }

    #[test]
    fn a_hosts_list_parses_as_one_here() {
        let json = br#"{"sessions":[{"default":true,"name":"default","running":true,"session_dir":"/home/you/.config/herdr","socket_path":"/home/you/.config/herdr/herdr.sock"},{"default":false,"name":"agents","running":false,"session_dir":"/home/you/.config/herdr/sessions/agents","socket_path":"/home/you/.config/herdr/sessions/agents/herdr.sock"}]}"#;
        let list = crate::herdr::session::parse_sessions(json).unwrap();
        assert_eq!(list.iter().map(|s| (s.name.as_str(), s.running, s.default)).collect::<Vec<_>>(), [("default", true, true), ("agents", false, false)]);
    }

    /// The scripts as a host's `sh` runs them, with a stand-in herdr first on its PATH that prints its arguments
    /// and the session variables it got.
    #[cfg(unix)]
    #[test]
    fn the_scripts_run_in_sh() {
        let dir = host(Some("echo \"args: $* session: ${HERDR_SESSION:-none}\""));
        let run = |command: &str| {
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .env("PATH", dir.path().join("bin"))
                .env("HOME", dir.path())
                .env("HERDR_SESSION", "agents")
                .output()
                .unwrap();
            (out.status.code(), String::from_utf8(out.stdout).unwrap())
        };
        assert_eq!(run(&remote_command(&list_script())), (Some(0), "SHEPHERD_OUT\nargs: session list --json session: none\n".into()));
        let bridge = run(&bridge_command("agents", "remote-api-bridge"));
        assert_eq!(bridge, (Some(0), "args: --session agents remote-api-bridge session: agents\n".into()));
    }

    /// A stand-in herdr: `add` is what its `machine add` does, `list` what its `machine list --json` prints.
    #[cfg(unix)]
    fn fake_herdr(add: &str, list: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        super::fake::script(&dir.path().join("herdr"), &format!("case \"$2\" in add) {add} ;; list) echo '{list}' ;; esac"));
        dir
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn adding_a_machine_says_why_it_did_not_and_stops_at_its_deadline() {
        let add = |dir: tempfile::TempDir, secs| async move {
            let herdr = dir.path().join("herdr");
            add_machine_with(herdr.as_os_str(), "you@workbox", "docs", Duration::from_secs(secs)).await
        };
        let listed = r#"[{"id":"m-9","label":"workbox/docs","target":"you@workbox","session":"docs","enabled":true}]"#;
        let added = add(fake_herdr("true", listed), 5).await.unwrap();
        assert_eq!((added.id.as_str(), added.session.as_str()), ("m-9", "docs"));
        assert!(matches!(add(fake_herdr("true", "[]"), 5).await, Err(AddError::Unsaved)), "added, yet not listed");
        let refused = add(fake_herdr("echo 'cannot reach you@workbox' >&2; exit 1", listed), 5).await;
        assert!(matches!(&refused, Err(AddError::Failed(why)) if why.starts_with("herdr machine add: cannot reach you@workbox\nIf the host")), "{refused:?}");
        let started = std::time::Instant::now();
        assert!(matches!(add(fake_herdr("exec /bin/sleep 30", listed), 1).await, Err(AddError::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(5), "herdr is ended, not waited for");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hosts_sessions_come_over_ssh() {
        let list = |ssh: std::path::PathBuf, secs| async move { sessions_over(ssh.as_os_str(), "you@workbox", Duration::from_secs(secs)).await };
        let ok = host(Some(r#"printf '{"sessions":[{"name":"default","running":true,"socket_path":"/s","default":true}]}\n'"#));
        let found = list(fake_ssh(&ok, "echo 'Welcome to workbox'"), 5).await.unwrap();
        assert_eq!(found.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["default"], "past the banner");

        let denied = host(None);
        let refused = list(fake_ssh(&denied, "echo 'you@workbox: Permission denied (publickey).' >&2; exit 255"), 5).await;
        assert_eq!(refused.unwrap_err(), ListError::SignIn);

        let missing = host(None);
        let absent = list(fake_ssh(&missing, "echo 'herdr is not installed on this host' >&2; exit 127"), 5).await;
        assert_eq!(absent.unwrap_err(), ListError::NotInstalled, "127 before the marker");
        let failing = host(Some("echo 'herdr: git: command not found' >&2; exit 127"));
        let ran = list(fake_ssh(&failing, ""), 5).await;
        assert_eq!(ran.unwrap_err(), ListError::Failed("herdr: git: command not found".into()), "127 after it");

        let slow = host(Some("exec /bin/sleep 30"));
        let started = std::time::Instant::now();
        let late = list(fake_ssh(&slow, ""), 1).await.unwrap_err();
        assert_eq!(late, ListError::Failed("no answer from you@workbox in 1 s".into()));
        assert!(started.elapsed() < Duration::from_secs(5), "ssh is ended, not waited for");

        let flood = host(Some("exec /bin/dd if=/dev/zero bs=1000 count=2000 2>/dev/null"));
        let over = list(fake_ssh(&flood, ""), 5).await.unwrap_err();
        assert_eq!(over, ListError::Failed("ssh: herdr's answer was over 1 MB".into()));

        // 200 kB of warnings, past the 64 kB kept and the pipe's own buffer, before the answer
        let chatty = host(Some("exec 3>&2; /bin/dd if=/dev/zero bs=1000 count=200 2>/dev/null >&3; printf '{\"sessions\":[]}\\n'"));
        let started = std::time::Instant::now();
        assert_eq!(list(fake_ssh(&chatty, ""), 5).await, Ok(Vec::new()));
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    }
}
