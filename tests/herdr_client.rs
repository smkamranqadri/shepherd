// ponytail: the fake herdr listens on a unix socket; Windows runs the unit tests only, a named pipe if it ever needs these
#![cfg(unix)]
use serde_json::{Value, json};
use shepherd::herdr::Client;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// A fake herdr: answers each request line with `reply(request)`; returns the socket path.
fn fake_server(dir: &tempfile::TempDir, reply: fn(&Value) -> Vec<Value>) -> std::path::PathBuf {
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let req: Value = serde_json::from_str(&line).unwrap();
                    for out in reply(&req) {
                        w.write_all(format!("{out}\n").as_bytes()).await.unwrap();
                    }
                }
            });
        }
    });
    path
}

#[tokio::test]
async fn call_returns_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_server(&dir, |req| vec![json!({"id": req["id"], "result": {"type": "pong"}})]);
    let result = Client::at(path).call("ping", json!({})).await.unwrap();
    assert_eq!(result, json!({"type": "pong"}));
}

#[tokio::test]
async fn call_error_carries_code_and_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_server(&dir, |req| {
        vec![json!({"id": req["id"], "error": {"code": "pane_not_found", "message": "no pane w9:p9"}})]
    });
    let err = Client::at(path).call("pane.get", json!({"pane_id": "w9:p9"})).await.unwrap_err();
    assert_eq!(err.to_string(), "pane_not_found: no pane w9:p9");
}

#[tokio::test]
async fn missing_socket_names_the_path() {
    let err = Client::at("/tmp/demo/no-such/herdr.sock".into()).call("ping", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("/tmp/demo/no-such/herdr.sock"), "{err}");
}

#[tokio::test]
async fn subscription_yields_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_server(&dir, |req| {
        assert_eq!(req["method"], "events.subscribe");
        // herdr follows an agent's status only per pane
        assert_eq!(req["params"]["subscriptions"][2], json!({"type": "pane.agent_status_changed", "pane_id": "w1:p1"}));
        vec![
            json!({"id": req["id"], "result": {"type": "subscription_started"}}),
            json!({"event": "tab.created", "data": {"type": "tab_created", "tab": {"tab_id": "w1:t2"}}}),
            json!({"event": "pane.closed", "data": {"type": "pane_closed", "pane_id": "w1:p2", "workspace_id": "w1"}}),
        ]
    });
    let mut rx = Client::at(path).subscribe(&["tab.created", "pane.closed"], &["w1:p1".into()]).await.unwrap();
    assert_eq!(rx.recv().await.unwrap()["event"], "tab.created");
    assert_eq!(rx.recv().await.unwrap()["event"], "pane.closed");
}

#[tokio::test]
async fn subscription_ends_when_server_closes() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_server(&dir, |req| {
        vec![
            json!({"id": req["id"], "result": {"type": "subscription_started"}}),
            json!({"id": req["id"], "error": {"code": "events_lost", "message": "fell behind"}}),
        ]
    });
    let mut rx = Client::at(path).subscribe(&["tab.created"], &[]).await.unwrap();
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn dropping_a_subscription_hangs_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r).lines();
        let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        w.write_all(format!("{}\n", json!({"id": req["id"], "result": {}})).as_bytes()).await.unwrap();
        lines.next_line().await.unwrap() // EOF once the client hangs up
    });
    drop(Client::at(path).subscribe(&["tab.created"], &[]).await.unwrap());
    let eof = tokio::time::timeout(std::time::Duration::from_secs(1), server).await.expect("the connection stayed open").unwrap();
    assert_eq!(eof, None);
}

#[tokio::test]
async fn a_missing_or_dead_socket_means_no_server() {
    let dir = tempfile::tempdir().unwrap();
    let missing = Client::at(dir.path().join("herdr.sock")).call("ping", json!({})).await.unwrap_err();
    assert!(shepherd::herdr::no_server(&missing), "{missing:#}");
    // A server that died leaves its socket file behind: connecting is refused.
    let stale = dir.path().join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    let refused = Client::at(stale).call("ping", json!({})).await.unwrap_err();
    assert!(shepherd::herdr::no_server(&refused), "{refused:#}");
    // An answer that is an error is a running server.
    let path = fake_server(&dir, |req| vec![json!({"id": req["id"], "error": {"code": "x", "message": "y"}})]);
    let answered = Client::at(path).call("ping", json!({})).await.unwrap_err();
    assert!(!shepherd::herdr::no_server(&answered));
}

#[tokio::test]
async fn events_that_come_together_are_one_burst() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_server(&dir, |req| {
        let updated = |p: &str| json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": {"pane_id": p}}});
        vec![
            json!({"id": req["id"], "result": {"type": "subscription_started"}}),
            updated("w1:p1"),
            updated("w1:p2"),
            updated("w1:p3"),
            json!({"id": req["id"], "error": {"code": "events_lost", "message": "fell behind"}}),
        ]
    });
    let mut rx = Client::at(path).subscribe(shepherd::herdr::WATCH_TYPES, &[]).await.unwrap();
    let window = || tokio::time::sleep(std::time::Duration::from_millis(50));
    let events = shepherd::herdr::burst(&mut rx, window).await.unwrap();
    let panes: Vec<&str> = events.iter().map(|e| e["data"]["pane"]["pane_id"].as_str().unwrap()).collect();
    assert_eq!(panes, ["w1:p1", "w1:p2", "w1:p3"]);
    assert!(shepherd::herdr::burst(&mut rx, window).await.is_none(), "events_lost ends the feed");
}

