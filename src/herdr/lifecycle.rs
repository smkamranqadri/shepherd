//! Sessions started, stopped and restarted from the sidebar's menus, machines turned on and off, and the list of
//! every session and machine read with a deadline (`herdr session list`, `herdr machine list`). Everything that waits
//! on herdr runs on the tokio runtime, killed past its deadline.
use super::{Machine, SessionInfo};
use serde_json::Value;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// How long a stop or a machine change may take before Shepherd gives up on it.
const DEADLINE: Duration = Duration::from_secs(10);

/// herdr's sessions and saved machines (enabled or not), each list read within `timeout`. The machines are `None`
/// when their list could not be read (it timed out, failed, or a herdr without machines): not known, rather than
/// none, so nothing kept for them is forgotten.
pub async fn list_places(timeout: Duration) -> anyhow::Result<(Vec<SessionInfo>, Option<Vec<Machine>>)> {
    let sessions = output(&["session", "list", "--json"], timeout).await?;
    let sessions = super::session::parse_sessions(&sessions)?;
    let machines = output(&["machine", "list", "--json"], timeout).await.ok().and_then(|json| serde_json::from_slice(&json).ok());
    Ok((sessions, machines))
}

/// `herdr <args>` run to its end within `timeout` (killed past it).
async fn run(args: &[&str], timeout: Duration) -> anyhow::Result<std::process::Output> {
    let mut command = tokio::process::Command::from(crate::process::command(super::session::binary()));
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| anyhow::anyhow!("herdr {} took over {} s", name(args), timeout.as_secs()))?
        .map_err(Into::into)
}

fn name(args: &[&str]) -> String {
    args[..2.min(args.len())].join(" ")
}

/// `herdr <args>`'s output, or why it failed or did not finish in time.
async fn output(args: &[&str], timeout: Duration) -> anyhow::Result<Vec<u8>> {
    let out = run(args, timeout).await?;
    anyhow::ensure!(out.status.success(), "herdr {}: {}", name(args), String::from_utf8_lossy(&out.stderr).trim());
    Ok(out.stdout)
}

/// `herdr <args>` that answers JSON: its error message when it answers with one.
async fn json_call(args: &[&str], timeout: Duration) -> anyhow::Result<()> {
    let out = run(args, timeout).await?;
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if let Some(message) = answer["error"]["message"].as_str() {
        anyhow::bail!("{message}");
    }
    anyhow::ensure!(out.status.success(), "herdr {}: {}", name(args), String::from_utf8_lossy(&out.stderr).trim());
    Ok(())
}

/// Stops the session `name`'s server (its panes close).
pub async fn stop_session(name: String) -> anyhow::Result<()> {
    json_call(&["session", "stop", &name, "--json"], DEADLINE).await
}

/// Starts the session's server, detached (spawning returns at once); `default` is herdr's default session, which
/// takes no `--session`.
pub fn start_session(name: &str, default: bool) -> anyhow::Result<()> {
    Ok(super::start_session_server((!default).then_some(name))?)
}

/// Stops the session, waits (at most 5 s) for its socket to stop answering, and starts it again. One that still
/// answers is left as it is, and says so: a second server would share its state.
pub async fn restart_session(info: SessionInfo) -> anyhow::Result<()> {
    stop_session(info.name.clone()).await?;
    let socket = std::path::PathBuf::from(&info.socket_path);
    let until = Instant::now() + Duration::from_secs(5);
    let mut answers = super::socket::connect(&socket).is_ok();
    while answers && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(100)).await;
        answers = super::socket::connect(&socket).is_ok();
    }
    anyhow::ensure!(!answers, "{} still answers after it was stopped, so it was not started again", info.name);
    start_session(&info.name, info.default)
}

/// Turns the saved machine on or off in herdr's catalog.
pub async fn set_machine_enabled(id: String, enabled: bool) -> anyhow::Result<()> {
    output(&["machine", if enabled { "enable" } else { "disable" }, &id], DEADLINE).await.map(|_| ())
}

/// The command that signs in to a machine in a terminal: Shepherd's SSH never prompts.
pub fn reconnect_command(id: &str) -> String {
    format!("herdr machine reconnect {id}")
}
