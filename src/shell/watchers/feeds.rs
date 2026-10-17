//! A watcher's feed: one `events.subscribe` to a session not shown, which only wakes its poll sooner. It never
//! decides the session's status (only a poll's answer does, through `apply`) and never starts a server: it opens
//! only on a session whose last poll answered, and while it is on, the poll slows to a heartbeat. A feed that ends,
//! or will not open, leaves the session polled as before, and the poll after it says what became of the session.
use super::*;
use crate::model::Pane;
use serde_json::Value;

/// The heartbeat poll while a feed is on: what herdr raises no event for (a folder an agent moved to, a workspace's
/// summed status) is at most this late. Twice as long for a machine from Windows, which shares no SSH connection.
const HEARTBEAT: Duration = Duration::from_secs(60);
const HEARTBEAT_WINDOWS_REMOTE: Duration = Duration::from_secs(120);

/// A herdr that does not take the subscription (older, without some event type) is polled only, and asked again
/// this long after, or once its server starts again.
const UNSUPPORTED: Duration = Duration::from_secs(10 * 60);

/// A feed follows another set of agents at most this often: agents coming and going quickly resubscribe once.
const RESUBSCRIBE_GAP: Duration = Duration::from_secs(2);

/// A feed that lasted this long ended in good health: the next is opened at once, not after a backoff.
const FEED_STEADY: Duration = Duration::from_secs(60);

/// A watcher's subscription, beside its poll.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::shell) enum Feed {
    /// None, and none wanted until the session answers a poll again.
    Off,
    /// Subscribing, following the agents of `panes`.
    Opening { panes: Vec<String> },
    /// Subscribed since `since`.
    On { panes: Vec<String>, since: Instant },
    /// Failed or ended: opened again from `at`, once a poll answered since.
    Retry { at: Instant },
    /// herdr does not take it (`herdr::unsupported`): polled only, until `until` or a restart of its server.
    Unsupported { until: Instant },
}

/// A feed to open: its key, socket, the agents whose status it follows, and the `feed_gen` its news brings back.
pub(in crate::shell) struct FeedOrder {
    pub key: SessionKey,
    pub client: Client,
    pub panes: Vec<String>,
    pub feed_gen: u64,
    pub kind: Kind,
}

impl Watcher {
    pub(in crate::shell) fn fed(&self) -> bool {
        matches!(self.feed, Feed::On { .. })
    }

    /// No feed, and any news of the last one is old: its task is dropped (`Shell::sync_feeds`), which hangs it up.
    pub(super) fn close_feed(&mut self) {
        self.feed = Feed::Off;
        self.feed_gen += 1;
    }

    /// An answer's agents: when they are not the ones the feed follows, the feed is closed to open again with them
    /// (true), at most every [`RESUBSCRIBE_GAP`]; inside it the session is asked again once it is over. `unfed` is
    /// the poll's interval without a feed, kept until the new one opens.
    pub(super) fn follow_agents(&mut self, panes: &[String], now: Instant, unfed: Duration) -> bool {
        let Feed::On { panes: followed, .. } = &self.feed else { return false };
        if followed.as_slice() == panes {
            return false;
        }
        if let Some(at) = self.resubscribed.filter(|at| now.duration_since(*at) < RESUBSCRIBE_GAP) {
            self.next_due = self.next_due.min(at + RESUBSCRIBE_GAP);
            return false;
        }
        self.resubscribed = Some(now);
        self.close_feed();
        self.next_due = self.next_due.min(now + unfed);
        true
    }

    /// News for the poll: asked now, or (a poll is out, whose snapshot may be older than the news) again as soon as it
    /// is answered, so nothing that happened is lost behind it.
    fn wake(&mut self, now: Instant) {
        if self.in_flight {
            self.dirty = true;
        } else {
            self.next_due = self.next_due.min(now);
        }
    }
}

impl Watchers {
    /// The feeds that should be open or opening, with their `feed_gen`: any other feed task is dropped.
    pub(in crate::shell) fn feeding(&self) -> Vec<(SessionKey, u64)> {
        self.map.iter().filter(|(_, w)| matches!(w.feed, Feed::Opening { .. } | Feed::On { .. })).map(|(k, w)| (k.clone(), w.feed_gen)).collect()
    }

