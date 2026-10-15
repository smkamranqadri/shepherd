//! Each workspace's root: the folder the right panel's Files, Changes and Search work in. herdr has no such thing (a
//! workspace is tabs of panes, each anywhere), so Shepherd guesses one once, from the workspace's worktree or its
//! first tab's pane, as the repo that folder is in, and keeps it in the settings per place and workspace until it is
//! chosen by hand. A guess never lands on `/`, the home folder or anything above it: listing or searching all of
//! those is what a root is there to avoid.
use super::runner::{self, Host, Limits};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `git rev-parse --show-toplevel` prints one path: a hung network mount is what the deadline is for.
const REV_PARSE: Limits = Limits { timeout: Duration::from_secs(5), max_bytes: 64 * 1024 };

/// The settings' `workspace_roots` key of a workspace: herdr's workspace ids are its session's own (`w1`, `w2`…), so
/// two places' workspaces share them; the place's key (a socket path, `machine:<id>`) has no newline to clash with.
pub fn root_key(place: &str, workspace_id: &str) -> String {
    format!("{place}\n{workspace_id}")
}

/// `path` is `/`, the home folder or one of its ancestors (`/home`, `/Users`): too much to list, watch or search.
/// Compared by component (a trailing slash is no matter), both as written and with symlinks resolved, so a link to
/// home is caught, and so is macOS's `/home`, an automount whose real path (`/System/Volumes/Data/home`) is no
/// ancestor of anything.
pub fn is_too_broad(path: &Path, home: &Path) -> bool {
    let written = |p: &Path| p.components().collect::<PathBuf>();
    let resolved = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| written(p));
    // no parent: `/`, or an empty path
    let broad = |path: PathBuf, home: PathBuf| path.parent().is_none() || home.starts_with(&path);
    broad(written(path), written(home)) || broad(resolved(path), resolved(home))
}

/// Drops `place`'s roots whose workspace is not in `live` (it was closed); other places' roots are left alone, as
/// their workspaces are not known here. True when something went, so the settings are saved only then.
pub fn prune<V>(roots: &mut BTreeMap<String, V>, place: &str, live: &[&str]) -> bool {
    let before = roots.len();
    roots.retain(|key, _| match key.split_once('\n') {
        Some((p, workspace)) if p == place => live.contains(&workspace),
        _ => true,
    });
    roots.len() != before
}

/// The first of `dirs` (best first) that makes a root: the top of the git repo it is in, else the folder itself
/// with its symlinks resolved; one that is too broad (`is_too_broad`) or gone is passed over.
pub async fn guess(host: &Host, dirs: &[String], home: &Path) -> Option<PathBuf> {
    // ponytail: a machine's folders are guessed there once the runner speaks SSH; `canonicalize` below is local
    if let Host::Ssh { .. } = host {
        return None;
    }
    for dir in dirs {
        let top = match runner::run(host, runner::git_cmd(Path::new(dir), &["rev-parse", "--show-toplevel"]), REV_PARSE).await {
            Ok(out) if out.code == Some(0) && !out.truncated => {
                let top = String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string();
                (!top.is_empty()).then(|| PathBuf::from(top))
            }
            // not a repo, git missing, the folder gone
            _ => None,
        };
        if let Some(top) = top.filter(|t| !is_too_broad(t, home)) {
            return Some(top);
        }
        // no repo, or one too broad to be a project's (dotfiles kept in a repo at home): the folder itself, if it is
        // still there
        match tokio::fs::canonicalize(dir).await {
            Ok(path) if !is_too_broad(&path, home) => return Some(path),
            _ => {}
        }
    }
    None
}

/// A root as the settings keep it: its folder, and what the workspace it was set for looked like then. herdr's
/// workspace ids are restored with its session but are not unique over time (a closed workspace's id can come
/// back on a new one while Shepherd was not looking), so a root whose fingerprint no longer fits is not used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Stored")]
pub struct WorkspaceRoot {
    pub path: String,
    /// `fingerprint` of the workspace when the root was set; empty fits any (a root saved before there was one).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
}

/// What `workspace_roots` held: a bare path at first, a `WorkspaceRoot` since.
#[derive(Deserialize)]
#[serde(untagged)]
enum Stored {
    Path(String),
    Full {
        path: String,
        #[serde(default)]
        fingerprint: String,
    },
}

impl From<Stored> for WorkspaceRoot {
    fn from(stored: Stored) -> Self {
        match stored {
            Stored::Path(path) => WorkspaceRoot { path, fingerprint: String::new() },
            Stored::Full { path, fingerprint } => WorkspaceRoot { path, fingerprint },
        }
    }
}

impl WorkspaceRoot {
    /// It was set for the workspace that now has the fingerprint `fingerprint`.
    pub fn fits(&self, fingerprint: &str) -> bool {
        self.fingerprint.is_empty() || self.fingerprint == fingerprint
    }
}

