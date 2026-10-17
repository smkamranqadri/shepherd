//! Next agent that needs you: one key goes to the agent that has waited longest for you, in whichever session, and
//! the next press to the one after, round again at the end. Blocked agents (asking for an answer) come before done
//! ones, each kind the oldest wait first, from the Agents section's own list (`session_agents`), so the key never goes
//! where the section does not show. A session out of reach is left out: going there would only fail.
use super::sidebar_tree::{Goal, SessionAgents, cross_agents};
use super::watchers::{SessionKey, Status};
use super::*;

/// An agent that needs you, where to show it: its session, tab and pane (pane ids repeat across sessions).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Wanted {
    pub key: SessionKey,
    pub tab: String,
    pub pane: String,
}

/// The agents that need you across `sessions`, in the order the key visits them: blocked before done, each the
/// oldest wait first (one whose start Shepherd did not see after), and the sessions in the tree's order within a wait.
/// Sessions `unreachable` says are out of reach are left out.
pub(super) fn queue(sessions: &[SessionAgents], unreachable: impl Fn(&SessionKey) -> bool, now: Instant) -> Vec<Wanted> {
    // Waiting puts the oldest wait first; the stable sort by kind then keeps that order within each kind
    let (mut waiting, _) = cross_agents(sessions, AgentSort::Waiting, true, now);
    waiting.retain(|e| !unreachable(&e.key));
    waiting.sort_by_key(|e| if e.pane.agent_status == AgentStatus::Blocked { 0 } else { 1 });
    waiting.into_iter().map(|e| Wanted { key: e.key, tab: e.pane.tab_id, pane: e.pane.pane_id }).collect()
}

/// Where the key goes in `queue`: the agent after the one you are at (`here`), else after the one it last went to
/// (`last`), round to the first after the end; the first when neither is in the queue. `None` when nothing waits.
pub(super) fn next(queue: &[Wanted], here: Option<(SessionKey, String)>, last: Option<(SessionKey, String)>) -> Option<&Wanted> {
    let at = |a: Option<(SessionKey, String)>| a.and_then(|(key, pane)| queue.iter().position(|w| w.key == key && w.pane == pane));
    match at(here).or_else(|| at(last)) {
        Some(i) => queue.get((i + 1) % queue.len()),
        None => queue.first(),
    }
}