    /// The feeds to open now, each marked opening at a new `feed_gen`: only a session not shown whose last poll
    /// answered and that owes no poll (a poll due or out decides first), with its socket known, and not a machine
    /// from Windows, which is polled only.
    pub(in crate::shell) fn feeds_due(&mut self, now: Instant) -> Vec<FeedOrder> {
        let windows = self.windows;
        let mut due = Vec::new();
        for key in &self.order {
            let Some(w) = self.map.get_mut(key) else { continue };
            let ready = match w.feed {
                Feed::Off => true,
                Feed::Retry { at } => now >= at,
                Feed::Unsupported { until } => now >= until,
                Feed::Opening { .. } | Feed::On { .. } => false,
            };
            let quiet = w.status == Status::Live && w.failures == 0 && !w.in_flight && now < w.next_due;
            if !ready || !quiet || (windows && w.kind() == Kind::Remote) {
                continue;
            }
            let Some(client) = w.client.clone() else { continue };
            let panes = w.snapshot.as_ref().map(agent_panes).unwrap_or_default();
            w.feed_gen += 1;
            w.feed = Feed::Opening { panes: panes.clone() };
            due.push(FeedOrder { key: key.clone(), client, panes, feed_gen: w.feed_gen, kind: w.kind() });
        }
        due
    }

    /// The subscription answered, at `feed_gen`: on, the session is asked at once (what changed before it opened
    /// came on no feed); unsupported, it is polled only for a while; turned down for what it named (a pane closed
    /// since the last poll), it is asked at once and opened again with that answer's panes; failed, it is asked at
    /// once (that answer says what became of it) and the feed waits out a backoff. News of a feed closed since
    /// changes nothing.
    pub(in crate::shell) fn feed_opened(&mut self, key: &SessionKey, feed_gen: u64, result: anyhow::Result<()>, now: Instant) {
        let Some(w) = self.map.get_mut(key).filter(|w| w.feed_gen == feed_gen) else { return };
        let kind = w.kind();
        match result {
            Ok(()) => {
                let panes = match std::mem::replace(&mut w.feed, Feed::Off) {
                    Feed::Opening { panes } => panes,
                    _ => Vec::new(),
                };
                w.feed = Feed::On { panes, since: now };
                w.wake(now);
            }
            Err(e) if crate::herdr::unsupported(&e) => w.feed = Feed::Unsupported { until: now + UNSUPPORTED },
            Err(e) if crate::herdr::api_error(&e).is_some() => {
                w.feed = Feed::Off;
                w.wake(now);
            }
            Err(_) => {
                w.feed_failures += 1;
                w.feed = Feed::Retry { at: now + feed_backoff(kind, w.feed_failures) };
                w.wake(now);
            }
        }
    }

    /// The feed at `feed_gen` ended (its server went, fell behind, or an SSH link dropped): the session is asked at
    /// once, and that answer decides its status. A feed that had lasted is opened again right after the answer; one
    /// that ended soon after opening backs off as a failure.
    pub(in crate::shell) fn feed_ended(&mut self, key: &SessionKey, feed_gen: u64, now: Instant) {
        let Some(w) = self.map.get_mut(key).filter(|w| w.feed_gen == feed_gen) else { return };
        let steady = matches!(w.feed, Feed::On { since, .. } if now.duration_since(since) >= FEED_STEADY);
        w.feed_failures = if steady { 0 } else { w.feed_failures + 1 };
        let at = if w.feed_failures == 0 { now } else { now + feed_backoff(w.kind(), w.feed_failures) };
        w.feed = Feed::Retry { at };
        w.wake(now);
    }

    /// A burst of events from the feed at `feed_gen`: wakes the poll (true) when one of them may change what the
    /// tree draws. News of a feed closed since, or of a session shown or stopped, is dropped.
    pub(in crate::shell) fn burst(&mut self, key: &SessionKey, feed_gen: u64, events: &[Value], now: Instant) -> bool {
        let Some(w) = self.map.get_mut(key) else { return false };
        if w.feed_gen != feed_gen || !w.fed() || matches!(w.status, Status::Shown | Status::Stopped) || !relevant(w.snapshot.as_ref(), events) {
            return false;
        }
        w.wake(now);
        true
    }

    /// How long a feed waits after a burst before it takes the next: a busy session is asked a few times a second
    /// at most with the window in front, less behind it, and once in seconds while you are away.
    pub(in crate::shell) fn refetch_gap(&self, kind: Kind) -> Duration {
        refetch_gap(kind, self.activity, self.windows)
    }
}

