//! Agents that need you, in every session: which status changes alert (`alerts`), which of those are told
//! (`Attention`: an exact repeat never, the same status again only after a while), what the notification says, and
//! where a click on it or its Approve goes (`route`).
use super::sidebar_tree::Goal;
use super::watchers::{Kind, SessionKey};
use super::*;
use crate::model::Pane;

/// An agent across sessions: pane ids repeat from one session to the next.
pub(super) type AgentKey = (SessionKey, String);

/// The same status from the same agent alerts again only this long after it last did.
pub(super) const COOLDOWN: Duration = Duration::from_secs(10);

/// What was told of an agent is let go this long after, so agents long gone are not kept.
const FORGET_AFTER: Duration = Duration::from_secs(10 * 60);

/// Panes of `new` whose agent just started waiting, finished, or went from working to idle: herdr reports a finish
/// as idle, not done, when a client shows the tab, so that is a finish too. Done → idle is the agent being seen, no
/// news. A pane `old` did not have is no alert, so a reconnect or a new pane never sets off a burst. A wait that came
/// back between two snapshots (blocked → working → blocked) shows in herdr's growing `state_change_seq`.
pub(super) fn alerts<'a>(old: &'a Snapshot, new: &'a Snapshot) -> impl Iterator<Item = &'a Pane> {
    new.panes.iter().filter(|p| {
        let Some(o) = old.panes.iter().find(|o| o.pane_id == p.pane_id) else { return false };
        match p.agent_status {
            s if crate::notify::wants_attention(s) => o.agent_status != s || p.state_change_seq > o.state_change_seq,
            AgentStatus::Idle => o.agent_status == AgentStatus::Working,
            _ => false,
        }
    })
}

/// The last alert told of an agent.
struct Told {
    status: AgentStatus,
    seq: u64,
    at: Instant,
}

/// What was told of each agent, in every session.
#[derive(Default)]
pub(super) struct Attention {
    told: HashMap<AgentKey, Told>,
}

impl Attention {
    /// Whether an alert is told, remembered when it is. One change of herdr's (its status and `seq`) is told once,
    /// however many snapshots bring it; the same status again only once [`COOLDOWN`] is up since it was last told, so
    /// an agent flipping between working and waiting does not alert on every flip.
    pub(super) fn tell(&mut self, key: AgentKey, status: AgentStatus, seq: u64, now: Instant) -> bool {
        self.told.retain(|_, t| now.duration_since(t.at) < FORGET_AFTER);
        if let Some(t) = self.told.get(&key)
            && t.status == status
            // an herdr that sends no seq sends 0: then only the cooldown holds repeats back
            && ((seq != 0 && t.seq == seq) || now.duration_since(t.at) < COOLDOWN)
        {
            return false;
        }
        self.told.insert(key, Told { status, seq, at: now });
        true
    }

    /// Lets go of the sessions no longer listed.
    pub(super) fn keep_sessions(&mut self, keep: impl Fn(&SessionKey) -> bool) {
        self.told.retain(|(k, _), _| keep(k));
    }
}

/// A notification's title and body: "claude · api — menqal" (the session named when it is not the one shown), and
/// what the agent does, with its terminal's title.
pub(super) fn message(pane: &Pane, snapshot: &Snapshot, session: Option<&str>) -> (String, String) {
    let agent = pane.agent.as_deref().unwrap_or("Agent");
    let workspace = snapshot.workspaces.iter().find(|w| w.workspace_id == pane.workspace_id).map(|w| w.label.as_str());
    let mut title = workspace.map_or_else(|| agent.to_string(), |w| format!("{agent} · {w}"));
    if let Some(session) = session {
        title = format!("{title} — {session}");
    }
    // a finish herdr reports as idle reads as done
    let status = if pane.agent_status == AgentStatus::Blocked { "Waiting for you" } else { "Done" };
    let body = match pane.terminal_title_stripped.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => format!("{status}: {t}"),
        None => status.to_string(),
    };
    (title, body)
}

