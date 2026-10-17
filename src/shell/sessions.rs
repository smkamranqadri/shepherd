//! herdr sessions and saved SSH machines, listed in the sidebar's tree (`sidebar_tree`), the keys' way from one to
//! the next, and the dialogs that make and delete them.
use super::*;
use crate::herdr::{Machine, SessionInfo};

/// A session here or a machine elsewhere.
#[derive(Clone, PartialEq)]
pub(super) enum Place {
    Session(SessionInfo),
    Machine(Machine),
}

impl Place {
    /// Its name and icon in Shepherd: the ones given in Edit, else herdr's name and no icon.
    pub(super) fn look(&self, cx: &App) -> (String, Option<String>) {
        let look = SettingsGlobal::of(cx).settings.place_looks.get(&self.key()).cloned().unwrap_or_default();
        let herdr = match self {
            Place::Session(s) => s.name.clone(),
            Place::Machine(m) => m.label.clone(),
        };
        (look.name.filter(|n| !n.is_empty()).unwrap_or(herdr), look.icon.filter(|i| !i.is_empty()))
    }

    /// herdr's default session keeps itself: it can be edited, not deleted.
    pub(super) fn deletable(&self) -> bool {
        !matches!(self, Place::Session(s) if s.default)
    }

    /// Where its last snapshot is kept while another place is shown, and its look in the settings.
    pub(super) fn key(&self) -> String {
        match self {
            Place::Session(s) => s.socket_path.clone(),
            Place::Machine(m) => format!("machine:{}", m.id),
        }
    }
}

/// Edit a place's name and icon.
pub(super) struct PlaceEdit {
    pub(super) place: Place,
    pub(super) name: Entity<InputState>,
    /// `icon:<name>` of a Solar Linear icon, an emoji, or `None` for the name's first letter.
    pub(super) icon: Option<String>,
    /// The picker shows the emoji rather than the drawn icons.
    pub(super) emoji: bool,
    /// Narrows the icons or emoji by name.
    pub(super) search: Entity<InputState>,
    /// The grid's scroll, which only lays out the rows in view.
    pub(super) emoji_scroll: gpui::UniformListScrollHandle,
    _events: Vec<Subscription>,
}