#[tokio::test]
async fn a_subscription_herdr_does_not_know_is_unsupported_and_its_server_runs() {
    // herdr 0.9.5's answer to an event type it does not have
    let dir = tempfile::tempdir().unwrap();
    let path = fake_server(&dir, |req| {
        vec![json!({"id": req["id"], "error": {"code": "invalid_request", "message": "invalid request: unknown variant `pane.no_such_event`"}})]
    });
    let err = Client::at(path).subscribe(shepherd::herdr::WATCH_TYPES, &[]).await.unwrap_err();
    assert!(shepherd::herdr::unsupported(&err), "{err:#}");
    assert!(!shepherd::herdr::no_server(&err), "{err:#}");
    assert_eq!(err.to_string(), "invalid_request: invalid request: unknown variant `pane.no_such_event`");
}

#[tokio::test]
async fn a_subscription_to_a_closed_pane_is_not_found_not_unsupported() {
    // herdr 0.9.5's answer when a followed pane closed before the subscription came
    let dir = tempfile::tempdir().unwrap();
    let path = fake_server(&dir, |req| {
        let panes = req["params"]["subscriptions"].as_array().unwrap().iter().filter(|s| s["pane_id"].is_string()).count();
        match panes {
            0 => vec![json!({"id": req["id"], "result": {"type": "subscription_started"}})],
            _ => vec![json!({"id": req["id"], "error": {"code": "pane_not_found", "message": "pane w9:p9 not found"}})],
        }
    });
    let client = Client::at(path);
    let err = client.subscribe(shepherd::herdr::WATCH_TYPES, &["w9:p9".into()]).await.unwrap_err();
    assert_eq!(shepherd::herdr::api_error(&err).map(|e| e.code.as_str()), Some("pane_not_found"));
    assert!(!shepherd::herdr::unsupported(&err) && !shepherd::herdr::no_server(&err), "{err:#}");
    assert!(client.subscribe(shepherd::herdr::WATCH_TYPES, &[]).await.is_ok(), "the same request without the old pane");
}

#[test]
fn events_are_named_either_way() {
    assert!(shepherd::herdr::event_is(&json!({"event": "pane_focused"}), "pane.focused"), "as herdr sends it");
    assert!(shepherd::herdr::event_is(&json!({"event": "pane.focused"}), "pane.focused"));
    assert!(!shepherd::herdr::event_is(&json!({"event": "pane_focused"}), "pane.updated"));
    assert!(!shepherd::herdr::event_is(&json!({"data": {}}), "pane.focused"));
}

#[tokio::test]
async fn a_server_that_hangs_up_is_neither_unsupported_nor_missing() {
    // a machine's relay out of reach: the local socket takes the connection, the SSH behind it ends
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        loop {
            drop(listener.accept().await.unwrap());
        }
    });
    let err = Client::at(path).subscribe(shepherd::herdr::WATCH_TYPES, &[]).await.unwrap_err();
    assert!(!shepherd::herdr::unsupported(&err) && !shepherd::herdr::no_server(&err), "{err:#}");
    let missing = Client::at(dir.path().join("none.sock")).subscribe(shepherd::herdr::WATCH_TYPES, &[]).await.unwrap_err();
    assert!(shepherd::herdr::no_server(&missing) && !shepherd::herdr::unsupported(&missing), "{missing:#}");
}

#[tokio::test]
async fn a_watch_asks_for_its_types_and_each_agents_status() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r).lines();
        let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        w.write_all(format!("{}\n", json!({"id": req["id"], "result": {}})).as_bytes()).await.unwrap();
        req
    });
    let panes = ["w1:p1".to_string(), "w2:p4".to_string()];
    let _rx = Client::at(path).subscribe(shepherd::herdr::WATCH_TYPES, &panes).await.unwrap();
    let req = server.await.unwrap();
    assert_eq!(req["method"], "events.subscribe");
    let mut want: Vec<Value> = shepherd::herdr::WATCH_TYPES.iter().map(|t| json!({"type": t})).collect();
    want.extend(panes.iter().map(|p| json!({"type": "pane.agent_status_changed", "pane_id": p})));
    assert_eq!(req["params"]["subscriptions"], Value::Array(want));
}

#[tokio::test]
async fn twenty_watches_dropped_all_hang_up_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (tx, mut eofs) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let tx = tx.clone();
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut lines = BufReader::new(r).lines();
                let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                w.write_all(format!("{}\n", json!({"id": req["id"], "result": {}})).as_bytes()).await.unwrap();
                if lines.next_line().await.unwrap().is_none() {
                    tx.send(()).unwrap(); // EOF: the client hung up
                }
            });
        }
    });
    let mut feeds = Vec::new();
    for _ in 0..20 {
        feeds.push(Client::at(path.clone()).subscribe(shepherd::herdr::WATCH_TYPES, &[]).await.unwrap());
    }
    drop(feeds);
    let all = async {
        for _ in 0..20 {
            eofs.recv().await.unwrap();
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), all).await.expect("a dropped watch kept its connection");
}
