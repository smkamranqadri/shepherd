//! Agents that need you, in every session: which status changes alert (`alerts`), which of those are posted
//! (`Attention`: an exact repeat never, the same status again only after a while unless the agent worked in
//! between), what the notification says, where a click on it or its Approve goes (`route`), and the Dock count of
//! every agent waiting anywhere (`badge`).
use super::sidebar_tree::Goal;
use super::watchers::{Kind, SessionKey};
use super::*;
use crate::model::Pane;
use crate::notify::Tag;

/// An agent across sessions: pane ids repeat from one session to the next.
pub(super) type AgentKey = (SessionKey, String);

/// The same status from the same agent alerts again only this long after it last did, unless it worked in between.
pub(super) const COOLDOWN: Duration = Duration::from_secs(10);

/// What was told of an agent is let go this long after, so agents long gone are not kept.
const FORGET_AFTER: Duration = Duration::from_secs(10 * 60);

/// An agent's change worth a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Alert {
    pub pane: String,
    /// It came out of work (or finished a turn) since the snapshot before: a new prompt, never a flap.
    pub worked: bool,
}

/// The agents of `new` that just started waiting or finished. Blocked and done alert as they start, or come back
/// (blocked → working → blocked between two snapshots shows in herdr's growing `state_change_seq`). Idle alerts only
/// when its `completion_seq` grew: herdr reports a finish as idle, not done, when a client shows the tab, while an
/// Esc or a blip at the start turns an agent idle with no finish, and done → idle is it being seen. A pane `old` did
/// not have is no alert, so a reconnect or a new pane never sets off a burst; a pane with no agent never alerts.
/// Nor does a pane whose id now names another terminal: herdr gave a closed pane's id to a new one, whose status
/// and counters are its own, not news of the old agent.
pub(super) fn alerts<'a>(old: &'a Snapshot, new: &'a Snapshot) -> impl Iterator<Item = (&'a Pane, bool)> {
    new.panes.iter().filter(|p| p.agent.is_some()).filter_map(|p| {
        let o = old.panes.iter().find(|o| o.pane_id == p.pane_id && o.terminal_id == p.terminal_id)?;
        let finished = p.completion_seq > o.completion_seq;
        let again = o.agent_status == p.agent_status && p.state_change_seq > o.state_change_seq;
        let alert = match p.agent_status {
            s if crate::notify::wants_attention(s) => o.agent_status != s || again,
            AgentStatus::Idle => finished,
            _ => false,
        };
        alert.then_some((p, o.agent_status == AgentStatus::Working || finished || again))
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
    /// however many snapshots bring it. The same status again waits out [`COOLDOWN`], so an agent flapping between
    /// two states does not alert on every flip; one that `worked` in between (a second prompt right after the first
    /// was answered) is told at once.
    pub(super) fn tell(&mut self, key: AgentKey, status: AgentStatus, seq: u64, worked: bool, now: Instant) -> bool {
        self.told.retain(|_, t| now.duration_since(t.at) < FORGET_AFTER);
        if let Some(t) = self.told.get(&key)
            && t.status == status
            // an herdr that sends no seq sends 0: then only the cooldown holds repeats back
            && ((seq != 0 && t.seq == seq) || (!worked && now.duration_since(t.at) < COOLDOWN))
        {
            return false;
        }
        self.told.insert(key, Told { status, seq, at: now });
        true
    }

    /// Whether to post the alert, and with an Approve button: never while notifications are off (nothing is
    /// remembered then), nor for a change told already; Approve only for a blocked agent of a session named by its
    /// key, so a press can never land in whatever session is shown by then.
    pub(super) fn post(&mut self, enabled: bool, key: Option<&SessionKey>, pane: &Pane, worked: bool, now: Instant) -> Option<bool> {
        let told = enabled && self.tell((key.cloned().unwrap_or_default(), pane.pane_id.clone()), pane.agent_status, pane.state_change_seq, worked, now);
        told.then_some(key.is_some() && pane.agent_status == AgentStatus::Blocked)
    }

    /// Lets go of the sessions no longer listed.
    pub(super) fn keep_sessions(&mut self, keep: impl Fn(&SessionKey) -> bool) {
        self.told.retain(|(k, _), _| keep(k));
    }
}

/// The Dock count: every agent waiting or finished, in every session's snapshot.
pub(super) fn badge<'a>(snapshots: impl IntoIterator<Item = &'a Snapshot>) -> usize {
    snapshots.into_iter().map(sidebar_tree::waiting).sum()
}

/// A notification's title and body: "claude · api — menqal" (the session always named, the shown one too, so two
/// sessions' notifications read apart; an unnamed one, with no name to give, goes without), and what the agent does,
/// with its terminal's title.
pub(super) fn message(pane: &Pane, snapshot: &Snapshot, session: &str) -> (String, String) {
    let agent = pane.agent.as_deref().unwrap_or("Agent");
    let workspace = snapshot.workspaces.iter().find(|w| w.workspace_id == pane.workspace_id).map(|w| w.label.as_str());
    let mut title = workspace.map_or_else(|| agent.to_string(), |w| format!("{agent} · {w}"));
    if !session.is_empty() {
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

/// The notifications for the session shown, named `session`: the agents of `next` that started waiting or finished
/// since `old`, but for those in the tab `in_view`, each with the alert's `worked` and its title and body.
pub(super) fn told_here(old: &Snapshot, next: &Snapshot, in_view: Option<&str>, session: &str) -> Vec<(Pane, bool, (String, String))> {
    alerts(old, next)
        .filter(|(p, _)| in_view != Some(p.tab_id.as_str()))
        .map(|(p, worked)| (p.clone(), worked, message(p, next, session)))
        .collect()
}

/// The pane a notification names, and the agent's state (herdr's seq, the pane's terminal) as it was posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub pane: String,
    pub state: Option<(u64, String)>,
}

/// Where a notification's click or Approve goes.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// To a pane of the session shown.
    Shown(Target),
    /// To a pane of another session.
    Other(SessionKey, Target),
    /// A tag that names no session (from before keys were in it): a click looks in the session shown, an Approve
    /// does nothing, as it could land in another session than the one posted.
    Unkeyed(Target),
    /// Its session is no longer there.
    Gone,
}

/// The notification's session and pane, by its tag; `known` is whether a session is in the tree.
pub(super) fn route(tag: &str, shown: Option<&SessionKey>, known: impl Fn(&SessionKey) -> bool) -> Route {
    let tag = Tag::parse(tag);
    let target = Target { pane: tag.pane.into(), state: tag.state.map(|(seq, terminal)| (seq, terminal.into())) };
    match tag.key.map(|k| SessionKey(k.into())) {
        None => Route::Unkeyed(target),
        Some(k) if shown == Some(&k) => Route::Shown(target),
        Some(k) if known(&k) => Route::Other(k, target),
        Some(_) => Route::Gone,
    }
}

/// The prompt the notification showed still waits: the pane's agent is blocked, at the seq it was posted at, in the
/// same terminal. An answer in herdr's TUI and a new prompt since, or a restart that reused the pane id, fail it,
/// so a late Approve never types Enter into a prompt you have not seen.
pub(super) fn approvable(snapshot: &Snapshot, target: &Target) -> bool {
    let Some((seq, terminal)) = &target.state else { return false };
    snapshot.panes.iter().any(|p| {
        p.pane_id == target.pane && p.agent_status == AgentStatus::Blocked && p.state_change_seq == *seq && &p.terminal_id == terminal
    })
}

/// What a click on another session's notification shows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Open {
    /// The agent's tab, its pane focused.
    Pane(String),
    /// The session, as it was left: none of its snapshot came yet, or the notification names no agent (a CI run's).
    Session,
    /// Its snapshot no longer lists the agent.
    AgentGone,
}