/// What tells one workspace from another that later got its id. herdr's API has no creation time or identity for a
/// workspace, only its label, number, tabs and (for one herdr made for a git worktree) its checkout path: the
/// checkout path where there is one, which stays put, else the label (a rename sends the root back to a guess).
pub fn fingerprint(label: &str, checkout_path: Option<&str>) -> String {
    match checkout_path.filter(|p| !p.is_empty()) {
        Some(path) => format!("worktree:{path}"),
        None => format!("label:{label}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_key_joins_place_and_workspace() {
        assert_eq!(root_key("/tmp/herdr.sock", "w1"), "/tmp/herdr.sock\nw1");
        assert_eq!(root_key("machine:7", "w1"), root_key("machine:7", "w1"));
        assert_ne!(root_key("machine:7", "w1"), root_key("machine:8", "w1"));
    }

    #[test]
    fn root_and_home_and_its_ancestors_are_too_broad() {
        let home = Path::new("/home/someone-not-here");
        assert!(is_too_broad(Path::new("/"), home));
        assert!(is_too_broad(Path::new("/home"), home));
        assert!(is_too_broad(Path::new("/home/"), home));
        assert!(is_too_broad(Path::new("/home/someone-not-here"), home));
        assert!(is_too_broad(Path::new("/home/someone-not-here/"), home));
        assert!(is_too_broad(Path::new(""), home));
        assert!(!is_too_broad(Path::new("/home/someone-not-here/src/x"), home));
        // a sibling whose name starts like home's is not its ancestor
        assert!(!is_too_broad(Path::new("/home/someone-not"), home));
        assert!(!is_too_broad(Path::new("/opt/app"), home));
    }

    #[test]
    fn a_link_to_home_is_too_broad() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("me");
        std::fs::create_dir_all(home.join("src")).unwrap();
        std::os::unix::fs::symlink(&home, dir.path().join("link")).unwrap();
        assert!(is_too_broad(&dir.path().join("link"), &home));
        assert!(is_too_broad(&dir.path().join("link/"), &home));
        assert!(!is_too_broad(&dir.path().join("link/src"), &home));
    }

    #[test]
    fn prune_drops_only_this_places_closed_workspaces() {
        let mut roots: BTreeMap<String, String> =
            [("a\nw1", "/x"), ("a\nw2", "/y"), ("b\nw1", "/z"), ("odd", "/q")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        assert!(!prune(&mut roots, "a", &["w1", "w2", "w3"]));
        assert_eq!(roots.len(), 4);
        assert!(prune(&mut roots, "a", &["w1"]));
        assert_eq!(roots.keys().map(String::as_str).collect::<Vec<_>>(), ["a\nw1", "b\nw1", "odd"]);
        assert!(!prune(&mut roots, "a", &["w1"]));
        assert!(!prune(&mut roots, "c", &[]));
    }

    #[tokio::test]
    async fn guess_takes_the_repo_top_else_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        let init = std::process::Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap();
        assert!(init.success());
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let home = Path::new("/home/someone-not-here");
        let s = |p: &Path| p.to_string_lossy().into_owned();

        let top = guess(&Host::Local, &[s(&repo.join("src/deep"))], home).await;
        assert_eq!(top, Some(std::fs::canonicalize(&repo).unwrap()));
        let folder = guess(&Host::Local, &[s(&plain)], home).await;
        assert_eq!(folder, Some(std::fs::canonicalize(&plain).unwrap()));
        // a gone folder and a too-broad one are passed over for the next
        let next = guess(&Host::Local, &[s(&dir.path().join("gone")), "/".into(), s(&plain)], home).await;
        assert_eq!(next, Some(std::fs::canonicalize(&plain).unwrap()));
        // home itself is never a root
        assert_eq!(guess(&Host::Local, &[s(&plain)], &plain).await, None);
        assert_eq!(guess(&Host::Ssh { target: "box".into() }, &[s(&plain)], home).await, None);
    }

    #[tokio::test]
    async fn a_dotfiles_repo_at_home_falls_back_to_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("me");
        let project = home.join("notes/today");
        std::fs::create_dir_all(&project).unwrap();
        let init = std::process::Command::new("git").args(["init", "-q"]).current_dir(&home).status().unwrap();
        assert!(init.success());
        let s = |p: &Path| p.to_string_lossy().into_owned();
        // the repo's top is home: the pane's own folder instead
        assert_eq!(guess(&Host::Local, &[s(&project)], &home).await, Some(std::fs::canonicalize(&project).unwrap()));
        // home itself is no root even then
        assert_eq!(guess(&Host::Local, &[s(&home)], &home).await, None);
    }

    #[test]
    fn a_root_reads_from_a_bare_path_or_with_its_fingerprint() {
        let old: BTreeMap<String, WorkspaceRoot> = serde_json::from_str(r#"{"s\nw1": "/src/app"}"#).unwrap();
        assert_eq!(old["s\nw1"], WorkspaceRoot { path: "/src/app".into(), fingerprint: String::new() });
        assert!(old["s\nw1"].fits("label:anything"));
        let root = WorkspaceRoot { path: "/src/app".into(), fingerprint: fingerprint("app", None) };
        let text = serde_json::to_string(&root).unwrap();
        assert_eq!(text, r#"{"path":"/src/app","fingerprint":"label:app"}"#);
        let back: WorkspaceRoot = serde_json::from_str(&text).unwrap();
        assert_eq!(back, root);
        assert!(back.fits(&fingerprint("app", Some(""))));
        assert!(!back.fits(&fingerprint("other", None)));
        assert_eq!(fingerprint("app", Some("/src/app-wt")), "worktree:/src/app-wt");
    }
}
