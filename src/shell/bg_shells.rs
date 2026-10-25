//! Agents resting with shells still running in the background. Claude Code runs a task it backgrounds as a child of
//! its own, in a process session of its own, with its output going to a `…/tasks/<id>.output` file; herdr reports
//! the agent idle (or done) as soon as its turn ends, however many of those still run. Shepherd finds them in the
//! process table (Linux `/proc`), a few seconds apart and off the UI thread, and the sidebar draws such an agent as
//! working, with the count. Local sessions only: an SSH machine's pids are not this machine's.
use super::watchers::{Kind, SessionKey};
use super::*;
use crate::herdr::Client;
use crate::model::{Pane, Workspace};
use std::path::Path;

/// Where the process table is.
const PROC: &str = "/proc";

/// How long herdr may take to name a pane's processes, and a whole scan.
const ASK: Duration = Duration::from_secs(1);
const SCAN: Duration = Duration::from_secs(5);

/// An agent and its pane's shell, as herdr named them once; they stay valid while the agent's process is still in
/// the shell's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Known {
    pub agent: u32,
    pub shell: u32,
}

/// What a scan found of one agent pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    /// The terminal it was found in: a pane id herdr gives another terminal later is another agent.
    pub terminal: String,
    pub known: Known,
    /// Its background shells now.
    pub count: usize,
}

/// An agent pane to scan: at rest, in a session on this machine.
pub(super) struct Target {
    pub key: SessionKey,
    pub client: Client,
    pub pane: String,
    pub terminal: String,
    /// Its agent from the scan before, when still the pane's.
    pub known: Option<Known>,
}

type PaneKey = (SessionKey, String);

/// The scans' result, and when the next one is due.
#[derive(Default)]
pub(super) struct Shells {
    found: HashMap<PaneKey, Found>,
    in_flight: bool,
    next: Option<Instant>,
}

impl Shells {
    /// Whether a scan starts now: none is out and `every` has passed since the last began. Marks the next due.
    pub(super) fn begin(&mut self, now: Instant, every: Duration) -> bool {
        if self.in_flight || self.next.is_some_and(|at| now < at) {
            return false;
        }
        (self.in_flight, self.next) = (true, Some(now + every));
        true
    }

    /// A scan's results replace everything: a pane not in them is no longer at rest, or gone, and its mark with it.
    /// Whether what the sidebar draws changed.
    pub(super) fn finish(&mut self, results: Vec<(PaneKey, Found)>) -> bool {
        self.in_flight = false;
        let next: HashMap<_, _> = results.into_iter().collect();
        std::mem::replace(&mut self.found, next) != self.found
    }

    /// The scan ended without a result (the runtime stopped).
    pub(super) fn abandon(&mut self) {
        self.in_flight = false;
    }

    /// What the next scan starts from, per pane: its agent from the scan before, while the pane holds the terminal
    /// it was found in.
    pub(super) fn known(&self, key: &SessionKey, pane: &str, terminal: &str) -> Option<Known> {
        self.found.get(&(key.clone(), pane.to_string())).filter(|f| f.terminal == terminal).map(|f| f.known)
    }

    /// The pane's background shells, while it rests in the terminal they were found in: an agent at work or
    /// asking for you is drawn as what it is.
    pub(super) fn of_pane(&self, key: &SessionKey, pane: &Pane) -> usize {
        let rests = pane.agent.is_some() && matches!(pane.agent_status, AgentStatus::Idle | AgentStatus::Done);
        self.found.get(&(key.clone(), pane.pane_id.clone())).filter(|f| rests && f.terminal == pane.terminal_id).map_or(0, |f| f.count)
    }

    /// A workspace's status as the sidebar draws it, and its background shells. Its agents at rest with shells work;
    /// a workspace herdr calls blocked stays so, and one done stays done while an agent without shells is.
    pub(super) fn workspace(&self, key: &SessionKey, snapshot: &Snapshot, w: &Workspace) -> (AgentStatus, usize) {
        if w.agent_status == AgentStatus::Blocked {
            return (AgentStatus::Blocked, 0);
        }
        let panes = || snapshot.panes.iter().filter(|p| p.workspace_id == w.workspace_id);
        let count: usize = panes().map(|p| self.of_pane(key, p)).sum();
        let waiting = panes().any(|p| p.agent.is_some() && p.agent_status == AgentStatus::Done && self.of_pane(key, p) == 0);
        let status = match w.agent_status {
            AgentStatus::Idle | AgentStatus::Done if count > 0 && !waiting => AgentStatus::Working,
            status => status,
        };
        (status, count)
    }
}