pub(super) fn open(snapshot: Option<&Snapshot>, pane: &str) -> Open {
    match snapshot {
        Some(_) if pane.is_empty() => Open::Session,
        Some(s) => s.panes.iter().find(|p| p.pane_id == pane).map_or(Open::AgentGone, |p| Open::Pane(p.tab_id.clone())),
        None => Open::Session,
    }
}

impl Shell {
    /// Posts an alert for the agent of session `key` (none only before the session shown is known), unless it was
    /// told already or notifications are off.
    pub(super) fn tell(&mut self, key: Option<&SessionKey>, pane: &Pane, worked: bool, (title, body): (String, String), cx: &mut Context<Self>) {
        let settings = &SettingsGlobal::of(cx).settings;
        let (notify, sound) = (settings.notify_agents, settings.notify_sound);
        let Some(approve) = self.attention.post(notify, key, pane, worked, Instant::now()) else { return };
        let tag = Tag { key: key.map(|k| k.0.as_str()), pane: &pane.pane_id, state: Some((pane.state_change_seq, &pane.terminal_id)) };
        crate::notify::show(cx, &tag, title, body, sound, approve);
    }

    /// The alerts a watcher's answer brought: told as from a session not shown.
    pub(super) fn tell_elsewhere(&mut self, key: &SessionKey, alerts: &[Alert], cx: &mut Context<Self>) {
        let Some(snapshot) = self.watchers.snapshot(key) else { return };
        let session = self.places.iter().find(|p| SessionKey::of(p) == *key).map(|p| p.look(cx).0).unwrap_or_default();
        let told: Vec<_> = alerts
            .iter()
            .filter_map(|a| Some((snapshot.panes.iter().find(|p| p.pane_id == a.pane)?, a.worked)))
            .map(|(p, worked)| (p.clone(), worked, message(p, snapshot, &session)))
            .collect();
        for (pane, worked, message) in told {
            self.tell(Some(key), &pane, worked, message, cx);
        }
    }