impl Shell {
    /// The NextNeedsYou key: shows the next agent that needs you (switching session when it is in another), or
    /// says none does.
    pub(super) fn next_needs_you(&mut self, cx: &mut Context<Self>) {
        let unreachable = |k: &SessionKey| self.watchers.get(k).is_some_and(|w| w.status == Status::Unreachable);
        let queue = queue(&self.session_agents(cx), unreachable, Instant::now());
        let here = self.shown_key().zip(self.focused.clone());
        let Some(wanted) = next(&queue, here, self.tree.needs_you_last.clone()).cloned() else {
            crate::ui::toast::info(cx, "No agent needs you", None);
            return;
        };
        self.tree.needs_you_last = Some((wanted.key.clone(), wanted.pane.clone()));
        self.go_to(wanted.key, Goal::Pane { tab: wanted.tab, pane: wanted.pane }, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Since;
    use std::time::Duration;

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    fn snapshot(panes: &[(&str, &str)]) -> Snapshot {
        let panes: Vec<_> = panes
            .iter()
            .map(|(id, status)| json!({"pane_id": id, "tab_id": format!("t-{id}"), "workspace_id": "w1", "terminal_id": id, "agent_status": status, "agent": "claude"}))
            .collect();
        let ws = json!([{"workspace_id": "w1", "label": "web", "number": 1, "active_tab_id": null, "agent_status": "idle", "worktree": null}]);
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": ws, "tabs": [], "panes": panes, "layouts": []}})).unwrap()
    }

    /// When each pane began to wait: seconds after `t0`.
    fn since(t0: Instant, waits: &[(&str, AgentStatus, u64)]) -> Since {
        waits.iter().map(|(id, status, s)| (id.to_string(), (*status, t0 + Duration::from_secs(*s)))).collect()
    }

    fn panes(q: &[Wanted]) -> Vec<&str> {
        q.iter().map(|w| w.pane.as_str()).collect()
    }

    fn at(name: &str, pane: &str) -> Option<(SessionKey, String)> {
        Some((key(name), pane.into()))
    }

    /// Two sessions: `alpha` with a done agent of 4 min, a blocked one of 1 min, one working; `beta` with a done one
    /// of 5 min, a blocked one of 3 min, and a blocked one whose wait Shepherd did not see start.
    fn two_sessions(t0: Instant) -> (Snapshot, Since, Snapshot, Since) {
        let a = snapshot(&[("a1", "done"), ("a2", "blocked"), ("a3", "working")]);
        let a_since = since(t0, &[("a1", AgentStatus::Done, 60), ("a2", AgentStatus::Blocked, 240)]);
        let b = snapshot(&[("b1", "done"), ("b2", "blocked"), ("b3", "blocked")]);
        let b_since = since(t0, &[("b1", AgentStatus::Done, 0), ("b2", AgentStatus::Blocked, 120)]);
        (a, a_since, b, b_since)
    }

    #[test]
    fn blocked_come_before_done_each_the_oldest_wait_first_across_sessions() {
        let t0 = Instant::now();
        let (a, a_since, b, b_since) = two_sessions(t0);
        let sessions = [
            SessionAgents { key: key("alpha"), name: "alpha".into(), shown: true, snapshot: &a, since: &a_since },
            SessionAgents { key: key("beta"), name: "beta".into(), shown: false, snapshot: &b, since: &b_since },
        ];
        let q = queue(&sessions, |_| false, t0 + Duration::from_secs(300));
        assert_eq!(panes(&q), ["b2", "a2", "b3", "b1", "a1"], "a wait not seen start comes after the timed ones of its kind");
        assert_eq!(q[0], Wanted { key: key("beta"), tab: "t-b2".into(), pane: "b2".into() });
    }

    #[test]
    fn a_session_out_of_reach_is_skipped() {
        let t0 = Instant::now();
        let (a, a_since, b, b_since) = two_sessions(t0);
        let sessions = [
            SessionAgents { key: key("alpha"), name: "alpha".into(), shown: true, snapshot: &a, since: &a_since },
            SessionAgents { key: key("beta"), name: "beta".into(), shown: false, snapshot: &b, since: &b_since },
        ];
        let q = queue(&sessions, |k| *k == key("beta"), t0 + Duration::from_secs(300));
        assert_eq!(panes(&q), ["a2", "a1"]);
    }

    #[test]
    fn nothing_waiting_is_an_empty_queue() {
        let s = snapshot(&[("a1", "working"), ("a2", "idle")]);
        let none = Since::new();
        let sessions = [SessionAgents { key: key("alpha"), name: "alpha".into(), shown: true, snapshot: &s, since: &none }];
        let q = queue(&sessions, |_| false, Instant::now());
        assert!(q.is_empty());
        assert_eq!(next(&q, at("alpha", "a1"), None), None);
    }

    #[test]
    fn the_key_goes_on_from_where_you_are_and_round_at_the_end() {
        let w = |pane: &str| Wanted { key: key(if pane.starts_with('a') { "alpha" } else { "beta" }), tab: format!("t-{pane}"), pane: pane.into() };
        let q = [w("b2"), w("a2"), w("b1")];
        assert_eq!(next(&q, None, None), Some(&q[0]), "from nowhere: the first");
        assert_eq!(next(&q, at("beta", "b2"), None), Some(&q[1]));
        assert_eq!(next(&q, at("beta", "b1"), None), Some(&q[0]), "round from the last");
        assert_eq!(next(&q, at("alpha", "b2"), None), Some(&q[0]), "a pane id of another session is not here");
        assert_eq!(next(&q, at("alpha", "a9"), at("alpha", "a2")), Some(&q[2]), "elsewhere: on from the last one gone to");
        assert_eq!(next(&q, at("alpha", "a2"), at("beta", "b2")), Some(&q[2]), "where you are wins");
        assert_eq!(next(&q, at("alpha", "a9"), at("beta", "b9")), Some(&q[0]), "neither waits any more: the first");
        assert_eq!(next(&q[..1], at("beta", "b2"), None), Some(&q[0]), "one alone stays");
    }
}