/// A tile of the icon picker: the name's first letter (no icon), or a Solar icon by name.
#[derive(Clone, Copy)]
pub(super) enum IconTile {
    Letter,
    Solar(&'static str),
}

/// The icon picker's rows: the letter, then every Solar Linear icon whose name holds `query` (`-` read as a space).
pub(super) fn icon_rows(query: &str) -> Vec<Vec<IconTile>> {
    let query = query.trim().to_lowercase();
    let tiles: Vec<IconTile> = query
        .is_empty()
        .then_some(IconTile::Letter)
        .into_iter()
        .chain(crate::assets::solar_names().filter(|n| query.is_empty() || n.replace('-', " ").contains(&query) || n.contains(&query)).map(IconTile::Solar))
        .collect();
    tiles.chunks(EMOJI_PER_ROW).map(<[IconTile]>::to_vec).collect()
}

/// Columns of the picker's grid; its tiles stretch to share the dialog's width.
pub(super) const EMOJI_PER_ROW: usize = 8;

/// A row of the emoji grid: a group's heading, or up to [`EMOJI_PER_ROW`] emoji.
pub(super) enum EmojiRow {
    Heading(&'static str),
    Emoji(Vec<&'static str>),
}

/// Every emoji (in its default skin tone) whose name or shortcode holds `query`, grouped as Unicode groups them.
pub(super) fn emoji_rows(query: &str) -> Vec<EmojiRow> {
    let query = query.trim().to_lowercase();
    let matches = |e: &emojis::Emoji| query.is_empty() || e.name().contains(&query) || e.shortcodes().any(|c| c.contains(&query));
    let mut rows = Vec::new();
    for group in emojis::Group::iter() {
        let found: Vec<&'static str> = group
            .emojis()
            .filter(|e| e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default))
            .filter(|e| matches(e))
            .map(|e| e.as_str())
            .collect();
        if found.is_empty() {
            continue;
        }
        rows.push(EmojiRow::Heading(group_name(group)));
        rows.extend(found.chunks(EMOJI_PER_ROW).map(|chunk| EmojiRow::Emoji(chunk.to_vec())));
    }
    rows
}

/// A group's heading in the emoji grid, one row tall, its name on the row's floor.
pub(super) fn emoji_heading(name: &'static str, color: gpui::Hsla) -> gpui::Div {
    div().h(px(36.)).flex().items_end().pb(px(4.)).text_size(px(11.)).text_color(color).child(name)
}

/// The emoji grid's sticky heading: the top row's group stays at the list's top on the dialog's card, and the next
/// group's heading pushes it up as it arrives.
pub(super) struct StickyEmojiHeading {
    pub(super) headings: Vec<(usize, &'static str)>,
    pub(super) bg: gpui::Hsla,
    pub(super) color: gpui::Hsla,
}

impl gpui::UniformListDecoration for StickyEmojiHeading {
    fn compute(
        &self,
        visible: std::ops::Range<usize>,
        _: gpui::Bounds<Pixels>,
        scroll: gpui::Point<Pixels>,
        row: Pixels,
        _: usize,
        _: &mut Window,
        _: &mut App,
    ) -> gpui::AnyElement {
        let Some(&(_, name)) = self.headings.iter().rev().find(|(i, _)| *i <= visible.start) else {
            return div().into_any_element();
        };
        // the next heading's top within the view, once it's under this one
        let push = self.headings.iter().find(|(i, _)| *i > visible.start).map_or(px(0.), |(i, _)| (row * *i as f32 + scroll.y - row).min(px(0.)));
        // laid out from the content's top (as a root, where a margin is dropped): undo the scroll to stay put
        div().w_full().pt(-scroll.y + push).child(emoji_heading(name, self.color).bg(self.bg)).into_any_element()
    }
}

fn group_name(group: emojis::Group) -> &'static str {
    use emojis::Group::*;
    match group {
        SmileysAndEmotion => "Smileys",
        PeopleAndBody => "People",
        AnimalsAndNature => "Nature",
        FoodAndDrink => "Food",
        TravelAndPlaces => "Places",
        Activities => "Activities",
        Objects => "Objects",
        Symbols => "Symbols",
        Flags => "Flags",
    }
}

pub(super) struct SessionDialog {
    pub(super) name: Entity<InputState>,
    /// The SSH host of a remote session; shown while `remote` is on.
    pub(super) host: Entity<InputState>,
    pub(super) remote: bool,
    /// Bumped on every flip of `remote`: the host field's slide restarts.
    pub(super) flips: u64,
    _events: Vec<Subscription>,
}

impl Shell {
    /// At launch, the machine the settings name gets its relay back, on the socket the settings already hold.
    pub(super) fn resume_machine(&mut self, cx: &App) {
        let Some(id) = SettingsGlobal::of(cx).settings.herdr_machine.clone() else { return };
        // ponytail: `herdr machine list` only reads herdr's catalog, so launch waits on it briefly
        let Some(machine) = crate::herdr::machines().ok().and_then(|l| l.into_iter().find(|m| m.id == id)) else { return };
        if crate::herdr::relay(&machine, &self.rt).is_ok() {
            crate::herdr::show_machine(Some(machine.target.clone()));
            self.machine_label = Some(machine.label);
            self.machine_id = Some(machine.id);
        }
    }

    /// The name of the place shown: its name in Shepherd, else the machine's label or the session's name, or `custom`
    /// for a socket of one's own.
    pub(super) fn place_label(&self, cx: &App) -> String {
        match self.places.iter().find(|p| self.places_shown(p)) {
            Some(place) => place.look(cx).0,
            None => self.machine_label.clone().or_else(crate::herdr::current_session).unwrap_or_else(|| "custom".into()),
        }
    }

    pub(super) fn show_place_emoji(&mut self, emoji: bool, cx: &mut Context<Self>) {
        if let Some(edit) = self.place_edit.as_mut() {
            edit.value.emoji = emoji;
            edit.value.emoji_scroll = Default::default(); // each tab opens at its top
            cx.notify();
        }
    }

