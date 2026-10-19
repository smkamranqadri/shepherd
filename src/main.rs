// a release build on Windows is a windowed app: no console window of its own
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // a PDF tab's worker process (`files::pdf_worker`): no window, no runtime, only its pipes
    if std::env::args_os().nth(1).is_some_and(|a| a == shepherd::files::pdf_worker::ARG) {
        shepherd::files::pdf_worker::serve();
        return;
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio");
    let handle = rt.handle().clone();
    gpui_platform::application().with_assets(shepherd::assets::Assets).run(move |cx| {
        gpui_base::init(cx);
        shepherd::notify::set_app_icon();
        shepherd::ui::command_panel::init(cx);
        shepherd::assets::register_fonts(cx);
        let path = shepherd::settings::Settings::path();
        let (settings, error) = shepherd::settings::Settings::load(&path);
        shepherd::herdr::set_binary(settings.herdr_bin.clone());
        shepherd::herdr::set_socket(settings.herdr_socket.clone());
        cx.set_global(settings.theme(cx.window_appearance()));
        let keys = settings.keys.clone();
        cx.set_global(shepherd::settings::SettingsGlobal { settings, error, path });
        // after the settings are global: a tap prefix is one of them
        shepherd::bindings::rebind(cx, &keys);
        shepherd::bindings::bind_editor_keys(cx);
        // herdr's chords follow its config.toml from here on, whoever edits it
        shepherd::bindings::watch_herdr_config(cx);
        // unsaved file tabs ask first
        cx.on_action(|_: &shepherd::bindings::Quit, cx| shepherd::shell::Shell::quit(cx));
        // off macOS an app with no windows left has nothing to show: closing the last one quits, as GNOME expects
        if !cfg!(target_os = "macos") {
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        }
        shepherd::bindings::init_menu_actions(cx);
        cx.on_action(|_: &shepherd::bindings::OpenSettings, cx| shepherd::shell::Shell::toggle_settings(cx));
        use shepherd::{settings::step_font_size, ui::settings::update};
        cx.on_action(|_: &shepherd::bindings::FontSizeUp, cx| update(cx, |s| s.font_size = step_font_size(s.font_size, 1.0)));
        cx.on_action(|_: &shepherd::bindings::FontSizeDown, cx| update(cx, |s| s.font_size = step_font_size(s.font_size, -1.0)));
        cx.on_action(|_: &shepherd::bindings::ToggleSecureInput, cx| update(cx, |s| s.secure_input = !s.secure_input));
        cx.on_action(|_: &shepherd::bindings::FontSizeReset, cx| {
            update(cx, |s| s.font_size = shepherd::settings::Settings::default().font_size)
        });
        cx.set_global(shepherd::ui::settings::Tokio(handle.clone()));
        shepherd::update::start(cx);
        shepherd::notify::warn_if_denied(cx);

        shepherd::shell::Shell::open(handle.clone(), cx);
        if std::env::var_os("SHEPHERD_SETTINGS").is_some() {
            shepherd::shell::Shell::toggle_settings(cx);
        }
        // `SHEPHERD_PANEL=commands|agents|workspaces|sessions|shortcuts`: opens that panel once the first snapshot is likely in (screenshots).
        if let Ok(which) = std::env::var("SHEPHERD_PANEL") {
            use shepherd::ui::command_panel::Kind;
            let kind = match which.as_str() {
                "workspaces" => Kind::Workspaces,
                "sessions" => Kind::Places,
                "shortcuts" => Kind::Shortcuts,
                _ => Kind::Commands,
            };
            cx.spawn(async move |cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                cx.update(|cx| {
                    let Some(window) = cx.windows().into_iter().find_map(|w| w.downcast::<shepherd::shell::Shell>()) else { return };
                    let _ = window.update(cx, |shell, window, cx| shell.toggle_command_panel(kind, window, cx));
                })
            })
            .detach();
        }
        cx.activate(true);
    });
    drop(rt);
}