fn refetch_gap(kind: Kind, activity: Activity, windows: bool) -> Duration {
    let ms = match (kind, activity) {
        (Kind::Remote, _) if windows => 10_000,
        (Kind::Local, Activity::Active) => 300,
        (Kind::Remote, Activity::Active) => 1_000,
        (Kind::Local, Activity::Inactive) => 1_000,
        (Kind::Remote, Activity::Inactive) => 2_000,
        (Kind::Local, Activity::Idle) => 3_000,
        (Kind::Remote, Activity::Idle) => 5_000,
    };
    Duration::from_millis(ms)
}

/// The poll's interval while fed: the heartbeat.
pub(super) fn heartbeat(kind: Kind, windows: bool) -> Duration {
    if windows && kind == Kind::Remote { HEARTBEAT_WINDOWS_REMOTE } else { HEARTBEAT }
}

/// How long the events of one burst are gathered after its first. A busy agent's `pane.updated` (its title, scroll,
/// token count) comes about every 100–200 ms, all of it news of nothing the tree draws; the window takes a few at a
/// time, and the gap after each burst ([`refetch_gap`]) bounds how often the main thread looks at them.
pub(in crate::shell) fn burst_window(kind: Kind) -> Duration {
    match kind {
        Kind::Local => Duration::from_millis(100),
        Kind::Remote => Duration::from_millis(200),
    }
}

/// How long a subscription may take to answer: a machine's may have to make its SSH connection first.
pub(in crate::shell) fn open_timeout(kind: Kind) -> Duration {
    match kind {
        Kind::Local => Duration::from_secs(2),
        Kind::Remote => Duration::from_secs(15),
    }
}

/// The wait before opening a feed again after `failures` in a row: 2 s here, 5 s over SSH, doubling to 5 minutes.
fn feed_backoff(kind: Kind, failures: u32) -> Duration {
    let base = match kind {
        Kind::Local => 2,
        Kind::Remote => 5,
    };
    Duration::from_secs((base * 2u64.saturating_pow(failures.saturating_sub(1).min(16))).min(300))
}

/// Whether a burst may change what the tree draws of the session, last seen as `old`: any event but `pane.updated`
/// does; that one only when the pane is new to `old`, or moved, or its agent, status, folder or terminal changed.
/// A busy agent sends it several times a second for its title, scroll and tokens alone.
pub(in crate::shell) fn relevant(old: Option<&Snapshot>, events: &[Value]) -> bool {
    events.iter().any(|e| !crate::herdr::event_is(e, "pane.updated") || pane_changed(old, &e["data"]["pane"]))
}

fn pane_changed(old: Option<&Snapshot>, pane: &Value) -> bool {
    let (Some(old), Ok(p)) = (old, serde_json::from_value::<Pane>(pane.clone())) else { return true };
    let drawn = |p: &Pane| (p.agent_status, p.agent.clone(), p.cwd.clone(), p.tab_id.clone(), p.workspace_id.clone(), p.terminal_id.clone());
    old.panes.iter().find(|o| o.pane_id == p.pane_id).is_none_or(|o| drawn(o) != drawn(&p))
}

/// The panes with an agent, whose status a feed follows (herdr sends it per pane only), sorted: a reorder is the same
/// set. A new agent is found by `pane.agent_detected`, whose poll brings it here.
pub(in crate::shell) fn agent_panes(s: &Snapshot) -> Vec<String> {
    let mut panes: Vec<String> = s.panes.iter().filter(|p| p.agent.is_some()).map(|p| p.pane_id.clone()).collect();
    panes.sort();
    panes
}

impl Shell {
    /// Every feed that should be open is, and no other: a task each, dropped (which hangs up its socket) once its
    /// watcher closed it, moved to another, or went.
    pub(in crate::shell) fn sync_feeds(&mut self, cx: &mut Context<Self>) {
        let feeding = self.watchers.feeding();
        self.feeds.retain(|k, (feed_gen, _)| feeding.iter().any(|(key, g)| key == k && g == feed_gen));
        for order in self.watchers.feeds_due(Instant::now()) {
            let (key, feed_gen, rt) = (order.key.clone(), order.feed_gen, self.rt.clone());
            let task = cx.spawn(async move |this, cx| feed_loop(this, rt, order, cx).await);
            self.feeds.insert(key, (feed_gen, task));
        }
    }

    /// The polls due now, as many as [`MAX_FETCHES`] allows.
    pub(in crate::shell) fn poll_due(&mut self, cx: &mut Context<Self>) {
        let activity = self.watchers.activity();
        for (key, client, timeout, generation) in self.watchers.due(Instant::now(), activity) {
            self.fetch(key, client, timeout, generation, cx);
        }
    }
}