/// "1 shell", "3 shells".
pub(super) fn label(count: usize) -> String {
    format!("{count} shell{}", if count == 1 { "" } else { "s" })
}

/// The agent panes to scan: those at rest in sessions on this machine. A session over SSH is skipped whole.
pub(super) fn targets<'a>(sessions: impl IntoIterator<Item = (&'a SessionKey, Kind, &'a Snapshot, Option<&'a Client>)>, shells: &Shells) -> Vec<Target> {
    let mut out = Vec::new();
    for (key, kind, snapshot, client) in sessions {
        let Some(client) = client.filter(|_| kind == Kind::Local) else { continue };
        for p in snapshot.panes.iter().filter(|p| p.agent.is_some() && matches!(p.agent_status, AgentStatus::Idle | AgentStatus::Done)) {
            let known = shells.known(key, &p.pane_id, &p.terminal_id);
            out.push(Target { key: key.clone(), client: client.clone(), pane: p.pane_id.clone(), terminal: p.terminal_id.clone(), known });
        }
    }
    out
}

/// The scan: each target's agent (from the last scan while it still stands, else as herdr names it) and its
/// background shells under `proc`. A pane herdr cannot name, or whose agent is not there, has no result.
pub(super) async fn scan(targets: Vec<Target>, proc: &Path) -> Vec<(PaneKey, Found)> {
    let mut out = Vec::new();
    for t in targets {
        let kept = t.known.and_then(|k| Some((k, count(proc, k)?)));
        let found = match kept {
            Some(found) => Some(found),
            None => ask(&t, proc).await,
        };
        if let Some((known, count)) = found {
            out.push(((t.key, t.pane), Found { terminal: t.terminal, known, count }));
        }
    }
    out
}

/// herdr's word on the pane's agent, and its shells.
async fn ask(t: &Target, proc: &Path) -> Option<(Known, usize)> {
    let info = tokio::time::timeout(ASK, t.client.pane_process(&t.pane)).await.ok()?.ok()?;
    let known = Known { agent: info.leader_pid?, shell: info.shell_pid };
    Some((known, count(proc, known)?))
}

/// The background shells of the agent: its direct children in a process session other than its own whose standard
/// output is a Claude Code task file. None when the agent is not what `known` says (gone, a zombie, or its pid now
/// another process's outside the pane's session).
pub(super) fn count(proc: &Path, known: Known) -> Option<usize> {
    let agent = stat(proc, known.agent).filter(|s| s.state != 'Z' && s.session == known.shell)?;
    let mut shells = std::collections::HashSet::new();
    for task in std::fs::read_dir(proc.join(known.agent.to_string()).join("task")).ok()?.flatten() {
        let Ok(children) = std::fs::read_to_string(task.path().join("children")) else { continue };
        shells.extend(children.split_whitespace().filter_map(|c| c.parse::<u32>().ok()));
    }
    let is_shell = |pid: &u32| {
        stat(proc, *pid).is_some_and(|s| s.state != 'Z' && s.parent == known.agent && s.session != agent.session)
            && std::fs::read_link(proc.join(pid.to_string()).join("fd").join("1")).is_ok_and(|out| is_task_output(&out))
    };
    Some(shells.iter().filter(|pid| is_shell(pid)).count())
}

/// Claude Code's file for a backgrounded task's output: `/tmp/claude-<uid>/<project>/<session>/tasks/<id>.output`.
fn is_task_output(path: &Path) -> bool {
    let name = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_owned);
    path.extension().is_some_and(|e| e == "output")
        && path.parent().and_then(name).as_deref() == Some("tasks")
        && path.ancestors().skip(3).any(|a| name(a).is_some_and(|n| n.starts_with("claude-")))
}

/// The fields of `/proc/<pid>/stat` the scan reads.
struct Stat {
    state: char,
    parent: u32,
    session: u32,
}

/// `pid`'s state, parent and session; none when it is gone. The name in the line is in parentheses and may hold
/// spaces and parentheses itself, so the rest is read from the last one.
fn stat(proc: &Path, pid: u32) -> Option<Stat> {
    let line = std::fs::read_to_string(proc.join(pid.to_string()).join("stat")).ok()?;
    let mut rest = line[line.rfind(')')? + 1..].split_whitespace();
    let state = rest.next()?.chars().next()?;
    let parent = rest.next()?.parse().ok()?;
    let session = rest.nth(1)?.parse().ok()?;
    Some(Stat { state, parent, session })
}