/// Where a notification's click or Approve goes.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// To this pane of the session shown (a tag with no session is one from before keys were in it).
    Shown(String),
    /// To this pane of another session.
    Other(SessionKey, String),
    /// Its session is no longer there.
    Gone,
}

/// The notification's session and pane, by its tag; `known` is whether a session is in the tree.
pub(super) fn route(tag: &str, shown: Option<&SessionKey>, known: impl Fn(&SessionKey) -> bool) -> Route {
    let (key, pane) = crate::notify::parse_tag(tag);
    match key.map(|k| SessionKey(k.into())) {
        None => Route::Shown(pane.into()),
        Some(k) if shown == Some(&k) => Route::Shown(pane.into()),
        Some(k) if known(&k) => Route::Other(k, pane.into()),
        Some(_) => Route::Gone,
    }
}

/// The pane's agent still waits: a late Approve must not type Enter into an agent at work.
pub(super) fn still_blocked(snapshot: &Snapshot, pane_id: &str) -> bool {
    snapshot.panes.iter().any(|p| p.pane_id == pane_id && p.agent_status == AgentStatus::Blocked)
}

impl Shell {
    /// Posts an alert for the agent of session `key` (none before herdr's list names the session shown), unless it
    /// was told already or notifications are off.
    pub(super) fn tell(&mut self, key: Option<&SessionKey>, pane: &Pane, (title, body): (String, String), cx: &mut Context<Self>) {
        let settings = &SettingsGlobal::of(cx).settings;
        let (notify, sound) = (settings.notify_agents, settings.notify_sound);
        if !notify || !self.attention.tell((key.cloned().unwrap_or_default(), pane.pane_id.clone()), pane.agent_status, pane.state_change_seq, Instant::now()) {
            return;
        }
        let blocked = pane.agent_status == AgentStatus::Blocked;
        crate::notify::show(cx, key.map(|k| k.0.as_str()), &pane.pane_id, title, body, sound, blocked);
    }

    /// The alerts a watcher's answer brought: told as from a session not shown.
    pub(super) fn tell_elsewhere(&mut self, key: &SessionKey, panes: &[String], cx: &mut Context<Self>) {
        let Some(snapshot) = self.watchers.snapshot(key) else { return };
        let session = self.places.iter().find(|p| SessionKey::of(p) == *key).map(|p| p.look(cx).0);
        let told: Vec<_> = panes
            .iter()
            .filter_map(|id| snapshot.panes.iter().find(|p| &p.pane_id == id))
            .map(|p| (p.clone(), message(p, snapshot, session.as_deref())))
            .collect();
        for (pane, message) in told {
            self.tell(Some(key), &pane, message, cx);
        }
    }

    /// A click on another session's notification: that session is shown, its agent's tab up and the pane focused.
    pub(super) fn open_elsewhere(&mut self, key: SessionKey, pane_id: String, cx: &mut Context<Self>) {
        let pane = self.watchers.snapshot(&key).and_then(|s| s.panes.iter().find(|p| p.pane_id == pane_id));
        match pane.map(|p| p.tab_id.clone()) {
            Some(tab) => self.go_to(key, Goal::Pane { tab, pane: pane_id }, cx),
            None => crate::ui::toast::error(cx, "That agent is no longer there"),
        }
    }

