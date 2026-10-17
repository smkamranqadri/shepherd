//! Whether the machine runs on its battery, so the background checks (the watchers' polls, the status bar's git and
//! PR lookups, the side panel's polls) can go at half the pace there: read every 30 s off the main thread, from
//! sysfs on Linux and IOKit on macOS; Windows is taken to be on mains power.
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The last reading: on battery. Mains power until the first one lands.
static ON_BATTERY: AtomicBool = AtomicBool::new(false);

/// How often the power source is read again: unplugging is noticed within this.
const REFRESH: Duration = Duration::from_secs(30);

/// Starts reading the power source on `rt`, now and every [`REFRESH`]; once per process, however many windows call it.
pub fn start(rt: &tokio::runtime::Handle) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    rt.spawn(async {
        loop {
            // sysfs and IOKit reads block, briefly; a panic in one keeps the last reading
            if let Ok(battery) = tokio::task::spawn_blocking(read).await {
                ON_BATTERY.store(battery, Ordering::Relaxed);
            }
            tokio::time::sleep(REFRESH).await;
        }
    });
}

/// The machine runs on its battery, as last read.
pub fn on_battery() -> bool {
    ON_BATTERY.load(Ordering::Relaxed)
}

/// A background check's interval as it stands: twice as long on battery.
pub fn stretch(d: Duration) -> Duration {
    stretch_on(d, on_battery())
}

/// `d`, doubled on battery; saturating, so a `Duration::MAX` meaning "never" stays never.
pub fn stretch_on(d: Duration, battery: bool) -> Duration {
    if battery { d.saturating_mul(2) } else { d }
}

/// One entry of `/sys/class/power_supply`, the attributes it has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Supply {
    /// `type`: `Mains`, `Battery`, `USB`, ...
    pub kind: String,
    /// `online`, for a supply that has it.
    pub online: Option<bool>,
    /// `scope`: `System`, or `Device` for a mouse's or a headset's own battery.
    pub scope: Option<String>,
    /// `status`, for a battery: `Charging`, `Discharging`, `Not charging`, `Full`, `Unknown`.
    pub status: Option<String>,
}

/// The kinds that bring power in. The kernel now reports USB-C ports as `USB` with a `usb_type`, older ones by these
/// names.
const EXTERNAL: [&str; 9] = ["Mains", "USB", "USB_C", "USB_PD", "USB_PD_DRP", "USB_DCP", "USB_CDP", "USB_ACA", "Wireless"];

/// On battery, from the machine's supplies: any external supply online means mains power; otherwise a battery that
/// says it discharges, or one with every external supply offline, means battery. A supply scoped to a device (a
/// mouse's or a headset's battery) says nothing about the machine and is left out. Anything else, a desktop with
/// no supplies listed, a battery alone saying `Full`, counts as mains power, so a missed reading never slows things.
pub fn on_battery_of(supplies: &[Supply]) -> bool {
    let system: Vec<&Supply> = supplies.iter().filter(|s| s.scope.as_deref() != Some("Device")).collect();
    let external: Vec<&Supply> = system.iter().copied().filter(|s| EXTERNAL.contains(&s.kind.as_str())).collect();
    if external.iter().any(|s| s.online == Some(true)) {
        return false;
    }
    let batteries: Vec<&Supply> = system.iter().copied().filter(|s| s.kind == "Battery").collect();
    let discharging = batteries.iter().any(|b| b.status.as_deref() == Some("Discharging"));
    let unplugged = !external.is_empty() && external.iter().all(|s| s.online == Some(false));
    discharging || (!batteries.is_empty() && unplugged)
}

