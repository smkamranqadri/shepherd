//! Shortcut hints (uherdr's `ShortcutHintMonitor`): hold the modifier of the tabs' chords a moment (Cmd on macOS,
//! Ctrl elsewhere, as Ctrl+1…9 go to a tab there) and each tab shows its number; hold the workspaces' (Ctrl on
//! macOS, Ctrl+Shift elsewhere) and each workspace in the sidebar does. Any key, a click, another modifier, the
//! window going to the back or a popup opening puts them away until the modifiers are all let go, so a quick Ctrl+C,
//! Ctrl+R or Ctrl+Shift+C in a pane never flashes them: the key comes inside the hold and spends it.
//!
//! `Hints` is the state machine, the clock passed in; the Shell feeds it the window's modifier changes
//! (`on_modifiers_changed` on the root), every key down (a keystroke interceptor: a key bound to an action is
//! dispatched before any key-down listener sees it), presses, activation and open popups, and wakes it once when
//! the hold is up. Chips are drawn over the tabs and rows (`chip`), so nothing moves when they show.
use super::*;
use gpui::Modifiers;

/// What the chips number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum HintMode {
    Tabs,
    Workspaces,
}

/// How long the modifier is held alone before the chips show. Short: what keeps a chord from flashing them is its
/// key, which puts them away at once (`cancel`), not the wait.
const HOLD: Duration = Duration::from_millis(150);
const FADE_IN: Duration = Duration::from_millis(120);
const FADE_OUT: Duration = Duration::from_millis(80);

/// What a set of held modifiers asks for.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Held {
    Nothing,
    /// On the way to a mode (Shift before Ctrl+Shift off macOS): no chips, nothing spent.
    Part,
    Mode(HintMode),
    /// A set no hints answer to (Ctrl+Alt, Alt alone off macOS, Shift alone on macOS): spent until everything is let go.
    Other,
}

fn held(m: &Modifiers, mac: bool) -> Held {
    let set = (m.control, m.alt, m.shift, m.platform, m.function);
    match (mac, set) {
        (_, (false, false, false, false, false)) => Held::Nothing,
        (true, (false, false, false, true, false)) => Held::Mode(HintMode::Tabs),
        (true, (true, false, false, false, false)) => Held::Mode(HintMode::Workspaces),
        (false, (true, false, false, false, false)) => Held::Mode(HintMode::Tabs),
        (false, (true, false, true, false, false)) => Held::Mode(HintMode::Workspaces),
        // Shift first, on the way to Ctrl+Shift
        (false, (false, false, true, false, false)) => Held::Part,
        _ => Held::Other,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Idle,
    Waiting { mode: HintMode, since: Instant },
    Shown(HintMode),
    /// Put away: nothing shows until every modifier is up.
    Spent,
}

/// The chips' opacity tween: toward shown or hidden, from the opacity they had when it turned.
#[derive(Clone, Copy, Debug)]
struct Fade {
    mode: HintMode,
    shown: bool,
    from: f32,
    since: Instant,
}

impl Fade {
    fn opacity(&self, now: Instant) -> f32 {
        let t = |d: Duration| (now.saturating_duration_since(self.since).as_secs_f32() / d.as_secs_f32()).min(1.);
        if self.shown {
            self.from + (1. - self.from) * ease_out(t(FADE_IN))
        } else {
            self.from * (1. - ease_out(t(FADE_OUT)))
        }
    }

    fn running(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) < if self.shown { FADE_IN } else { FADE_OUT }
    }
}

pub(super) struct Hints {
    state: State,
    mac: bool,
    hold: Duration,
    fade: Option<Fade>,
}

impl Default for Hints {
    fn default() -> Self {
        Hints::with(cfg!(target_os = "macos"), HOLD)
    }
}

impl Hints {
    fn with(mac: bool, hold: Duration) -> Self {
        Hints { state: State::Idle, mac, hold, fade: None }
    }

