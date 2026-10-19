//! A modifier tapped on its own as herdr's prefix: Super (Cmd on a Mac) or Ctrl+Shift pressed and let go with no key
//! in between. GPUI reports no keystroke for a lone modifier, so Shepherd watches the window's modifier changes
//! itself. A tap that fires dispatches [`VIRTUAL_PREFIX`], a keystroke no keyboard has, to the window; herdr's
//! `prefix+X` chords are bound after it in a pane (`herdr::keys`) and GPUI's pending-chord handling does the rest:
//! the same waiting state, timeout and second-key lookup as a real prefix chord. Pure: the state machines take their
//! events and the time, so every rule here has a test.
use std::time::{Duration, Instant};

/// A tap is the modifiers down and up within this long: longer is a modifier held (to hold Super while doing
/// something else), not a tap. 400 ms is long enough for a slow tap and shorter than a deliberate hold.
pub const TAP_MAX: Duration = Duration::from_millis(400);

/// The keystroke a tap stands for: herdr's `prefix+X` chords are bound as this, then X. No keyboard has an F24 with
/// every modifier on it, and a pane's own key handler never types it (`terminal::pane::on_key_down`).
pub const VIRTUAL_PREFIX: &str = "ctrl-alt-shift-f24";

/// The modifier keys, as the window reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    /// Super off macOS, Cmd on it.
    pub platform: bool,
    pub function: bool,
}

impl Mods {
    pub fn of(m: &gpui::Modifiers) -> Mods {
        Mods { control: m.control, alt: m.alt, shift: m.shift, platform: m.platform, function: m.function }
    }

    pub fn is_empty(self) -> bool {
        self == Mods::default()
    }

    /// Every modifier of `self` is in `other`.
    pub fn is_subset_of(self, other: Mods) -> bool {
        (!self.control || other.control)
            && (!self.alt || other.alt)
            && (!self.shift || other.shift)
            && (!self.platform || other.platform)
            && (!self.function || other.function)
    }

    pub fn union(self, other: Mods) -> Mods {
        Mods {
            control: self.control || other.control,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
            platform: self.platform || other.platform,
            function: self.function || other.function,
        }
    }

    /// As Shepherd's settings keep it: `super`, `ctrl-shift`; modifiers in the order ctrl, alt, shift, super. None when
    /// empty.
    pub fn label(self) -> Option<String> {
        let names: Vec<&str> = [(self.control, "ctrl"), (self.alt, "alt"), (self.shift, "shift"), (self.platform, "super")]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect();
        (!names.is_empty() && !self.function).then(|| names.join("-"))
    }

    /// The modifiers a settings label names (`cmd` and `super` are the platform key); None for an empty or unknown one.
    pub fn parse(label: &str) -> Option<Mods> {
        let mut m = Mods::default();
        for part in label.split('-').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => m.control = true,
                "alt" | "option" => m.alt = true,
                "shift" => m.shift = true,
                "super" | "cmd" | "command" | "win" => m.platform = true,
                _ => return None,
            }
        }
        (!m.is_empty()).then_some(m)
    }

    /// As the Shortcuts page shows a tap: `Super tap` (`Cmd tap` on a Mac), `Ctrl+Shift tap`.
    pub fn display(self) -> String {
        let (alt, platform) = if cfg!(target_os = "macos") { ("Opt", "Cmd") } else { ("Alt", "Super") };
        let names: Vec<&str> = [(self.control, "Ctrl"), (self.alt, alt), (self.shift, "Shift"), (self.platform, platform)]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect();
        format!("{} tap", names.join("+"))
    }
}

/// What the window tells the tap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The modifiers held changed to these.
    Mods(Mods),
    /// A key went down (a held key repeats this): the modifiers are part of a chord.
    Key,
    /// A mouse button went down: Ctrl+click is a chord too.
    Click,
}

/// Watches for one modifier set tapped alone.
#[derive(Debug, Default)]
pub struct Tap {
    /// The modifiers held as of the last event.
    held: Mods,
    /// When the wanted set came to be held, while it is held and nothing else has happened.
    armed: Option<Instant>,
}