/// The supplies listed under `dir` (`/sys/class/power_supply`), each with the attributes it has; none when it
/// cannot be read.
#[cfg(target_os = "linux")]
fn supplies(dir: &std::path::Path) -> Vec<Supply> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let attr = |p: &std::path::Path, name: &str| std::fs::read_to_string(p.join(name)).ok().map(|s| s.trim().to_string());
    entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            Some(Supply {
                kind: attr(&p, "type")?,
                online: attr(&p, "online").map(|v| v != "0"),
                scope: attr(&p, "scope"),
                status: attr(&p, "status"),
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn read() -> bool {
    on_battery_of(&supplies(std::path::Path::new("/sys/class/power_supply")))
}

/// On battery, from IOKit's name for the source that powers the Mac.
pub fn mac_source(name: &str) -> bool {
    name == "Battery Power"
}

/// The source that powers the Mac (`IOPSGetProvidingPowerSourceType`): "AC Power", "Battery Power" or "UPS Power".
#[cfg(target_os = "macos")]
fn read() -> bool {
    use std::ffi::{c_char, c_void};
    type Ref = *const c_void;
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> Ref;
        fn IOPSGetProvidingPowerSourceType(blob: Ref) -> Ref;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(s: Ref, buf: *mut c_char, len: isize, encoding: u32) -> bool;
        fn CFRelease(r: Ref);
    }
    const UTF8: u32 = 0x0800_0100;
    // SAFETY: the blob is created here (the Copy rule) and released here; the name is borrowed from it (the Get
    // rule) and copied into `buf` before the release
    unsafe {
        let blob = IOPSCopyPowerSourcesInfo();
        if blob.is_null() {
            return false;
        }
        let kind = IOPSGetProvidingPowerSourceType(blob);
        let mut buf = [0 as c_char; 64];
        let named = !kind.is_null() && CFStringGetCString(kind, buf.as_mut_ptr(), buf.len() as isize, UTF8);
        CFRelease(blob);
        named && std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().is_ok_and(mac_source)
    }
}

/// Windows, and anything else: taken to be on mains power.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(kind: &str, online: Option<bool>, scope: Option<&str>, status: Option<&str>) -> Supply {
        Supply { kind: kind.into(), online, scope: scope.map(Into::into), status: status.map(Into::into) }
    }

    /// A ThinkPad on its charger, as its sysfs read: two batteries, the mains adapter and two USB-C ports, one
    /// of them charging.
    fn thinkpad() -> Vec<Supply> {
        vec![
            supply("Mains", Some(true), None, None),
            supply("Battery", None, None, Some("Not charging")),
            supply("Battery", None, None, Some("Charging")),
            supply("USB", Some(false), Some("System"), Some("Not charging")),
            supply("USB", Some(true), Some("System"), Some("Charging")),
        ]
    }

    #[test]
    fn this_laptop_on_its_charger_is_on_mains() {
        assert!(!on_battery_of(&thinkpad()));
        // a charger too weak to keep up: the battery drains, but the machine is plugged in
        let weak = vec![supply("Mains", Some(true), None, None), supply("Battery", None, None, Some("Discharging"))];
        assert!(!on_battery_of(&weak), "external online wins");
    }

    #[test]
    fn unplugged_is_on_battery() {
        let unplugged = vec![
            supply("Mains", Some(false), None, None),
            supply("Battery", None, None, Some("Discharging")),
            supply("Battery", None, None, Some("Not charging")),
            supply("USB", Some(false), Some("System"), Some("Not charging")),
            supply("USB", Some(false), Some("System"), Some("Not charging")),
        ];
        assert!(on_battery_of(&unplugged));
        // a full battery says "Not charging" or "Full", not "Discharging": every external offline says it
        let full = vec![supply("Mains", Some(false), None, None), supply("Battery", None, None, Some("Full"))];
        assert!(on_battery_of(&full), "all external offline");
    }

    #[test]
    fn discharging_alone_is_on_battery() {
        assert!(on_battery_of(&[supply("Battery", None, None, Some("Discharging"))]));
    }

    #[test]
    fn a_desktop_with_no_supplies_is_on_mains() {
        assert!(!on_battery_of(&[]));
        assert!(!on_battery_of(&[supply("Mains", Some(true), None, None)]));
        assert!(!on_battery_of(&[supply("Battery", None, None, Some("Full"))]), "a battery alone, no adapter listed");
    }

    #[test]
    fn a_peripherals_battery_says_nothing_about_the_machine() {
        let mouse = vec![supply("Mains", Some(true), None, None), supply("Battery", None, Some("Device"), Some("Discharging"))];
        assert!(!on_battery_of(&mouse));
        let only = vec![supply("Battery", None, Some("Device"), Some("Discharging"))];
        assert!(!on_battery_of(&only), "a desktop with a wireless mouse");
    }

    #[test]
    fn a_usb_c_charger_alone_is_on_mains() {
        for kind in ["USB", "USB_PD", "USB_PD_DRP", "USB_C", "Wireless"] {
            let supplies = vec![supply("Battery", None, None, Some("Charging")), supply(kind, Some(true), Some("System"), None)];
            assert!(!on_battery_of(&supplies), "{kind}");
        }
    }

    #[test]
    fn the_macs_source_names_battery_power() {
        assert!(mac_source("Battery Power"));
        assert!(!mac_source("AC Power"));
        assert!(!mac_source("UPS Power"));
        assert!(!mac_source(""));
    }

    #[test]
    fn stretching_doubles_on_battery_and_keeps_never() {
        assert_eq!(stretch_on(Duration::from_secs(5), true), Duration::from_secs(10));
        assert_eq!(stretch_on(Duration::from_secs(5), false), Duration::from_secs(5));
        assert_eq!(stretch_on(Duration::MAX, true), Duration::MAX);
    }
}
