//! Keeping the window in step with herdr: subscribe, snapshot, and start herdr's server when nothing answers.
use super::*;

/// Subscribe, then snapshot; every event (coalesced) triggers another snapshot. Reconnects forever.
pub(super) async fn sync_loop(this: gpui::WeakEntity<Shell>, rt: tokio::runtime::Handle, client: Client, resync: std::sync::Arc<tokio::sync::Notify>, cx: &mut gpui::AsyncApp) {
    // When nothing listens on the socket, Shepherd starts herdr's server itself: again after 10 s if it never came
    // up, after a minute if it could not be run at all.
    let (mut next_start, mut spawned, mut start_error, mut downloaded) = (Instant::now(), None::<Instant>, None::<String>, false);
    // a fixture (`SHEPHERD_MOCK`) is the one snapshot there is
    if let Some((snapshot, waited)) = crate::mock::snapshot() {
        let _ = this.update(cx, |s, cx| {
            let now = Instant::now();
            snapshot.track(&mut s.since, now);
            for (id, secs) in waited {
                if let Some((_, at)) = s.since.get_mut(&id) {
                    *at = now.checked_sub(Duration::from_secs(secs)).unwrap_or(now);
                }
            }
            (s.snapshot, s.error, s.connecting) = (Some(snapshot), None, None);
            cx.notify();
        });
        return;
    }
    // the agents' panes, whose status the subscription follows: herdr only sends that per pane
    let mut watched: Vec<String> = Vec::new();
    loop {
        let mut rx = match subscribe(&rt, &client, &mut watched).await {
            Ok(rx) => {
                check_protocol(&this, &rt, &client, cx).await;
                rx
            }
            Err(err) => {
                let no_server = crate::herdr::no_server(&err);
                // a server that answers but won't subscribe may speak another protocol: its ping says which
                let outdated = if no_server { None } else { check_protocol(&this, &rt, &client, cx).await };
                if no_server && Instant::now() >= next_start {
                    // no herdr anywhere: Shepherd downloads it, once a launch, then starts it
                    let fetched = if crate::herdr::missing() && !std::mem::replace(&mut downloaded, true) {
                        let _ = this.update(cx, |s, cx| {
                            s.error = Some("Downloading herdr…".into());
                            cx.notify()
                        });
                        rt.spawn_blocking(crate::herdr::install).await.expect("tokio").map(drop).map_err(|e| format!("herdr isn't installed and Shepherd couldn't download it: {e:#}"))
                    } else {
                        Ok(())
                    };
                    match fetched.and_then(|()| crate::herdr::start_server().map_err(|e| format!("herdr isn't running and Shepherd couldn't start it: {e}"))) {
                        Ok(()) => (spawned, start_error, next_start) = (Some(Instant::now()), None, Instant::now() + Duration::from_secs(10)),
                        Err(e) => {
                            start_error = Some(format!("{e}. Install herdr or set its path in Settings."));
                            next_start = Instant::now() + Duration::from_secs(60);
                        }
                    }
                }
                let message = match (&start_error, spawned) {
                    _ if outdated.is_some() => outdated.unwrap_or_default(),
                    (Some(e), _) if no_server => e.clone(),
                    (None, Some(at)) if no_server && at.elapsed() < Duration::from_secs(10) => "Starting herdr…".into(),
                    _ => format!("{err:#}"),
                };
                let alive = this.update(cx, |s, cx| {
                    s.error = Some(message);
                    cx.notify()
                });
                if alive.is_err() {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
                continue;
            }
        };
        'snapshots: loop {
            let c = client.clone();
            let result = rt.spawn(async move { c.call("session.snapshot", json!({})).await }).await.expect("tokio");
            let update = result.and_then(|r| Snapshot::from_result(&r));
            let alive = this.update(cx, |s, cx| {
                let mut panes = None;
                match update {
                    Ok(snapshot) => {
                        // herdr polls each watched pane for its status: only the panes with an agent are worth it, and
                        // one found later (pane.agent_detected) changes the set
                        let mut ids: Vec<String> = snapshot.panes.iter().filter(|p| p.agent.is_some()).map(|p| p.pane_id.clone()).collect();
                        ids.sort(); // a reorder (a dragged tab) is the same set
                        panes = Some(ids);
                        // a workspace's agent that starts or stops has likely edited its worktree
                        for w in snapshot.workspaces.iter().filter(|w| {
                            s.snapshot.as_ref().is_some_and(|old| old.workspaces.iter().any(|o| o.workspace_id == w.workspace_id && o.agent_status != w.agent_status))
                        }) {
                            if let Some(t) = &w.worktree {
                                crate::ui::statusbar::refresh_changes(&crate::herdr::locate(&t.checkout_path), cx);
                            }
                        }
                        crate::analytics::seen(snapshot.panes.iter().filter_map(|p| p.agent.as_deref()));
                        s.agents_changed(&snapshot, cx);
                        // herdr's focus moved (its TUI, another window, a command): Shepherd shows the same tab. Only a
                        // move counts, so a snapshot taken before your own switch reached herdr does not undo it.
                        let old = s.snapshot.as_ref().map(|o| o.focused_tab_id.as_deref());
                        // A move to the tab already shown, or to one Shepherd itself switched herdr to lately (A, B, A
                        // clicked quickly: B's echo lands after), is Shepherd's own switch coming back: a file tab shown
                        // since stays.
                        let moved = moved_focus(old, snapshot.focused_tab_id.as_deref());
                        if let Some(tab) = moved.filter(|t| s.selected_tab.as_deref() != Some(*t) && !own_echo(&mut s.own_focus, t, Instant::now())) {
                            // herdr's own focus move leaves a web page shown where it is
                            let page = s.pages.shown.clone();
                            s.show_herdr_tab(Some(tab.into()), true, cx);
                            s.pages.shown = page;
                        }
                        snapshot.track(&mut s.since, Instant::now());
                        s.snapshot = Some(snapshot);
                        s.after_snapshot(cx);
                        s.prune_roots(cx);
                        s.prune_file_tabs(cx);
                        s.prune_kept_views(cx);
                        s.error = None;
                        s.connecting = None;
                    }
                    Err(err) => s.error = Some(format!("{err:#}")),
                }
                cx.notify();
                panes
            });
            let Ok(panes) = alive else {
                return; // window closed
            };
            if let Some(panes) = panes.filter(|p| *p != watched) {
                watched = panes;
                break; // agents came or went: follow the new set
            }
            // ponytail: the whole snapshot per event burst, but for a burst that changes nothing it holds (Claude Code's
            // title spinner); apply pane_updated's pane in place if the remaining snapshots show up in profiles.
            loop {
                let event = tokio::select! {
                    event = rx.recv() => event,
                    _ = resync.notified() => Some(serde_json::Value::Null),
                };
                let Some(event) = event else {
                    break 'snapshots; // server gone or events_lost: resubscribe
                };
                cx.background_executor().timer(Duration::from_millis(30)).await;
                let mut burst = vec![event];
                while let Ok(event) = rx.try_recv() {
                    burst.push(event);
                }
                // ponytail: herdr's events do not name the client; a focus change Shepherd did not just cause is
                // another client's (its TUI in a terminal, over ssh), and the client used last sizes the panes
                let focused = burst.iter().any(|e| crate::herdr::event_is(e, "pane.focused"));
                let held = this.update(cx, |s, cx| {
                    if focused && s.touched.elapsed() > Duration::from_secs(2) {
                        cx.set_global(crate::terminal::pane::Yielded(true));
                    }
                    s.snapshot.as_ref().is_some_and(|snap| burst.iter().all(|e| snap.holds(e)))
                });
                if !held.unwrap_or(false) {
                    break;
                }
            }
        }
    }
}

