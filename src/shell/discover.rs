//! Sessions on a host: a machine row's Sessions on this host… lists the herdr sessions running there or stopped, over
//! SSH (`remote_sessions`), less those saved as machines already, and a click on one saves it as a machine too
//! (`herdr machine add`, which starts a stopped one). Add SSH machine (`HostFlow`) asks for the host first, reads
//! the same list, and adds the sessions picked from it, or one named in New session…; nothing else. The list is read
//! once a popup or the dialog asks, never in the background.
use super::*;
use crate::herdr::{AddError, ListError, Machine, SessionInfo};

/// The popup's host and what its list said.
pub(super) struct Discover {
    pub target: String,
    found: Found,
    /// Bumped on each read of the host's list, so one that lands after a newer one began is dropped.
    run: usize,
}

/// The sessions being added as machines, by (SSH target, session), kept beside the tree rather than in a popup: a
/// popup closed and opened again mid add still knows, and no second `herdr machine add` starts for the session.
#[derive(Default)]
pub(super) struct Adds(HashMap<(String, String), Add>);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Add {
    /// `herdr machine add` runs.
    Running,
    /// It finished, saved or not listed yet: guarded until a read of herdr's list begun after `at` lands.
    Waiting { at: Instant },
}

impl Adds {
    /// Whether `session` on `target` may be added now: false while it is being added already (a second click, a
    /// double click, the popup opened again).
    pub(super) fn begin(&mut self, target: &str, session: &str) -> bool {
        let key = (target.to_string(), session.to_string());
        if self.0.contains_key(&key) {
            return false;
        }
        self.0.insert(key, Add::Running);
        true
    }

    /// The add finished at `at`: a failure frees the row at once; a success, or herdr's list not having the machine
    /// yet, keeps it guarded until herdr's list is read again (`listed`).
    pub(super) fn finished(&mut self, target: &str, session: &str, failed: bool, at: Instant) {
        let key = (target.to_string(), session.to_string());
        if failed {
            self.0.remove(&key);
        } else {
            self.0.insert(key, Add::Waiting { at });
        }
    }

    /// A read of herdr's list begun at `started` landed: the adds finished before it are in it, or not, by now.
    pub(super) fn listed(&mut self, started: Instant) {
        self.0.retain(|_, add| !matches!(add, Add::Waiting { at } if *at <= started));
    }

    fn busy(&self, target: &str, session: &str) -> bool {
        self.0.contains_key(&(target.to_string(), session.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Found {
    Loading,
    Failed(ListError),
    Listed(Vec<SessionInfo>),
}

/// What the popup shows, from [`view`].
#[derive(Clone, Debug, PartialEq)]
pub(super) enum View {
    Loading,
    /// Why the list could not be read, and what to do.
    Failed(String),
    /// Every session on the host is saved already.
    Empty(String),
    Rows(Vec<Row>),
}

/// A session that could be added: its name and the line under it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub name: String,
    pub note: &'static str,
    /// Being added: not clicked again.
    pub adding: bool,
    /// Picked in Add SSH machine, to be added with the others picked.
    pub picked: bool,
}

impl Discover {
    pub(super) fn new(target: String) -> Self {
        Discover { target, found: Found::Loading, run: 0 }
    }

    /// A new read of the host's list begins: its number, for [`Self::listed`].
    fn relist(&mut self) -> usize {
        self.found = Found::Loading;
        self.run += 1;
        self.run
    }

    /// The read numbered `run` landed: kept unless a newer one began meanwhile.
    fn listed(&mut self, run: usize, found: Result<Vec<SessionInfo>, ListError>) {
        if run == self.run {
            self.found = match found {
                Ok(list) => Found::Listed(list),
                Err(e) => Found::Failed(e),
            };
        }
    }
}

/// What the popup for `d` shows, given herdr's saved machines (turned on or off) and the adds under way; `None` once
/// none is for its host any more (all forgotten meanwhile), when the popup closes. A session saved since the list was
/// read leaves it, so each render reads it again from what is saved.
pub(super) fn view(d: &Discover, saved: &[Machine], adds: &Adds) -> Option<View> {
    saved.iter().find(|m| m.target == d.target)?;
    let host = &d.target;
    Some(match &d.found {
        Found::Loading => View::Loading,
        Found::Failed(e) => View::Failed(list_failure(e, host, saved)),
        Found::Listed(found) => {
            let rows: Vec<Row> = crate::herdr::unsaved(found, saved, host)
                .into_iter()
                .map(|s| offered(&s, host, adds, false))
                .collect();
            if rows.is_empty() { View::Empty(format!("No other sessions on {host}")) } else { View::Rows(rows) }
        }
    })
}

/// A session on the host as a row to pick: being added already (`adds`) it is not picked again.
fn offered(s: &SessionInfo, host: &str, adds: &Adds, picked: bool) -> Row {
    let adding = adds.busy(host, &s.name);
    let note = match (adding, s.running, s.default) {
        (true, ..) => "Adding…",
        (false, true, _) => "running",
        (false, false, true) => "herdr's default session (adding starts it)",
        (false, false, false) => "stopped (adding starts it)",
    };
    Row { name: s.name.clone(), note, adding, picked: picked && !adding }
}

/// A session's row to pick, as Sessions on this host… and Add SSH machine list them: its icon, its name and note, a
/// spinner while it is being added, and a check once picked.
pub(super) fn session_choice(theme: &Theme, id: (&'static str, usize), r: &Row) -> Stateful<Div> {
    row(theme, false, id)
        .mx(px(-8.))
        .h(px(40.))
        .rounded(px(6.))
        .child(gpui::svg().path("icons/remote-server.svg").size(px(14.)).flex_none().text_color(theme.text_muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().child(r.name.clone()))
                .child(div().truncate().text_size(px(11.)).text_color(theme.text_faint).child(r.note)),
        )
        .when(r.picked, |el| el.child(gpui::svg().path("icons/check.svg").size(px(14.)).flex_none().text_color(theme.accent)))
        .when(r.adding, |el| el.child(spinner(theme, format!("{}-adding-{}", id.0, id.1))).text_color(theme.text_faint))
}

/// Why the host's sessions could not be read, and what to do: as the machine menu's Reconnect… says it, Shepherd's
/// SSH runs without a prompt; a host with no machine saved yet has none to reconnect, so a plain `ssh` signs in.
pub(super) fn list_failure(e: &ListError, host: &str, saved: &[Machine]) -> String {
    match e {
        ListError::SignIn => {
            let command = match saved.iter().find(|m| m.target == host) {
                Some(m) => crate::herdr::lifecycle::reconnect_command(&m.id),
                None => format!("ssh {host}"),
            };
            format!("Shepherd's SSH never asks for a password: run `{command}` in a terminal, then Try again.")
        }
        ListError::NotInstalled => format!("herdr is not installed on {host}."),
        ListError::Failed(why) => format!("Couldn't list the sessions on {host}: {why}"),
    }
}

/// Where Add SSH machine is: the host typed, its sessions read, those to add picked, being added.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Stage {
    /// The host is typed; nothing is read yet.
    Host,
    /// The host's sessions are being read.
    Listing,
    /// Read: the sessions to add are picked.
    Choose,
    /// They could not be read.
    Failed(ListError),
    /// The sessions picked are being added; the names left.
    Adding(Vec<String>),
    /// Every one picked was added: the dialog closes.
    Done,
}

/// What Enter and the dialog's main button do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Primary {
    /// Read the typed host's sessions.
    Connect,
    /// Add the sessions picked.
    Add,
    /// Nothing until what runs lands.
    Wait,
}

/// What Add SSH machine lists to pick from: a line about the host, and its sessions not saved yet.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Choices {
    pub line: Option<String>,
    pub rows: Vec<Row>,
}

/// Add SSH machine: the host first, its herdr sessions read over SSH as Sessions on this host… reads them, then the
/// ones picked added, or a new one named in New session…. A session is only ever added because it was picked.
#[derive(Debug)]
pub(super) struct HostFlow {
    /// This dialog's flow among every one opened: an add or read that lands is routed by it (`route`).
    pub id: u64,
    pub stage: Stage,
    /// The host read, or being read.
    pub target: String,
    found: Vec<SessionInfo>,
    /// Bumped on each read: one that lands after a newer one began is dropped.
    run: usize,
    /// The host's sessions picked, in the order clicked.
    picked: Vec<String>,
    /// New session… is picked: its name field shows.
    pub naming: bool,
    /// Why the last Add or Connect did not go ahead, said under the list.
    pub problem: Option<String>,
    /// The machines this dialog's adds saved: the first is switched to once they are done.
    pub saved: Vec<Machine>,
}

impl HostFlow {
    pub(super) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        HostFlow { id, stage: Stage::Host, target: String::new(), found: Vec::new(), run: 0, picked: Vec::new(), naming: false, problem: None, saved: Vec::new() }
    }

