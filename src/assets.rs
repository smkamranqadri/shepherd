//! Fonts compiled into the binary (licenses in assets/NOTICE). From Collie's assets.rs.
use gpui::App;
use std::borrow::Cow;

const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/Geist.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMono.ttf"),
];

/// Adds Geist and Geist Mono; without them gpui falls back to the system font.
pub fn register_fonts(cx: &App) {
    if let Err(e) = cx.text_system().add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect()) {
        eprintln!("shepherd: cannot load the bundled fonts: {e:#}");
    }
}

/// Zeron's sidebar, settings and tab icons (agent marks included), served at `icons/<name>.svg`.
pub struct Assets;

macro_rules! icons {
    ($name:expr, $($icon:literal),+ $(,)?) => {
        match $name {
            $($icon => Some(Cow::Borrowed(include_bytes!(concat!("../assets/icons/", $icon, ".svg")).as_slice())),)+
            _ => None,
        }
    };
}

/// The names of the colour file-type and folder icons in assets/icons/material/ (SVG by name, `-open` for an open
/// folder), once, for `material_svg` here and `ALL` in files/icons.rs.
macro_rules! material_names {
    ($m:ident) => {
        $m!(
            "angular",
            "astro",
            "audio",
            "babel",
            "bun",
            "certificate",
            "changelog",
            "clojure",
            "cmake",
            "console",
            "contributing",
            "cpp",
            "crystal",
            "csharp",
            "css",
            "c",
            "dart",
            "database",
            "deno",
            "diff",
            "docker",
            "document",
            "editorconfig",
            "elixir",
            "erlang",
            "eslint",
            "exe",
            "folder-android-open",
            "folder-android",
            "folder-api-open",
            "folder-api",
            "folder-app-open",
            "folder-app",
            "folder-ci-open",
            "folder-ci",
            "folder-client-open",
            "folder-client",
            "folder-components-open",
            "folder-components",
            "folder-config-open",
            "folder-config",
            "folder-core-open",
            "folder-core",
            "folder-coverage-open",
            "folder-coverage",
            "folder-css-open",
            "folder-css",
            "folder-database-open",
            "folder-database",
            "folder-dist-open",
            "folder-dist",
            "folder-docker-open",
            "folder-docker",
            "folder-docs-open",
            "folder-docs",
            "folder-environment-open",
            "folder-environment",
            "folder-examples-open",
            "folder-examples",
            "folder-gh-workflows-open",
            "folder-gh-workflows",
            "folder-github-open",
            "folder-github",
            "folder-git-open",
            "folder-git",
            "folder-helm-open",
            "folder-helm",
            "folder-hook-open",
            "folder-hook",
            "folder-i18n-open",
            "folder-i18n",
            "folder-images-open",
            "folder-images",
            "folder-ios-open",
            "folder-ios",
            "folder-javascript-open",
            "folder-javascript",
            "folder-kubernetes-open",
            "folder-kubernetes",
            "folder-layout-open",
            "folder-layout",
            "folder-lib-open",
            "folder-lib",
            "folder-log-open",
            "folder-log",
            "folder-migrations-open",
            "folder-migrations",
            "folder-mock-open",
            "folder-mock",
            "folder-node-open",
            "folder-node",
            "folder-open",
            "folder-packages-open",
            "folder-packages",
            "folder-plugin-open",
            "folder-plugin",
            "folder-public-open",
            "folder-public",
            "folder-python-open",
            "folder-python",
            "folder-resource-open",
            "folder-resource",
            "folder-routes-open",
            "folder-routes",
            "folder-rust-open",
            "folder-rust",
            "folder-sass-open",
            "folder-sass",
            "folder-scripts-open",
            "folder-scripts",
            "folder-server-open",
            "folder-server",
            "folder-src-open",
            "folder-src",
            "folder",
            "folder-target-open",
            "folder-target",
            "folder-temp-open",
            "folder-temp",
            "folder-terraform-open",
            "folder-terraform",
            "folder-test-open",
            "folder-test",
            "folder-tools-open",
            "folder-tools",
            "folder-typescript-open",
            "folder-typescript",
            "folder-utils-open",
            "folder-utils",
            "folder-views-open",
            "folder-views",
            "folder-vscode-open",
            "folder-vscode",
            "font",
            "gemfile",
            "github-actions-workflow",
            "gitlab",
            "git",
            "gleam",
            "go",
            "graphql",
            "haskell",
            "helm",
            "hpp",
            "h",
            "html",
            "http",
            "image",
            "jar",
            "javascript",
            "java",
            "jest",
            "json",
            "julia",
            "just",
            "key",
            "kotlin",
            "kubernetes",
            "less",
            "license",
            "lock",
            "log",
            "lua",
            "makefile",
            "markdown",
            "next",
            "nginx",
            "nim",
            "nix",
            "nodejs",
            "npm",
            "nuxt",
            "ocaml",
            "odin",
            "pdf",
            "perl",
            "php",
            "pnpm",
            "powershell",
            "prettier",
            "prisma",
            "proto",
            "python",
            "react",
            "react_ts",
            "readme",
            "rollup",
            "r",
            "ruby",
            "rust",
            "sass",
            "scala",
            "settings",
            "shader",
            "svelte",
            "svg",
            "swift",
            "table",
            "terraform",
            "tex",
            "todo",
            "toml",
            "tsconfig",
            "tune",
            "typescript",
            "video",
            "vitest",
            "vue",
            "webassembly",
            "webpack",
            "word",
            "xml",
            "yaml",
            "yarn",
            "zig",
            "zip",
        );
    };
}
pub(crate) use material_names;

