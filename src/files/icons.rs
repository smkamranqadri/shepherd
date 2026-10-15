//! Which Solar Linear icon (`assets/solar-linear.json`, served at `icons/solar/<name>.svg`) a tree entry gets: a
//! folder's, a link's, or one by the file's kind, told by its name. A handful of kinds, not one per language: the
//! icons are monochrome and 14 px, there to tell code from text from images at a glance.
use super::tree::EntryKind;

/// The icon for an entry named `name`; `open` for a folder shown open.
pub fn icon_for(name: &str, kind: EntryKind, open: bool) -> &'static str {
    match kind {
        EntryKind::Symlink { dir: true, .. } => return "icons/solar/folder-symlink.svg",
        EntryKind::Dir if open => return "icons/solar/folder-open.svg",
        EntryKind::Dir => return "icons/solar/folder.svg",
        EntryKind::Dangling => return "icons/solar/file-corrupted.svg",
        EntryKind::Symlink { dir: false, .. } | EntryKind::File => {}
    }
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "dockerfile" | "makefile" | "justfile" | "procfile" | "rakefile" | "gemfile" => return "icons/solar/file-terminal.svg",
        "license" | "licence" | "copying" | "notice" | "authors" => return "icons/solar/file-text.svg",
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".editorconfig" | ".dockerignore" | ".npmrc" | ".prettierrc" | ".env" => {
            return "icons/solar/file-cog.svg";
        }
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb" | "go.sum" | "poetry.lock" | "flake.lock" | "gemfile.lock" => {
            return "icons/solar/file-lock.svg";
        }
        _ => {}
    }
    if lower.starts_with(".env.") {
        return "icons/solar/file-cog.svg";
    }
    let ext = match lower.rsplit_once('.') {
        // `.bashrc` is a dotfile, not a file of kind "bashrc"
        Some((stem, ext)) if !stem.is_empty() => ext,
        _ => return "icons/solar/file.svg",
    };
    match ext {
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "m" | "mm" | "swift" | "go" | "py" | "rb" | "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "java"
        | "kt" | "kts" | "scala" | "cs" | "fs" | "php" | "lua" | "zig" | "nim" | "ex" | "exs" | "erl" | "hs" | "ml" | "mli" | "clj" | "dart" | "vue" | "svelte" | "astro"
        | "html" | "htm" | "css" | "scss" | "sass" | "less" | "sql" | "graphql" | "gql" | "proto" | "wgsl" | "glsl" | "metal" | "r" | "jl" | "elm" | "v" | "sol" => {
            "icons/solar/file-code.svg"
        }
        "json" | "jsonc" | "json5" => "icons/solar/file-braces.svg",
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "xml" | "plist" | "properties" | "nix" | "lock" => "icons/solar/file-cog.svg",
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "mk" => "icons/solar/file-terminal.svg",
        "md" | "mdx" | "markdown" | "txt" | "rst" | "adoc" | "org" | "tex" | "log" | "rtf" => "icons/solar/file-text.svg",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg" | "ico" | "icns" | "heic" | "avif" | "psd" => "icons/solar/file-image.svg",
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar" | "deb" | "rpm" | "dmg" | "pkg" | "whl" | "crate" => "icons/solar/file-zip.svg",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => "icons/solar/file-audio.svg",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "icons/solar/file-video-camera.svg",
        "csv" | "tsv" | "xls" | "xlsx" | "ods" | "numbers" => "icons/solar/file-spreadsheet.svg",
        "pem" | "key" | "crt" | "cer" | "p12" | "gpg" | "asc" => "icons/solar/file-lock.svg",
        _ => "icons/solar/file.svg",
    }
}


// ---- Colour file-type and folder icons (Material Icon Theme, MIT; assets/icons/material/SOURCES.md) ----

macro_rules! all_paths {
    ($($name:literal),+ $(,)?) => {
        /// Every icon path `material_icon` can return (`icons/material/<name>.svg`), for tests and preloading.
        pub const ALL: &[&str] = &[$(concat!("icons/material/", $name, ".svg")),+];
    };
}
crate::assets::material_names!(all_paths);