    /// What Enter does with `host` typed: a host other than the one read is read first.
    pub(super) fn primary(&self, host: &str) -> Primary {
        let other = host.trim().is_empty() || host.trim() != self.target;
        match &self.stage {
            Stage::Adding(_) | Stage::Done => Primary::Wait,
            _ if other => Primary::Connect,
            Stage::Listing => Primary::Wait,
            Stage::Choose => Primary::Add,
            Stage::Host | Stage::Failed(_) => Primary::Connect,
        }
    }

    /// The rows listed are another host's than the one typed (`host`): they are dimmed and not picked, and this
    /// says what to do.
    pub(super) fn stale(&self, host: &str) -> Option<String> {
        let host = host.trim();
        if host == self.target || !matches!(self.stage, Stage::Choose | Stage::Failed(_)) {
            return None;
        }
        Some(if host.is_empty() { "Type a host, then Connect.".to_string() } else { format!("Press Connect to read {host}.") })
    }

    /// The Remote switch holds while sessions are being added: their results come back to this flow.
    pub(super) fn locks_remote(&self) -> bool {
        matches!(self.stage, Stage::Adding(_))
    }

    /// Reads `host`'s sessions: the read's number, for [`Self::listed`]; or why not.
    pub(super) fn connect(&mut self, host: &str) -> Result<usize, &'static str> {
        let host = host.trim();
        if matches!(self.stage, Stage::Adding(_) | Stage::Done) {
            return Err("Wait for the sessions being added.");
        }
        if host.is_empty() {
            return Err("Which host? An SSH target such as user@host or a Host from ~/.ssh/config.");
        }
        if host != self.target {
            self.picked.clear();
        }
        self.target = host.to_string();
        self.found.clear();
        self.problem = None;
        self.stage = Stage::Listing;
        self.run += 1;
        Ok(self.run)
    }

    /// The read numbered `run` landed: kept unless a newer one began meanwhile.
    pub(super) fn listed(&mut self, run: usize, found: Result<Vec<SessionInfo>, ListError>) {
        if run != self.run || self.stage != Stage::Listing {
            return;
        }
        match found {
            Ok(list) => {
                self.found = list;
                self.stage = Stage::Choose;
            }
            Err(e) => self.stage = Stage::Failed(e),
        }
    }

    /// The rows to pick from, given herdr's saved machines and the adds under way: the host's sessions not saved
    /// yet; a host with none at all offers its default session, which herdr starts when it is added.
    pub(super) fn choices(&self, saved: &[Machine], adds: &Adds) -> Choices {
        let host = &self.target;
        let row = |s: &SessionInfo| offered(s, host, adds, self.picked.contains(&s.name));
        if self.found.is_empty() {
            let default = SessionInfo { name: "default".into(), running: false, socket_path: String::new(), default: true };
            let rows = crate::herdr::unsaved(&[default], saved, host).iter().map(row).collect();
            return Choices { line: Some(format!("No herdr sessions on {host} yet.")), rows };
        }
        let rows: Vec<Row> = crate::herdr::unsaved(&self.found, saved, host).iter().map(row).collect();
        let line = rows.is_empty().then(|| format!("Every session on {host} is added already."));
        Choices { line, rows }
    }

    /// A click on a session's row: picked, or no longer.
    pub(super) fn pick(&mut self, name: &str) {
        if self.stage != Stage::Choose {
            return;
        }
        match self.picked.iter().position(|p| p == name) {
            Some(at) => {
                self.picked.remove(at);
            }
            None => self.picked.push(name.to_string()),
        }
        self.problem = None;
    }

    /// A click on New session…: its name field shows, or goes.
    pub(super) fn toggle_naming(&mut self) {
        if self.stage == Stage::Choose {
            self.naming = !self.naming;
            self.problem = None;
        }
    }

    /// The sessions Add adds: those picked that are still offered, and the one named in New session… (`name`); or
    /// why there are none to add.
    pub(super) fn chosen(&self, name: &str, saved: &[Machine], adds: &Adds) -> Result<Vec<String>, String> {
        if self.stage != Stage::Choose {
            return Err("Connect to the host first.".into());
        }
        let rows = self.choices(saved, adds).rows;
        let mut out: Vec<String> = self.picked.iter().filter(|p| rows.iter().any(|r| &r.name == *p && !r.adding)).cloned().collect();
        if self.naming {
            let name = name.trim();
            if name.is_empty() {
                return Err("Name the new session, or pick one above.".into());
            }
            if !crate::herdr::valid_session_name(name) {
                return Err("A session's name takes ASCII letters, digits, '.', '_' and '-'.".into());
            }
            if saved.iter().any(|m| m.target == self.target && m.session == name) {
                return Err(format!("{name} on {} is added already.", self.target));
            }
            if adds.busy(&self.target, name) {
                return Err(format!("{name} on {} is being added.", self.target));
            }
            if !out.iter().any(|p| p == name) {
                out.push(name.to_string());
            }
        }
        // every one picked is being added already, from here or Sessions on this host…
        if out.is_empty() && self.picked.iter().any(|p| rows.iter().any(|r| &r.name == p && r.adding)) {
            return Err(ALREADY_ADDING.into());
        }
        if out.is_empty() {
            return Err("Pick a session to add, or New session….".into());
        }
        Ok(out)
    }

    /// The sessions `names` are being added.
    pub(super) fn adding(&mut self, names: Vec<String>) {
        if !names.is_empty() {
            self.problem = None;
            self.stage = Stage::Adding(names);
        }
    }

    /// The add of `name` finished: saved, it is picked no more; else why goes under the list. Once the last lands,
    /// the dialog closes when every one was saved, and offers the rest again when one was not.
    pub(super) fn added(&mut self, name: &str, added: &Result<Machine, AddError>) {
        let Stage::Adding(left) = &mut self.stage else { return };
        left.retain(|n| n != name);
        let done = left.is_empty();
        match added {
            Ok(machine) => {
                self.picked.retain(|p| p != name);
                self.saved.push(machine.clone());
            }
            Err(_) => {
                let note = add_note(added, &self.target, name).unwrap_or_default();
                self.problem = Some(match self.problem.take() {
                    Some(before) => format!("{before}\n{note}"),
                    None => note,
                });
            }
        }
        if done {
            self.stage = if self.problem.is_none() { Stage::Done } else { Stage::Choose };
        }
    }
}