    /// A click on another session's notification: that session is shown, its agent's tab up and the pane focused.
    pub(super) fn open_elsewhere(&mut self, key: SessionKey, target: Target, cx: &mut Context<Self>) {
        match open(self.watchers.snapshot(&key), &target.pane) {
            Open::Pane(tab) => self.go_to(key, Goal::Pane { tab, pane: target.pane }, cx),
            Open::AgentGone => crate::ui::toast::error(cx, "That agent is no longer there"),
            Open::Session => {
                if let Some(place) = self.places.iter().find(|p| SessionKey::of(p) == key).cloned() {
                    self.show_place(place, true, cx);
                }
            }
        }
    }

    /// Approve on another session's notification: Enter in its pane, through that session's own socket, once a fresh
    /// snapshot says the prompt shown still waits. Shepherd stays where it is.
    pub(super) fn approve_elsewhere(&mut self, key: SessionKey, target: Target, cx: &mut Context<Self>) {
        let Some(w) = self.watchers.get(&key) else { return };
        let name = w.place.look(cx).0;
        let Some(client) = w.client.clone() else {
            return crate::ui::toast::error(cx, format!("Couldn't approve in {name}: not connected to it yet"));
        };
        let timeout = if w.kind() == Kind::Remote { Duration::from_secs(10) } else { Duration::from_secs(2) };
        let call = self.rt.spawn(async move {
            let ask = async {
                let snapshot = Snapshot::from_result(&client.call("session.snapshot", json!({})).await?)?;
                if !approvable(&snapshot, &target) {
                    return Ok(false);
                }
                let (method, params) = Command::Approve { pane_id: target.pane }.request();
                client.call(method, params).await.map(|_| true)
            };
            match tokio::time::timeout(timeout, ask).await {
                Ok(done) => done,
                Err(_) => Err(anyhow::anyhow!("no answer in {} s", timeout.as_secs())),
            }
        });
        cx.spawn(async move |_, cx| match call.await.map_err(anyhow::Error::from).and_then(|r| r) {
            Ok(true) => {}
            Ok(false) => cx.update(|cx| crate::ui::toast::error(cx, format!("That prompt in {name} has changed"))),
            Err(e) => cx.update(|cx| crate::ui::toast::error(cx, format!("Couldn't approve in {name}: {e:#}"))),
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Panes as (id, status, seq, completion seq), each a claude agent of workspace w1 ("api") in terminal
    /// `term-<id>`; an id starting `sh` is a plain shell, no agent.
    fn snapshot(panes: &[(&str, &str, u64, Option<u64>)]) -> Snapshot {
        let agents: Vec<_> = panes.iter().map(|(id, _, seq, done)| json!({"pane_id": id, "state_change_seq": seq, "completion_seq": done})).collect();
        let panes: Vec<_> = panes
            .iter()
            .map(|(id, status, _, _)| {
                let agent = (!id.starts_with("sh")).then_some("claude");
                json!({"pane_id": id, "tab_id": format!("t-{id}"), "workspace_id": "w1", "terminal_id": format!("term-{id}"), "agent_status": status, "agent": agent})
            })
            .collect();
        let ws = json!([{"workspace_id": "w1", "label": "api", "number": 1, "active_tab_id": null, "agent_status": "idle", "worktree": null}]);
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": ws, "tabs": [], "panes": panes, "agents": agents, "layouts": []}})).unwrap()
    }

    fn ids(old: &Snapshot, new: &Snapshot) -> Vec<String> {
        alerts(old, new).map(|(p, _)| p.pane_id.clone()).collect()
    }

    fn worked(old: &Snapshot, new: &Snapshot) -> Vec<bool> {
        alerts(old, new).map(|(_, w)| w).collect()
    }

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    #[test]
    fn completion_seq_is_read_from_the_agents() {
        let s = snapshot(&[("p1", "idle", 852, Some(852)), ("p2", "working", 898, None)]);
        assert_eq!((s.panes[0].completion_seq, s.panes[1].completion_seq), (Some(852), None));
    }

    #[test]
    fn waiting_and_finishing_alert_and_staying_put_does_not() {
        let old = snapshot(&[("p1", "working", 1, None), ("p2", "working", 1, None), ("p3", "idle", 1, None)]);
        let new = snapshot(&[("p1", "blocked", 2, None), ("p2", "done", 2, Some(2)), ("p3", "working", 2, None)]);
        assert_eq!(ids(&old, &new), ["p1", "p2"]);
        assert_eq!(worked(&old, &new), [true, true], "both came out of work");
        assert!(ids(&new, &new).is_empty(), "a status that stays is no news");
    }

    #[test]
    fn answered_then_done_alerts_again() {
        assert_eq!(ids(&snapshot(&[("p1", "blocked", 1, None)]), &snapshot(&[("p1", "done", 3, Some(3))])), ["p1"]);
    }

    #[test]
    fn a_wait_that_came_back_between_snapshots_alerts_by_its_seq() {
        let (old, new) = (snapshot(&[("p1", "blocked", 4, None)]), snapshot(&[("p1", "blocked", 6, None)]));
        assert_eq!((ids(&old, &new), worked(&old, &new)), (vec!["p1".to_string()], vec![true]));
        assert!(ids(&new, &new).is_empty());
    }

    #[test]
    fn idle_is_a_finish_only_when_a_turn_completed() {
        // a finish herdr reports as idle (a client shows the tab), also when the poll missed the work in between
        assert_eq!(ids(&snapshot(&[("p1", "working", 1, Some(1))]), &snapshot(&[("p1", "idle", 2, Some(2))])), ["p1"]);
        assert_eq!(ids(&snapshot(&[("p1", "working", 1, None)]), &snapshot(&[("p1", "idle", 2, Some(2))])), ["p1"]);
        assert_eq!(ids(&snapshot(&[("p1", "done", 2, Some(2))]), &snapshot(&[("p1", "idle", 5, Some(5))])), ["p1"], "done → working → idle");
        assert_eq!(ids(&snapshot(&[("p1", "blocked", 2, None)]), &snapshot(&[("p1", "idle", 5, Some(5))])), ["p1"], "blocked → working → idle");
        // an Esc, a blip at the start, or a done agent seen: idle without a finish
        assert!(ids(&snapshot(&[("p1", "working", 1, Some(1))]), &snapshot(&[("p1", "idle", 2, Some(1))])).is_empty());
        assert!(ids(&snapshot(&[("p1", "working", 1, None)]), &snapshot(&[("p1", "idle", 2, None)])).is_empty());
        assert!(ids(&snapshot(&[("p1", "done", 2, Some(2))]), &snapshot(&[("p1", "idle", 3, Some(2))])).is_empty());
        assert!(ids(&snapshot(&[("p1", "idle", 2, None)]), &snapshot(&[("p1", "working", 3, None)])).is_empty());
    }

    #[test]
    fn panes_new_to_the_snapshot_or_without_an_agent_do_not_alert() {
        assert!(ids(&snapshot(&[]), &snapshot(&[("p1", "done", 1, Some(1)), ("p2", "idle", 1, Some(1))])).is_empty());
        let old = snapshot(&[("sh1", "working", 1, None), ("sh2", "working", 1, Some(1))]);
        assert!(ids(&old, &snapshot(&[("sh1", "done", 2, Some(2)), ("sh2", "idle", 2, Some(2))])).is_empty());
    }

    #[test]
    fn a_pane_id_given_to_another_terminal_does_not_alert() {
        let old = snapshot(&[("p1", "working", 1, None), ("p2", "working", 1, None)]);
        let mut new = snapshot(&[("p1", "done", 2, Some(2)), ("p2", "blocked", 2, None)]);
        new.panes[0].terminal_id = "term-new".into();
        assert_eq!(ids(&old, &new), ["p2"], "p1 is another agent now");
    }

    #[test]
    fn one_change_is_told_once_however_late_it_comes_again() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        let p1 = (key("menqal"), "w1:p1".to_string());
        assert!(a.tell(p1.clone(), AgentStatus::Blocked, 7, true, t0));
        assert!(!a.tell(p1.clone(), AgentStatus::Blocked, 7, true, t0 + COOLDOWN * 3), "the same seq is the same change");
        assert!(a.tell(p1, AgentStatus::Blocked, 8, false, t0 + COOLDOWN * 3), "a new one after the cooldown");
    }

    #[test]
    fn the_same_status_flapping_waits_out_the_cooldown() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        let p1 = (key("menqal"), "w1:p1".to_string());
        assert!(a.tell(p1.clone(), AgentStatus::Done, 1, true, t0));
        assert!(!a.tell(p1.clone(), AgentStatus::Done, 3, false, t0 + COOLDOWN - Duration::from_millis(1)), "no work in between");
        assert!(a.tell(p1.clone(), AgentStatus::Blocked, 4, false, t0 + Duration::from_secs(1)), "another status is news at once");
        assert!(a.tell(p1.clone(), AgentStatus::Blocked, 6, false, t0 + Duration::from_secs(1) + COOLDOWN), "again once it is up");
        // an herdr that sends no seq: the cooldown alone holds repeats back
        let p2 = (key("menqal"), "w1:p2".to_string());
        assert!(a.tell(p2.clone(), AgentStatus::Done, 0, true, t0));
        assert!(!a.tell(p2.clone(), AgentStatus::Done, 0, false, t0 + Duration::from_secs(1)));
        assert!(a.tell(p2, AgentStatus::Done, 0, false, t0 + COOLDOWN));
    }

    #[test]
    fn back_to_back_prompts_are_both_told() {
        // blocked, answered (working), blocked again 3 s on: a second permission prompt, not a flap
        let (mut a, t0) = (Attention::default(), Instant::now());
        let p1 = (key("menqal"), "w1:p1".to_string());
        let first = snapshot(&[("p1", "blocked", 5, None)]);
        let answered = snapshot(&[("p1", "working", 6, None)]);
        let second = snapshot(&[("p1", "blocked", 7, None)]);
        assert!(ids(&first, &answered).is_empty());
        assert_eq!(worked(&answered, &second), [true]);
        assert!(a.tell(p1.clone(), AgentStatus::Blocked, 5, true, t0));
        assert!(a.tell(p1.clone(), AgentStatus::Blocked, 7, true, t0 + Duration::from_secs(3)));
        // the poll missed the work in between: the seq grew on the same status, still a new prompt
        assert_eq!(worked(&second, &snapshot(&[("p1", "blocked", 9, None)])), [true]);
        assert!(a.tell(p1, AgentStatus::Blocked, 9, true, t0 + Duration::from_secs(5)));
    }

    #[test]
    fn the_same_pane_id_in_two_sessions_is_two_agents() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        assert!(a.tell((key("default"), "w1:p1".into()), AgentStatus::Blocked, 3, true, t0));
        assert!(a.tell((key("menqal"), "w1:p1".into()), AgentStatus::Blocked, 3, true, t0));
    }