macro_rules! material_bytes {
    ($($name:literal),+ $(,)?) => {
        /// A Material icon's SVG by name, served at `icons/material/<name>.svg`.
        pub fn material_svg(name: &str) -> Option<&'static [u8]> {
            match name {
                $($name => Some(include_bytes!(concat!("../assets/icons/material/", $name, ".svg")).as_slice()),)+
                _ => None,
            }
        }
    };
}
material_names!(material_bytes);

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let Some(name) = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg")) else { return Ok(None) };
        if let Some(icon) = name.strip_prefix("solar/") {
            return Ok(solar_icon(icon).map(|svg| Cow::Owned(svg.into_bytes())));
        }
        if let Some(icon) = name.strip_prefix("material/") {
            return Ok(material_svg(icon).map(Cow::Borrowed));
        }
        Ok(icons!(
            name,
            "add-circle",
            "alt-arrow-down",
            "alt-arrow-left",
            "alt-arrow-right",
            "alt-arrow-up",
            "antigravity-mark",
            "arrow-down",
            "arrow-left",
            "arrow-right",
            "arrow-up",
            "bell",
            "bot",
            "bot-filled",
            "check",
            "checklist",
            "claude-mark",
            "close",
            "close-circle",
            "copilot-mark",
            "clock-circle",
            "cursor-mark",
            "devin-mark",
            "factory-mark",
            "drag-handle",
            "expand-arrows",
            "folder",
            "git-branch",
            "git-pull-request",
            "grok-mark",
            "hermes-mark",
            "kilo-mark",
            "kimi-mark",
            "mastra-mark",
            "info-circle",
            "keyboard",
            "logout-2",
            "minus",
            "monitor",
            "moon",
            "more-horizontal",
            "openai-mark",
            "opencode-mark",
            "palette-search",
            "pi-mark",
            "qoder-mark",
            "qwen-mark",
            "plus",
            "prompt",
            "remote-server",
            "settings",
            "sidebar-minimalistic-left",
            "sidebar-minimalistic-right",
            "spinner",
            "split-columns",
            "status-blocked",
            "status-done",
            "status-question",
            "status-dot",
            "status-idle",
            "status-unknown",
            "status-working",
            "split-rows",
            "sun",
            "terminal",
            "trash",
            "tuning",
            "widget",
            "window-maximize",
            "window-restore",
        ))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        Ok(Vec::new())
    }
}

/// The Solar icon set's Linear style (iconify's `solar` collection, `-linear` icons), by name: each icon's SVG body
/// on a 24 px canvas. The sessions' icons; licence in assets/NOTICE.
pub fn solar_names() -> impl Iterator<Item = &'static str> {
    solar().keys().map(String::as_str)
}

/// The Solar Linear icon `name` as a whole SVG, served at `icons/solar/<name>.svg`.
pub fn solar_icon(name: &str) -> Option<String> {
    solar().get(name).map(|body| format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">{body}</svg>"#))
}

// the bodies hold escaped quotes, so they are owned rather than borrowed from the JSON
fn solar() -> &'static std::collections::BTreeMap<String, String> {
    static SOLAR: std::sync::OnceLock<std::collections::BTreeMap<String, String>> = std::sync::OnceLock::new();
    SOLAR.get_or_init(|| serde_json::from_str(include_str!("../assets/solar-linear.json")).expect("assets/solar-linear.json"))
}
