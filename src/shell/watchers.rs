//! The sessions and machines not shown, kept fresh in the background: one watcher each polls its `session.snapshot`
//! (at most one call in flight, each with a deadline), backing off while it fails, so the sidebar's tree shows every
//! session's workspaces and agents. The session shown has no poll: the sync loop follows it, and a switch hands its
//! last snapshot to its watcher (`park`) and takes the next session's from its watcher (`unpark`).
use super::sessions::Place;
use super::*;
use crate::herdr::Machine;
use crate::model::Since;
use std::hash::{Hash, Hasher};

/// A session's key: [`Place::key`] (its socket, or `machine:<id>`), as the settings and the file tabs key it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub(super) struct SessionKey(pub String);

impl SessionKey {
    pub(super) fn of(place: &Place) -> Self {
        Self(place.key())
    }
}

/// Where a session runs: polled across the machine, or over SSH.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Local,
    Remote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    /// The window shows it: the sync loop follows it.
    Shown,
    /// Its last snapshot came in its last poll.
    Live,
    /// Not heard from yet (or since it started, or was left).
    Connecting,
    /// No server: it starts when you go there, or from its menu; never in the background.
    Stopped,
    /// Two polls in a row failed.
    Unreachable,
}

/// How much the person is around, which sets how often the sessions not shown are asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Activity {
    /// The window is in front.
    #[default]
    Active,
    /// Another window is.
    Inactive,
    /// Nobody has touched the machine for a while, or its screen is locked.
    Idle,
}

/// The machine counts as left alone after this long without a key or a pointer move.
const IDLE_AFTER: Duration = Duration::from_secs(10 * 60);

/// How long a session started from the tree counts as starting: drawn connecting, and asked every second, while
/// its server comes up.
const STARTING: Duration = Duration::from_secs(10);

/// How often a starting session is asked.
const STARTING_POLL: Duration = Duration::from_secs(1);

/// How often the scheduler looks for due watchers.
const TICK: Duration = Duration::from_secs(1);

/// herdr's sessions and machines are listed again this often, while the window is in front and behind.
const LIST_ACTIVE: Duration = Duration::from_secs(15);
const LIST_INACTIVE: Duration = Duration::from_secs(60);

pub(super) struct Watcher {
    pub place: Place,
    /// The socket it is asked on; a machine's relay is started on its first poll.
    pub client: Option<Client>,
    pub snapshot: Option<Snapshot>,
    /// [`digest`] of `snapshot`: an answer with the same one changes nothing drawn.
    pub digest: u64,
    pub status: Status,
    /// Since when each of its agents has had its status, carried across switches.
    pub since: Since,
    /// When its last snapshot came.
    pub fetched: Option<Instant>,
    /// Why its last poll failed.
    pub error: Option<String>,
    failures: u32,
    next_due: Instant,
    in_flight: bool,
    /// herdr's list said its server runs: a session found stopped is asked again only once the list changes its
    /// mind (or a minute on, where the list keeps saying it runs).
    listed_running: bool,
    /// Started from the tree: until then it is connecting, its missing server is no news, and a second start is
    /// ignored.
    starting_until: Option<Instant>,
    /// Moves on with each switch to or from the session (`park`, `unpark`): a poll asked for before one is answered
    /// against a snapshot it never saw, so its answer is dropped.
    generation: u64,
}

impl Watcher {
    fn new(place: Place, now: Instant) -> Self {
        Self {
            client: None,
            snapshot: None,
            digest: 0,
            status: Status::Connecting,
            since: Default::default(),
            fetched: None,
            error: None,
            failures: 0,
            next_due: now,
            in_flight: false,
            listed_running: running(&place),
            starting_until: None,
            generation: 0,
            place,
        }
    }

    pub(super) fn kind(&self) -> Kind {
        kind_of(&self.place)
    }

    /// Started from the tree, and its server has not answered yet.
    pub(super) fn starting(&self, now: Instant) -> bool {
        self.starting_until.is_some_and(|until| now < until)
    }

    /// Live, but nothing came for three of its intervals: drawn as connecting.
    pub(super) fn stale(&self, now: Instant, activity: Activity) -> bool {
        self.status == Status::Live && self.fetched.is_some_and(|at| now.duration_since(at) > interval(self.kind(), activity, 0) * 3)
    }
}

fn kind_of(place: &Place) -> Kind {
    match place {
        Place::Session(_) => Kind::Local,
        Place::Machine(_) => Kind::Remote,
    }
}

fn running(place: &Place) -> bool {
    match place {
        Place::Session(s) => s.running,
        Place::Machine(_) => true,
    }
}

/// What an answer changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Applied {
    /// Something drawn changed: the window renders again.
    pub changed: bool,
    /// The session turned out stopped: herdr's list is read again.
    pub stopped: bool,
    /// The agents that just started waiting or finished (`attention::alerts`): only from a session that was live
    /// with a snapshot before, so its first answer, and one after it was unreachable or stopped, tell nothing.
    pub alerts: Vec<super::attention::Alert>,
}