const DOCUMENT: &str = "icons/material/document.svg";

/// The coloured icon for a tree entry named `name` (a folder if `is_dir`, drawn open if `open`), as an asset path for
/// `img()`. Order: a folder's exact name; a file's exact name; `readme*`; `.env*`; `dockerfile*`; the extension; a
/// plain document.
pub fn material_icon(name: &str, is_dir: bool, open: bool) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if is_dir {
        return find(folder_icon(&lower).unwrap_or("folder"), open);
    }
    find(file_icon(&lower), false)
}

/// The path in `ALL` for the icon `base` (its `-open` variant if `open`); the plain document if there is none.
fn find(base: &str, open: bool) -> &'static str {
    ALL.iter()
        .copied()
        .find(|p| {
            let Some(n) = p.strip_prefix("icons/material/").and_then(|p| p.strip_suffix(".svg")) else { return false };
            if open { n.strip_suffix("-open") == Some(base) } else { n == base }
        })
        .unwrap_or(DOCUMENT)
}

fn folder_icon(n: &str) -> Option<&'static str> {
    Some(match n {
        "src" | "source" | "sources" => "folder-src",
        "test" | "tests" | "__tests__" | "spec" | "specs" => "folder-test",
        "doc" | "docs" => "folder-docs",
        "script" | "scripts" | "bin" => "folder-scripts",
        "lib" | "libs" | "vendor" => "folder-lib",
        "app" | "apps" => "folder-app",
        "components" => "folder-components",
        "public" | "static" => "folder-public",
        "config" | "configs" | ".config" | "conf" => "folder-config",
        "images" | "img" | "assets" | "icons" => "folder-images",
        "node_modules" => "folder-node",
        "dist" | "build" | ".build" | "out" => "folder-dist",
        ".github" => "folder-github",
        "workflows" => "folder-gh-workflows",
        ".vscode" => "folder-vscode",
        "resources" | "resource" => "folder-resource",
        ".git" => "folder-git",
        "ci" | ".ci" => "folder-ci",
        "docker" | ".docker" => "folder-docker",
        "env" | "environment" | "environments" => "folder-environment",
        "tmp" | "temp" | ".tmp" => "folder-temp",
        "db" | "database" | "databases" => "folder-database",
        "api" => "folder-api",
        "hooks" | "hook" => "folder-hook",
        "utils" | "util" | "helpers" => "folder-utils",
        "routes" | "router" => "folder-routes",
        "mock" | "mocks" | "__mocks__" => "folder-mock",
        "plugin" | "plugins" => "folder-plugin",
        "log" | "logs" => "folder-log",
        "server" | "backend" => "folder-server",
        "client" | "frontend" => "folder-client",
        "core" => "folder-core",
        "layout" | "layouts" => "folder-layout",
        "view" | "views" | "pages" => "folder-views",
        "css" | "styles" | "style" => "folder-css",
        "sass" | "scss" => "folder-sass",
        "i18n" | "locales" | "locale" => "folder-i18n",
        "migration" | "migrations" => "folder-migrations",
        "packages" | "crates" => "folder-packages",
        "target" => "folder-target",
        "android" => "folder-android",
        "ios" => "folder-ios",
        "kubernetes" | "k8s" => "folder-kubernetes",
        "helm" | "charts" => "folder-helm",
        "terraform" => "folder-terraform",
        "typescript" => "folder-typescript",
        "javascript" => "folder-javascript",
        "rust" => "folder-rust",
        "python" => "folder-python",
        "coverage" => "folder-coverage",
        "example" | "examples" | "samples" => "folder-examples",
        "tools" | "tooling" => "folder-tools",
        _ => return None,
    })
}