    /// The window's modifiers changed. True when the state did (to draw, or to wait anew).
    pub(super) fn modifiers(&mut self, m: &Modifiers, enabled: bool, now: Instant) -> bool {
        let next = match (held(m, self.mac), self.state) {
            (Held::Nothing, _) => State::Idle,
            (_, _) if !enabled => State::Spent,
            (_, State::Spent) | (Held::Other, _) => State::Spent,
            (Held::Part, _) => State::Idle,
            (Held::Mode(mode), State::Waiting { mode: was, .. } | State::Shown(was)) if was == mode => self.state,
            // Ctrl+Shift let go to Ctrl, or the other way, with the chips up: the other chips, at once
            (Held::Mode(mode), State::Shown(_)) => State::Shown(mode),
            (Held::Mode(mode), _) => State::Waiting { mode, since: now },
        };
        self.go(next, now)
    }

    /// A key, a press or a popup: the chips go until the modifiers are let go.
    pub(super) fn cancel(&mut self, now: Instant) -> bool {
        match self.state {
            State::Waiting { .. } | State::Shown(_) => self.go(State::Spent, now),
            State::Idle | State::Spent => false,
        }
    }

    /// The window went to the back: what is held there is not known, so nothing shows until all of it is let go.
    pub(super) fn blur(&mut self, now: Instant) -> bool {
        self.go(State::Spent, now)
    }

    /// The window came to the front with `m` held: a modifier held through the switch shows nothing.
    pub(super) fn activate(&mut self, m: &Modifiers, now: Instant) -> bool {
        let next = if held(m, self.mac) == Held::Nothing { State::Idle } else { State::Spent };
        self.go(next, now)
    }

    /// When the modifier will have been held long enough, while it is held.
    pub(super) fn deadline(&self) -> Option<Instant> {
        match self.state {
            State::Waiting { since, .. } => Some(since + self.hold),
            _ => None,
        }
    }

    /// The hold is up: the chips show. True when they did.
    pub(super) fn tick(&mut self, now: Instant) -> bool {
        match self.state {
            State::Waiting { mode, since } if now >= since + self.hold => self.go(State::Shown(mode), now),
            _ => false,
        }
    }

    /// The chips to draw and their opacity.
    pub(super) fn shown(&self, now: Instant) -> Option<(HintMode, f32)> {
        let fade = self.fade?;
        let opacity = fade.opacity(now);
        (fade.shown || opacity > 0.).then_some((fade.mode, opacity))
    }

    /// The chips are fading in or out: another frame is wanted.
    pub(super) fn fading(&self, now: Instant) -> bool {
        self.fade.is_some_and(|f| f.running(now))
    }

    fn go(&mut self, next: State, now: Instant) -> bool {
        if next == self.state {
            return false;
        }
        let was = match self.state {
            State::Shown(mode) => Some(mode),
            _ => None,
        };
        self.state = next;
        match (was, next) {
            (Some(a), State::Shown(b)) if a == b => {}
            // another mode's chips come in from nothing; a fade out of the same ones turns where it is
            (_, State::Shown(mode)) => {
                let from = self.fade.filter(|f| f.mode == mode && !f.shown).map_or(0., |f| f.opacity(now));
                self.fade = Some(Fade { mode, shown: true, from, since: now });
            }
            (Some(mode), _) => {
                let from = self.fade.map_or(1., |f| f.opacity(now));
                self.fade = Some(Fade { mode, shown: false, from, since: now });
            }
            (None, _) => {}
        }
        true
    }
}

/// What a chip reads for `keys`: the digit the chord ends in (Ctrl+Shift+1 too, which Linux hears as `ctrl-!`), as
/// the modifier held is the one the chips answer to; a chord the user moved off the digits reads in full.
pub(super) fn chip_text(keys: &str) -> String {
    let last = keys.split_whitespace().last().and_then(|k| gpui::Keystroke::parse(k).ok());
    let digit = last.and_then(|k| match k.key.chars().next() {
        Some(c) if c.is_ascii_digit() && k.key.len() == 1 => Some(c),
        _ => crate::bindings::shifted_digit(&k.key),
    });
    match digit {
        Some(d) if keys.split_whitespace().count() == 1 => d.to_string(),
        _ => keys.split_whitespace().map(crate::ui::command_panel::glyphs).collect::<Vec<_>>().join(" "),
    }
}