/// Every place's watcher, in the tree's order.
#[derive(Default)]
pub(super) struct Watchers {
    order: Vec<SessionKey>,
    map: HashMap<SessionKey, Watcher>,
    activity: Activity,
    /// The window is in front.
    active: bool,
    /// Since when the window has been behind others.
    inactive_since: Option<Instant>,
    /// herdr's saved machines that are turned off: the tree lists them greyed, to turn back on.
    pub disabled: Vec<Machine>,
    /// herdr's list is being read; asked again when it lands (`relist`).
    listing: bool,
    relist: bool,
    listed: Option<Instant>,
}

impl Watchers {
    pub(super) fn new() -> Self {
        Self { active: true, ..Default::default() }
    }

    /// Follows herdr's list of places: new ones get a watcher, gone ones lose theirs, and `shown` is left to the
    /// sync loop. A session herdr says stopped drops its snapshot; one it says started again is asked at once.
    pub(super) fn reconcile(&mut self, places: &[Place], shown: Option<&SessionKey>, now: Instant) {
        let keys: Vec<SessionKey> = places.iter().map(SessionKey::of).collect();
        self.map.retain(|k, _| keys.contains(k));
        for (place, key) in places.iter().zip(&keys) {
            let w = self.map.entry(key.clone()).or_insert_with(|| Watcher::new(place.clone(), now));
            let starting = w.starting(now);
            let (was_running, now_running) = (w.listed_running, running(place));
            w.listed_running = now_running;
            if let Place::Session(s) = place {
                w.client = Some(Client::at(s.socket_path.clone().into()));
            }
            w.place = place.clone();
            w.status = match w.status {
                _ if shown == Some(key) => Status::Shown,
                // a list read before the start reached herdr still says stopped
                _ if starting => Status::Connecting,
                _ if !now_running => {
                    w.snapshot = None;
                    w.digest = 0;
                    Status::Stopped
                }
                Status::Shown => {
                    w.next_due = now;
                    Status::Connecting
                }
                Status::Stopped if !was_running => {
                    (w.next_due, w.failures) = (now, 0);
                    Status::Connecting
                }
                status => status,
            };
        }
        self.order = keys;
    }

    /// The watchers due a poll now, each marked in flight until its answer is applied: its key, its socket, how long
    /// the call may take and its generation, which the answer brings back. A machine's watcher without its relay yet
    /// is skipped (`without_client`).
    pub(super) fn due(&mut self, now: Instant, activity: Activity) -> Vec<(SessionKey, Client, Duration, u64)> {
        self.activity = activity;
        let mut due = Vec::new();
        for key in &self.order {
            let Some(w) = self.map.get_mut(key) else { continue };
            let skip = match w.status {
                Status::Shown => true,
                Status::Stopped => !w.listed_running,
                _ => false,
            };
            if skip || w.in_flight || now < w.next_due {
                continue;
            }
            let Some(client) = w.client.clone() else { continue };
            w.in_flight = true;
            due.push((key.clone(), client, timeout(w.kind(), w.failures, w.fetched.is_none()), w.generation));
        }
        due
    }

    /// A poll's answer, asked for at `generation`: dropped when a switch to or from the session came since.
    pub(super) fn answer(&mut self, key: &SessionKey, generation: u64, result: anyhow::Result<Snapshot>, now: Instant) -> Applied {
        match self.map.get_mut(key) {
            Some(w) if w.generation != generation => {
                w.in_flight = false;
                Applied::default()
            }
            _ => self.apply(key, result, now),
        }
    }

    /// A poll's answer: a snapshot (`changed` only when its digest or the status moved), no server (stopped), or a
    /// failure, which waits longer before the next poll and after two in a row calls the session unreachable. An
    /// answer for a session shown since is dropped: the sync loop has it.
    pub(super) fn apply(&mut self, key: &SessionKey, result: anyhow::Result<Snapshot>, now: Instant) -> Applied {
        let activity = self.activity;
        let Some(w) = self.map.get_mut(key) else { return Applied::default() };
        w.in_flight = false;
        if w.status == Status::Shown {
            return Applied::default();
        }
        let kind = w.kind();
        let starting = w.starting(now);
        match result {
            Err(_) if starting => {
                // its server is still coming up: asked again in a second, nothing drawn changes
                w.next_due = now + STARTING_POLL;
                Applied::default()
            }
            Ok(snapshot) => {
                w.starting_until = None;
                let digest = digest(&snapshot);
                snapshot.track(&mut w.since, now);
                let changed = digest != w.digest || w.status != Status::Live || w.error.is_some() || w.snapshot.is_none();
                let alerts = match (&w.snapshot, w.status) {
                    (Some(old), Status::Live) => super::attention::alerts(old, &snapshot)
                        .map(|(p, worked)| super::attention::Alert { pane: p.pane_id.clone(), worked })
                        .collect(),
                    _ => Vec::new(),
                };
                (w.digest, w.snapshot, w.status, w.error) = (digest, Some(snapshot), Status::Live, None);
                (w.failures, w.fetched) = (0, Some(now));
                w.next_due = now + interval(kind, activity, 0);
                Applied { changed, stopped: false, alerts }
            }
            Err(e) if crate::herdr::no_server(&e) => {
                let changed = w.status != Status::Stopped;
                (w.status, w.snapshot, w.digest, w.error, w.failures) = (Status::Stopped, None, 0, None, 0);
                w.next_due = now + Duration::from_secs(60);
                Applied { changed, stopped: changed, alerts: Vec::new() }
            }
            Err(e) => {
                let before = (w.status, w.error.clone());
                w.failures += 1;
                if w.failures >= 2 {
                    w.status = Status::Unreachable;
                }
                w.error = Some(format!("{e:#}"));
                w.next_due = now + interval(kind, activity, w.failures);
                Applied { changed: before != (w.status, w.error.clone()), stopped: false, alerts: Vec::new() }
            }
        }
    }