/// What Add says when every session picked is being added already.
pub(super) const ALREADY_ADDING: &str = "Already being added.";

/// Where an add's (or a read's) result goes: into the dialog while the flow it was begun from (`flow`) is the one
/// the open dialog holds (`open`), else a toast. Whether Remote is on or which host is typed does not matter: a
/// dialog opened again holds another flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    Dialog,
    Toast,
}

pub(super) fn route(open: Option<u64>, flow: u64) -> Route {
    if open == Some(flow) { Route::Dialog } else { Route::Toast }
}

/// Add SSH machine's adds, run one after another: each begins only while its flow is still up, so a dialog closed
/// (or opened again) mid queue starts no more.
#[derive(Debug)]
pub(super) struct AddQueue {
    pub flow: u64,
    pub target: String,
    left: std::collections::VecDeque<String>,
    added: usize,
}

/// What an [`AddQueue`] does next.
#[derive(Debug, PartialEq)]
pub(super) enum Next {
    Add(String),
    /// Its flow is gone: these were not begun, and the toast says so.
    Cancelled(Vec<String>, String),
    Finished,
}

impl AddQueue {
    pub(super) fn new(flow: u64, target: String, names: Vec<String>) -> Self {
        AddQueue { flow, target, left: names.into(), added: 0 }
    }

    /// The next add, given the flow the open dialog holds now (`open`).
    pub(super) fn next(&mut self, open: Option<u64>) -> Next {
        if self.left.is_empty() {
            return Next::Finished;
        }
        if route(open, self.flow) == Route::Toast {
            let names: Vec<String> = self.left.drain(..).collect();
            let note = format!("{} added on {}, {} cancelled when the dialog closed.", self.added, self.target, names.len());
            return Next::Cancelled(names, note);
        }
        Next::Add(self.left.pop_front().expect("not empty"))
    }

    /// The add begun last landed, saved (`saved`) or not.
    pub(super) fn landed(&mut self, saved: bool) {
        if saved {
            self.added += 1;
        }
    }
}

/// The toast after an add from the popup, when it did not save a listed machine.
pub(super) fn add_note(added: &Result<Machine, AddError>, host: &str, session: &str) -> Option<String> {
    match added {
        Ok(_) => None,
        Err(AddError::Unsaved) => Some(format!("{session} on {host} was added, but herdr does not list it yet.")),
        Err(e) => Some(format!("Couldn't add {session} on {host}: {e}")),
    }
}

impl Shell {
    /// Sessions on this host…: the popup for `target`, its list being read.
    pub(super) fn open_discover(&mut self, target: String, cx: &mut Context<Self>) {
        self.tree.discover = Some(crate::ui::Popup::new(Discover::new(target)));
        self.list_discover(cx);
    }