/// One feed: subscribes on the runtime within [`open_timeout`], then wakes the watcher's poll per burst of events
/// that matter, waiting [`refetch_gap`] after each, until the subscription ends or the task is dropped.
async fn feed_loop(this: gpui::WeakEntity<Shell>, rt: tokio::runtime::Handle, order: FeedOrder, cx: &mut gpui::AsyncApp) {
    let FeedOrder { key, client, panes, feed_gen, kind } = order;
    let limit = open_timeout(kind);
    let opening = rt.spawn(async move {
        match tokio::time::timeout(limit, client.subscribe(crate::herdr::WATCH_TYPES, &panes)).await {
            Ok(rx) => rx,
            Err(_) => Err(anyhow::anyhow!("no answer in {} s", limit.as_secs())),
        }
    });
    let (rx, result) = match opening.await.map_err(anyhow::Error::from).and_then(|r| r) {
        Ok(rx) => (Some(rx), Ok(())),
        Err(e) => (None, Err(e)),
    };
    let opened = this.update(cx, |s, cx| {
        s.watchers.feed_opened(&key, feed_gen, result, Instant::now());
        s.poll_due(cx);
    });
    let (Ok(()), Some(mut rx)) = (opened, rx) else { return };
    let executor = cx.background_executor().clone();
    loop {
        let Some(events) = crate::herdr::burst(&mut rx, || executor.timer(burst_window(kind))).await else {
            let _ = this.update(cx, |s, cx| {
                s.watchers.feed_ended(&key, feed_gen, Instant::now());
                s.poll_due(cx);
            });
            return;
        };
        let woke = this.update(cx, |s, cx| {
            if s.watchers.burst(&key, feed_gen, &events, Instant::now()) {
                s.poll_due(cx);
            }
            s.watchers.refetch_gap(kind)
        });
        let Ok(gap) = woke else { return };
        executor.timer(gap).await;
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{agents, key, machine, refused, session, told};
    use super::*;
    use Activity::Active;

    fn pane_updated(id: &str, status: &str) -> Value {
        let pane = json!({"pane_id": id, "tab_id": "t1", "workspace_id": "w1", "terminal_id": id, "agent_status": status, "agent": "claude", "revision": 7, "terminal_title": "◐ busy", "tokens": {"group": "api"}});
        json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": pane}})
    }

    fn event(name: &str) -> Value {
        json!({"event": name, "data": {}})
    }

    fn feed_gen(w: &Watchers, k: &SessionKey) -> u64 {
        w.get(k).unwrap().feed_gen
    }

    /// Answers the poll due for `k` at `now` with `snapshot`.
    fn poll(w: &mut Watchers, k: &SessionKey, snapshot: Snapshot, now: Instant) -> Applied {
        let generation = w.due(now, Active).into_iter().find(|d| d.0 == *k).expect("a poll due").3;
        w.answer(k, generation, Ok(snapshot), now)
    }

    /// menqal, not shown, live with agent p1 working, its feed on and the poll its opening asked for answered.
    fn fed(now: Instant) -> Watchers {
        let mut w = Watchers::new();
        w.windows = false;
        w.reconcile(&[session("default", true), session("menqal", true)], Some(&key("default")), now);
        let k = key("menqal");
        poll(&mut w, &k, agents(&[("p1", "working")]), now);
        let order = w.feeds_due(now).pop().unwrap();
        w.feed_opened(&k, order.feed_gen, Ok(()), now);
        poll(&mut w, &k, agents(&[("p1", "working")]), now);
        assert!(w.get(&k).unwrap().fed());
        w
    }

    #[test]
    fn feeds_go_only_to_live_sessions_not_shown() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.windows = false;
        w.reconcile(&[session("default", true), session("menqal", true), session("old", false), machine("box")], Some(&key("default")), now);
        assert!(w.feeds_due(now).is_empty(), "nothing has answered yet: connecting is no reason for a feed");
        let k = key("menqal");
        w.due(now, Active);
        assert!(w.feeds_due(now).is_empty(), "a poll out decides first");
        w.answer(&k, 0, Ok(agents(&[("p2", "idle"), ("p1", "working")])), now);
        let due = w.feeds_due(now);
        assert_eq!(due.iter().map(|o| (o.key.clone(), o.panes.clone())).collect::<Vec<_>>(), [(k.clone(), vec!["p1".to_string(), "p2".to_string()])]);
        assert!(w.feeds_due(now).is_empty(), "one feed each");
        assert_eq!(w.feeding(), [(k.clone(), due[0].feed_gen)]);
        // left before it ever answered, and stopped: neither owes a poll now, neither is live
        w.park(&key("default"), None, Default::default(), now);
        assert_eq!(w.get(&key("default")).unwrap().status, Status::Connecting);
        let stopped = key("menqal");
        w.feed_ended(&stopped, due[0].feed_gen, now);
        let generation = w.due(now, Active).into_iter().find(|d| d.0 == stopped).unwrap().3;
        w.answer(&stopped, generation, Err(refused()), now);
        assert!(w.feeds_due(now).is_empty(), "only a live session is fed");
        // a machine: from Windows it is polled only
        let m = SessionKey("machine:box".into());
        w.set_client(&m, Client::at("/tmp/box.sock".into()));
        poll(&mut w, &m, agents(&[]), now);
        w.windows = true;
        assert!(w.feeds_due(now).is_empty());
        w.windows = false;
        assert_eq!(w.feeds_due(now).len(), 1);
    }

    #[test]
    fn an_opened_feed_asks_at_once_and_old_news_changes_nothing() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", true)], None, now);
        let k = key("menqal");
        poll(&mut w, &k, agents(&[("p1", "working")]), now);
        let g = w.feeds_due(now).pop().unwrap().feed_gen;
        w.feed_opened(&k, g - 1, Ok(()), now);
        assert!(!w.get(&k).unwrap().fed() && w.due(now, Active).is_empty(), "news of an older feed");
        w.feed_opened(&k, g, Ok(()), now);
        assert!(w.get(&k).unwrap().fed());
        assert_eq!(w.due(now, Active).len(), 1, "what changed before it opened came on no feed");
        w.answer(&k, 0, Ok(agents(&[("p1", "working")])), now);
        assert!(!w.burst(&k, g - 1, &[event("tab_created")], now), "an older feed's burst");
        assert!(w.due(now, Active).is_empty());
        assert!(w.burst(&k, g, &[event("tab_created")], now));
        assert_eq!(w.due(now, Active).len(), 1);
    }

    #[test]
    fn news_during_a_poll_asks_again_once_it_is_answered() {
        let now = Instant::now();
        let mut w = fed(now);
        let k = key("menqal");
        w.poll_now(&k, now);
        let generation = w.due(now, Active)[0].3;
        assert!(w.burst(&k, feed_gen(&w, &k), &[event("pane_created")], now));
        assert!(w.due(now, Active).is_empty(), "one poll out at a time");
        let applied = w.answer(&k, generation, Ok(agents(&[("p1", "working")])), now);
        assert!(applied.again, "the snapshot out may be older than the news");
        assert_eq!(w.due(now, Active).len(), 1);
        assert!(!w.answer(&k, generation, Ok(agents(&[("p1", "working")])), now).again, "asked again once");
    }

    #[test]
    fn an_ended_feed_asks_at_once_and_that_answer_decides() {
        let now = Instant::now();
        let mut w = fed(now);
        let k = key("menqal");
        w.feed_ended(&k, feed_gen(&w, &k), now);
        assert_eq!(w.get(&k).unwrap().status, Status::Live, "a feed never decides the status");
        let generation = w.due(now, Active)[0].3;
        assert!(w.feeds_due(now).is_empty() && w.feeding().is_empty(), "the poll decides first");
        w.answer(&k, generation, Err(anyhow::anyhow!("no answer in 2 s")), now);
        assert!(w.feeds_due(now + Duration::from_secs(5)).is_empty(), "a failed poll: no feed");
        let later = now + Duration::from_secs(6);
        let generation = w.due(later, Active)[0].3;
        w.answer(&k, generation, Err(anyhow::anyhow!("no answer in 2 s")), later);
        assert_eq!(w.get(&k).unwrap().status, Status::Unreachable);
        let back = later + Duration::from_secs(12);
        poll(&mut w, &k, agents(&[("p1", "working")]), back);
        // it ended soon after opening, and its backoff (2 s) is over: the answer opens it again
        assert_eq!(w.feeds_due(back).len(), 1);
    }

    #[test]
    fn an_unsupported_feed_is_polled_only_until_its_server_starts_again() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", true)], None, now);
        let k = key("menqal");
        poll(&mut w, &k, agents(&[("p1", "working")]), now);
        let g = w.feeds_due(now).pop().unwrap().feed_gen;
        let no = anyhow::Error::new(crate::herdr::ApiError { code: "invalid_request".into(), message: "invalid request: unknown variant".into() });
        w.feed_opened(&k, g, Err(no), now);
        assert!(matches!(w.get(&k).unwrap().feed, Feed::Unsupported { .. }));
        assert!(w.due(now, Active).is_empty(), "polled as before, nothing asked at once");
        assert!(w.feeds_due(now + Duration::from_secs(2)).is_empty());
        poll(&mut w, &k, agents(&[("p1", "working")]), now + Duration::from_secs(3));
        assert!(w.feeds_due(now + Duration::from_secs(4)).is_empty(), "not asked again on every answer");
        // its server stopped and started again
        w.reconcile(&[session("menqal", false)], None, now);
        w.reconcile(&[session("menqal", true)], None, now);
        poll(&mut w, &k, agents(&[("p1", "working")]), now);
        assert_eq!(w.feeds_due(now).len(), 1, "a new server is asked again");
    }

    #[test]
    fn a_feed_naming_a_closed_pane_opens_again_with_the_next_answers_panes() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", true)], None, now);
        let k = key("menqal");
        poll(&mut w, &k, agents(&[("p1", "working"), ("p2", "working")]), now);
        let g = w.feeds_due(now).pop().unwrap().feed_gen;
        let gone = anyhow::Error::new(crate::herdr::ApiError { code: "pane_not_found".into(), message: "pane p2 not found".into() });
        w.feed_opened(&k, g, Err(gone), now);
        assert_eq!(w.get(&k).unwrap().feed, Feed::Off, "not unsupported: the request was fine, a pane in it was old");
        assert!(w.feeds_due(now).is_empty(), "the poll first, for the panes there are now");
        poll(&mut w, &k, agents(&[("p1", "working")]), now);
        let order = w.feeds_due(now).pop().expect("opened again at once");
        assert_eq!(order.panes, ["p1"]);
    }

    #[test]
    fn a_failed_feed_backs_off_and_retry_clears_it() {
        let secs = |kind, n| feed_backoff(kind, n).as_secs();
        assert_eq!((1..=4).map(|n| secs(Kind::Local, n)).collect::<Vec<_>>(), [2, 4, 8, 16]);
        assert_eq!((1..=3).map(|n| secs(Kind::Remote, n)).collect::<Vec<_>>(), [5, 10, 20]);
        assert_eq!((secs(Kind::Local, 20), secs(Kind::Remote, 99)), (300, 300));
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", true)], None, now);
        let k = key("menqal");
        poll(&mut w, &k, agents(&[]), now);
        let g = w.feeds_due(now).pop().unwrap().feed_gen;
        w.feed_opened(&k, g, Err(anyhow::anyhow!("herdr closed the connection")), now);
        assert!(!w.get(&k).unwrap().fed());
        poll(&mut w, &k, agents(&[]), now); // asked at once: the answer says what became of it
        assert!(w.feeds_due(now + Duration::from_secs(1)).is_empty());
        let g = w.feeds_due(now + Duration::from_secs(2)).pop().expect("2 s on").feed_gen;
        w.feed_opened(&k, g, Err(anyhow::anyhow!("herdr closed the connection")), now + Duration::from_secs(2));
        assert_eq!(w.get(&k).unwrap().feed, Feed::Retry { at: now + Duration::from_secs(6) }, "4 s the second time");
        w.poll_now(&k, now + Duration::from_secs(2));
        assert_eq!((w.get(&k).unwrap().feed.clone(), w.get(&k).unwrap().feed_failures), (Feed::Off, 0));
    }

    #[test]
    fn a_fed_poll_is_a_heartbeat_and_goes_stale_only_after_three() {
        use Activity::*;
        assert_eq!(interval_on(Kind::Local, Active, 0, true, false), HEARTBEAT);
        assert_eq!(interval_on(Kind::Local, Idle, 0, true, false), HEARTBEAT);
        assert_eq!(interval_on(Kind::Remote, Active, 0, true, false), HEARTBEAT);
        assert_eq!(interval_on(Kind::Remote, Active, 0, true, true), Duration::from_secs(120));
        assert_eq!(interval_on(Kind::Local, Active, 1, true, false), Duration::from_secs(6), "failing: backing off as unfed");
        let now = Instant::now();
        let mut w = fed(now);
        let k = key("menqal");
        assert!(w.due(now + Duration::from_secs(59), Active).is_empty());
        assert!(!w.get(&k).unwrap().stale(now + Duration::from_secs(20), Active), "fed: quiet is no news");
        assert!(!w.get(&k).unwrap().stale(now + HEARTBEAT * 3, Active));
        assert!(w.get(&k).unwrap().stale(now + HEARTBEAT * 3 + Duration::from_secs(1), Active));
        w.feed_ended(&k, feed_gen(&w, &k), now);
        assert!(w.get(&k).unwrap().stale(now + Duration::from_secs(10), Active), "unfed: three polls of 3 s");
    }

    #[test]
    fn a_feed_follows_new_agents_at_most_every_two_seconds() {
        let now = Instant::now();
        let mut w = fed(now);
        let k = key("menqal");
        let g = feed_gen(&w, &k);
        w.poll_now(&k, now);
        assert!(!poll(&mut w, &k, agents(&[("p1", "working")]), now).resubscribe, "the same agents");
        w.poll_now(&k, now);
        assert!(poll(&mut w, &k, agents(&[("p2", "working"), ("p1", "working")]), now).resubscribe);
        assert!(!w.get(&k).unwrap().fed() && feed_gen(&w, &k) > g, "closed, its news old");
        let order = w.feeds_due(now).pop().expect("opened again at once");
        assert_eq!(order.panes, ["p1", "p2"]);
        w.feed_opened(&k, order.feed_gen, Ok(()), now);
        let soon = now + Duration::from_secs(1);
        assert!(!poll(&mut w, &k, agents(&[("p1", "working")]), soon).resubscribe, "too soon after the last");
        assert!(w.get(&k).unwrap().fed());
        let gap = now + RESUBSCRIBE_GAP;
        assert!(poll(&mut w, &k, agents(&[("p1", "working")]), gap).resubscribe, "asked again once the gap is over");
        // a reorder is the same set
        let order = w.feeds_due(gap).pop().unwrap();
        w.feed_opened(&k, order.feed_gen, Ok(()), gap);
        let later = gap + RESUBSCRIBE_GAP;
        w.poll_now(&k, later);
        poll(&mut w, &k, agents(&[("p3", "idle"), ("p1", "working")]), later);
        let order = w.feeds_due(later).pop().unwrap();
        w.feed_opened(&k, order.feed_gen, Ok(()), later);
        let last = later + RESUBSCRIBE_GAP;
        assert!(!poll(&mut w, &k, agents(&[("p1", "working"), ("p3", "idle")]), last).resubscribe);
    }

    #[test]
    fn a_switch_closes_the_feed_and_the_parked_snapshot_is_the_baseline() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("default", true), session("menqal", true)], Some(&key("default")), now);
        let k = key("default");
        w.park(&k, Some(agents(&[("p1", "working")])), Default::default(), now);
        let order = w.feeds_due(now).pop().expect("the session left is fed at once");
        w.feed_opened(&k, order.feed_gen, Ok(()), now);
        assert_eq!(told(poll(&mut w, &k, agents(&[("p1", "blocked")]), now)), ["p1"], "against the snapshot it was left with");
        let g = feed_gen(&w, &k);
        w.unpark(&k);
        assert!(w.feeding().is_empty() && feed_gen(&w, &k) > g, "the session shown has no feed");
        assert!(w.feeds_due(now).is_empty());
        w.park(&k, Some(agents(&[("p1", "working")])), Default::default(), now);
        assert!(feed_gen(&w, &k) > g + 1, "parking moves it on again");
    }

    #[test]
    fn news_of_a_session_shown_or_stopped_is_dropped() {
        let now = Instant::now();
        let mut w = fed(now);
        let k = key("menqal");
        let g = feed_gen(&w, &k);
        w.unpark(&k);
        assert!(!w.burst(&k, g, &[event("tab_created")], now));
        assert!(!w.burst(&k, feed_gen(&w, &k), &[event("tab_created")], now));
        let mut w = fed(now);
        w.feed_ended(&k, feed_gen(&w, &k), now);
        let generation = w.due(now, Active)[0].3;
        w.answer(&k, generation, Err(refused()), now);
        assert!(!w.burst(&k, feed_gen(&w, &k), &[event("tab_created")], now));
        assert!(w.due(now, Active).is_empty());
    }

    #[test]
    fn a_feed_that_ends_brings_no_alert_burst_after_it() {
        let now = Instant::now();
        let mut w = fed(now);
        let k = key("menqal");
        w.feed_ended(&k, feed_gen(&w, &k), now);
        let generation = w.due(now, Active)[0].3;
        w.answer(&k, generation, Err(anyhow::anyhow!("no answer in 2 s")), now);
        let later = now + Duration::from_secs(6);
        let generation = w.due(later, Active)[0].3;
        w.answer(&k, generation, Err(anyhow::anyhow!("no answer in 2 s")), later);
        assert!(poll(&mut w, &k, agents(&[("p1", "blocked")]), later + Duration::from_secs(12)).alerts.is_empty(), "after unreachable");
        let mut w = fed(now);
        w.feed_ended(&k, feed_gen(&w, &k), now);
        let generation = w.due(now, Active)[0].3;
        w.answer(&k, generation, Err(refused()), now);
        w.reconcile(&[session("default", true), session("menqal", true)], Some(&key("default")), now);
        assert!(poll(&mut w, &k, agents(&[("p1", "done")]), now + Duration::from_secs(61)).alerts.is_empty(), "after stopped");
    }

    #[test]
    fn only_pane_updates_that_change_the_tree_count() {
        let old = agents(&[("p1", "working")]);
        let old = Some(&old);
        assert!(!relevant(old, &[pane_updated("p1", "working")]), "title, revision, tokens");
        assert!(!relevant(old, &[]));
        let mut dotted = pane_updated("p1", "working");
        dotted["event"] = json!("pane.updated");
        assert!(!relevant(old, &[dotted]), "either spelling");
        assert!(relevant(old, &[pane_updated("p1", "working"), pane_updated("p1", "blocked")]), "its status");
        assert!(relevant(old, &[pane_updated("p9", "working")]), "a pane not seen");
        assert!(relevant(None, &[pane_updated("p1", "working")]), "nothing seen yet");
        let mut moved = pane_updated("p1", "working");
        moved["data"]["pane"]["tab_id"] = json!("t2");
        assert!(relevant(old, &[moved]));
        let mut cd = pane_updated("p1", "working");
        cd["data"]["pane"]["cwd"] = json!("/tmp/demo");
        assert!(relevant(old, &[cd]));
        let mut other = pane_updated("p1", "working");
        other["data"]["pane"]["terminal_id"] = json!("term-new");
        assert!(relevant(old, &[other]));
        assert!(relevant(old, &[json!({"event": "pane_updated", "data": {"pane": {"pane_id": "p1"}}})]), "a pane it cannot read");
        assert!(relevant(old, &[event("tab_created")]));
        assert!(relevant(old, &[event("pane_agent_status_changed")]));
    }

    #[test]
    fn at_most_six_polls_are_out_at_once() {
        let now = Instant::now();
        let mut w = Watchers::new();
        let names: Vec<String> = (0..8).map(|i| format!("s{i}")).collect();
        let places: Vec<Place> = names.iter().map(|n| session(n, true)).collect();
        w.reconcile(&places, None, now);
        let out = w.due(now, Active);
        assert_eq!(out.len(), MAX_FETCHES);
        assert!(w.due(now, Active).is_empty(), "the rest wait their turn");
        w.answer(&out[0].0, out[0].3, Ok(agents(&[])), now);
        assert_eq!(w.due(now, Active).iter().map(|d| d.0.clone()).collect::<Vec<_>>(), [key("s6")], "one answered, one more out");
    }

    #[test]
    fn a_machine_on_another_socket_closes_its_feed() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.windows = false;
        let m = SessionKey("machine:box".into());
        w.reconcile(&[machine("box")], None, now);
        w.set_client(&m, Client::at("/tmp/box.sock".into()));
        poll(&mut w, &m, agents(&[]), now);
        let order = w.feeds_due(now).pop().unwrap();
        w.feed_opened(&m, order.feed_gen, Ok(()), now);
        w.set_client(&m, Client::at("/tmp/box.sock".into()));
        assert!(w.get(&m).unwrap().fed(), "the same socket");
        w.set_client(&m, Client::at("/tmp/box2.sock".into()));
        assert!(!w.get(&m).unwrap().fed() && feed_gen(&w, &m) > order.feed_gen);
    }

    #[test]
    fn a_stopped_session_closes_its_feed_and_says_so() {
        let now = Instant::now();
        let mut w = fed(now);
        let k = key("menqal");
        w.feed_ended(&k, feed_gen(&w, &k), now);
        let generation = w.due(now, Active)[0].3;
        let applied = w.answer(&k, generation, Err(refused()), now);
        assert!(applied.stopped, "herdr's list is read again");
        assert_eq!((w.get(&k).unwrap().status, w.get(&k).unwrap().feed.clone()), (Status::Stopped, Feed::Off));
        assert!(w.feeding().is_empty() && w.feeds_due(now).is_empty());
        // a feed open when herdr's list says stopped closes too
        let mut w = fed(now);
        w.reconcile(&[session("default", true), session("menqal", false)], Some(&key("default")), now);
        assert!(w.feeding().is_empty());
    }
}