    /// The session left keeps its last snapshot and its agents' clocks in its watcher, and is polled from now on.
    pub(super) fn park(&mut self, key: &SessionKey, snapshot: Option<Snapshot>, since: Since, now: Instant) {
        let Some(w) = self.map.get_mut(key) else { return };
        w.generation += 1;
        w.digest = snapshot.as_ref().map_or(0, digest);
        w.status = if snapshot.is_some() { Status::Live } else { Status::Connecting };
        w.fetched = snapshot.is_some().then_some(now);
        (w.snapshot, w.since, w.failures, w.error, w.next_due) = (snapshot, since, 0, None, now + interval(w.kind(), self.activity, 0));
    }

    /// The session switched to: its watcher's snapshot and clocks seed the window until the sync loop's own come.
    pub(super) fn unpark(&mut self, key: &SessionKey) -> (Option<Snapshot>, Since) {
        let Some(w) = self.map.get_mut(key) else { return Default::default() };
        w.generation += 1;
        w.status = Status::Shown;
        w.digest = 0;
        (w.snapshot.take(), std::mem::take(&mut w.since))
    }

    pub(super) fn get(&self, key: &SessionKey) -> Option<&Watcher> {
        self.map.get(key)
    }

    pub(super) fn snapshot(&self, key: &SessionKey) -> Option<&Snapshot> {
        self.map.get(key)?.snapshot.as_ref()
    }

    /// The last snapshot of every session not shown that has one, but for those out of reach: what they last said
    /// may be long over.
    pub(super) fn snapshots(&self) -> impl Iterator<Item = &Snapshot> {
        self.map.values().filter(|w| !matches!(w.status, Status::Shown | Status::Unreachable)).filter_map(|w| w.snapshot.as_ref())
    }

    /// The key's place in the tree, for element ids that tell sessions' rows apart.
    pub(super) fn index_of(&self, key: &SessionKey) -> Option<usize> {
        self.order.iter().position(|k| k == key)
    }

    pub(super) fn activity(&self) -> Activity {
        self.activity
    }

    /// Asked again now: Retry.
    pub(super) fn poll_now(&mut self, key: &SessionKey, now: Instant) {
        if let Some(w) = self.map.get_mut(key) {
            w.next_due = now;
        }
    }

    /// A start of the session from the tree (its menu or its notice): it shows connecting and is asked every second
    /// for a while. False while a start is already under way, or for a session not in the tree: the click is ignored,
    /// so two clicks never run two servers.
    pub(super) fn start(&mut self, key: &SessionKey, now: Instant) -> bool {
        let Some(w) = self.map.get_mut(key) else { return false };
        if w.starting(now) {
            return false;
        }
        w.starting_until = Some(now + STARTING);
        if w.status != Status::Shown {
            w.status = Status::Connecting;
        }
        (w.failures, w.error, w.next_due, w.listed_running) = (0, None, now + STARTING_POLL, true);
        true
    }

    /// A session started from the tree that has not answered yet.
    pub(super) fn starting(&self, key: &SessionKey, now: Instant) -> bool {
        self.map.get(key).is_some_and(|w| w.starting(now))
    }

    /// The window came to the front, or went behind: back in front, every session is asked at once.
    pub(super) fn set_active(&mut self, active: bool, now: Instant) {
        self.active = active;
        self.inactive_since = if active { None } else { self.inactive_since.or(Some(now)) };
        if active {
            for w in self.map.values_mut() {
                w.next_due = w.next_due.min(now);
            }
        }
    }