    /// Reads the popup's host's sessions over SSH (15 s at most), into the popup if it is still up for that host.
    fn list_discover(&mut self, cx: &mut Context<Self>) {
        let Some(popup) = self.tree.discover.as_mut().filter(|p| p.is_open()) else { return };
        let run = popup.value.relist();
        let target = popup.value.target.clone();
        let list = self.rt.spawn({
            let target = target.clone();
            async move { crate::herdr::remote_sessions(&target).await }
        });
        cx.spawn(async move |this, cx| {
            let found = list.await.unwrap_or_else(|e| Err(ListError::Failed(e.to_string())));
            let _ = this.update(cx, |s, cx| {
                if let Some(popup) = s.tree.discover.as_mut().filter(|p| p.is_open() && p.value.target == target) {
                    popup.value.listed(run, found);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Saves the session `name` of the popup's host as a machine, then reads herdr's list again; the session shown
    /// stays. One being added already ignores the click.
    fn add_discovered(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(target) = self.tree.discover.as_ref().filter(|p| p.is_open()).map(|p| p.value.target.clone()) else { return };
        if !self.tree.adds.begin(&target, &name) {
            return;
        }
        let task = self.rt.spawn({
            let (target, name) = (target.clone(), name.clone());
            async move { crate::herdr::add_machine(&target, &name).await }
        });
        cx.spawn(async move |this, cx| {
            let added = task.await.unwrap_or_else(|e| Err(AddError::Failed(e.to_string())));
            let _ = this.update(cx, |s, cx| {
                let failed = matches!(added, Err(AddError::Failed(_) | AddError::TimedOut));
                s.tree.adds.finished(&target, &name, failed, Instant::now());
                if let Some(note) = add_note(&added, &target, &name) {
                    crate::ui::toast::error(cx, note);
                }
                s.refresh_places(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Add SSH machine's Connect: the typed host's sessions read over SSH (15 s at most), into the dialog while it
    /// is still up on that read; an empty host says what to type instead.
    pub(super) fn connect_add_machine(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.session_dialog.as_mut().filter(|d| d.is_open() && d.value.remote) else { return };
        let host = dialog.value.host.read(cx).value().to_string();
        let flow = &mut dialog.value.flow;
        let run = match flow.connect(&host) {
            Ok(run) => run,
            Err(why) => {
                flow.problem = Some(why.into());
                return cx.notify();
            }
        };
        let (target, flow_id) = (flow.target.clone(), flow.id);
        let list = self.rt.spawn({
            let target = target.clone();
            async move { crate::herdr::remote_sessions(&target).await }
        });
        cx.spawn(async move |this, cx| {
            let found = list.await.unwrap_or_else(|e| Err(ListError::Failed(e.to_string())));
            let _ = this.update(cx, |s, cx| {
                // a dialog closed and opened again meanwhile holds another flow
                if let Some(d) = s.session_dialog.as_mut().filter(|d| route(d.is_open().then_some(d.value.flow.id), flow_id) == Route::Dialog) {
                    d.value.flow.listed(run, found);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Add SSH machine's Add: the sessions picked (and the one named in New session…) saved as machines, one after
    /// another; a session being added already, from here or Sessions on this host…, is not added again. Nothing is
    /// added that was not picked.
    pub(super) fn add_picked_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.session_dialog.as_ref().filter(|d| d.is_open() && d.value.remote).map(|d| d.value.flow.target.clone()) else { return };
        let saved = self.machine_records(&target);
        let Some(dialog) = self.session_dialog.as_mut() else { return };
        let name = dialog.value.name.read(cx).value().to_string();
        let flow = &mut dialog.value.flow;
        let names = match flow.chosen(&name, &saved, &self.tree.adds) {
            Ok(names) => names,
            Err(why) => {
                flow.problem = Some(why);
                return cx.notify();
            }
        };
        let begun: Vec<String> = names.into_iter().filter(|n| self.tree.adds.begin(&target, n)).collect();
        if begun.is_empty() {
            flow.problem = Some(ALREADY_ADDING.into());
            return cx.notify();
        }
        flow.adding(begun.clone());
        let mut queue = AddQueue::new(flow.id, target, begun);
        let rt = self.rt.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(next) = this.update(cx, |s, _| queue.next(s.open_flow())) else { return }; // window closed
                let name = match next {
                    Next::Add(name) => name,
                    Next::Finished => return,
                    Next::Cancelled(names, note) => {
                        let _ = this.update(cx, |s, cx| {
                            for name in &names {
                                s.tree.adds.finished(&queue.target, name, true, Instant::now());
                            }
                            crate::ui::toast::info(cx, note, None);
                            cx.notify();
                        });
                        return;
                    }
                };
                let task = rt.spawn({
                    let (target, name) = (queue.target.clone(), name.clone());
                    async move { crate::herdr::add_machine(&target, &name).await }
                });
                let added = task.await.unwrap_or_else(|e| Err(AddError::Failed(e.to_string())));
                queue.landed(added.is_ok());
                let flow = queue.flow;
                let target = queue.target.clone();
                if this.update(cx, |s, cx| s.session_added(flow, &target, &name, added, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// The flow the open session dialog holds, if it is open.
    fn open_flow(&self) -> Option<u64> {
        self.session_dialog.as_ref().filter(|d| d.is_open()).map(|d| d.value.flow.id)
    }

    /// One add of Add SSH machine's flow `flow` landed: into the dialog while it holds that flow, else a toast says
    /// how it went. Once the last is saved, the dialog closes and the first machine saved is shown.
    fn session_added(&mut self, flow: u64, target: &str, name: &str, added: Result<Machine, AddError>, cx: &mut Context<Self>) {
        let failed = matches!(added, Err(AddError::Failed(_) | AddError::TimedOut));
        self.tree.adds.finished(target, name, failed, Instant::now());
        match (route(self.open_flow(), flow), self.session_dialog.as_mut()) {
            (Route::Dialog, Some(dialog)) => {
                dialog.value.flow.added(name, &added);
                if dialog.value.flow.stage == Stage::Done {
                    let first = dialog.value.flow.saved.first().cloned();
                    dialog.close();
                    self.focused = None; // an input held focus: back to herdr's focused pane
                    if let Some(machine) = first {
                        self.switch_machine(machine, cx);
                    }
                }
            }
            _ => match add_note(&added, target, name) {
                Some(note) => crate::ui::toast::error(cx, note),
                None => crate::ui::toast::info(cx, format!("{name} on {target} was added."), None),
            },
        }
        self.refresh_places(cx);
        cx.notify();
    }

    /// Closes the popup; the keyboard goes back to herdr's focused pane.
    fn close_discover(&mut self, cx: &mut Context<Self>) {
        if let Some(popup) = self.tree.discover.as_mut() {
            popup.close();
        }
        self.focused = None;
        cx.notify();
    }

    /// The popup closes once its host has no machine left to add to; called each frame.
    pub(super) fn reap_discover(&mut self, window: &mut Window) {
        let gone = self.tree.discover.as_ref().filter(|p| p.is_open()).is_some_and(|p| view(&p.value, &self.machine_records(&p.value.target), &self.tree.adds).is_none());
        if gone && let Some(popup) = self.tree.discover.as_mut() {
            popup.close();
        }
        crate::ui::reap_popup(&mut self.tree.discover, window);
    }

    /// The popup over the window, while it is up.
    pub(super) fn discover_popup(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let popup = self.tree.discover.as_ref()?;
        let target = popup.value.target.clone();
        // closing: the last frames show the list as it was
        let shown = view(&popup.value, &self.machine_records(&target), &self.tree.adds).unwrap_or(View::Loading);
        let line = |text: String| div().mt(px(8.)).line_height(px(19.)).text_color(theme.text_muted).child(text);
        let mut card = dialog_card(theme, "discover-dialog", "Sessions on this host")
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_discover(cx)))
            .track_focus(&self.confirm_focus)
            .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_discover(cx);
                }
            }))
            .child(div().mt(px(2.)).text_size(px(12.)).text_color(theme.text_faint).truncate().child(target.clone()));
        let mut buttons = div().mt(px(16.)).flex().justify_end().gap(px(8.));
        match shown {
            View::Loading => card = card.child(div().mt(px(12.)).flex().items_center().gap(px(8.)).text_color(theme.text_muted).child(spinner(theme, "discover-loading".into())).child("Asking the host…")),
            View::Failed(why) => {
                card = card.child(line(why));
                buttons = buttons.child(ghost_button(theme, "discover-retry", "Try again").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.list_discover(cx))));
            }
            View::Empty(text) => {
                card = card.child(line(text));
                let host = target.clone();
                buttons = buttons.child(ghost_button(theme, "discover-new", "New session on this host…").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.close_discover(cx);
                    this.open_add_machine(Some(host.clone()), true, window, cx);
                })));
            }
            View::Rows(rows) => {
                let list = rows.into_iter().enumerate().map(|(i, r)| {
                    let name = r.name.clone();
                    session_choice(theme, ("discover-session", i), &r)
                        .when(!r.adding, |el| el.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.add_discovered(name.clone(), cx))))
                });
                card = card.child(div().mt(px(10.)).flex().flex_col().gap(px(2.)).children(list));
            }
        }
        card = card.child(buttons.child(solid_button("discover-close", "Close", theme.text, theme.surface).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_discover(cx)))));
        Some(modal("discover-dialog", popup, window, card).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, running: bool) -> SessionInfo {
        SessionInfo { name: name.into(), running, socket_path: format!("/home/you/.config/herdr/sessions/{name}/herdr.sock"), default: name == "default" }
    }

    fn saved(target: &str, session: &str) -> Machine {
        Machine { id: format!("m-{session}"), label: format!("workbox/{session}"), target: target.into(), session: session.into(), enabled: true }
    }

    fn none() -> Adds {
        Adds::default()
    }

    fn listed(found: Vec<SessionInfo>) -> Discover {
        let mut d = Discover::new("you@workbox".into());
        let run = d.relist();
        d.listed(run, Ok(found));
        d
    }

    fn rows(v: Option<View>) -> Vec<(String, &'static str, bool)> {
        match v {
            Some(View::Rows(rows)) => rows.into_iter().map(|r| (r.name, r.note, r.adding)).collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_popup_lists_the_sessions_not_saved_and_says_which_are_stopped() {
        let d = listed(vec![info("docs", false), info("agents", true), info("default", true)]);
        let machines = [saved("you@workbox", "agents"), saved("you@otherbox", "docs")];
        assert_eq!(
            rows(view(&d, &machines, &none())),
            [("default".to_string(), "running", false), ("docs".to_string(), "stopped (adding starts it)", false)],
            "agents is saved for this host; docs only for another"
        );
        let all = [saved("you@workbox", "agents"), saved("you@workbox", "docs"), saved("you@workbox", "default")];
        assert_eq!(view(&d, &all, &none()), Some(View::Empty("No other sessions on you@workbox".into())));
    }

    #[test]
    fn a_session_being_added_is_not_added_twice() {
        let d = listed(vec![info("default", true), info("docs", false)]);
        let machines = [saved("you@workbox", "agents")];
        let mut adds = none();
        let t0 = Instant::now();
        assert!(adds.begin("you@workbox", "docs"));
        assert!(!adds.begin("you@workbox", "docs"), "a second click while it is added");
        assert!(adds.begin("you@otherbox", "docs"), "the same name on another host is another session");
        assert_eq!(rows(view(&d, &machines, &adds))[1], ("docs".to_string(), "Adding…", true));
        adds.finished("you@workbox", "docs", true, t0);
        assert!(adds.begin("you@workbox", "docs"), "clickable again after a failure");
        // added, or herdr not listing it yet: guarded until a list read begun after it lands
        let done = t0 + Duration::from_secs(1);
        adds.finished("you@workbox", "docs", false, done);
        assert!(!adds.begin("you@workbox", "docs"));
        adds.listed(t0);
        assert!(!adds.begin("you@workbox", "docs"), "a read begun before the add finished");
        adds.listed(done + Duration::from_secs(1));
        assert!(adds.begin("you@workbox", "docs"), "the next read");
        // added: herdr's list has it now, and the row goes
        let machines = [saved("you@workbox", "agents"), saved("you@workbox", "docs")];
        assert_eq!(rows(view(&d, &machines, &adds)), [("default".to_string(), "running", false)]);
    }

    #[test]
    fn an_add_under_way_is_known_when_the_popup_opens_again() {
        let machines = [saved("you@workbox", "agents")];
        let mut adds = none();
        let first = listed(vec![info("docs", false)]);
        assert!(adds.begin(&first.target, "docs"));
        drop(first);
        let again = listed(vec![info("docs", false)]);
        assert_eq!(rows(view(&again, &machines, &adds)), [("docs".to_string(), "Adding…", true)]);
        assert!(!adds.begin(&again.target, "docs"));
    }

    #[test]
    fn an_add_that_saved_no_machine_says_why() {
        let m = saved("you@workbox", "docs");
        assert_eq!(add_note(&Ok(m), "you@workbox", "docs"), None);
        assert_eq!(add_note(&Err(AddError::Unsaved), "you@workbox", "docs").as_deref(), Some("docs on you@workbox was added, but herdr does not list it yet."));
        assert_eq!(add_note(&Err(AddError::Failed("no route".into())), "you@workbox", "docs").as_deref(), Some("Couldn't add docs on you@workbox: no route"));
    }

    #[test]
    fn the_popup_closes_once_its_host_has_no_machine() {
        let d = listed(vec![info("default", true)]);
        assert!(view(&d, &[saved("you@workbox", "agents")], &none()).is_some());
        let off = Machine { enabled: false, ..saved("you@workbox", "agents") };
        assert!(view(&d, &[off], &none()).is_some(), "turned off is still saved");
        assert_eq!(view(&d, &[saved("you@otherbox", "agents")], &none()), None, "forgotten");
        assert_eq!(view(&d, &[], &none()), None);
    }

    #[test]
    fn a_failed_list_says_what_to_do() {
        let machines = [saved("you@workbox", "agents")];
        let failed = |e: ListError| {
            let mut d = Discover::new("you@workbox".into());
            let run = d.relist();
            d.listed(run, Err(e));
            view(&d, &machines, &none())
        };
        assert_eq!(
            failed(ListError::SignIn),
            Some(View::Failed("Shepherd's SSH never asks for a password: run `herdr machine reconnect m-agents` in a terminal, then Try again.".into()))
        );
        assert_eq!(failed(ListError::NotInstalled), Some(View::Failed("herdr is not installed on you@workbox.".into())));
        assert_eq!(
            failed(ListError::Failed("no answer from you@workbox in 15 s".into())),
            Some(View::Failed("Couldn't list the sessions on you@workbox: no answer from you@workbox in 15 s".into()))
        );
        assert_eq!(view(&Discover::new("you@workbox".into()), &machines, &none()), Some(View::Loading));
    }

    #[test]
    fn an_older_read_landing_late_is_dropped() {
        let mut d = Discover::new("you@workbox".into());
        let old = d.relist();
        let new = d.relist();
        d.listed(old, Err(ListError::NotInstalled));
        assert_eq!(d.found, Found::Loading);
        d.listed(new, Ok(vec![info("default", true)]));
        assert_eq!(d.found, Found::Listed(vec![info("default", true)]));
    }

    /// Add SSH machine read `you@workbox` and listed `found`.
    fn flow_listed(found: Vec<SessionInfo>) -> HostFlow {
        let mut f = HostFlow::new();
        let run = f.connect(" you@workbox ").unwrap();
        f.listed(run, Ok(found));
        f
    }

    fn choice_rows(c: &Choices) -> Vec<(&str, &'static str, bool, bool)> {
        c.rows.iter().map(|r| (r.name.as_str(), r.note, r.picked, r.adding)).collect()
    }

    #[test]
    fn add_ssh_machine_goes_from_the_host_to_its_sessions() {
        let mut f = HostFlow::new();
        assert_eq!(f.stage, Stage::Host);
        assert_eq!(f.primary(""), Primary::Connect);
        assert_eq!(f.connect("  "), Err("Which host? An SSH target such as user@host or a Host from ~/.ssh/config."));
        assert_eq!(f.stage, Stage::Host, "nothing read without a host");
        let run = f.connect("you@workbox").unwrap();
        assert_eq!((f.stage.clone(), f.target.as_str()), (Stage::Listing, "you@workbox"));
        assert_eq!(f.primary("you@workbox"), Primary::Wait, "Enter waits for the list");
        assert_eq!(f.primary("you@otherbox"), Primary::Connect, "another host typed meanwhile is read instead");
        f.listed(run, Ok(vec![info("default", true), info("docs", false)]));
        assert_eq!(f.stage, Stage::Choose);
        assert_eq!(f.primary("you@workbox"), Primary::Add);
        assert_eq!(f.primary("you@otherbox"), Primary::Connect);
        let c = f.choices(&[saved("you@workbox", "default")], &none());
        assert_eq!(choice_rows(&c), [("docs", "stopped (adding starts it)", false, false)], "the saved one left out");
        assert_eq!(c.line, None);
        f.pick("docs");
        assert_eq!(f.chosen("", &[saved("you@workbox", "default")], &none()), Ok(vec!["docs".to_string()]));
        f.adding(vec!["docs".into()]);
        assert_eq!(f.stage, Stage::Adding(vec!["docs".into()]));
        assert_eq!(f.primary("you@workbox"), Primary::Wait, "a second Enter adds nothing more");
        assert!(f.connect("you@workbox").is_err());
        let m = saved("you@workbox", "docs");
        f.added("docs", &Ok(m.clone()));
        assert_eq!(f.stage, Stage::Done);
        assert_eq!(f.saved, [m], "the one switched to");
    }

    #[test]
    fn nothing_is_added_that_was_not_picked() {
        let f = flow_listed(vec![info("default", true), info("docs", false)]);
        assert_eq!(f.chosen("", &[], &none()), Err("Pick a session to add, or New session….".into()));
        assert_eq!(f.chosen("agents", &[], &none()), Err("Pick a session to add, or New session….".into()), "a name typed without New session…");
        let mut f = f;
        f.pick("docs");
        f.pick("default");
        f.pick("docs");
        assert_eq!(f.chosen("", &[], &none()), Ok(vec!["default".to_string()]), "a second click unpicks");
        // picked, then saved elsewhere meanwhile: not added again
        assert_eq!(f.chosen("", &[saved("you@workbox", "default")], &none()), Err("Pick a session to add, or New session….".into()));
        // nothing picked before the list is read
        let mut early = HostFlow::new();
        early.connect("you@workbox").unwrap();
        early.pick("docs");
        early.toggle_naming();
        assert!(!early.naming);
        assert_eq!(early.chosen("docs", &[], &none()), Err("Connect to the host first.".into()));
    }

    #[test]
    fn new_session_adds_the_one_named() {
        let mut f = flow_listed(vec![info("default", true)]);
        f.toggle_naming();
        assert!(f.naming);
        assert_eq!(f.chosen(" ", &[], &none()), Err("Name the new session, or pick one above.".into()));
        assert_eq!(f.chosen("my box", &[], &none()), Err("A session's name takes ASCII letters, digits, '.', '_' and '-'.".into()));
        assert_eq!(f.chosen("agents", &[saved("you@workbox", "agents")], &none()), Err("agents on you@workbox is added already.".into()));
        assert_eq!(f.chosen(" agents ", &[], &none()), Ok(vec!["agents".to_string()]));
        f.pick("default");
        assert_eq!(f.chosen("agents", &[], &none()), Ok(vec!["default".to_string(), "agents".to_string()]));
        assert_eq!(f.chosen("default", &[], &none()), Ok(vec!["default".to_string()]), "named as one picked: once");
        f.toggle_naming();
        assert_eq!(f.chosen("agents", &[], &none()), Ok(vec!["default".to_string()]), "New session… unpicked: the name goes unused");
    }

    #[test]
    fn a_host_without_sessions_offers_its_default_one() {
        let f = flow_listed(vec![]);
        let c = f.choices(&[], &none());
        assert_eq!(c.line.as_deref(), Some("No herdr sessions on you@workbox yet."));
        assert_eq!(choice_rows(&c), [("default", "herdr's default session (adding starts it)", false, false)], "offered, not picked");
        assert!(f.chosen("", &[], &none()).is_err());
        let mut f = f;
        f.pick("default");
        assert_eq!(f.chosen("", &[], &none()), Ok(vec!["default".to_string()]));
        // every session saved already: only New session… is left
        let all = flow_listed(vec![info("docs", true)]);
        let c = all.choices(&[saved("you@workbox", "docs")], &none());
        assert_eq!((c.line.as_deref(), c.rows.len()), (Some("Every session on you@workbox is added already."), 0));
    }

    #[test]
    fn add_ssh_machine_never_adds_a_session_twice() {
        let mut adds = none();
        assert!(adds.begin("you@workbox", "docs"), "from Sessions on this host…");
        let mut f = flow_listed(vec![info("docs", false), info("agents", true)]);
        f.pick("docs");
        let c = f.choices(&[], &adds);
        assert_eq!(choice_rows(&c), [("agents", "running", false, false), ("docs", "Adding…", false, true)]);
        assert_eq!(f.chosen("", &[], &adds), Err(ALREADY_ADDING.into()), "being added already: said so, nothing added");
        f.toggle_naming();
        assert_eq!(f.chosen("docs", &[], &adds), Err("docs on you@workbox is being added.".into()));
        f.toggle_naming();
        f.pick("agents");
        assert_eq!(f.chosen("", &[], &adds), Ok(vec!["agents".to_string()]));
    }

    #[test]
    fn a_failed_add_stays_open_with_why_and_the_rest_saved() {
        let mut f = flow_listed(vec![info("docs", false), info("agents", true)]);
        f.pick("docs");
        f.pick("agents");
        f.adding(vec!["docs".into(), "agents".into()]);
        f.added("docs", &Err(AddError::Failed("no route".into())));
        assert_eq!(f.stage, Stage::Adding(vec!["agents".into()]), "waits for the last");
        f.added("agents", &Ok(saved("you@workbox", "agents")));
        assert_eq!(f.stage, Stage::Choose);
        assert_eq!(f.problem.as_deref(), Some("Couldn't add docs on you@workbox: no route"));
        assert_eq!(f.chosen("", &[saved("you@workbox", "agents")], &none()), Ok(vec!["docs".to_string()]), "the failed one still picked");
        // a late landing outside an add changes nothing
        f.added("docs", &Ok(saved("you@workbox", "docs")));
        assert_eq!(f.stage, Stage::Choose);
    }

    #[test]
    fn add_ssh_machine_says_why_a_host_could_not_be_read() {
        let mut f = HostFlow::new();
        let old = f.connect("you@workbox").unwrap();
        let new = f.connect("you@workbox").unwrap();
        f.listed(old, Ok(vec![info("default", true)]));
        assert_eq!(f.stage, Stage::Listing, "an older read landing late is dropped");
        f.listed(new, Err(ListError::SignIn));
        assert_eq!(f.stage, Stage::Failed(ListError::SignIn));
        assert_eq!(f.primary("you@workbox"), Primary::Connect, "Try again");
        assert_eq!(list_failure(&ListError::SignIn, "you@workbox", &[]), "Shepherd's SSH never asks for a password: run `ssh you@workbox` in a terminal, then Try again.");
        assert_eq!(
            list_failure(&ListError::SignIn, "you@workbox", &[saved("you@otherbox", "a"), saved("you@workbox", "agents")]),
            "Shepherd's SSH never asks for a password: run `herdr machine reconnect m-agents` in a terminal, then Try again.",
            "a host saved already reconnects its machine"
        );
        assert_eq!(list_failure(&ListError::NotInstalled, "you@workbox", &[]), "herdr is not installed on you@workbox.");
        assert_eq!(list_failure(&ListError::Failed("no answer from you@workbox in 15 s".into()), "you@workbox", &[]), "Couldn't list the sessions on you@workbox: no answer from you@workbox in 15 s");
        // another host typed after a pick starts afresh
        let mut f = flow_listed(vec![info("docs", true)]);
        f.pick("docs");
        let run = f.connect("you@otherbox").unwrap();
        f.listed(run, Ok(vec![info("docs", true)]));
        assert!(f.chosen("", &[], &none()).is_err(), "docs was picked on you@workbox");
    }

    #[test]
    fn an_add_lands_in_the_flow_it_was_begun_from() {
        let flow = HostFlow::new();
        assert_eq!(route(Some(flow.id), flow.id), Route::Dialog, "the dialog still up");
        assert_eq!(route(None, flow.id), Route::Toast, "the dialog closed");
        let again = HostFlow::new();
        assert_ne!(again.id, flow.id);
        assert_eq!(route(Some(again.id), flow.id), Route::Toast, "opened again: another flow");
        // Remote can't be flipped off while its adds run, and with the same flow up they come back to it
        let mut f = flow_listed(vec![info("docs", false)]);
        assert!(!f.locks_remote());
        f.pick("docs");
        f.adding(vec!["docs".into()]);
        assert!(f.locks_remote());
        assert_eq!(route(Some(f.id), f.id), Route::Dialog);
        f.added("docs", &Ok(saved("you@workbox", "docs")));
        assert!(!f.locks_remote());
    }

    #[test]
    fn a_dialog_closed_mid_queue_begins_no_more_adds() {
        let mut q = AddQueue::new(7, "you@workbox".into(), vec!["docs".into(), "agents".into(), "notes".into()]);
        assert_eq!(q.next(Some(7)), Next::Add("docs".into()));
        q.landed(true);
        assert_eq!(q.next(None), Next::Cancelled(vec!["agents".into(), "notes".into()], "1 added on you@workbox, 2 cancelled when the dialog closed.".into()));
        assert_eq!(q.next(Some(7)), Next::Finished, "none left to begin");
        let mut q = AddQueue::new(7, "you@workbox".into(), vec!["docs".into(), "agents".into()]);
        assert_eq!(q.next(Some(7)), Next::Add("docs".into()));
        q.landed(false);
        assert_eq!(q.next(Some(8)), Next::Cancelled(vec!["agents".into()], "0 added on you@workbox, 1 cancelled when the dialog closed.".into()), "opened again");
        let mut q = AddQueue::new(7, "you@workbox".into(), vec!["docs".into()]);
        assert_eq!(q.next(Some(7)), Next::Add("docs".into()));
        assert_eq!(q.next(Some(7)), Next::Finished);
    }

    #[test]
    fn another_host_typed_dims_the_old_list() {
        let f = flow_listed(vec![info("docs", false)]);
        assert_eq!(f.stale("you@workbox"), None);
        assert_eq!(f.stale(" you@workbox "), None);
        assert_eq!(f.stale("you@otherbox").as_deref(), Some("Press Connect to read you@otherbox."));
        assert_eq!(f.stale("").as_deref(), Some("Type a host, then Connect."));
        assert_eq!(f.primary("you@otherbox"), Primary::Connect);
        let mut failed = HostFlow::new();
        let run = failed.connect("you@workbox").unwrap();
        failed.listed(run, Err(ListError::NotInstalled));
        assert_eq!(failed.stale("you@otherbox").as_deref(), Some("Press Connect to read you@otherbox."));
        assert_eq!(HostFlow::new().stale("you@workbox"), None, "nothing listed yet");
    }

    /// Add SSH machine's list as it comes over SSH from a stand-in host, and a host where herdr is missing.
    #[cfg(unix)]
    #[tokio::test]
    async fn add_ssh_machine_reads_the_host_over_ssh() {
        use crate::herdr::fake::{fake_ssh, host};
        let json = r#"{"sessions":[{"name":"default","running":true,"socket_path":"/s/d","default":true},{"name":"docs","running":false,"socket_path":"/s/o"}]}"#;
        let box_ = host(Some(&format!("printf '%s\\n' '{json}'")));
        let mut f = HostFlow::new();
        let run = f.connect("you@workbox").unwrap();
        f.listed(run, crate::herdr::sessions_over(fake_ssh(&box_, "echo 'Welcome to workbox'").as_os_str(), "you@workbox", std::time::Duration::from_secs(5)).await);
        assert_eq!(choice_rows(&f.choices(&[], &none())), [("default", "running", false, false), ("docs", "stopped (adding starts it)", false, false)]);
        let empty = host(Some(r#"printf '%s\n' '{"sessions":[]}'"#));
        let run = f.connect("you@workbox").unwrap();
        f.listed(run, crate::herdr::sessions_over(fake_ssh(&empty, "true").as_os_str(), "you@workbox", std::time::Duration::from_secs(5)).await);
        assert_eq!(f.choices(&[], &none()).line.as_deref(), Some("No herdr sessions on you@workbox yet."));
        let bare = host(None);
        let run = f.connect("you@workbox").unwrap();
        f.listed(run, crate::herdr::sessions_over(fake_ssh(&bare, "true").as_os_str(), "you@workbox", std::time::Duration::from_secs(5)).await);
        assert_eq!(f.stage, Stage::Failed(ListError::NotInstalled));
    }

    /// The popup's list as it comes over SSH from a stand-in host: past its banner, the saved session left out.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hosts_list_over_ssh_becomes_the_popups_rows() {
        use crate::herdr::fake::{fake_ssh, host};
        let json = r#"{"sessions":[{"name":"default","running":true,"socket_path":"/s/d","default":true},{"name":"agents","running":true,"socket_path":"/s/a"},{"name":"docs","running":false,"socket_path":"/s/o"}]}"#;
        let box_ = host(Some(&format!("printf '%s\\n' '{json}'")));
        let ssh = fake_ssh(&box_, "echo 'Welcome to workbox'");
        let found = crate::herdr::sessions_over(ssh.as_os_str(), "you@workbox", std::time::Duration::from_secs(5)).await;
        let mut d = Discover::new("you@workbox".into());
        let run = d.relist();
        d.listed(run, found);
        assert_eq!(
            rows(view(&d, &[saved("you@workbox", "agents")], &none())),
            [("default".to_string(), "running", false), ("docs".to_string(), "stopped (adding starts it)", false)]
        );
        let denied = host(None);
        let refused = crate::herdr::sessions_over(fake_ssh(&denied, "echo 'Permission denied (publickey).' >&2; exit 255").as_os_str(), "you@workbox", std::time::Duration::from_secs(5)).await;
        let mut d = Discover::new("you@workbox".into());
        let run = d.relist();
        d.listed(run, refused);
        assert!(matches!(view(&d, &[saved("you@workbox", "agents")], &none()), Some(View::Failed(why)) if why.contains("herdr machine reconnect m-agents")));
    }
}