/// A chip: the chord's number (`chip_text`), over the right end of a tab or a workspace row, at `opacity`. Square at
/// its least, so the single digits line up as one size.
pub(super) fn chip(theme: &Theme, keys: &str, opacity: f32) -> Div {
    let text = chip_text(keys);
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(4.))
        .flex()
        .items_center()
        .opacity(opacity)
        .child(
            div()
                .h(px(18.))
                .min_w(px(18.))
                .px(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.card)
                .shadow_sm()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .font_family("Geist Mono")
                .text_color(theme.accent)
                .child(text),
        )
}

impl Shell {
    /// The chips of `mode` now, if those show: their opacity.
    pub(super) fn hints_of(&self, mode: HintMode) -> Option<f32> {
        self.hints.shown(Instant::now()).filter(|(m, _)| *m == mode).map(|(_, o)| o)
    }

    pub(super) fn hint_modifiers(&mut self, m: &Modifiers, cx: &mut Context<Self>) {
        let enabled = SettingsGlobal::of(cx).settings.show_shortcut_hints;
        if self.hints.modifiers(m, enabled, Instant::now()) {
            self.hints_changed(cx);
        }
    }

    /// A key down or a press anywhere in the window.
    pub(super) fn hint_cancel(&mut self, cx: &mut Context<Self>) {
        if self.hints.cancel(Instant::now()) {
            self.hints_changed(cx);
        }
    }

    pub(super) fn hint_activation(&mut self, active: bool, window: &Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let changed = if active { self.hints.activate(&window.modifiers(), now) } else { self.hints.blur(now) };
        if changed {
            self.hints_changed(cx);
        }
    }