impl Tap {
    /// Feeds an event; true when it completes a tap of `wanted`: the set came to be held, nothing but its own
    /// release followed (no key, no click, no other modifier), and it was let go within [`TAP_MAX`]. The release
    /// of the first of its modifiers completes it, so Ctrl+Shift taps do not wait for both.
    pub fn step(&mut self, wanted: Mods, event: Event, now: Instant) -> bool {
        match event {
            Event::Key => {
                self.armed = None;
                false
            }
            Event::Click => {
                self.armed = None;
                false
            }
            Event::Mods(m) => {
                let before = std::mem::replace(&mut self.held, m);
                if m == wanted {
                    // only on the change that makes the set from fewer modifiers: a repeat of the same state (a lock key
                    // changing), or the set come back by letting go of an extra one, is no fresh press
                    if before != wanted && before.is_subset_of(wanted) && self.armed.is_none() {
                        self.armed = Some(now);
                    }
                    return false;
                }
                let Some(since) = self.armed.take() else { return false };
                m.is_subset_of(wanted) && now.saturating_duration_since(since) <= TAP_MAX
            }
        }
    }
}

/// Watches the Shortcuts page's recorder for a modifier set tapped alone: whatever was held, once all are let go
/// with no key or click in between.
#[derive(Debug, Default)]
pub struct TapRecorder {
    peak: Mods,
    spoiled: bool,
}

impl TapRecorder {
    /// Feeds an event; the modifiers tapped when the last is let go.
    pub fn step(&mut self, event: Event) -> Option<Mods> {
        match event {
            Event::Key => {
                self.spoiled = true;
                None
            }
            Event::Click => {
                self.spoiled = true;
                None
            }
            Event::Mods(m) if m.is_empty() => {
                let tapped = (!self.spoiled && !self.peak.is_empty()).then_some(self.peak);
                *self = TapRecorder::default();
                tapped
            }
            Event::Mods(m) => {
                self.peak = self.peak.union(m);
                None
            }
        }
    }