/// Subscribes, following the agents of `watched`. A failure lets go of them: herdr turns down a subscription that
/// names a pane closed since (`pane_not_found`), so asking with the same ones would fail forever and the window
/// would never sync again; the next try asks with none, and the snapshot after it names the panes to follow.
async fn subscribe(rt: &tokio::runtime::Handle, client: &Client, watched: &mut Vec<String>) -> anyhow::Result<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>> {
    let (c, panes) = (client.clone(), watched.clone());
    let sub = rt.spawn(async move { c.subscribe(EVENT_TYPES, &panes).await }).await.expect("tokio");
    if sub.is_err() {
        watched.clear();
    }
    sub
}

/// Asks the server which protocol it speaks, and returns the complaint when it is not Shepherd's. A change of
/// verdict is told once, as a toast; the verdict stays in [`crate::herdr::OutdatedServer`] for the panes whose
/// connection it breaks. No answer to the ping changes nothing.
async fn check_protocol(this: &gpui::WeakEntity<Shell>, rt: &tokio::runtime::Handle, client: &Client, cx: &mut gpui::AsyncApp) -> Option<String> {
    let c = client.clone();
    let Ok(Ok(pong)) = rt.spawn(async move { c.call("ping", json!({})).await }).await else { return None };
    let mismatch = crate::herdr::protocol_mismatch(&pong);
    let told = mismatch.clone();
    let _ = this.update(cx, |_, cx| {
        let before = cx.try_global::<crate::herdr::OutdatedServer>().and_then(|o| o.0.clone());
        if before == mismatch {
            return;
        }
        if let Some(why) = &mismatch {
            crate::ui::toast::error(cx, why.clone());
        }
        cx.set_global(crate::herdr::OutdatedServer(mismatch));
        cx.refresh_windows();
    });
    told
}