    /// How much the person is around: `touched` is the last key or click in the window, `system_idle` the machine's
    /// own idle time (macOS only; zero elsewhere), `locked` its screen lock.
    pub(super) fn activity_now(&self, now: Instant, touched: Instant, system_idle: Duration, locked: bool) -> Activity {
        activity(self.active, self.inactive_since, now, touched, system_idle, locked)
    }

    /// The machines' watchers that have no relay yet and are due a poll: a relay that failed to start waits out its
    /// backoff as a failed poll does.
    pub(super) fn without_client(&self, now: Instant) -> Vec<(SessionKey, Machine)> {
        self.order
            .iter()
            .filter_map(|k| match self.map.get(k) {
                Some(Watcher { place: Place::Machine(m), client: None, status, next_due, in_flight: false, .. })
                    if *status != Status::Shown && now >= *next_due =>
                {
                    Some((k.clone(), m.clone()))
                }
                _ => None,
            })
            .collect()
    }

    pub(super) fn set_client(&mut self, key: &SessionKey, client: Client) {
        if let Some(w) = self.map.get_mut(key) {
            w.client = Some(client);
        }
    }

    /// Starts reading herdr's list unless a read is under way, which then reads it again when it lands. True when
    /// the caller should read it now.
    pub(super) fn start_listing(&mut self) -> bool {
        if self.listing {
            self.relist = true;
            return false;
        }
        self.listing = true;
        true
    }

    /// The read landed: true when another was asked for meanwhile.
    pub(super) fn listed(&mut self, now: Instant) -> bool {
        self.listing = false;
        self.listed = Some(now);
        std::mem::take(&mut self.relist)
    }

    /// herdr's list is due another read: 15 s on with the window in front, a minute behind.
    pub(super) fn list_due(&self, now: Instant) -> bool {
        let every = if self.active { LIST_ACTIVE } else { LIST_INACTIVE };
        !self.listing && self.listed.is_none_or(|at| now.duration_since(at) >= every)
    }
}

/// Idle once the machine has been left alone for a while (macOS says so), its screen locked, or (anywhere) when
/// nothing was typed or clicked in the window, or it stayed behind other windows, for that long.
fn activity(active: bool, inactive_since: Option<Instant>, now: Instant, touched: Instant, system_idle: Duration, locked: bool) -> Activity {
    let behind_long = inactive_since.is_some_and(|at| now.duration_since(at) > IDLE_AFTER);
    if locked || system_idle > IDLE_AFTER || now.duration_since(touched) > IDLE_AFTER || behind_long {
        Activity::Idle
    } else if active {
        Activity::Active
    } else {
        Activity::Inactive
    }
}

/// How long a watcher waits between polls: a session here every 3 s with the window in front, 5 s behind it and 15 s
/// once the machine is left alone; a machine over SSH every 10 s, 20 s behind (a minute on Windows, which shares no
/// SSH connection between them). Each failure in a row doubles it, up to a minute here and two over SSH.
pub(super) fn interval(kind: Kind, activity: Activity, failures: u32) -> Duration {
    interval_on(kind, activity, failures, cfg!(windows))
}

fn interval_on(kind: Kind, activity: Activity, failures: u32, windows: bool) -> Duration {
    let (base, cap) = match (kind, activity) {
        (Kind::Local, Activity::Active) => (3, 60),
        (Kind::Local, Activity::Inactive) => (5, 60),
        (Kind::Local, Activity::Idle) => (15, 60),
        (Kind::Remote, _) if windows => (60, 120),
        (Kind::Remote, Activity::Active) => (10, 120),
        (Kind::Remote, _) => (20, 120),
    };
    Duration::from_secs((base * 2u64.saturating_pow(failures.min(16))).min(cap.max(base)))
}

/// How long one poll may take: 2 s here, 10 s over SSH, 15 s on first contact or after a failure (the SSH
/// connection may have to be made).
fn timeout(kind: Kind, failures: u32, first: bool) -> Duration {
    match kind {
        Kind::Local => Duration::from_secs(2),
        Kind::Remote if first || failures > 0 => Duration::from_secs(15),
        Kind::Remote => Duration::from_secs(10),
    }
}

/// What the tree draws of a snapshot: its workspaces, tabs and agents' states, not titles or layouts, so a poll that
/// changes only those renders nothing.
pub(super) fn digest(s: &Snapshot) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.focused_tab_id.hash(&mut h);
    for w in &s.workspaces {
        (&w.workspace_id, &w.label, w.number, &w.active_tab_id, format!("{:?}", w.agent_status)).hash(&mut h);
        w.worktree.as_ref().map(|t| (&t.repo_key, t.is_linked_worktree, &t.checkout_path)).hash(&mut h);
    }
    for t in &s.tabs {
        (&t.tab_id, &t.workspace_id).hash(&mut h);
    }
    for p in &s.panes {
        (&p.pane_id, &p.tab_id, &p.workspace_id, format!("{:?}", p.agent_status), &p.agent, &p.cwd, p.state_change_seq).hash(&mut h);
    }
    h.finish()
}