    pub(super) fn pick_place_icon(&mut self, icon: Option<String>, cx: &mut Context<Self>) {
        if let Some(edit) = self.place_edit.as_mut() {
            edit.value.icon = icon;
            cx.notify();
        }
    }

    pub(super) fn open_place_menu(&mut self, place: Place, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.place_menu = Some(crate::ui::Popup::new((place, at)));
        cx.notify();
    }

    pub(super) fn open_place_edit(&mut self, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.place_menu.as_mut() {
            m.close()
        }
        let theme = Theme::of(cx).clone();
        let look = SettingsGlobal::of(cx).settings.place_looks.get(&place.key()).cloned().unwrap_or_default();
        let (shown_name, _) = place.look(cx);
        let mut field = |placeholder: &'static str, value: String, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder).default_value(value);
                input.set_editor_style(crate::ui::input_style(&theme));
                input
            })
        };
        let name = field("Name", shown_name, cx);
        let search = field("Search", String::new(), cx);
        name.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        let enter = |shell: &mut Shell, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Shell>| {
            if let InputEvent::PressEnter { .. } = event {
                shell.finish_place_edit(true, cx);
            }
        };
        let refilter = |_: &mut Shell, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Shell>| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        };
        let events = vec![cx.subscribe_in(&name, window, enter), cx.subscribe_in(&search, window, refilter)];
        self.place_edit = Some(crate::ui::Popup::new(PlaceEdit {
            place,
            name,
            emoji: look.icon.as_deref().is_some_and(|i| !i.starts_with("icon:")),
            icon: look.icon,
            search,
            emoji_scroll: Default::default(),
            _events: events,
        }));
        cx.notify();
    }

    /// Saves the name and icon in the settings; a machine's name goes to herdr too, as its label.
    pub(super) fn finish_place_edit(&mut self, save: bool, cx: &mut Context<Self>) {
        let Some(edit) = self.place_edit.as_mut().filter(|e| e.is_open()) else { return };
        edit.close();
        self.focused = None;
        if save {
            let place = edit.value.place.clone();
            let name = edit.value.name.read(cx).value().trim().to_string();
            let icon = edit.value.icon.clone().unwrap_or_default();
            let herdr = match &place {
                Place::Session(s) => s.name.clone(),
                Place::Machine(m) => m.label.clone(),
            };
            let look = crate::settings::PlaceLook { name: (!name.is_empty() && name != herdr).then(|| name.clone()), icon: (!icon.is_empty()).then_some(icon) };
            let key = place.key();
            save_from_sidebar(cx, move |s| {
                if look == Default::default() {
                    s.place_looks.remove(&key);
                } else {
                    s.place_looks.insert(key, look);
                }
            });
            if let Place::Machine(m) = place.clone() && !name.is_empty() && name != herdr {
                let task = cx.background_spawn(async move { crate::herdr::rename_machine(&m.id, &name) });
                cx.spawn(async move |this, cx| {
                    let done = task.await;
                    let _ = this.update(cx, |s, cx| {
                        if let Err(e) = done {
                            crate::ui::toast::error(cx, format!("{e:#}"));
                        }
                        s.refresh_places(cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// Asks herdr for its sessions and machines off the main thread (5 s at most, one read at a time), for the keys and
    /// the tree, in the tree's order; the watchers follow the list.
    pub(super) fn refresh_places(&mut self, cx: &mut Context<Self>) {
        if !self.watchers.start_listing() {
            return; // read again when the read under way lands
        }
        let list = self.rt.spawn(crate::herdr::lifecycle::list_places(Duration::from_secs(5)));
        cx.spawn(async move |this, cx| {
            let listed = list.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |s, cx| {
                let again = s.watchers.listed(Instant::now());
                if !s.session_watch_settled {
                    s.start_session_watch(cx); // herdr's folder may be there now: its server made it
                }
                if let Ok((sessions, machines)) = listed {
                    // the one record of whether herdr's machines were read: views kept here, and a drop's saved order
                    s.tree.machines_known = machines.is_some();
                    let machines_known = s.tree.machines_known;
                    let settings = &SettingsGlobal::of(cx).settings;
                    let (places, disabled) = sidebar_tree::tree_order(sessions, machines.unwrap_or_default(), &settings.session_order, &settings.machine_order);
                    let changed = places != s.places || disabled != s.watchers.disabled;
                    s.places = places;
                    s.watchers.disabled = disabled;
                    let shown = s.shown_key();
                    s.watchers.reconcile(&s.places, shown.as_ref(), Instant::now());
                    let listed: Vec<watchers::SessionKey> = s.places.iter().map(watchers::SessionKey::of).collect();
                    s.attention.keep_sessions(|k| listed.contains(k) || shown.as_ref() == Some(k));
                    // a session deleted takes its view and keyboard panes along (a machine turned off, or every machine
                    // while herdr's list of them could not be read, keeps them); one herdr says stopped keeps its
                    // view, without its terminals
                    let off: Vec<String> = s.watchers.disabled.iter().map(|m| format!("machine:{}", m.id)).collect();
                    let keep = |k: &str| session_views::still_known(k, &listed, shown.as_ref(), &off, machines_known);
                    let gone = s.views.keep_sessions(|k| keep(&k.0));
                    let stopped: Vec<watchers::SessionKey> = listed.iter().filter(|k| s.watchers.get(k).is_some_and(|w| w.status == watchers::Status::Stopped)).cloned().collect();
                    let stopped: Vec<_> = stopped.iter().flat_map(|k| s.views.drop_panes(k)).collect();
                    s.retire_panes(gone.into_iter().chain(stopped), cx);
                    let before = s.keyboard.len();
                    s.keyboard.retain(|k, _| keep(k));
                    if s.keyboard.len() != before {
                        s.save_kept_views(cx);
                    }
                    if changed {
                        cx.notify();
                    }
                }
                if again {
                    s.refresh_places(cx);
                }
            });
        })
        .detach();
    }

    /// Reads herdr's list again when a session starts or stops (`session_watch`): a burst of events waits a moment
    /// (`DEBOUNCE`) and is one read, then one more read follows `SETTLE` later, since a server makes its socket
    /// before it listens on it and the first read may still find it not running. The watch lives with this task, as
    /// long as the window; without one the 15 s read is all there is, as before. Before herdr ever ran its folder
    /// is not there to watch: each read of the list that lands tries again, until it is.
    pub(super) fn start_session_watch(&mut self, cx: &mut Context<Self>) {
        use crate::herdr::session_watch::{NoWatch, SessionWatch, gather};
        use futures::StreamExt;
        /// Events this close are one read.
        const DEBOUNCE: Duration = Duration::from_millis(250);
        /// The second read after a burst: a socket just made is listened on by then.
        const SETTLE: Duration = Duration::from_millis(1500);
        let (mut watch, mut seen) = match SessionWatch::start() {
            Ok(watch) => watch,
            Err(why) => {
                self.session_watch_settled = why == NoWatch::Failed;
                return;
            }
        };
        self.session_watch_settled = true;
        cx.spawn(async move |this, cx| {
            while let Some(first) = seen.next().await {
                let mut burst = vec![first];
                for pause in [DEBOUNCE, SETTLE] {
                    cx.background_executor().timer(pause).await;
                    while let Ok(more) = seen.try_recv() {
                        burst.push(more);
                    }
                    // only a change or a new folder is ever sent, so both reads happen; new folders are watched first
                    if gather(burst.drain(..)).1 {
                        watch.catch_up();
                    }
                    if this.update(cx, |s, cx| s.refresh_places(cx)).is_err() {
                        return; // window closed
                    }
                }
            }
        })
        .detach();
    }

    pub(super) fn switch_to(&mut self, place: Place, cx: &mut Context<Self>) {
        match place {
            Place::Session(s) => self.switch_session(s, cx),
            Place::Machine(m) => self.switch_machine(m, cx),
        }
    }

    /// The next place in the tree (or the last one), round to the first from the end and back: the keyboard's way
    /// along the tree. Shown from a socket of one's own, it starts at either end.
    pub(super) fn step_place(&mut self, forward: bool, cx: &mut Context<Self>) {
        let n = self.places.len();
        let to = match self.places.iter().position(|p| self.places_shown(p)) {
            Some(at) => step(at, n, forward, true),
            None => (n > 0).then(|| if forward { 0 } else { n - 1 }),
        };
        if let Some(place) = to.and_then(|i| self.places.get(i).cloned()) {
            self.show_place(place, forward, cx);
        }
    }

    /// Shows `place` (a machine out of reach stays where it was, with a toast). The tree lists every place at once,
    /// so nothing slides: `forward` is the way the keys went.
    pub(super) fn show_place(&mut self, place: Place, _forward: bool, cx: &mut Context<Self>) {
        if self.places_shown(&place) {
            return;
        }
        self.switch_to(place, cx);
        cx.notify();
    }

    /// Whether `place` is the one shown.
    pub(super) fn places_shown(&self, place: &Place) -> bool {
        match (place, &self.machine_label) {
            (Place::Machine(m), Some(label)) => &m.label == label,
            (Place::Session(s), None) => crate::herdr::current_session().as_deref() == Some(s.name.as_str()),
            _ => false,
        }
    }

    /// Moves the window to a session on this Mac: its socket goes in the settings, and everything the last server's
    /// ids named is dropped. A stopped session's server is started by the sync loop, as at launch.
    pub(super) fn switch_session(&mut self, session: SessionInfo, cx: &mut Context<Self>) {
        // the default session is where Shepherd looks without a socket set, so it leaves the setting empty
        let socket = (!session.default).then_some(session.socket_path);
        self.move_to(socket, None, cx);
    }

    /// Moves the window to a saved machine, through its relay.
    pub(super) fn switch_machine(&mut self, machine: Machine, cx: &mut Context<Self>) {
        match crate::herdr::relay(&machine, &self.rt) {
            Ok(socket) => {
                self.move_to(Some(socket.to_string_lossy().into_owned()), Some(machine.id.clone()), cx);
                crate::herdr::show_machine(Some(machine.target.clone()));
                self.machine_label = Some(machine.label);
                self.machine_id = Some(machine.id);
            }
            Err(e) => crate::ui::toast::error(cx, format!("Couldn't reach {}: {e:#}", machine.label)),
        }
    }

    fn move_to(&mut self, socket: Option<String>, machine: Option<String>, cx: &mut Context<Self>) {
        // the place left keeps its last snapshot and its agents' clocks in its watcher, which polls it from now on, so
        // the tree shows its workspaces and coming back shows them before herdr answers
        if let Some(left) = self.shown_key() {
            // before its snapshot goes: the view is read from it
            self.park_view(&left, cx);
            self.watchers.park(&left, self.snapshot.take(), std::mem::take(&mut self.since), Instant::now());
        }
        save_from_sidebar(cx, |s| {
            s.herdr_socket = socket.clone();
            s.herdr_machine = machine.clone();
        });
        crate::herdr::set_socket(socket.clone()); // also when the settings could not be saved
        crate::herdr::show_machine(None); // a machine's switch names it right after
        self.machine_label = None;
        self.machine_id = None;
        self.client = socket.map_or_else(Client::local, |p| Client::at(p.into()));
        let key = match &machine {
            Some(id) => format!("machine:{id}"),
            None => self.places.iter().find_map(|p| match p {
                Place::Session(s) if crate::herdr::current_session().as_deref() == Some(s.name.as_str()) => Some(s.socket_path.clone()),
                _ => None,
            }).unwrap_or_default(),
        };
        let key = watchers::SessionKey(key);
        // a click that switched elsewhere waits no more for the session it named
        self.tree.switching_to(&key);
        (self.snapshot, self.since) = self.watchers.unpark(&key);
        // the session shown has no feed, the one left gets one
        self.sync_feeds(cx);
        // the new server is told the tab shown, whatever ids the last one gave its tabs
        self.focused_tab = None;
        self.own_focus.clear();
        self.error = None;
        self.connecting = Some(Instant::now());
        self.panes.clear();
        self.recent_panes.clear();
        self.parked_check = None;
        self.focused = None;
        self.marked_seen = None;
        self.focus_next = None;
        // the tab the session showed, with its parked panes (after `set_socket`: they attach there as they show)
        self.restore = None;
        self.keyboard_tab = None;
        self.restore_view(&key, cx);
        self.renaming = None;
        self.settling = None;
        self.pending_worktrees.clear();
        self.removing.clear();
        self.connect(cx);
        self.refresh_places(cx);
        cx.notify();
    }

    pub(super) fn open_session_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = Theme::of(cx).clone();
        let mut field = |placeholder: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                input.set_editor_style(crate::ui::input_style(&theme));
                input
            })
        };
        let (name, host) = (field("Name", cx), field("user@host or a Host from ~/.ssh/config", cx));
        name.update(cx, |input, cx| input.focus(window, cx));
        let enter = |shell: &mut Shell, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Shell>| {
            if let InputEvent::PressEnter { .. } = event {
                shell.finish_session_dialog(true, cx);
            }
        };
        let events = vec![cx.subscribe_in(&name, window, enter), cx.subscribe_in(&host, window, enter)];
        self.session_dialog = Some(crate::ui::Popup::new(SessionDialog { name, host, remote: false, flips: 0, _events: events }));
        cx.notify();
    }

    pub(super) fn toggle_session_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.session_dialog.as_mut() else { return };
        dialog.value.remote = !dialog.value.remote;
        dialog.value.flips += 1;
        let focus = if dialog.value.remote { &dialog.value.host } else { &dialog.value.name };
        focus.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Create (and move to) the session, here or on an SSH host, or cancel. Input herdr would refuse keeps the dialog
    /// open.
    pub(super) fn finish_session_dialog(&mut self, create: bool, cx: &mut Context<Self>) {
        let Some(dialog) = self.session_dialog.as_mut().filter(|d| d.is_open()) else { return };
        let name = dialog.value.name.read(cx).value().trim().to_string();
        let host = dialog.value.host.read(cx).value().trim().to_string();
        let remote = dialog.value.remote;
        // a remote session may go unnamed: the host's default one
        let name = if remote && name.is_empty() { "default".to_string() } else { name };
        if create && !crate::herdr::valid_session_name(&name) {
            crate::ui::toast::error(cx, "A session's name takes ASCII letters, digits, '.', '_' and '-'.");
            return;
        }
        if create && remote && host.is_empty() {
            crate::ui::toast::error(cx, "Which host? An SSH target such as user@host or a Host from ~/.ssh/config.");
            return;
        }
        dialog.close();
        self.focused = None; // an input held focus: back to herdr's focused pane
        if create && remote {
            crate::ui::toast::info(cx, format!("Setting up {name} on {host}…"), None);
            let task = cx.background_spawn(async move { crate::herdr::add_machine(&host, &name) });
            cx.spawn(async move |this, cx| {
                let added = task.await;
                let _ = this.update(cx, |s, cx| match added {
                    Ok(machine) => s.switch_machine(machine, cx),
                    Err(e) => crate::ui::toast::error(cx, format!("{e:#}")),
                });
            })
            .detach();
        } else if create {
            // a name that exists moves there instead
            let known = self.places.iter().find_map(|p| match p {
                Place::Session(s) if s.name == name => Some(s.clone()),
                _ => None,
            });
            self.switch_session(known.unwrap_or_else(|| crate::herdr::new_session(&name)), cx);
        }
        cx.notify();
    }

    pub(super) fn confirm_delete(&mut self, place: Place, cx: &mut Context<Self>) {
        self.session_delete = Some(crate::ui::Popup::new(place));
        cx.notify();
    }

    /// A session: herdr stops its server, closing its panes, and deletes it. A machine: herdr forgets it, and its
    /// sessions keep running on the host. The one shown now leaves for this Mac's default session first.
    pub(super) fn finish_delete(&mut self, delete: bool, cx: &mut Context<Self>) {
        let Some(confirm) = self.session_delete.as_mut().filter(|c| c.is_open()) else { return };
        confirm.close();
        // the confirm held focus: give it back to herdr's focused pane on the next render
        self.focused = None;
        let place = confirm.value.clone();
        if !delete {
            return cx.notify();
        }
        if self.places_shown(&place) {
            self.move_to(None, None, cx);
        }
        let (label, task) = match place {
            Place::Session(s) => (s.name.clone(), cx.background_spawn(async move { crate::herdr::delete_session(&s.name, s.running) })),
            Place::Machine(m) => (m.label.clone(), cx.background_spawn(async move { crate::herdr::remove_machine(&m.id) })),
        };
        cx.spawn(async move |this, cx| {
            let done = task.await;
            let _ = this.update(cx, |s, cx| {
                if let Err(e) = done {
                    crate::ui::toast::error(cx, format!("Couldn't delete {label}: {e:#}"));
                }
                s.refresh_places(cx);
            });
        })
        .detach();
        cx.notify();
    }
}

/// The index one place on from `at` among `len`, forward or back: past either end none, or with `wrap` the one at
/// the other end (the keys' way). None where `at` is the only place.
fn step(at: usize, len: usize, forward: bool, wrap: bool) -> Option<usize> {
    if at >= len || len < 2 {
        return None;
    }
    match (forward, wrap) {
        (true, _) if at + 1 < len => Some(at + 1),
        (false, _) if at > 0 => Some(at - 1),
        (true, true) => Some(0),
        (false, true) => Some(len - 1),
        (_, false) => None,
    }
}

/// A place's icon drawn at `size`: a drawn icon (`icon:<name>`) in `color`, or the emoji.
pub(crate) fn icon_mark(icon: &str, size: f32, color: gpui::Hsla) -> gpui::AnyElement {
    match icon.strip_prefix("icon:") {
        Some(name) => gpui::svg().path(SharedString::from(format!("icons/solar/{name}.svg"))).size(px(size)).flex_none().text_color(color).into_any_element(),
        None => div().text_size(px(size)).child(icon.to_string()).into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_wrap_a_step_stops_at_either_end() {
        assert_eq!(step(0, 3, true, false), Some(1));
        assert_eq!(step(2, 3, true, false), None);
        assert_eq!(step(0, 3, false, false), None);
        assert_eq!(step(2, 3, false, false), Some(1));
    }

    #[test]
    fn the_keys_wrap_round_the_bar() {
        assert_eq!(step(2, 3, true, true), Some(0));
        assert_eq!(step(0, 3, false, true), Some(2));
        assert_eq!(step(1, 3, true, true), Some(2));
        assert_eq!(step(1, 3, false, true), Some(0));
        assert_eq!(step(0, 2, true, true), Some(1));
        assert_eq!(step(1, 2, true, true), Some(0));
    }

    #[test]
    fn one_place_has_nowhere_to_go() {
        assert_eq!(step(0, 1, true, true), None);
        assert_eq!(step(0, 1, false, true), None);
        assert_eq!(step(0, 0, true, true), None);
        assert_eq!(step(3, 2, true, true), None, "a stale index");
    }

    #[test]
    fn emoji_search_by_name() {
        let all: usize = emoji_rows("").iter().map(|r| if let EmojiRow::Emoji(e) = r { e.len() } else { 0 }).sum();
        assert!(all > 1500, "{all}");
        let rocket = emoji_rows("rocket");
        assert!(rocket.iter().any(|r| matches!(r, EmojiRow::Emoji(e) if e.contains(&"🚀"))));
        assert!(emoji_rows("no such emoji at all").is_empty());
    }

    #[test]
    fn solar_icons_by_name() {
        assert!(crate::assets::solar_names().count() > 1000);
        assert!(crate::assets::solar_icon("rocket-2").is_some_and(|svg| svg.starts_with("<svg") && svg.contains("currentColor")));
        let found: Vec<_> = icon_rows("home").concat();
        assert!(found.iter().any(|t| matches!(t, IconTile::Solar("home-2"))));
        assert!(matches!(icon_rows("")[0][0], IconTile::Letter));
    }
}