    /// Approve on another session's notification: Enter in its pane, through that session's own socket, once a fresh
    /// snapshot says the agent still waits. Shepherd stays where it is.
    pub(super) fn approve_elsewhere(&mut self, key: SessionKey, pane_id: String, cx: &mut Context<Self>) {
        let Some(w) = self.watchers.get(&key) else { return };
        let Some(client) = w.client.clone() else { return };
        let name = w.place.look(cx).0;
        let timeout = if w.kind() == Kind::Remote { Duration::from_secs(10) } else { Duration::from_secs(2) };
        let call = self.rt.spawn(async move {
            let ask = async {
                let snapshot = Snapshot::from_result(&client.call("session.snapshot", json!({})).await?)?;
                if !still_blocked(&snapshot, &pane_id) {
                    return Ok(());
                }
                let (method, params) = Command::Approve { pane_id }.request();
                client.call(method, params).await.map(|_| ())
            };
            match tokio::time::timeout(timeout, ask).await {
                Ok(done) => done,
                Err(_) => Err(anyhow::anyhow!("no answer in {} s", timeout.as_secs())),
            }
        });
        cx.spawn(async move |_, cx| {
            if let Err(e) = call.await.map_err(anyhow::Error::from).and_then(|r| r) {
                cx.update(|cx| crate::ui::toast::error(cx, format!("Couldn't approve in {name}: {e:#}")));
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Panes as (id, status, seq), each a claude agent of workspace w1 ("api").
    fn snapshot(panes: &[(&str, &str, u64)]) -> Snapshot {
        let agents: Vec<_> = panes.iter().map(|(id, _, seq)| json!({"pane_id": id, "state_change_seq": seq})).collect();
        let panes: Vec<_> = panes
            .iter()
            .map(|(id, status, _)| json!({"pane_id": id, "tab_id": format!("t-{id}"), "workspace_id": "w1", "terminal_id": id, "agent_status": status, "agent": "claude"}))
            .collect();
        let ws = json!([{"workspace_id": "w1", "label": "api", "number": 1, "active_tab_id": null, "agent_status": "idle", "worktree": null}]);
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": ws, "tabs": [], "panes": panes, "agents": agents, "layouts": []}})).unwrap()
    }

    fn ids(old: &Snapshot, new: &Snapshot) -> Vec<String> {
        alerts(old, new).map(|p| p.pane_id.clone()).collect()
    }

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    #[test]
    fn waiting_and_finishing_alert_and_staying_put_does_not() {
        let old = snapshot(&[("p1", "working", 1), ("p2", "working", 1), ("p3", "idle", 1)]);
        let new = snapshot(&[("p1", "blocked", 2), ("p2", "done", 2), ("p3", "working", 2)]);
        assert_eq!(ids(&old, &new), ["p1", "p2"]);
        assert!(ids(&new, &new).is_empty(), "a status that stays is no news");
    }

    #[test]
    fn answered_then_done_alerts_again() {
        assert_eq!(ids(&snapshot(&[("p1", "blocked", 1)]), &snapshot(&[("p1", "done", 3)])), ["p1"]);
    }

    #[test]
    fn a_wait_that_came_back_between_snapshots_alerts_by_its_seq() {
        assert_eq!(ids(&snapshot(&[("p1", "blocked", 4)]), &snapshot(&[("p1", "blocked", 6)])), ["p1"]);
        assert!(ids(&snapshot(&[("p1", "blocked", 6)]), &snapshot(&[("p1", "blocked", 6)])).is_empty());
    }

    #[test]
    fn working_to_idle_is_a_finish_and_seen_idle_is_not() {
        assert_eq!(ids(&snapshot(&[("p1", "working", 1)]), &snapshot(&[("p1", "idle", 2)])), ["p1"]);
        // working → done told the finish; done → idle (seen) and blocked → idle add nothing
        assert!(ids(&snapshot(&[("p1", "done", 2)]), &snapshot(&[("p1", "idle", 3)])).is_empty());
        assert!(ids(&snapshot(&[("p1", "blocked", 2)]), &snapshot(&[("p1", "idle", 3)])).is_empty());
        assert!(ids(&snapshot(&[("p1", "idle", 2)]), &snapshot(&[("p1", "idle", 5)])).is_empty());
        assert!(ids(&snapshot(&[("p1", "idle", 2)]), &snapshot(&[("p1", "working", 3)])).is_empty());
    }

    #[test]
    fn panes_new_to_the_snapshot_do_not_alert() {
        assert!(ids(&snapshot(&[]), &snapshot(&[("p1", "done", 1), ("p2", "idle", 1)])).is_empty());
    }

    #[test]
    fn one_change_is_told_once_however_late_it_comes_again() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        let p1 = (key("menqal"), "w1:p1".to_string());
        assert!(a.tell(p1.clone(), AgentStatus::Blocked, 7, t0));
        assert!(!a.tell(p1.clone(), AgentStatus::Blocked, 7, t0 + COOLDOWN * 3), "the same seq is the same change");
        assert!(a.tell(p1, AgentStatus::Blocked, 8, t0 + COOLDOWN * 3), "a new one after the cooldown");
    }

    #[test]
    fn the_same_status_again_waits_out_the_cooldown() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        let p1 = (key("menqal"), "w1:p1".to_string());
        assert!(a.tell(p1.clone(), AgentStatus::Done, 1, t0));
        assert!(!a.tell(p1.clone(), AgentStatus::Done, 3, t0 + COOLDOWN - Duration::from_millis(1)));
        assert!(a.tell(p1.clone(), AgentStatus::Blocked, 4, t0 + Duration::from_secs(1)), "another status is news at once");
        assert!(a.tell(p1.clone(), AgentStatus::Blocked, 6, t0 + Duration::from_secs(1) + COOLDOWN), "again once it is up");
        // an herdr that sends no seq: the cooldown alone holds repeats back
        let p2 = (key("menqal"), "w1:p2".to_string());
        assert!(a.tell(p2.clone(), AgentStatus::Done, 0, t0));
        assert!(!a.tell(p2.clone(), AgentStatus::Done, 0, t0 + Duration::from_secs(1)));
        assert!(a.tell(p2, AgentStatus::Done, 0, t0 + COOLDOWN));
    }

    #[test]
    fn the_same_pane_id_in_two_sessions_is_two_agents() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        assert!(a.tell((key("default"), "w1:p1".into()), AgentStatus::Blocked, 3, t0));
        assert!(a.tell((key("menqal"), "w1:p1".into()), AgentStatus::Blocked, 3, t0));
    }

    #[test]
    fn a_session_let_go_and_old_alerts_are_forgotten() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        let p1 = (key("menqal"), "w1:p1".to_string());
        a.tell(p1.clone(), AgentStatus::Blocked, 3, t0);
        a.tell((key("default"), "w1:p1".into()), AgentStatus::Blocked, 3, t0);
        a.keep_sessions(|k| *k == key("default"));
        assert_eq!(a.told.len(), 1);
        assert!(a.tell(p1, AgentStatus::Blocked, 3, t0), "menqal's was let go");
        a.tell((key("x"), "w1:p1".into()), AgentStatus::Done, 1, t0 + FORGET_AFTER);
        assert_eq!(a.told.len(), 1, "only the one told just now is kept");
    }

