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
}