    /// Starts over: a new recording.
    pub fn reset(&mut self) {
        *self = TapRecorder::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPER: Mods = Mods { control: false, alt: false, shift: false, platform: true, function: false };
    const CTRL: Mods = Mods { control: true, alt: false, shift: false, platform: false, function: false };
    const SHIFT: Mods = Mods { control: false, alt: false, shift: true, platform: false, function: false };
    const CTRL_SHIFT: Mods = Mods { control: true, alt: false, shift: true, platform: false, function: false };
    const NONE: Mods = Mods { control: false, alt: false, shift: false, platform: false, function: false };

    /// Feeds `events` at the given milliseconds; whether each completed a tap.
    fn run(wanted: Mods, events: &[(u64, Event)]) -> Vec<bool> {
        let (mut tap, t0) = (Tap::default(), Instant::now());
        events.iter().map(|(ms, e)| tap.step(wanted, *e, t0 + Duration::from_millis(*ms))).collect()
    }

    fn m(m: Mods) -> Event {
        Event::Mods(m)
    }

    #[test]
    fn a_modifier_pressed_and_released_alone_is_a_tap() {
        assert_eq!(run(SUPER, &[(0, m(SUPER)), (120, m(NONE))]), [false, true]);
        assert_eq!(run(CTRL_SHIFT, &[(0, m(CTRL)), (30, m(CTRL_SHIFT)), (150, m(CTRL)), (170, m(NONE))]), [false, false, true, false], "the first release of the pair ends it, once");
        assert_eq!(run(CTRL_SHIFT, &[(0, m(SHIFT)), (30, m(CTRL_SHIFT)), (150, m(SHIFT)), (170, m(NONE))]), [false, false, true, false], "either order");
    }

    #[test]
    fn a_modifier_in_a_chord_is_no_tap() {
        // Super+T: the key goes down while Super is held
        assert_eq!(run(SUPER, &[(0, m(SUPER)), (60, Event::Key), (120, m(NONE))]), [false, false, false]);
        // a key held to repeat
        assert_eq!(run(SUPER, &[(0, m(SUPER)), (60, Event::Key), (110, Event::Key), (160, Event::Key), (400, m(NONE))]), [false, false, false, false, false]);
        // Ctrl+click
        assert_eq!(run(CTRL, &[(0, m(CTRL)), (50, Event::Click), (90, m(NONE))]), [false, false, false]);
        // a second modifier joins: Super+Shift is another chord
        assert_eq!(run(SUPER, &[(0, m(SUPER)), (40, m(Mods { shift: true, ..SUPER })), (90, m(SUPER)), (130, m(NONE))]), [false, false, false, false]);
    }

    #[test]
    fn a_key_that_ended_a_chord_leaves_the_modifier_unarmed_until_it_is_pressed_again() {
        // Super held through a chord, the key let go, then Super let go: nothing; a repeat of the same state does not arm
        let events = [(0, m(SUPER)), (50, Event::Key), (100, m(SUPER)), (150, m(NONE)), (300, m(SUPER)), (360, m(NONE))];
        assert_eq!(run(SUPER, &events), [false, false, false, false, false, true], "only the fresh press and release");
    }

    #[test]
    fn a_hold_is_not_a_tap() {
        assert_eq!(run(SUPER, &[(0, m(SUPER)), (400, m(NONE))]), [false, true], "at the limit it still is");
        assert_eq!(run(SUPER, &[(0, m(SUPER)), (401, m(NONE))]), [false, false]);
        assert_eq!(run(SUPER, &[(0, m(SUPER)), (2000, m(NONE))]), [false, false]);
    }

    #[test]
    fn other_modifiers_alone_are_no_tap_of_this_one() {
        assert_eq!(run(SUPER, &[(0, m(CTRL)), (50, m(NONE))]), [false, false]);
        assert_eq!(run(CTRL_SHIFT, &[(0, m(CTRL)), (50, m(NONE))]), [false, false], "half of the pair");
        assert_eq!(run(NONE, &[(0, m(SUPER)), (50, m(NONE))]), [false, false], "no tap set: nothing fires");
    }

    #[test]
    fn two_taps_fire_twice() {
        assert_eq!(run(SUPER, &[(0, m(SUPER)), (60, m(NONE)), (200, m(SUPER)), (260, m(NONE))]), [false, true, false, true]);
    }

    #[test]
    fn labels_round_trip_and_read_by_platform() {
        for (label, mods) in [("super", SUPER), ("ctrl-shift", CTRL_SHIFT), ("ctrl", CTRL)] {
            assert_eq!(mods.label().as_deref(), Some(label));
            assert_eq!(Mods::parse(label), Some(mods));
        }
        assert_eq!(Mods::parse("cmd"), Some(SUPER), "Cmd is the platform key");
        assert_eq!(Mods::parse("shift-ctrl"), Some(CTRL_SHIFT));
        assert_eq!((Mods::parse(""), Mods::parse("super-t"), Mods::parse("hyper")), (None, None, None));
        assert_eq!(NONE.label(), None);
        assert_eq!(SUPER.display(), if cfg!(target_os = "macos") { "Cmd tap" } else { "Super tap" });
        assert_eq!(CTRL_SHIFT.display(), "Ctrl+Shift tap");
    }

    fn record(events: &[Event]) -> Vec<Option<Mods>> {
        let mut r = TapRecorder::default();
        events.iter().map(|e| r.step(*e)).collect()
    }

    #[test]
    fn the_recorder_takes_the_modifiers_tapped_and_nothing_with_a_key() {
        assert_eq!(record(&[m(SUPER), m(NONE)]), [None, Some(SUPER)]);
        assert_eq!(record(&[m(CTRL), m(CTRL_SHIFT), m(CTRL), m(NONE)]), [None, None, None, Some(CTRL_SHIFT)], "the pair, once all are up");
        assert_eq!(record(&[m(SUPER), Event::Key, m(NONE)]), [None, None, None], "Super+T is a chord");
        assert_eq!(record(&[m(SUPER), Event::Click, m(NONE)]), [None, None, None]);
        // and it starts clean after a chord
        assert_eq!(record(&[m(SUPER), Event::Key, m(NONE), m(CTRL), m(NONE)]), [None, None, None, None, Some(CTRL)]);
        assert_eq!(record(&[m(NONE)]), [None], "nothing held, nothing tapped");
    }
}