    /// Wakes once when the hold is up (a later change replaces the wait, dropping the last), and draws the change.
    pub(super) fn hints_changed(&mut self, cx: &mut Context<Self>) {
        self.hints_wait = self.hints.deadline().map(|at| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(at.saturating_duration_since(Instant::now())).await;
                let _ = this.update(cx, |s, cx| {
                    if s.hints.tick(Instant::now()) || s.hints.deadline().is_some() {
                        s.hints_changed(cx);
                    }
                });
            })
        });
        cx.notify();
    }

    /// A menu, dialog, panel or herdr's popup is up: the chips are not for it.
    pub(super) fn popup_open(&self) -> bool {
        fn open<T>(p: &Option<crate::ui::Popup<T>>) -> bool {
            p.as_ref().is_some_and(|p| p.is_open())
        }
        open(&self.view_menu)
            || open(&self.agents_menu)
            || open(&self.ws_menu)
            || open(&self.pane_menu)
            || open(&self.spent_menu)
            || open(&self.usage_menu)
            || open(&self.statusbar_menu)
            || open(&self.tab_menu)
            || open(&self.root_menu)
            || open(&self.files_view_menu)
            || open(&self.worktree_dialog)
            || open(&self.remove_confirm)
            || open(&self.merge_confirm)
            || open(&self.session_dialog)
            || open(&self.place_menu)
            || self.tree.menu_open()
            || open(&self.place_edit)
            || open(&self.session_delete)
            || open(&self.command_panel)
            || self.floating_open()
            || self.settings.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn mods(control: bool, shift: bool, alt: bool, platform: bool) -> Modifiers {
        Modifiers { control, shift, alt, platform, ..Default::default() }
    }

    fn linux() -> Hints {
        Hints::with(false, HOLD)
    }

    fn mac() -> Hints {
        Hints::with(true, HOLD)
    }

    const NONE: (bool, bool, bool, bool) = (false, false, false, false);
    const CTRL: (bool, bool, bool, bool) = (true, false, false, false);
    const ALT: (bool, bool, bool, bool) = (false, false, true, false);
    const SHIFT: (bool, bool, bool, bool) = (false, true, false, false);
    const CTRL_SHIFT: (bool, bool, bool, bool) = (true, true, false, false);

    fn press(h: &mut Hints, (c, s, a, p): (bool, bool, bool, bool), now: Instant) -> bool {
        h.modifiers(&mods(c, s, a, p), true, now)
    }

    fn mode(h: &Hints, now: Instant) -> Option<HintMode> {
        h.shown(now).filter(|(_, o)| *o > 0.).map(|(m, _)| m)
    }

    #[test]
    fn holding_ctrl_shows_the_tabs_after_the_hold() {
        let (mut h, t0) = (linux(), Instant::now());
        assert!(press(&mut h, CTRL, t0));
        assert_eq!(h.deadline(), Some(t0 + ms(150)));
        assert!(!h.tick(t0 + ms(149)), "not before the hold is up");
        assert_eq!(h.shown(t0 + ms(149)), None);
        assert!(h.tick(t0 + ms(150)));
        assert_eq!(h.deadline(), None);
        assert_eq!(mode(&h, t0 + ms(160)), Some(HintMode::Tabs));
        assert!(h.fading(t0 + ms(200)));
        assert_eq!(h.shown(t0 + ms(150) + FADE_IN), Some((HintMode::Tabs, 1.)));
        assert!(!h.fading(t0 + ms(150) + FADE_IN));
    }

    #[test]
    fn ctrl_shift_shows_the_workspaces_and_shift_first_counts() {
        let (mut h, t0) = (linux(), Instant::now());
        press(&mut h, SHIFT, t0);
        assert_eq!(h.deadline(), None, "Shift alone is on its way, not spent");
        press(&mut h, CTRL_SHIFT, t0 + ms(10));
        assert!(h.tick(t0 + ms(160)));
        assert_eq!(mode(&h, t0 + ms(200)), Some(HintMode::Workspaces));
        // Ctrl first: the tabs' wait, then the workspaces' from the Shift on
        press(&mut h, NONE, t0 + ms(600));
        press(&mut h, CTRL, t0 + ms(700));
        assert_eq!(h.deadline(), Some(t0 + ms(850)), "Ctrl alone waits for the tabs");
        press(&mut h, CTRL_SHIFT, t0 + ms(710));
        assert_eq!(h.deadline(), Some(t0 + ms(860)), "the hold starts anew for the workspaces");
        assert!(!h.tick(t0 + ms(850)));
        assert!(h.tick(t0 + ms(860)));
        assert_eq!(mode(&h, t0 + ms(1000)), Some(HintMode::Workspaces));
        // Shift let go with Ctrl still down: the tabs' numbers at once
        press(&mut h, CTRL, t0 + ms(1300));
        assert_eq!(mode(&h, t0 + ms(1300) + FADE_IN), Some(HintMode::Tabs));
    }

    #[test]
    fn alt_alone_shows_nothing_off_macos() {
        let (mut h, t0) = (linux(), Instant::now());
        press(&mut h, ALT, t0);
        assert_eq!(h.deadline(), None, "Alt is no longer the tabs' modifier");
        assert!(!h.tick(t0 + ms(2000)));
        assert_eq!(h.shown(t0 + ms(2000)), None);
        // nor does Ctrl join it: spent until both are let go
        press(&mut h, (true, false, true, false), t0 + ms(2100));
        press(&mut h, CTRL, t0 + ms(2200));
        assert!(!h.tick(t0 + ms(3000)));
    }

    #[test]
    fn ctrl_c_ctrl_r_and_ctrl_shift_c_never_flash_them() {
        let (mut h, t0) = (linux(), Instant::now());
        for held in [CTRL, CTRL, CTRL_SHIFT, CTRL] {
            press(&mut h, held, t0);
            assert!(h.cancel(t0 + ms(80)), "the key puts them away");
            assert!(!h.tick(t0 + ms(900)));
            assert_eq!(h.shown(t0 + ms(900)), None);
            assert_eq!(h.deadline(), None);
            press(&mut h, NONE, t0 + ms(950));
        }
    }

    /// The hold is short, so what keeps a chord from flashing them is the key: Ctrl+S, Ctrl+C, Ctrl+Tab, Ctrl+1 and
    /// Ctrl+Shift+1 typed at any point inside the hold, even just before it is up, put them away for good.
    #[test]
    fn a_chord_inside_the_hold_never_shows_them() {
        assert_eq!(HOLD, ms(150));
        for key_at in [1, 30, 100, 149] {
            for held in [CTRL, CTRL_SHIFT] {
                let (mut h, t0) = (linux(), Instant::now());
                press(&mut h, held, t0);
                assert!(h.cancel(t0 + ms(key_at)), "the key at {key_at} ms puts them away");
                assert_eq!(h.deadline(), None);
                assert!(!h.tick(t0 + ms(150)) && !h.tick(t0 + ms(5000)));
                assert_eq!(h.shown(t0 + ms(5000)), None, "never shown for a key at {key_at} ms");
                // the chord's key repeats or a second chord follows with the modifier still down: still nothing
                assert!(!h.cancel(t0 + ms(400)));
                assert!(!press(&mut h, held, t0 + ms(450)));
                assert!(!h.tick(t0 + ms(5000)));
            }
        }
    }

    #[test]
    fn a_key_hides_them_until_the_modifiers_are_let_go() {
        let (mut h, t0) = (linux(), Instant::now());
        press(&mut h, CTRL, t0);
        h.tick(t0 + ms(150));
        assert!(h.cancel(t0 + ms(600)));
        assert!(h.fading(t0 + ms(620)));
        assert_eq!(h.shown(t0 + ms(600) + FADE_OUT), None, "faded out");
        assert!(!press(&mut h, CTRL_SHIFT, t0 + ms(700)), "still spent");
        assert!(!h.tick(t0 + ms(2000)));
        press(&mut h, NONE, t0 + ms(2100));
        press(&mut h, CTRL, t0 + ms(2200));
        assert!(h.tick(t0 + ms(2350)), "held again, shown again");
    }

    #[test]
    fn another_modifier_spends_them() {
        let (mut h, t0) = (linux(), Instant::now());
        press(&mut h, CTRL, t0);
        press(&mut h, (true, false, true, false), t0 + ms(60));
        assert!(!h.tick(t0 + ms(600)));
        press(&mut h, CTRL, t0 + ms(700)); // Alt let go, Ctrl still down
        assert!(!h.tick(t0 + ms(1200)), "spent until Ctrl is let go too");
    }

    #[test]
    fn switching_modes_while_shown_swaps_the_chips_and_while_waiting_restarts_the_hold() {
        let (mut h, t0) = (linux(), Instant::now());
        press(&mut h, CTRL, t0);
        press(&mut h, CTRL_SHIFT, t0 + ms(60));
        assert_eq!(h.deadline(), Some(t0 + ms(210)));
        h.tick(t0 + ms(210));
        assert_eq!(mode(&h, t0 + ms(260)), Some(HintMode::Workspaces));
        press(&mut h, CTRL, t0 + ms(500));
        assert_eq!(mode(&h, t0 + ms(550)), Some(HintMode::Tabs));
    }

    #[test]
    fn blur_and_activation() {
        let (mut h, t0) = (linux(), Instant::now());
        press(&mut h, CTRL, t0);
        h.tick(t0 + ms(150));
        assert!(h.blur(t0 + ms(500)));
        assert_eq!(h.shown(t0 + ms(700)), None);
        // back with Ctrl still held: nothing until it is let go
        h.activate(&mods(true, false, false, false), t0 + ms(800));
        assert!(!press(&mut h, CTRL, t0 + ms(810)));
        assert!(!h.tick(t0 + ms(1500)));
        h.activate(&Modifiers::default(), t0 + ms(1600));
        press(&mut h, CTRL, t0 + ms(1700));
        assert!(h.tick(t0 + ms(1850)));
    }

    #[test]
    fn turned_off_nothing_shows() {
        let (mut h, t0) = (linux(), Instant::now());
        h.modifiers(&mods(true, false, false, false), false, t0);
        assert_eq!(h.deadline(), None);
        assert!(!h.tick(t0 + ms(1000)));
    }

    #[test]
    fn mac_holds_cmd_for_tabs_and_ctrl_for_workspaces() {
        let (mut h, t0) = (mac(), Instant::now());
        press(&mut h, (false, false, false, true), t0);
        assert_eq!(h.deadline(), Some(t0 + ms(150)));
        h.tick(t0 + ms(150));
        assert_eq!(mode(&h, t0 + ms(300)), Some(HintMode::Tabs));
        press(&mut h, NONE, t0 + ms(400));
        press(&mut h, CTRL, t0 + ms(500));
        h.tick(t0 + ms(650));
        assert_eq!(mode(&h, t0 + ms(800)), Some(HintMode::Workspaces));
        press(&mut h, NONE, t0 + ms(900));
        press(&mut h, (false, true, false, false), t0 + ms(1000));
        assert!(!press(&mut h, (false, true, false, true), t0 + ms(1010)), "Shift first is spent on macOS");
        assert_eq!(h.deadline(), None);
    }

    #[test]
    fn letting_go_fades_them_out() {
        let (mut h, t0) = (linux(), Instant::now());
        press(&mut h, CTRL, t0);
        h.tick(t0 + ms(150));
        press(&mut h, NONE, t0 + ms(150) + FADE_IN);
        let mid = h.shown(t0 + ms(150) + FADE_IN + ms(40)).unwrap().1;
        assert!(mid > 0. && mid < 1., "{mid}");
        assert_eq!(h.shown(t0 + ms(150) + FADE_IN + FADE_OUT), None);
        assert!(!h.fading(t0 + ms(150) + FADE_IN + FADE_OUT));
    }

    /// What a chord's keys hold down: its modifiers, and Shift for the shifted digit Linux hears Ctrl+Shift+1 as.
    fn held_by(chord: &str) -> Modifiers {
        let k = gpui::Keystroke::parse(chord).unwrap();
        let mut m = k.modifiers;
        m.shift |= crate::bindings::shifted_digit(&k.key).is_some();
        m
    }

    #[test]
    fn the_hint_modifiers_are_the_numbered_chords_own() {
        // what is held to see the chips is what goes with the digit: Cmd and Ctrl on macOS, Ctrl and Ctrl+Shift elsewhere
        let default = |action: &str| crate::bindings::DEFAULTS.iter().find(|(a, _)| *a == action).unwrap().1;
        let mac = cfg!(target_os = "macos");
        for (actions, mode) in [(crate::bindings::TAB_ACTIONS, HintMode::Tabs), (crate::bindings::WORKSPACE_ACTIONS, HintMode::Workspaces)] {
            for action in actions {
                assert_eq!(held(&held_by(default(action)), mac), Held::Mode(mode), "{action}");
            }
        }
    }

    #[test]
    fn chips_read_just_the_number() {
        assert_eq!(chip_text("ctrl-1"), "1");
        assert_eq!(chip_text("cmd-9"), "9");
        assert_eq!(chip_text("alt-4"), "4");
        // Ctrl+Shift+3 as Linux hears it
        assert_eq!(chip_text("ctrl-#"), "3");
        assert_eq!(chip_text("ctrl-("), "9");
        // a chord the user moved off the digits reads in full
        assert_ne!(chip_text("ctrl-alt-j"), "j");
        // every default numbered chord reads as its own number
        let default = |action: &str| crate::bindings::DEFAULTS.iter().find(|(a, _)| *a == action).unwrap().1;
        for actions in [crate::bindings::TAB_ACTIONS, crate::bindings::WORKSPACE_ACTIONS] {
            for (i, action) in actions.iter().enumerate() {
                assert_eq!(chip_text(default(action)), (i + 1).to_string(), "{action}");
            }
        }
    }
}
