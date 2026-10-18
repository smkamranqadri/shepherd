//! Sessions on a host: a machine row's Sessions on this host… lists the herdr sessions running there or stopped, over
//! SSH (`remote_sessions`), less those saved as machines already, and a click on one saves it as a machine too
//! (`herdr machine add`, which starts a stopped one). The list is read once a popup opens, never in the background;
//! Add SSH machine reads it once more after a host is added, and a toast offers the rest.
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
    let first = saved.iter().find(|m| m.target == d.target)?;
    let host = &d.target;
    Some(match &d.found {
        Found::Loading => View::Loading,
        // as the machine menu's Reconnect… says it: Shepherd's SSH runs without a prompt
        Found::Failed(ListError::SignIn) => View::Failed(format!(
            "Shepherd's SSH never asks for a password: run `{}` in a terminal, then Try again.",
            crate::herdr::lifecycle::reconnect_command(&first.id)
        )),
        Found::Failed(ListError::NotInstalled) => View::Failed(format!("herdr is not installed on {host}.")),
        Found::Failed(ListError::Failed(why)) => View::Failed(format!("Couldn't list the sessions on {host}: {why}")),
        Found::Listed(found) => {
            let rows: Vec<Row> = crate::herdr::unsaved(found, saved, host)
                .into_iter()
                .map(|s| Row {
                    adding: adds.busy(host, &s.name),
                    note: match (adds.busy(host, &s.name), s.running) {
                        (true, _) => "Adding…",
                        (false, true) => "running",
                        (false, false) => "stopped (adding starts it)",
                    },
                    name: s.name,
                })
                .collect();
            if rows.is_empty() { View::Empty(format!("No other sessions on {host}")) } else { View::Rows(rows) }
        }
    })
}

/// The toast after an add from the popup, when it did not save a listed machine.
pub(super) fn add_note(added: &Result<Machine, AddError>, host: &str, session: &str) -> Option<String> {
    match added {
        Ok(_) => None,
        Err(AddError::Unsaved) => Some(format!("{session} on {host} was added, but herdr does not list it yet.")),
        Err(e) => Some(format!("Couldn't add {session} on {host}: {e}")),
    }
}

/// The machine Add SSH machine saved, which is switched to and whose host's other sessions the toast offers; none
/// when the add failed, which an error toast says instead.
pub(super) fn offered(added: &Result<Machine, AddError>) -> Option<&Machine> {
    added.as_ref().ok()
}

/// The toast after Add SSH machine, when the host runs `more` sessions besides those saved.
pub(super) fn more_sessions_note(more: usize, host: &str) -> Option<String> {
    match more {
        0 => None,
        1 => Some(format!("1 more session on {host}. Click to add it.")),
        n => Some(format!("{n} more sessions on {host}. Click to add them.")),
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

    /// After Add SSH machine saved `machine`: its host's list read once, and a toast offering the sessions there not
    /// saved yet, which opens the popup. Nothing when the list can't be read: the machine was added all the same.
    pub(super) fn offer_more_sessions(&mut self, machine: Machine, cx: &mut Context<Self>) {
        let list = self.rt.spawn({
            let target = machine.target.clone();
            async move { crate::herdr::remote_sessions(&target).await }
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(found)) = list.await else { return };
            let weak = this.clone();
            let _ = this.update(cx, |s, cx| {
                // herdr's list read since may not have the new machine yet
                let mut saved = s.machine_records(&machine.target);
                saved.push(machine.clone());
                let more = crate::herdr::unsaved(&found, &saved, &machine.target).len();
                let Some(note) = more_sessions_note(more, &machine.target) else { return };
                let target = machine.target.clone();
                let open: Rc<dyn Fn(&mut App)> = Rc::new(move |cx: &mut App| {
                    let _ = weak.update(cx, |s, cx| s.open_discover(target.clone(), cx));
                });
                crate::ui::toast::info(cx, note, Some(open));
            });
        })
        .detach();
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
                    this.open_session_dialog(window, cx);
                    this.toggle_session_remote(window, cx);
                    if let Some(dialog) = this.session_dialog.as_ref() {
                        dialog.value.host.update(cx, |input, cx| input.set_value(host.clone(), window, cx));
                    }
                })));
            }
            View::Rows(rows) => {
                let list = rows.into_iter().enumerate().map(|(i, r)| {
                    let name = r.name.clone();
                    row(theme, false, ("discover-session", i))
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
                                .child(div().truncate().child(r.name))
                                .child(div().truncate().text_size(px(11.)).text_color(theme.text_faint).child(r.note)),
                        )
                        .when(r.adding, |el| el.child(spinner(theme, format!("discover-adding-{i}"))).text_color(theme.text_faint))
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
    fn only_a_saved_machine_is_switched_to_and_offered() {
        let m = saved("you@workbox", "docs");
        assert_eq!(offered(&Ok(m.clone())), Some(&m));
        for e in [AddError::Failed("no route".into()), AddError::TimedOut, AddError::Unsaved] {
            assert_eq!(offered(&Err(e)), None);
        }
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

    #[test]
    fn the_toast_counts_the_sessions_left() {
        assert_eq!(more_sessions_note(0, "you@workbox"), None);
        assert_eq!(more_sessions_note(1, "you@workbox").as_deref(), Some("1 more session on you@workbox. Click to add it."));
        assert_eq!(more_sessions_note(3, "you@workbox").as_deref(), Some("3 more sessions on you@workbox. Click to add them."));
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