    #[test]
    fn a_session_let_go_and_old_alerts_are_forgotten() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        let p1 = (key("menqal"), "w1:p1".to_string());
        a.tell(p1.clone(), AgentStatus::Blocked, 3, true, t0);
        a.tell((key("default"), "w1:p1".into()), AgentStatus::Blocked, 3, true, t0);
        a.keep_sessions(|k| *k == key("default"));
        assert_eq!(a.told.len(), 1);
        assert!(a.tell(p1, AgentStatus::Blocked, 3, true, t0), "menqal's was let go");
        a.tell((key("x"), "w1:p1".into()), AgentStatus::Done, 1, true, t0 + FORGET_AFTER);
        assert_eq!(a.told.len(), 1, "only the one told just now is kept");
    }

    #[test]
    fn nothing_posts_with_notifications_off_and_approve_needs_a_known_session() {
        let (mut a, t0) = (Attention::default(), Instant::now());
        let s = snapshot(&[("p1", "blocked", 3, None), ("p2", "done", 3, Some(3))]);
        assert_eq!(a.post(false, Some(&key("menqal")), &s.panes[0], true, t0), None, "notifications off");
        assert!(a.told.is_empty(), "nothing is remembered while they are off");
        assert_eq!(a.post(true, Some(&key("menqal")), &s.panes[0], true, t0), Some(true));
        assert_eq!(a.post(true, Some(&key("menqal")), &s.panes[0], true, t0), None, "told already");
        assert_eq!(a.post(true, Some(&key("menqal")), &s.panes[1], true, t0), Some(false), "a finish has nothing to approve");
        assert_eq!(a.post(true, None, &s.panes[0], true, t0), Some(false), "no key: no Approve");
    }

    #[test]
    fn the_badge_counts_every_waiting_agent_in_every_session() {
        let shown = snapshot(&[("p1", "blocked", 1, None), ("p2", "working", 1, None), ("p3", "idle", 1, None)]);
        let other = snapshot(&[("p1", "done", 1, Some(1)), ("p2", "blocked", 1, None)]);
        assert_eq!(badge([&shown, &other]), 3);
        assert_eq!(badge([&other]), 2);
        assert_eq!(badge([]), 0);
    }

    #[test]
    fn the_title_names_the_session_always_the_shown_one_too() {
        let s = snapshot(&[("p1", "blocked", 1, None), ("p2", "idle", 2, Some(2))]);
        assert_eq!(message(&s.panes[0], &s, "default"), ("claude · api — default".into(), "Waiting for you".into()), "the session shown");
        assert_eq!(message(&s.panes[1], &s, "menqal"), ("claude · api — menqal".into(), "Done".into()), "another session");
        assert_eq!(message(&s.panes[0], &s, "").0, "claude · api", "no name to give");
    }

    #[test]
    fn the_session_shown_tells_its_name_and_not_the_tab_in_view() {
        let old = snapshot(&[("p1", "working", 1, None), ("p2", "working", 1, None)]);
        let new = snapshot(&[("p1", "blocked", 2, None), ("p2", "idle", 2, Some(2))]);
        let titles = |in_view: Option<&str>| told_here(&old, &new, in_view, "default").into_iter().map(|(p, _, (title, _))| (p.pane_id, title)).collect::<Vec<_>>();
        assert_eq!(titles(None), [("p1".to_string(), "claude · api — default".to_string()), ("p2".to_string(), "claude · api — default".to_string())]);
        assert_eq!(titles(Some("t-p1")), [("p2".to_string(), "claude · api — default".to_string())], "the agent in view is not told");
    }

    #[test]
    fn a_notification_routes_by_its_session() {
        let (shown, other) = (key("default"), key("menqal"));
        let known = |k: &SessionKey| *k == shown || *k == other;
        let tag = |k: &SessionKey| Tag { key: Some(&k.0), pane: "w1:p1", state: Some((5, "term-p1")) }.format(9);
        let target = Target { pane: "w1:p1".into(), state: Some((5, "term-p1".into())) };
        assert_eq!(route(&tag(&shown), Some(&shown), known), Route::Shown(target.clone()));
        assert_eq!(route(&tag(&other), Some(&shown), known), Route::Other(other.clone(), target.clone()));
        assert_eq!(route(&tag(&key("gone")), Some(&shown), known), Route::Gone);
        let machine = SessionKey("machine:box".into());
        assert_eq!(route(&tag(&machine), None, |k| *k == machine), Route::Other(machine.clone(), target));
    }

    #[test]
    fn a_tag_without_a_session_never_routes_into_one() {
        let shown = key("default");
        let unkeyed = Route::Unkeyed(Target { pane: "w1:p1".into(), state: None });
        assert_eq!(route("w1:p1#5", Some(&shown), |_| true), unkeyed, "a tag from before keys");
        let bare = Tag { key: None, pane: "w1:p1", state: Some((5, "term-p1")) }.format(9);
        assert_eq!(route(&bare, Some(&shown), |_| true), Route::Unkeyed(Target { pane: "w1:p1".into(), state: Some((5, "term-p1".into())) }));
    }

    #[test]
    fn approve_needs_the_same_prompt_still_waiting() {
        let s = snapshot(&[("p1", "blocked", 5, None), ("p2", "working", 5, None)]);
        let t = |pane: &str, seq: u64, terminal: &str| Target { pane: pane.into(), state: Some((seq, terminal.into())) };
        assert!(approvable(&s, &t("p1", 5, "term-p1")));
        assert!(!approvable(&s, &t("p1", 4, "term-p1")), "answered and blocked anew since");
        assert!(!approvable(&s, &t("p1", 5, "term-old")), "a restart reused the pane id");
        assert!(!approvable(&s, &t("p2", 5, "term-p2")), "at work");
        assert!(!approvable(&s, &t("p9", 5, "term-p9")), "gone");
        assert!(!approvable(&s, &Target { pane: "p1".into(), state: None }), "a tag without the state approves nothing");
    }

    #[test]
    fn a_click_elsewhere_shows_the_pane_else_the_session() {
        let s = snapshot(&[("p1", "blocked", 5, None)]);
        assert_eq!(open(Some(&s), "p1"), Open::Pane("t-p1".into()));
        assert_eq!(open(Some(&s), "p9"), Open::AgentGone);
        assert_eq!(open(None, "p1"), Open::Session, "connecting, or stopped: no snapshot yet");
        assert_eq!(open(Some(&s), ""), Open::Session, "a CI run's notification names no agent");
    }
}