impl Shell {
    /// The scheduler: once a second, the watchers due a poll are asked, each on the runtime with its deadline; herdr's
    /// list is read again when due. Runs while the window lives.
    pub(super) fn start_watching(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            if this.update(cx, |s, cx| s.tick_watchers(cx)).is_err() {
                return; // window closed
            }
        })
        .detach();
    }

    fn tick_watchers(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let activity = self.watchers.activity_now(now, self.touched, mac_idle(), screen_locked());
        if self.watchers.list_due(now) {
            self.refresh_places(cx);
        }
        for (key, machine) in self.watchers.without_client(now) {
            match crate::herdr::relay(&machine, &self.rt) {
                Ok(socket) => self.watchers.set_client(&key, Client::at(socket)),
                Err(e) => {
                    if self.watchers.apply(&key, Err(e), now).changed {
                        cx.notify();
                    }
                }
            }
        }
        for (key, client, timeout, generation) in self.watchers.due(now, activity) {
            let call = self.rt.spawn(async move {
                match tokio::time::timeout(timeout, client.call("session.snapshot", json!({}))).await {
                    Ok(answer) => answer,
                    Err(_) => Err(anyhow::anyhow!("no answer in {} s", timeout.as_secs())),
                }
            });
            cx.spawn(async move |this, cx| {
                let answer = call.await.map_err(anyhow::Error::from).and_then(|r| r);
                let parsed = cx.background_executor().spawn(async move { answer.and_then(|r| Snapshot::from_result(&r)) }).await;
                let _ = this.update(cx, |s, cx| {
                    let applied = s.watchers.answer(&key, generation, parsed, Instant::now());
                    s.tell_elsewhere(&key, &applied.alerts, cx);
                    // its parked panes follow it: all go when it stopped, those it no longer backs when it changed
                    let parked = match s.watchers.snapshot(&key) {
                        _ if applied.stopped => s.views.drop_panes(&key),
                        Some(snapshot) if applied.changed => s.views.prune_against(&key, snapshot),
                        _ => Vec::new(),
                    };
                    s.retire_panes(parked, cx);
                    if applied.stopped {
                        s.refresh_places(cx);
                    }
                    if applied.changed {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// The shown session's key: as herdr's list names it, else (before the list came) a machine's by its id, and this
    /// machine's session by the socket Shepherd talks to, so folds are kept under the right key from the first frame
    /// and an alert posted then names its session.
    pub(super) fn shown_key(&self) -> Option<SessionKey> {
        shown_key_of(self.shown_place(), self.machine_id.as_deref(), &crate::herdr::api_socket())
    }
}

/// The shown session's key: its place's key once herdr's list has it, else a machine's by its id (its watcher's key,
/// known before the list comes), else this machine's session by its socket. Keys must match the watchers' and the
/// tree's ([`SessionKey::of`]): a notification's routing and the alerts already told go by them.
fn shown_key_of(listed: Option<String>, machine_id: Option<&str>, socket: &std::path::Path) -> Option<SessionKey> {
    Some(SessionKey(match (listed, machine_id) {
        (Some(key), _) => key,
        (None, Some(id)) => format!("machine:{id}"),
        (None, None) => socket.to_string_lossy().into_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::SessionInfo;

    fn session(name: &str, running: bool) -> Place {
        Place::Session(SessionInfo { name: name.into(), running, socket_path: format!("/tmp/{name}.sock"), default: name == "default" })
    }

    fn machine(id: &str) -> Place {
        Place::Machine(Machine { id: id.into(), label: id.into(), target: format!("you@{id}"), session: "default".into(), enabled: true })
    }

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    fn snapshot(workspaces: &[&str]) -> Snapshot {
        let ws: Vec<_> = workspaces
            .iter()
            .enumerate()
            .map(|(i, id)| json!({"workspace_id": id, "label": id, "number": i + 1, "active_tab_id": null, "agent_status": "idle"}))
            .collect();
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": ws, "tabs": [], "panes": [], "layouts": []}})).unwrap()
    }

    fn refused() -> anyhow::Error {
        anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)).context("herdr is not reachable")
    }

    #[test]
    fn intervals_back_off_to_their_caps() {
        use Activity::*;
        assert_eq!(interval_on(Kind::Local, Active, 0, false), Duration::from_secs(3));
        assert_eq!(interval_on(Kind::Local, Inactive, 0, false), Duration::from_secs(5));
        assert_eq!(interval_on(Kind::Local, Idle, 0, false), Duration::from_secs(15));
        assert_eq!(interval_on(Kind::Remote, Active, 0, false), Duration::from_secs(10));
        assert_eq!(interval_on(Kind::Remote, Inactive, 0, false), Duration::from_secs(20));
        assert_eq!(interval_on(Kind::Remote, Active, 0, true), Duration::from_secs(60), "Windows shares no SSH connection");
        assert_eq!(interval_on(Kind::Local, Active, 2, false), Duration::from_secs(12));
        assert_eq!(interval_on(Kind::Local, Active, 9, false), Duration::from_secs(60));
        assert_eq!(interval_on(Kind::Remote, Active, 9, false), Duration::from_secs(120));
        assert_eq!(interval_on(Kind::Remote, Active, 99, true), Duration::from_secs(120));
    }

    #[test]
    fn reconcile_adds_and_removes_and_leaves_the_shown_one_unpolled() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("default", true), session("menqal", true), session("old", false)], Some(&key("default")), now);
        assert_eq!(w.get(&key("default")).unwrap().status, Status::Shown);
        assert_eq!(w.get(&key("menqal")).unwrap().status, Status::Connecting);
        assert_eq!(w.get(&key("old")).unwrap().status, Status::Stopped);
        let due: Vec<_> = w.due(now, Activity::Active).into_iter().map(|(k, _, _, _)| k).collect();
        assert_eq!(due, [key("menqal")], "neither the shown session nor a stopped one");
        assert!(w.due(now, Activity::Active).is_empty(), "one call in flight each");
        w.reconcile(&[session("default", true), machine("box")], Some(&key("default")), now);
        assert!(w.get(&key("menqal")).is_none() && w.get(&key("old")).is_none());
        assert_eq!(w.index_of(&SessionKey("machine:box".into())), Some(1));
        assert_eq!(w.without_client(now).len(), 1, "a machine's relay starts on its first poll");
        // a relay that failed to start waits out its backoff
        let m = SessionKey("machine:box".into());
        w.apply(&m, Err(anyhow::anyhow!("no room for the socket")), now);
        assert!(w.without_client(now).is_empty());
        assert_eq!(w.without_client(now + Duration::from_secs(20)).len(), 1);
    }

    #[test]
    fn a_session_started_elsewhere_is_asked_at_once() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", false)], None, now);
        assert!(w.due(now, Activity::Active).is_empty());
        w.reconcile(&[session("menqal", true)], None, now);
        assert_eq!(w.get(&key("menqal")).unwrap().status, Status::Connecting);
        assert_eq!(w.due(now, Activity::Active).len(), 1);
    }

    #[test]
    fn apply_follows_answers_and_failures() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", true)], None, now);
        let k = key("menqal");
        w.due(now, Activity::Active);
        assert_eq!(w.apply(&k, Ok(snapshot(&["w1"])), now), Applied { changed: true, stopped: false, alerts: vec![] });
        assert_eq!(w.get(&k).unwrap().status, Status::Live);
        // the same snapshot again draws nothing new
        assert_eq!(w.apply(&k, Ok(snapshot(&["w1"])), now), Applied::default());
        assert!(w.apply(&k, Ok(snapshot(&["w1", "w2"])), now).changed);
        // one failure keeps it live, waiting longer; two make it unreachable
        assert!(w.apply(&k, Err(anyhow::anyhow!("no answer in 2 s")), now).changed);
        assert_eq!(w.get(&k).unwrap().status, Status::Live);
        assert!(w.due(now + Duration::from_secs(3), Activity::Active).is_empty(), "backed off to 6 s");
        assert_eq!(w.due(now + Duration::from_secs(6), Activity::Active).len(), 1);
        w.apply(&k, Err(anyhow::anyhow!("no answer in 2 s")), now);
        assert_eq!(w.get(&k).unwrap().status, Status::Unreachable);
        assert!(w.get(&k).unwrap().snapshot.is_some(), "its last snapshot stays");
        // an answer brings it back
        assert!(w.apply(&k, Ok(snapshot(&["w1", "w2"])), now).changed);
        assert_eq!((w.get(&k).unwrap().status, w.get(&k).unwrap().failures), (Status::Live, 0));
    }

    #[test]
    fn no_server_is_stopped_and_never_started_here() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", true)], None, now);
        let k = key("menqal");
        w.due(now, Activity::Active);
        assert_eq!(w.apply(&k, Err(refused()), now), Applied { changed: true, stopped: true, alerts: vec![] });
        assert_eq!(w.get(&k).unwrap().status, Status::Stopped);
        assert_eq!(w.apply(&k, Err(refused()), now), Applied::default(), "told once");
        // herdr's list still says it runs: asked again a minute on, not every second
        w.reconcile(&[session("menqal", true)], None, now);
        assert!(w.due(now + Duration::from_secs(5), Activity::Active).is_empty());
        assert_eq!(w.due(now + Duration::from_secs(61), Activity::Active).len(), 1);
    }

    #[test]
    fn a_switch_parks_and_unparks_snapshots() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("default", true), session("menqal", true)], Some(&key("default")), now);
        w.due(now, Activity::Active);
        w.apply(&key("menqal"), Ok(snapshot(&["w1"])), now);
        // default left for menqal
        w.park(&key("default"), Some(snapshot(&["w7"])), Default::default(), now);
        let (seed, _) = w.unpark(&key("menqal"));
        assert_eq!(seed.unwrap().workspaces[0].workspace_id, "w1");
        assert_eq!(w.get(&key("menqal")).unwrap().status, Status::Shown);
        assert_eq!(w.get(&key("default")).unwrap().status, Status::Live);
        assert_eq!(w.snapshot(&key("default")).unwrap().workspaces[0].workspace_id, "w7");
        // an answer asked for before the switch is dropped
        assert_eq!(w.apply(&key("menqal"), Ok(snapshot(&["w9"])), now), Applied::default());
        w.reconcile(&[session("default", true), session("menqal", true)], Some(&key("menqal")), now);
        assert_eq!(w.get(&key("default")).unwrap().status, Status::Live);
    }

    #[test]
    fn a_start_shows_connecting_until_the_server_answers() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", false)], None, now);
        let k = key("menqal");
        assert!(w.start(&k, now));
        assert!(!w.start(&k, now + Duration::from_secs(2)), "a second click starts nothing");
        assert!(w.starting(&k, now));
        assert_eq!(w.get(&k).unwrap().status, Status::Connecting);
        // a list read before the server came up still says stopped
        w.reconcile(&[session("menqal", false)], None, now);
        assert_eq!(w.get(&k).unwrap().status, Status::Connecting);
        // no server yet: asked again a second later, still connecting
        assert_eq!(w.due(now + STARTING_POLL, Activity::Active).len(), 1);
        assert_eq!(w.apply(&k, Err(refused()), now + STARTING_POLL), Applied::default());
        assert_eq!(w.get(&k).unwrap().status, Status::Connecting);
        assert_eq!(w.due(now + STARTING_POLL * 2, Activity::Active).len(), 1);
        assert!(w.apply(&k, Ok(snapshot(&["w1"])), now + STARTING_POLL * 2).changed);
        assert_eq!(w.get(&k).unwrap().status, Status::Live);
        assert!(!w.starting(&k, now + STARTING_POLL * 2));
        // once the start is over, a missing server is news again
        assert!(w.start(&k, now + STARTING * 2));
        w.due(now + STARTING * 4, Activity::Active);
        assert!(w.apply(&k, Err(refused()), now + STARTING * 4).stopped);
    }

    #[test]
    fn retry_does_not_make_a_stopped_session_look_running() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", false)], None, now);
        w.poll_now(&key("menqal"), now);
        assert!(w.due(now, Activity::Active).is_empty());
    }

    #[test]
    fn idle_without_a_key_or_with_the_window_behind_for_long() {
        let now = Instant::now();
        let long = IDLE_AFTER + Duration::from_secs(1);
        let later = now + long;
        assert_eq!(activity(true, None, now, now, Duration::ZERO, false), Activity::Active);
        assert_eq!(activity(false, Some(now), now, now, Duration::ZERO, false), Activity::Inactive);
        assert_eq!(activity(true, None, later, now, Duration::ZERO, false), Activity::Idle, "nothing typed for long");
        assert_eq!(activity(false, Some(now), later, later, Duration::ZERO, false), Activity::Idle, "behind for long");
        assert_eq!(activity(true, None, now, now, long, false), Activity::Idle, "the machine left alone");
        assert_eq!(activity(true, None, now, now, Duration::ZERO, true), Activity::Idle, "locked");
        let mut w = Watchers::new();
        w.set_active(false, now);
        w.set_active(false, now + Duration::from_secs(5));
        assert_eq!(w.inactive_since, Some(now), "behind since the first time");
        w.set_active(true, later);
        assert_eq!(w.inactive_since, None);
    }

    fn agents(panes: &[(&str, &str)]) -> Snapshot {
        let panes: Vec<_> = panes
            .iter()
            .map(|(id, status)| json!({"pane_id": id, "tab_id": "t1", "workspace_id": "w1", "terminal_id": id, "agent_status": status, "agent": "claude"}))
            .collect();
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": [], "tabs": [], "panes": panes, "layouts": []}})).unwrap()
    }

    fn told(applied: Applied) -> Vec<String> {
        applied.alerts.into_iter().map(|a| a.pane).collect()
    }

    #[test]
    fn alerts_come_only_between_two_live_answers() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", true)], None, now);
        let k = key("menqal");
        assert!(w.apply(&k, Ok(agents(&[("p1", "blocked")])), now).alerts.is_empty(), "the first answer is a baseline");
        assert!(w.apply(&k, Ok(agents(&[("p1", "working")])), now).alerts.is_empty());
        assert_eq!(told(w.apply(&k, Ok(agents(&[("p1", "done")])), now)), ["p1"]);
        // one failure keeps it live; two make it unreachable, and the answer after that is a baseline again
        w.apply(&k, Ok(agents(&[("p1", "working")])), now);
        w.apply(&k, Err(anyhow::anyhow!("no answer in 2 s")), now);
        assert_eq!(told(w.apply(&k, Ok(agents(&[("p1", "blocked")])), now)), ["p1"], "still live after one failure");
        w.apply(&k, Ok(agents(&[("p1", "working")])), now);
        w.apply(&k, Err(anyhow::anyhow!("no answer in 2 s")), now);
        w.apply(&k, Err(anyhow::anyhow!("no answer in 2 s")), now);
        assert_eq!(w.get(&k).unwrap().status, Status::Unreachable);
        assert!(w.apply(&k, Ok(agents(&[("p1", "blocked")])), now).alerts.is_empty(), "no burst after unreachable");
        // stopped drops the snapshot: its first answer once started again tells nothing
        w.apply(&k, Err(refused()), now);
        assert!(w.apply(&k, Ok(agents(&[("p1", "done")])), now).alerts.is_empty(), "no burst after stopped");
    }

    #[test]
    fn a_parked_session_alerts_against_the_snapshot_it_was_left_with() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("default", true), session("menqal", true)], Some(&key("default")), now);
        w.park(&key("default"), Some(agents(&[("p1", "working"), ("p2", "working")])), Default::default(), now);
        assert_eq!(told(w.apply(&key("default"), Ok(agents(&[("p1", "blocked"), ("p2", "working")])), now)), ["p1"]);
        assert_eq!(w.snapshots().count(), 1, "a parked session counts toward the badge");
        // the shown session's answers are the sync loop's: nothing comes of them here
        w.unpark(&key("default"));
        assert_eq!(w.snapshots().count(), 0, "the shown session's snapshot is the window's");
        assert!(w.apply(&key("default"), Ok(agents(&[("p1", "working"), ("p2", "done")])), now).alerts.is_empty());
    }

    #[test]
    fn an_answer_asked_for_before_a_switch_is_dropped() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("default", true), session("menqal", true)], Some(&key("default")), now);
        let k = key("menqal");
        w.apply(&k, Ok(agents(&[("p1", "working")])), now);
        let due = w.due(now + Duration::from_secs(5), Activity::Active);
        let generation = due.iter().find(|d| d.0 == k).unwrap().3;
        // switched to menqal and back while the poll was out: parked with a snapshot newer than the poll's
        w.unpark(&k);
        w.park(&k, Some(agents(&[("p1", "done")])), Default::default(), now);
        let stale = w.answer(&k, generation, Ok(agents(&[("p1", "blocked")])), now);
        assert_eq!(stale, Applied::default(), "nothing drawn, nothing told");
        assert_eq!(w.snapshot(&k).unwrap().panes[0].agent_status, AgentStatus::Done, "the parked snapshot stays");
        let fresh = w.due(now + Duration::from_secs(10), Activity::Active).into_iter().find(|d| d.0 == k).unwrap().3;
        assert_eq!(w.answer(&k, fresh, Ok(agents(&[("p1", "blocked")])), now).alerts.len(), 1, "polled again, answered at its own generation");
    }

    #[test]
    fn an_unreachable_session_leaves_the_badge() {
        let now = Instant::now();
        let mut w = Watchers::new();
        w.reconcile(&[session("menqal", true)], None, now);
        let k = key("menqal");
        w.apply(&k, Ok(agents(&[("p1", "blocked")])), now);
        assert_eq!(w.snapshots().count(), 1);
        w.apply(&k, Err(anyhow::anyhow!("no answer in 2 s")), now);
        assert_eq!(w.snapshots().count(), 1, "one failure: still live");
        w.apply(&k, Err(anyhow::anyhow!("no answer in 2 s")), now);
        assert_eq!(w.snapshots().count(), 0, "unreachable: its last word may be long over");
    }

    #[test]
    fn the_shown_key_is_the_one_the_tree_and_the_watchers_use() {
        let socket = std::path::Path::new("/tmp/default.sock");
        for place in [session("default", true), session("menqal", true), machine("box")] {
            assert_eq!(shown_key_of(Some(place.key()), None, socket), Some(SessionKey::of(&place)));
        }
        // before herdr's list names it: a machine by its id, a session here by the socket it is reached on
        assert_eq!(shown_key_of(None, Some("box"), socket), Some(SessionKey::of(&machine("box"))));
        assert_eq!(shown_key_of(None, None, socket), Some(SessionKey::of(&session("default", true))));
    }

    #[test]
    fn listing_runs_one_at_a_time() {
        let now = Instant::now();
        let mut w = Watchers::new();
        assert!(w.list_due(now));
        assert!(w.start_listing());
        assert!(!w.start_listing(), "one read in flight");
        assert!(w.listed(now), "the second ask reads again");
        assert!(!w.list_due(now + Duration::from_secs(5)));
        assert!(w.list_due(now + LIST_ACTIVE));
        w.set_active(false, now);
        assert!(!w.list_due(now + LIST_ACTIVE));
    }
}