impl Shell {
    /// The other way: the tab you show gets herdr's focus, through the public API, which moves every client shell's
    /// tab with it (herdr's TUI, this window's shell, whose Kitty images are its own tab's only).
    pub(super) fn follow_shown_tab(&mut self) {
        let Some(tab) = self.shown_tab().map(|t| t.tab_id) else { return };
        if self.focused_tab.as_ref() == Some(&tab) {
            return;
        }
        if self.snapshot.as_ref().and_then(|s| s.focused_tab_id.as_ref()) != Some(&tab) {
            let (client, id) = (self.client.clone(), tab.clone());
            self.rt.spawn(async move { client.call("tab.focus", json!({"tab_id": id})).await });
            own_echo(&mut self.own_focus, "", Instant::now());
            self.own_focus.push((tab.clone(), Instant::now()));
        }
        self.focused_tab = Some(tab);
    }
}

/// How long a `tab.focus` Shepherd sent may take to come back in a snapshot: a move of herdr's focus to that tab
/// within it is Shepherd's own.
const ECHO: Duration = Duration::from_secs(3);

/// `tab` is one Shepherd gave herdr's focus within `ECHO` of `now` (`recent`, oldest first, is pruned of older ones).
fn own_echo(recent: &mut Vec<(String, Instant)>, tab: &str, now: Instant) -> bool {
    recent.retain(|(_, at)| now.duration_since(*at) < ECHO);
    recent.iter().any(|(t, _)| t == tab)
}

/// herdr's focused tab when its focus moved since the last snapshot (`old`: none yet, or its focused tab).
fn moved_focus<'a>(old: Option<Option<&str>>, new: Option<&'a str>) -> Option<&'a str> {
    new.filter(|t| old.is_some_and(|o| o != Some(*t)))
}

#[cfg(test)]
mod tests {
    use super::{ECHO, moved_focus, own_echo};
    use std::time::Instant;

    /// A fake herdr whose pane `w1:p2` has closed: a subscription that names it is turned down, as herdr 0.9.5 does.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_subscription_naming_a_closed_pane_is_tried_again_without_it() {
        use serde_json::{Value, json};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("herdr.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut lines = BufReader::new(r).lines();
                    let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                    let closed = req["params"]["subscriptions"].as_array().unwrap().iter().any(|s| s["pane_id"] == "w1:p2");
                    let answer = match closed {
                        true => json!({"id": req["id"], "error": {"code": "pane_not_found", "message": "pane w1:p2 not found"}}),
                        false => json!({"id": req["id"], "result": {"type": "subscription_started"}}),
                    };
                    w.write_all(format!("{answer}\n").as_bytes()).await.unwrap();
                    let _ = lines.next_line().await; // held open until the client hangs up
                });
            }
        });
        let (rt, client) = (tokio::runtime::Handle::current(), crate::herdr::Client::at(path));
        let mut watched = vec!["w1:p1".to_string(), "w1:p2".to_string()];
        assert!(super::subscribe(&rt, &client, &mut watched).await.is_err());
        assert!(watched.is_empty(), "the closed pane is not asked for again");
        assert!(super::subscribe(&rt, &client, &mut watched).await.is_ok());
    }

    #[test]
    fn shepherds_own_switches_echo_for_a_while() {
        let t0 = Instant::now();
        let mut recent = vec![("w1:t2".to_string(), t0), ("w1:t1".to_string(), t0)];
        assert!(own_echo(&mut recent, "w1:t2", t0 + ECHO / 2), "B's echo after A, B, A");
        assert!(!own_echo(&mut recent, "w1:t3", t0), "another client's move");
        assert!(!own_echo(&mut recent, "w1:t2", t0 + ECHO), "too late to be ours");
        assert!(recent.is_empty());
    }

    #[test]
    fn follows_herdrs_focus_only_when_it_moves() {
        assert_eq!(moved_focus(Some(Some("w1:t1")), Some("w1:t2")), Some("w1:t2"));
        // unchanged: Shepherd's own choice stands (a snapshot from before its switch reached herdr)
        assert_eq!(moved_focus(Some(Some("w1:t1")), Some("w1:t1")), None);
        // the first snapshot: Shepherd's start-up tab stands
        assert_eq!(moved_focus(None, Some("w1:t1")), None);
        assert_eq!(moved_focus(Some(None), Some("w1:t1")), Some("w1:t1"));
        assert_eq!(moved_focus(Some(Some("w1:t1")), None), None);
    }
}