impl Shell {
    /// Starts a scan when one is due: the shown session's agents and every other local session's, at rest. The
    /// timer is the scheduler's tick, not herdr's events, so a shell that ends clears its mark with none to tell it.
    pub(super) fn scan_shells(&mut self, now: Instant, every: Duration, cx: &mut Context<Self>) {
        if !cfg!(target_os = "linux") || !self.shells.begin(now, every) {
            return;
        }
        let shown_key = self.shown_key();
        // the shown session is followed by the sync loop, the others by their watchers; a machine shown is over SSH
        let shown = shown_key.as_ref().zip(self.snapshot.as_ref()).filter(|_| self.machine_id.is_none()).map(|(key, snapshot)| (key, Kind::Local, snapshot, Some(&self.client)));
        let others = self.watchers.snapshots_with_clients().filter(|(key, ..)| Some(*key) != shown_key.as_ref());
        let targets = targets(shown.into_iter().chain(others), &self.shells);
        if targets.is_empty() {
            self.abandon_scan(cx);
            return;
        }
        let call = self.rt.spawn(async move { tokio::time::timeout(SCAN, scan(targets, Path::new(PROC))).await.unwrap_or_default() });
        cx.spawn(async move |this, cx| {
            let results = call.await;
            let _ = this.update(cx, |s, cx| match results {
                Ok(results) => {
                    if s.shells.finish(results) {
                        cx.notify();
                    }
                }
                Err(_) => s.abandon_scan(cx),
            });
        })
        .detach();
    }