/// A file's icon (a name in `ALL`) by its lower-cased name: exact names, then `readme*`, `.env*`, `dockerfile*`, then
/// the extension.
fn file_icon(n: &str) -> &'static str {
    match n {
        "package.json" | "package-lock.json" | ".nvmrc" | ".node-version" => return "nodejs",
        ".npmrc" | "npm-shrinkwrap.json" => return "npm",
        "pnpm-workspace.yaml" => return "pnpm",
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" | ".mailmap" => return "git",
        ".gitlab-ci.yml" => return "gitlab",
        "license" | "licence" | "copying" | "unlicense" | "license.md" | "license.txt" | "licence.md" | "licence.txt" => return "license",
        "makefile" | "gnumakefile" => return "makefile",
        "justfile" => return "just",
        "cmakelists.txt" => return "cmake",
        "package.resolved" => return "json",
        "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml" | ".dockerignore" | "containerfile" => return "docker",
        "yarn.lock" | "pnpm-lock.yaml" | "cargo.lock" | "podfile.lock" | "gemfile.lock" | "poetry.lock" | "flake.lock" | "composer.lock" | "bun.lockb" | "bun.lock"
        | "deno.lock" | "go.sum" => return "lock",
        ".editorconfig" => return "editorconfig",
        "changelog" | "changelog.md" | "changes.md" | "history.md" => return "changelog",
        "contributing" | "contributing.md" | "authors" | "authors.md" | "maintainers" => return "contributing",
        "todo" | "todo.md" | "todo.txt" => return "todo",
        "gemfile" => return "gemfile",
        "rakefile" => return "ruby",
        "go.mod" => return "go",
        "angular.json" => return "angular",
        "deno.json" | "deno.jsonc" => return "deno",
        "nginx.conf" => return "nginx",
        "chart.yaml" => return "helm",
        "tsconfig.json" | "jsconfig.json" => return "tsconfig",
        "build.zig" | "build.zig.zon" => return "zig",
        ".babelrc" => return "babel",
        _ => {}
    }
    if n.starts_with("readme") {
        return "readme";
    }
    if n.starts_with(".env") {
        return "tune";
    }
    if n.starts_with("dockerfile") || n.ends_with(".dockerfile") {
        return "docker";
    }
    if n.starts_with("tsconfig") || n.starts_with("jsconfig") {
        return "tsconfig";
    }
    for (prefix, icon) in [
        (".prettierrc", "prettier"),
        ("prettier.config", "prettier"),
        (".prettierignore", "prettier"),
        (".eslintrc", "eslint"),
        ("eslint.config", "eslint"),
        (".eslintignore", "eslint"),
        ("babel.config", "babel"),
        ("jest.config", "jest"),
        ("vitest.config", "vitest"),
        ("rollup.config", "rollup"),
        ("webpack.config", "webpack"),
        ("next.config", "next"),
        ("nuxt.config", "nuxt"),
    ] {
        if n.starts_with(prefix) {
            return icon;
        }
    }
    let Some((_, ext)) = n.rsplit_once('.') else { return "document" };
    match ext {
        "swift" => "swift",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "jsx" => "react",
        "tsx" => "react_ts",
        "json" | "jsonc" | "json5" => "json",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "xml" | "plist" => "xml",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "sass",
        "less" => "less",
        "md" | "markdown" | "mdx" => "markdown",
        "py" | "pyi" => "python",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "jar" => "jar",
        "kt" | "kts" => "kotlin",
        "scala" | "sc" => "scala",
        "c" | "m" => "c",
        "cpp" | "cc" | "cxx" | "mm" => "cpp",
        "h" => "h",
        "hpp" | "hh" | "hxx" => "hpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "pl" | "pm" => "perl",
        "r" => "r",
        "jl" => "julia",
        "ml" | "mli" => "ocaml",
        "clj" | "cljs" | "cljc" | "edn" => "clojure",
        "erl" | "hrl" => "erlang",
        "ex" | "exs" | "heex" | "eex" => "elixir",
        "hs" | "lhs" => "haskell",
        "nix" => "nix",
        "zig" => "zig",
        "nim" => "nim",
        "cr" => "crystal",
        "gleam" => "gleam",
        "odin" => "odin",
        "lua" => "lua",
        "dart" => "dart",
        "svelte" => "svelte",
        "astro" => "astro",
        "vue" => "vue",
        "tf" | "tfvars" | "hcl" => "terraform",
        "proto" => "proto",
        "prisma" => "prisma",
        "graphql" | "gql" => "graphql",
        "http" | "rest" => "http",
        "wasm" | "wat" => "webassembly",
        "tex" | "latex" => "tex",
        "glsl" | "wgsl" | "vert" | "frag" | "metal" | "hlsl" => "shader",
        "sh" | "bash" | "zsh" | "fish" | "bat" | "cmd" => "console",
        "ps1" | "psm1" => "powershell",
        "mk" => "makefile",
        "cmake" => "cmake",
        "lock" => "lock",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "avif" | "ico" | "icns" | "psd" => "image",
        "svg" => "svg",
        "pdf" => "pdf",
        "zip" | "gz" | "tgz" | "tar" | "bz2" | "xz" | "zst" | "7z" | "rar" => "zip",
        "sql" | "sqlite" | "sqlite3" | "db" => "database",
        "csv" | "tsv" | "xls" | "xlsx" | "ods" => "table",
        "doc" | "docx" | "odt" | "rtf" => "word",
        "ttf" | "otf" | "woff" | "woff2" => "font",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => "audio",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "video",
        "exe" | "dll" | "so" | "dylib" => "exe",
        "diff" | "patch" => "diff",
        "log" => "log",
        "ini" | "conf" | "cfg" | "properties" => "settings",
        "env" => "tune",
        "pem" | "crt" | "cer" => "certificate",
        "key" | "pub" | "gpg" | "asc" => "key",
        _ => "document",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_get_their_kind_by_name_and_folders_and_links_their_own() {
        let file = EntryKind::File;
        assert_eq!(icon_for("main.rs", file, false), "icons/solar/file-code.svg");
        assert_eq!(icon_for("App.TSX", file, false), "icons/solar/file-code.svg");
        assert_eq!(icon_for("README.md", file, false), "icons/solar/file-text.svg");
        assert_eq!(icon_for("logo.png", file, false), "icons/solar/file-image.svg");
        assert_eq!(icon_for("package.json", file, false), "icons/solar/file-braces.svg");
        assert_eq!(icon_for("Cargo.lock", file, false), "icons/solar/file-lock.svg");
        assert_eq!(icon_for("Makefile", file, false), "icons/solar/file-terminal.svg");
        assert_eq!(icon_for(".env.local", file, false), "icons/solar/file-cog.svg");
        assert_eq!(icon_for(".bashrc", file, false), "icons/solar/file.svg");
        assert_eq!(icon_for("noext", file, false), "icons/solar/file.svg");
        assert_eq!(icon_for("src", EntryKind::Dir, false), "icons/solar/folder.svg");
        assert_eq!(icon_for("src", EntryKind::Dir, true), "icons/solar/folder-open.svg");
        assert_eq!(icon_for("vendor", EntryKind::Symlink { dir: true, outside: true }, false), "icons/solar/folder-symlink.svg");
        assert_eq!(icon_for("a.rs", EntryKind::Symlink { dir: false, outside: false }, false), "icons/solar/file-code.svg");
    }

    /// Every icon named here is one the asset source serves: a typo would draw nothing.
    #[test]
    fn every_icon_is_in_the_solar_set() {
        let names = [
            "a.rs", "a.json", "a.toml", "a.sh", "a.md", "a.png", "a.zip", "a.mp3", "a.mp4", "a.csv", "a.pem", "a.bin", "Dockerfile", "LICENSE", ".gitignore", "Cargo.lock",
        ];
        let kinds = [EntryKind::File, EntryKind::Dir, EntryKind::Symlink { dir: true, outside: false }, EntryKind::Dangling];
        for kind in kinds {
            for open in [false, true] {
                for name in names {
                    let icon = icon_for(name, kind, open);
                    let solar = icon.strip_prefix("icons/solar/").and_then(|i| i.strip_suffix(".svg")).unwrap();
                    assert!(crate::assets::solar_icon(solar).is_some(), "{icon}");
                }
            }
        }
    }

    #[test]
    fn material_icons_follow_the_lookup_order() {
        assert_eq!(material_icon("notes.md", false, false), "icons/material/markdown.svg");
        assert_eq!(material_icon("Cargo.lock", false, false), "icons/material/lock.svg");
        assert_eq!(material_icon("src", true, true), "icons/material/folder-src-open.svg");
        assert_eq!(material_icon("src", true, false), "icons/material/folder-src.svg");
        assert_eq!(material_icon("whatever", true, false), "icons/material/folder.svg");
        assert_eq!(material_icon("whatever", true, true), "icons/material/folder-open.svg");
        assert_eq!(material_icon("mystery.zzz", false, false), "icons/material/document.svg");
        assert_eq!(material_icon("noext", false, false), "icons/material/document.svg");
        // an exact name beats the extension, a prefix beats it too
        assert_eq!(material_icon("package.json", false, false), "icons/material/nodejs.svg");
        assert_eq!(material_icon("tsconfig.json", false, false), "icons/material/tsconfig.svg");
        assert_eq!(material_icon("README.rst", false, false), "icons/material/readme.svg");
        assert_eq!(material_icon(".env.local", false, false), "icons/material/tune.svg");
        assert_eq!(material_icon("Dockerfile.dev", false, false), "icons/material/docker.svg");
        assert_eq!(material_icon("web.dockerfile", false, false), "icons/material/docker.svg");
        assert_eq!(material_icon("App.TSX", false, false), "icons/material/react_ts.svg");
        assert_eq!(material_icon("flake.nix", false, false), "icons/material/nix.svg");
        // a folder name only counts for a folder
        assert_eq!(material_icon("src", false, false), "icons/material/document.svg");
        assert_eq!(material_icon("Node_Modules", true, false), "icons/material/folder-node.svg");
    }

    #[test]
    fn every_material_icon_loads_as_an_svg_and_every_file_is_listed() {
        use gpui::AssetSource;
        for path in ALL {
            let bytes = crate::assets::Assets.load(path).unwrap().unwrap_or_else(|| panic!("{path} is not served"));
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.starts_with("<svg") || text.starts_with("<?xml"), "{path}");
            assert!(!text.contains("<script"), "{path}");
        }
        // everything the mapping can name is in ALL (a typo would draw the document)
        for name in ["a.rs", "a.ts", "a.py", "Makefile", ".gitignore", "x.nix", "x.zig", "x.svelte", "x.tf", "x.proto", ".env", "a.csv"] {
            assert_ne!(material_icon(name, false, false), "icons/material/document.svg", "{name}");
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/material");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|n| n.ends_with(".svg")).collect();
        on_disk.sort();
        let mut listed: Vec<String> = ALL.iter().map(|p| p.strip_prefix("icons/material/").unwrap().to_string()).collect();
        listed.sort();
        assert_eq!(on_disk, listed);
    }

    /// Every name the tables return is an icon that exists, folders with their open variant.
    #[test]
    fn every_mapped_name_exists() {
        let exists = |n: &str| ALL.contains(&format!("icons/material/{n}.svg").as_str());
        for f in ["src", "tests", "docs", "scripts", "lib", "app", "components", "public", "config", "images", "node_modules", "dist", ".github", "workflows", ".vscode", "resources", ".git", "ci", "docker", "env", "tmp", "db", "api", "hooks", "utils", "routes", "mocks", "plugins", "logs", "server", "client", "core", "layouts", "views", "css", "sass", "i18n", "migrations", "packages", "target", "android", "ios", "k8s", "helm", "terraform", "typescript", "javascript", "rust", "python", "coverage", "examples", "tools"] {
            let n = folder_icon(f).unwrap();
            assert!(exists(n) && exists(&format!("{n}-open")), "{f} -> {n}");
        }
    }
}