    #[test]
    fn the_title_names_the_session_only_when_asked() {
        let s = snapshot(&[("p1", "blocked", 1), ("p2", "idle", 2)]);
        assert_eq!(message(&s.panes[0], &s, None), ("claude · api".into(), "Waiting for you".into()));
        assert_eq!(message(&s.panes[1], &s, Some("menqal")), ("claude · api — menqal".into(), "Done".into()));
    }

    #[test]
    fn a_notification_routes_by_its_session() {
        let (shown, other) = (key("default"), key("menqal"));
        let known = |k: &SessionKey| *k == shown || *k == other;
        let tag = |k: &SessionKey| crate::notify::tag(Some(&k.0), "w1:p1", 5);
        assert_eq!(route(&tag(&shown), Some(&shown), known), Route::Shown("w1:p1".into()));
        assert_eq!(route(&tag(&other), Some(&shown), known), Route::Other(other.clone(), "w1:p1".into()));
        assert_eq!(route(&tag(&key("gone")), Some(&shown), known), Route::Gone);
        assert_eq!(route("w1:p1#5", Some(&shown), known), Route::Shown("w1:p1".into()), "a tag from before keys");
        let machine = SessionKey("machine:box".into());
        assert_eq!(route(&tag(&machine), None, |k| *k == machine), Route::Other(machine.clone(), "w1:p1".into()));
    }

    #[test]
    fn approve_needs_the_agent_still_waiting() {
        let s = snapshot(&[("p1", "blocked", 1), ("p2", "working", 1)]);
        assert!(still_blocked(&s, "p1"));
        assert!(!still_blocked(&s, "p2") && !still_blocked(&s, "p9"));
    }
}