    /// No agent at rest to scan (or the runtime stopped): every mark goes.
    fn abandon_scan(&mut self, cx: &mut Context<Self>) {
        self.shells.abandon();
        if self.shells.finish(Vec::new()) {
            cx.notify();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use std::path::PathBuf;

    const OUTPUT: &str = "/tmp/claude-1000/-home-me-demo/0f3a9c1e-5b7d-4e21-8a60-c4d2b9e1f735/tasks/bk4m2x9qa.output";

    /// A fake `/proc`: processes as (pid, parent, session, state), their children listed as the kernel does, and
    /// the file a pid's standard output points at.
    struct Proc {
        dir: tempfile::TempDir,
    }

    impl Proc {
        fn new() -> Self {
            Self { dir: tempfile::tempdir().unwrap() }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        /// A process, as `/proc/<pid>/stat` writes it (the name with a space and a parenthesis in it, as a real
        /// one can have), listed among its parent's children.
        fn spawn(&self, pid: u32, parent: u32, session: u32, state: char, stdout: Option<&str>) {
            let root = self.path().join(pid.to_string());
            std::fs::create_dir_all(root.join("task").join(pid.to_string())).unwrap();
            std::fs::create_dir_all(root.join("fd")).unwrap();
            std::fs::write(root.join("stat"), format!("{pid} (b ash) ) {state} {parent} {pid} {session} 0 -1 4194304 97 0 0 0 0 0 0 0 20 0 1 0 18804801 6389760 512\n")).unwrap();
            if let Some(out) = stdout {
                std::os::unix::fs::symlink(out, root.join("fd").join("1")).unwrap();
            }
            let children = self.path().join(parent.to_string()).join("task").join(parent.to_string()).join("children");
            if children.parent().is_some_and(|d| d.exists()) {
                let mut now = std::fs::read_to_string(&children).unwrap_or_default();
                now.push_str(&format!("{pid} "));
                std::fs::write(children, now).unwrap();
            }
        }

        /// The process ended: gone from the table and from its parent's list.
        fn exit(&self, pid: u32, parent: u32) {
            std::fs::remove_dir_all(self.path().join(pid.to_string())).unwrap();
            let children = self.path().join(parent.to_string()).join("task").join(parent.to_string()).join("children");
            let now = std::fs::read_to_string(&children).unwrap();
            std::fs::write(children, now.replace(&format!("{pid} "), "")).unwrap();
        }
    }

    /// The pane's shell 4100 (its session), the agent 4150 and what Claude Code 2.1.300 runs under it: two MCP
    /// servers in the shell's session, the status line script in a session of its own with a pipe for output.
    fn pane() -> (Proc, Known) {
        let p = Proc::new();
        p.spawn(4100, 1, 4100, 'S', None);
        p.spawn(4150, 4100, 4100, 'S', None);
        p.spawn(4161, 4150, 4100, 'S', Some("socket:[84972113]"));
        p.spawn(4162, 4150, 4100, 'S', Some("socket:[84972119]"));
        p.spawn(4170, 4150, 4170, 'S', Some("pipe:[84972200]"));
        (p, Known { agent: 4150, shell: 4100 })
    }

    #[test]
    fn a_task_shell_in_its_own_session_with_a_tasks_output_is_counted() {
        let (p, known) = pane();
        assert_eq!(count(p.path(), known), Some(0), "MCP servers share the session, the status line writes to a pipe");
        p.spawn(4300, 4150, 4300, 'S', Some(OUTPUT));
        assert_eq!(count(p.path(), known), Some(1));
        p.spawn(4301, 4150, 4301, 'S', Some("/tmp/claude-1000/-home-me-demo/0f3a9c1e-5b7d-4e21-8a60-c4d2b9e1f735/tasks/bz8q1w4np.output"));
        assert_eq!(count(p.path(), known), Some(2));
    }

    #[test]
    fn what_is_not_a_task_shell_is_not_counted() {
        let (p, known) = pane();
        // an MCP server in the shell's session even when it writes to a tasks file; a child of another process; a
        // zombie; a log elsewhere; a tasks file outside Claude Code's folder
        p.spawn(4310, 4150, 4100, 'S', Some(OUTPUT));
        p.spawn(4311, 4170, 4311, 'S', Some(OUTPUT));
        p.spawn(4312, 4150, 4312, 'Z', Some(OUTPUT));
        p.spawn(4313, 4150, 4313, 'S', Some("/tmp/claude-1000/-home-me-demo/build.output"));
        p.spawn(4314, 4150, 4314, 'S', Some("/var/tmp/other/0f3a/tasks/bk4m2x9qa.output"));
        p.spawn(4315, 4150, 4315, 'S', Some("/tmp/claude-1000/-home-me-demo/0f3a/tasks/bk4m2x9qa.txt"));
        assert_eq!(count(p.path(), known), Some(0));
    }

    #[test]
    fn a_shell_that_ends_is_not_counted_and_a_reused_pid_is_not_a_child() {
        let (p, known) = pane();
        p.spawn(4300, 4150, 4300, 'S', Some(OUTPUT));
        assert_eq!(count(p.path(), known), Some(1));
        p.exit(4300, 4150);
        assert_eq!(count(p.path(), known), Some(0), "ended");
        // the pid given to a process of another parent, while the agent's children file still lists it
        p.spawn(4300, 4100, 4300, 'S', Some(OUTPUT));
        std::fs::write(p.path().join("4150/task/4150/children"), "4161 4162 4170 4300 ").unwrap();
        assert_eq!(count(p.path(), known), Some(0), "its parent is the shell now");
    }

    #[test]
    fn an_agent_that_is_gone_or_whose_pid_was_reused_has_no_count() {
        let (p, known) = pane();
        p.spawn(4300, 4150, 4300, 'S', Some(OUTPUT));
        assert_eq!(count(p.path(), Known { agent: 4150, shell: 4999 }), None, "the pid now sits in another session");
        assert_eq!(count(p.path(), Known { agent: 9999, shell: 4100 }), None, "gone");
        p.exit(4150, 4100);
        assert_eq!(count(p.path(), known), None, "exited");
        let (p, known) = pane();
        p.spawn(4150, 4100, 4100, 'Z', None);
        assert_eq!(count(p.path(), known), None, "a zombie");
    }

    #[test]
    fn a_process_name_with_parentheses_does_not_shift_the_fields() {
        let (p, known) = pane();
        let s = stat(p.path(), 4150).unwrap();
        assert_eq!((s.state, s.parent, s.session), ('S', 4100, 4100));
        assert_eq!(count(p.path(), known), Some(0));
    }

    #[test]
    fn only_claude_codes_task_files_are_outputs() {
        assert!(is_task_output(Path::new(OUTPUT)));
        assert!(!is_task_output(Path::new("/tmp/claude-1000/-home-me-demo/tasks.output")));
        assert!(!is_task_output(Path::new("/tmp/claude-1000/tasks/bk4m2x9qa.output")), "no session folder between");
        assert!(!is_task_output(Path::new("/tmp/other/0f3a/tasks/bk4m2x9qa.output")));
        assert!(!is_task_output(Path::new("/tmp/claude-1000/-home-me-demo/0f3a9c1e/logs/bk4m2x9qa.output")), "not the tasks folder");
        assert!(!is_task_output(Path::new("pipe:[84972200]")));
    }

    fn snapshot(panes: &[(&str, &str, &str, &str)]) -> Snapshot {
        let panes: Vec<_> = panes
            .iter()
            .map(|(id, ws, status, terminal)| {
                let agent = (!id.starts_with("sh")).then_some("claude");
                json!({"pane_id": id, "tab_id": format!("t-{id}"), "workspace_id": ws, "terminal_id": terminal, "agent_status": status, "agent": agent})
            })
            .collect();
        let ws = |id: &str, status: &str| json!({"workspace_id": id, "label": id, "number": 1, "active_tab_id": null, "agent_status": status, "worktree": null});
        // each workspace's status as herdr aggregates it: blocked, then done, then working, then idle
        let rank = |s: &str| ["blocked", "done", "working", "idle", "unknown"].iter().position(|r| *r == s).unwrap();
        let workspaces: Vec<_> = ["w1", "w2"]
            .iter()
            .map(|w| ws(w, panes.iter().filter(|p| p["workspace_id"] == *w).map(|p| p["agent_status"].as_str().unwrap()).min_by_key(|s| rank(s)).unwrap_or("idle")))
            .collect();
        Snapshot::from_result(&json!({"snapshot": {"focused_workspace_id": null, "focused_tab_id": null, "focused_pane_id": null, "workspaces": workspaces, "tabs": [], "panes": panes, "layouts": []}})).unwrap()
    }

    fn key(name: &str) -> SessionKey {
        SessionKey(format!("/tmp/{name}.sock"))
    }

    fn found(terminal: &str, count: usize) -> Found {
        Found { terminal: terminal.into(), known: Known { agent: 4150, shell: 4100 }, count }
    }

    fn with(marks: &[(&str, &str, usize)]) -> Shells {
        let mut s = Shells::default();
        s.finish(marks.iter().map(|(pane, terminal, n)| ((key("default"), pane.to_string()), found(terminal, *n))).collect());
        s
    }

    fn drawn(s: &Shells, snapshot: &Snapshot, ws: &str) -> (AgentStatus, usize) {
        s.workspace(&key("default"), snapshot, snapshot.workspaces.iter().find(|w| w.workspace_id == ws).unwrap())
    }

    #[test]
    fn an_idle_or_done_agent_with_shells_reads_as_working() {
        let s = with(&[("p1", "x1", 2), ("p2", "x2", 1)]);
        let idle = snapshot(&[("p1", "w1", "idle", "x1"), ("p2", "w2", "done", "x2")]);
        assert_eq!(drawn(&s, &idle, "w1"), (AgentStatus::Working, 2));
        assert_eq!(drawn(&s, &idle, "w2"), (AgentStatus::Working, 1), "a finish not seen yet reads as working too");
        let none = with(&[("p1", "x1", 0)]);
        assert_eq!(drawn(&none, &idle, "w1"), (AgentStatus::Idle, 0));
        assert_eq!(drawn(&Shells::default(), &idle, "w2"), (AgentStatus::Done, 0));
    }

    #[test]
    fn blocked_wins_over_the_mark() {
        let s = with(&[("p1", "x1", 2), ("p2", "x2", 1)]);
        let snap = snapshot(&[("p1", "w1", "blocked", "x1"), ("p2", "w2", "idle", "x2"), ("p3", "w2", "blocked", "x3")]);
        assert_eq!(drawn(&s, &snap, "w1"), (AgentStatus::Blocked, 0), "its own prompt");
        assert_eq!(drawn(&s, &snap, "w2"), (AgentStatus::Blocked, 0), "another agent of the workspace asks");
    }

    #[test]
    fn a_workspace_s_dot_follows_its_agents() {
        let s = with(&[("p1", "x1", 1)]);
        // p1 rests with a shell, p2 works: working; p2 idle: working; p2 done (not marked): done stays
        assert_eq!(drawn(&s, &snapshot(&[("p1", "w1", "idle", "x1"), ("p2", "w1", "working", "x2")]), "w1").0, AgentStatus::Working);
        assert_eq!(drawn(&s, &snapshot(&[("p1", "w1", "idle", "x1"), ("p2", "w1", "idle", "x2")]), "w1").0, AgentStatus::Working);
        assert_eq!(drawn(&s, &snapshot(&[("p1", "w1", "idle", "x1"), ("p2", "w1", "done", "x2")]), "w1"), (AgentStatus::Done, 1));
        // both marked: the counts add up
        let two = with(&[("p1", "x1", 1), ("p2", "x2", 2)]);
        assert_eq!(drawn(&two, &snapshot(&[("p1", "w1", "idle", "x1"), ("p2", "w1", "done", "x2")]), "w1"), (AgentStatus::Working, 3));
    }

    #[test]
    fn a_mark_holds_only_for_the_terminal_it_was_found_in_and_a_resting_agent() {
        let s = with(&[("p1", "x1", 2)]);
        let working = snapshot(&[("p1", "w1", "working", "x1")]);
        assert_eq!(drawn(&s, &working, "w1"), (AgentStatus::Working, 0), "working shows no count");
        let reused = snapshot(&[("p1", "w1", "idle", "x9")]);
        assert_eq!(drawn(&s, &reused, "w1"), (AgentStatus::Idle, 0), "the pane id belongs to another terminal now");
        let shell = snapshot(&[("sh1", "w1", "idle", "x1")]);
        assert_eq!(drawn(&with(&[("sh1", "x1", 2)]), &shell, "w1"), (AgentStatus::Idle, 0), "no agent");
        let other = snapshot(&[("p1", "w1", "idle", "x1")]);
        assert_eq!(Shells::default().of_pane(&key("menqal"), &other.panes[0]), 0);
        assert_eq!(s.of_pane(&key("menqal"), &other.panes[0]), 0, "another session's pane of the same id");
    }

    /// The status cell as a row draws it: the count sits before the dot, and a row without shells is the dot alone.
    #[gpui::test]
    fn the_count_is_drawn_before_the_dot_and_only_when_there_is_one(cx: &mut gpui::TestAppContext) {
        struct Cells(usize);
        impl gpui::Render for Cells {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = Theme::of(cx).clone();
                div().w(px(256.)).flex().child(status_cell(AgentStatus::Working, self.0, &theme))
            }
        }
        cx.update(|cx| cx.set_global(Theme::for_appearance(gpui::WindowAppearance::Dark)));
        let (_, with) = cx.add_window_view(|_, _| Cells(2));
        with.run_until_parked();
        let (count, dot) = (with.debug_bounds("ws-shells").expect("the count"), with.debug_bounds("ws-status-dot").expect("the dot"));
        assert!(count.right() <= dot.left(), "the count is before the dot");
        let (_, without) = cx.add_window_view(|_, _| Cells(0));
        without.run_until_parked();
        assert!(without.debug_bounds("ws-shells").is_none() && without.debug_bounds("ws-status-dot").is_none(), "the dot alone, as before");
    }

    #[test]
    fn the_label_counts_the_shells() {
        assert_eq!(label(1), "1 shell");
        assert_eq!(label(3), "3 shells");
    }

    #[test]
    fn a_session_over_ssh_is_not_scanned() {
        let snap = snapshot(&[("p1", "w1", "idle", "x1"), ("p2", "w1", "done", "x2"), ("p3", "w1", "working", "x3"), ("sh1", "w1", "idle", "x4")]);
        let client = Client::at(PathBuf::from("/tmp/none.sock"));
        let (local, machine) = (key("default"), SessionKey("machine:box".into()));
        let shells = Shells::default();
        let panes = |t: Vec<Target>| t.into_iter().map(|t| format!("{}:{}", t.key.0, t.pane)).collect::<Vec<_>>();
        assert_eq!(panes(targets([(&local, Kind::Local, &snap, Some(&client)), (&machine, Kind::Remote, &snap, Some(&client))], &shells)), ["/tmp/default.sock:p1", "/tmp/default.sock:p2"], "agents at rest, in the local session");
        assert!(targets([(&machine, Kind::Remote, &snap, Some(&client))], &shells).is_empty());
        assert!(targets([(&local, Kind::Local, &snap, None)], &shells).is_empty(), "not connected yet");
    }

    #[test]
    fn the_scan_keeps_what_is_known_while_it_stands_and_asks_herdr_otherwise() {
        let (p, known) = pane();
        p.spawn(4300, 4150, 4300, 'S', Some(OUTPUT));
        // the pane's agent known: no word from herdr is needed (nothing listens on this socket)
        let client = Client::at(p.path().join("none.sock"));
        let target = |known| Target { key: key("default"), client: client.clone(), pane: "p1".into(), terminal: "x1".into(), known };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let run = |t: Target| rt.block_on(scan(vec![t], p.path()));
        let out = run(target(Some(known)));
        assert_eq!(out, [((key("default"), "p1".to_string()), found("x1", 1))]);
        // its pid reused by a process outside the pane's session, or herdr not answering: no result, so no mark
        assert!(run(target(Some(Known { agent: 4150, shell: 4999 }))).is_empty());
        assert!(run(target(None)).is_empty());
    }

    /// A fake herdr that names pane `p1`'s processes.
    async fn herdr(dir: &Path, shell: u32, agent: u32) -> Client {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let path = dir.join("herdr.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let req: serde_json::Value = serde_json::from_str(&BufReader::new(r).lines().next_line().await.unwrap().unwrap()).unwrap();
                    assert_eq!((req["method"].as_str(), req["params"]["pane_id"].as_str()), (Some("pane.process_info"), Some("p1")));
                    let info = json!({"pane_id": "p1", "shell_pid": shell, "foreground_process_group_id": agent, "foreground_processes": [{"pid": agent, "name": "claude"}]});
                    let answer = json!({"id": req["id"], "result": {"type": "pane_process_info", "process_info": info}});
                    w.write_all(format!("{answer}\n").as_bytes()).await.unwrap();
                });
            }
        });
        Client::at(path)
    }

    #[tokio::test]
    async fn a_shell_that_ends_clears_its_mark_on_the_next_scan_with_no_event() {
        let (p, _) = pane();
        p.spawn(4300, 4150, 4300, 'S', Some(OUTPUT));
        let client = herdr(p.path(), 4100, 4150).await;
        let (snap, k) = (snapshot(&[("p1", "w1", "idle", "x1")]), key("default"));
        let mut shells = Shells::default();
        let proc = p.path().to_path_buf();
        let tick = |shells: &mut Shells, now| {
            assert!(shells.begin(now, Duration::from_secs(3)), "due");
            targets([(&k, Kind::Local, &snap, Some(&client))], shells)
        };
        let t0 = Instant::now();
        let first = scan(tick(&mut shells, t0), &proc).await;
        assert!(shells.finish(first), "drawn changed");
        assert_eq!(drawn(&shells, &snap, "w1"), (AgentStatus::Working, 1));
        // the shell ends; no herdr event follows, only the timer
        p.exit(4300, 4150);
        assert!(!shells.begin(t0 + Duration::from_secs(2), Duration::from_secs(3)), "not due yet");
        let again = scan(tick(&mut shells, t0 + Duration::from_secs(3)), &proc).await;
        assert!(shells.finish(again));
        assert_eq!(drawn(&shells, &snap, "w1"), (AgentStatus::Idle, 0));
        assert_eq!(shells.known(&k, "p1", "x1"), Some(Known { agent: 4150, shell: 4100 }), "its agent stays known for the next scan");
    }

    #[test]
    fn one_scan_is_out_at_a_time_and_the_next_waits_for_its_turn() {
        let (mut s, t0) = (Shells::default(), Instant::now());
        let every = Duration::from_secs(3);
        assert!(s.begin(t0, every));
        assert!(!s.begin(t0 + every, every), "one is out");
        s.abandon();
        assert!(s.begin(t0 + every, every));
        s.finish(Vec::new());
        assert!(!s.begin(t0 + every + Duration::from_secs(1), every), "too soon");
        assert!(s.begin(t0 + every * 2, every));
    }

    #[test]
    fn results_replace_the_marks_so_a_pane_no_longer_at_rest_loses_its_own() {
        let mut s = with(&[("p1", "x1", 2), ("p2", "x2", 1)]);
        assert!(s.finish(vec![((key("default"), "p2".into()), found("x2", 1))]), "p1 is not at rest or gone");
        assert_eq!(s.known(&key("default"), "p1", "x1"), None);
        assert!(!s.finish(vec![((key("default"), "p2".into()), found("x2", 1))]), "the same is no change");
        assert_eq!(s.known(&key("default"), "p2", "x2"), Some(Known { agent: 4150, shell: 4100 }));
        assert_eq!(s.known(&key("default"), "p2", "x9"), None, "the pane id is another terminal's now");
    }
}
