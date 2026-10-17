use anyhow::{Context, anyhow};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use super::socket::{AsyncStream, connect_async};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

/// Lifecycle events that need no `pane_id` filter (herdr rejects `pane.agent_status_changed` and friends without one,
/// and sends no `pane.updated` when only an agent's status changes: [`Client::subscribe`] follows that per pane).
pub const EVENT_TYPES: &[&str] = &[
    "workspace.created", "workspace.updated", "workspace.closed", "workspace.renamed",
    "workspace.moved", "workspace.reordered", "workspace.focused",
    "worktree.created", "worktree.opened", "worktree.removed",
    "tab.created", "tab.closed", "tab.renamed", "tab.moved", "tab.focused",
    "pane.created", "pane.closed", "pane.updated", "pane.focused", "pane.moved", "pane.exited",
    "pane.agent_detected", "layout.updated",
];

/// What a session not shown is watched for: every change its tree draws (workspaces, worktrees, tabs, panes coming,
/// going or moving, an agent found), not the focus or the layout, which it does not draw. `pane.updated` comes
/// several times a second from a busy agent (its title, scroll, tokens): the watcher sorts those out
/// (`watchers::feeds::relevant`).
pub const WATCH_TYPES: &[&str] = &[
    "workspace.created", "workspace.updated", "workspace.closed", "workspace.renamed", "workspace.moved", "workspace.reordered",
    "worktree.created", "worktree.opened", "worktree.removed",
    "tab.created", "tab.closed", "tab.moved", "tab.focused",
    "pane.created", "pane.closed", "pane.updated", "pane.moved", "pane.exited", "pane.agent_detected",
];

/// herdr answered, with an error: a running server that turned the request down (`unsupported`, or ids it does
/// not know), not one out of reach.
#[derive(Debug)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

/// herdr's answer, when it answered with an error.
pub fn api_error(err: &anyhow::Error) -> Option<&ApiError> {
    err.chain().find_map(|e| e.downcast_ref::<ApiError>())
}

/// herdr does not take the request at all: an unknown method or event type (an older herdr) answers
/// `invalid_request`, and asking again the same way gets the same answer. An error about what the request named
/// (`pane_not_found`: a pane closed since) is not this: the request is fine, its ids are old.
pub fn unsupported(err: &anyhow::Error) -> bool {
    api_error(err).is_some_and(|e| e.code == "invalid_request")
}

/// The event is a `kind` (as subscribed to, `pane.focused`): herdr names the events it sends `pane_focused`.
pub fn event_is(event: &Value, kind: &str) -> bool {
    event["event"].as_str().is_some_and(|e| e == kind || e == kind.replace('.', "_"))
}

/// The next burst of events: waits for one, then for `window()` (made only once the first came, so a timer that
/// starts when made counts from it), and takes every event come by then. `None` once the subscription ended
/// (the server went away, or fell behind and said `events_lost`); events that came before the end come first.
pub async fn burst<F: std::future::Future<Output = ()>>(rx: &mut mpsc::UnboundedReceiver<Value>, window: impl FnOnce() -> F) -> Option<Vec<Value>> {
    let mut events = vec![rx.recv().await?];
    window().await;
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    Some(events)
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct Client {
    path: PathBuf,
}

impl Client {
    /// Same resolution as herdr for the default session: `HERDR_SOCKET_PATH`, else `herdr.sock` in herdr's folder.
    pub fn local() -> Self {
        let path = std::env::var_os("HERDR_SOCKET_PATH").map(PathBuf::from).unwrap_or_else(|| super::config::dir().join("herdr.sock"));
        Self { path }
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // ponytail: one connection per request, like herdr's own CLI; pool if latency ever shows up.
    /// The write half is returned so the caller keeps it alive: the connection lasts while both halves do.
    async fn send(&self, method: &str, params: Value) -> anyhow::Result<(WriteHalf<AsyncStream>, Lines<BufReader<ReadHalf<AsyncStream>>>)> {
        let stream = connect_async(&self.path)
            .await
            .with_context(|| format!("herdr is not reachable at {}", self.path.display()))?;
        let (r, mut w) = tokio::io::split(stream);
        let id = format!("s{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
        let line = json!({"id": id, "method": method, "params": params});
        w.write_all(format!("{line}\n").as_bytes()).await?;
        Ok((w, BufReader::new(r).lines()))
    }

    pub async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let (_w, mut lines) = self.send(method, params).await?;
        let line = lines.next_line().await?.ok_or_else(|| anyhow!("herdr closed the connection"))?;
        into_result(serde_json::from_str(&line)?)
    }

    /// `types`, and `pane.agent_status_changed` for each of `panes`.
    pub async fn subscribe(&self, types: &[&str], panes: &[String]) -> anyhow::Result<mpsc::UnboundedReceiver<Value>> {
        let statuses = panes.iter().map(|p| json!({"type": "pane.agent_status_changed", "pane_id": p}));
        let subscriptions: Vec<Value> = types.iter().map(|t| json!({"type": t})).chain(statuses).collect();
        let (w, mut lines) = self.send("events.subscribe", json!({"subscriptions": subscriptions})).await?;
        let first = lines.next_line().await?.ok_or_else(|| anyhow!("herdr closed the connection"))?;
        into_result(serde_json::from_str(&first)?)?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _w = w;
            loop {
                let line = tokio::select! {
                    line = lines.next_line() => line,
                    _ = tx.closed() => break, // the caller resubscribed: hang up now, not on the next event
                };
                let Ok(Some(line)) = line else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else { break };
                // An error line (events_lost) ends the stream; the caller resubscribes and refetches.
                if value.get("error").is_some() || tx.send(value).is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }
}

fn into_result(mut response: Value) -> anyhow::Result<Value> {
    if let Some(error) = response.get("error") {
        let code = error["code"].as_str().unwrap_or("error").to_string();
        let message = error["message"].as_str().unwrap_or("").to_string();
        return Err(ApiError { code, message }.into());
    }
    Ok(response["result"].take())
}
