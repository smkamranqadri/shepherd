//! Quick Open (`QuickOpen`): the command panel over the shown workspace's files (`files::quick_open`), its file tabs
//! first. The root is resolved first (its real path keys everything, as it prefixes the file tabs' paths). Each
//! root's listing is kept for the app's windows and shown at once; one older than `FRESH` is listed again behind it,
//! and a listing finishes into the cache even when the panel that asked has closed.
use super::*;
use crate::files::quick_open as qo;
use crate::files::runner::Host;
use crate::files::tabs::TabKind;
use crate::ui::command_panel::{FilesState, Kind, Panel};
use gpui::WeakEntity;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A listing younger than this is used as it is.
const FRESH: Duration = Duration::from_secs(10);
/// The roots whose listings are kept (200,000 paths are some 10 MB); the oldest goes first.
const KEPT_ROOTS: usize = 4;
/// The files opened from Quick Open this run, per root.
const RECENT: usize = 20;
/// How long the root's real path, and a look at whether its recent files are still there, may take (a hung mount).
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
/// The roots as written whose real paths are remembered.
const KEPT_SPELLINGS: usize = 64;

struct Listed {
    paths: Arc<Vec<Box<str>>>,
    capped: bool,
    at: Instant,
}

/// Every root's listing and its recent files, by its real path, shared by the windows.
#[derive(Default)]
struct Listings {
    roots: HashMap<PathBuf, Listed>,
    recent: HashMap<PathBuf, Vec<String>>,
    /// A root as written, to its real path.
    real: HashMap<PathBuf, PathBuf>,
    /// The roots being listed, and the panels waiting on each.
    listing: HashMap<PathBuf, Vec<WeakEntity<Panel>>>,
}

impl gpui::Global for Listings {}

/// `path` from `root` (or its real path) with `/` between folders; `None` outside it.
fn relative(path: &Path, root: &Path, real_root: Option<&Path>) -> Option<String> {
    let rel = real_root.and_then(|r| path.strip_prefix(r).ok()).or_else(|| path.strip_prefix(root).ok())?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

impl Shell {
    /// `QuickOpen`: opens Quick Open over the shown workspace's files, or closes it when it is open.
    pub(super) fn quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_command_panel(Kind::Files, window, cx);
        let Some(panel) = self.command_panel.as_ref().filter(|p| p.is_open() && p.value.0.read(cx).kind == Kind::Files).map(|p| p.value.0.clone()) else { return };
        let context = self.panel_context(window, cx);
        if context.remote {
            return panel.update(cx, |p, cx| p.files_unavailable(FilesState::Remote, cx));
        }
        let Some(root) = context.root.map(PathBuf::from) else {
            return panel.update(cx, |p, cx| p.files_unavailable(FilesState::NoFolder, cx));
        };
        panel.update(cx, |p, cx| p.files_root(root.clone(), cx));
        // the shown tab, then the strip's order
        let tabs: Vec<PathBuf> = self
            .shown_tabs_key()
            .and_then(|key| self.file_tabs.get(&key))
            .map(|tabs| {
                let files = tabs.files.iter().filter(|t| t.kind == TabKind::File);
                let (active, rest): (Vec<_>, Vec<_>) = files.partition(|t| Some(t.id) == tabs.active);
                active.into_iter().chain(rest).map(|t| t.path.clone()).collect()
            })
            .unwrap_or_default();
        // the recent files of the root as last resolved; looked at again below, as they may have gone
        let listings = cx.default_global::<Listings>();
        let recent = listings.real.get(&root).and_then(|real| listings.recent.get(real)).cloned().unwrap_or_default();
        let asked = root.clone();
        let job = self.rt.spawn(async move {
            let fallback = asked.clone();
            let look = tokio::task::spawn_blocking(move || {
                let real = qo::real_root(&asked);
                let recent = qo::existing(&real, recent);
                (real, recent)
            });
            match tokio::time::timeout(RESOLVE_TIMEOUT, look).await {
                Ok(Ok(found)) => found,
                _ => (fallback, Vec::new()),
            }
        });
        let panel = panel.downgrade();
        cx.spawn(async move |this, cx| {
            let Ok((real, recent)) = job.await else { return };
            let _ = this.update(cx, |shell, cx| {
                let open = tabs.iter().filter_map(|t| relative(t, &root, Some(&real))).collect();
                let listings = cx.default_global::<Listings>();
                if listings.real.len() >= KEPT_SPELLINGS {
                    listings.real.clear();
                }
                listings.real.insert(root, real.clone());
                let listed = listings.roots.get(&real);
                let stale = listed.is_none_or(|l| l.at.elapsed() >= FRESH);
                let kept = listed.map(|l| (l.paths.clone(), l.capped));
                let Some(panel) = panel.upgrade() else { return };
                panel.update(cx, |p, cx| p.files_context(open, recent, kept, stale, cx));
                if stale {
                    shell.list_files(real, &panel, cx);
                }
            });
        })
        .detach();
    }

    /// Lists `root` (a real path) on the runtime, unless it is being listed already; the answer is kept for every
    /// window and handed to each panel still waiting on it.
    fn list_files(&mut self, root: PathBuf, panel: &Entity<Panel>, cx: &mut Context<Self>) {
        let listings = cx.default_global::<Listings>();
        let waiting = listings.listing.entry(root.clone()).or_default();
        waiting.push(panel.downgrade());
        if waiting.len() > 1 {
            return;
        }
        let asked = root.clone();
        let job = self.rt.spawn(async move { qo::list(&Host::Local, &asked, &crate::ui::statusbar::tool("rg")).await });
        cx.spawn(async move |this, cx| {
            let listed = job.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update(cx, |_, cx| {
                let listings = cx.default_global::<Listings>();
                let waiting = listings.listing.remove(&root).unwrap_or_default();
                let shown = listed.map(|l| {
                    let paths = Arc::new(l.paths);
                    if !listings.roots.contains_key(&root)
                        && listings.roots.len() >= KEPT_ROOTS
                        && let Some(oldest) = listings.roots.iter().min_by_key(|(_, l)| l.at).map(|(r, _)| r.clone())
                    {
                        listings.roots.remove(&oldest);
                    }
                    listings.roots.insert(root, Listed { paths: paths.clone(), capped: l.capped, at: Instant::now() });
                    (paths, l.capped)
                });
                // a panel closed meanwhile only drops the answer
                for panel in waiting.iter().filter_map(WeakEntity::upgrade) {
                    panel.update(cx, |p, cx| p.files_listed(shown.clone(), cx));
                }
            });
        })
        .detach();
    }

    /// A file picked in Quick Open: opened pinned (the keyboard goes to it), at `line`, and remembered for its root.
    pub(super) fn open_quick_file(&mut self, path: PathBuf, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = self.panel_context(window, cx).root.map(PathBuf::from) {
            let listings = cx.default_global::<Listings>();
            let real = listings.real.get(&root).cloned().unwrap_or_else(|| root.clone());
            if let Some(rel) = relative(&path, &root, Some(&real)) {
                let recent = listings.recent.entry(real).or_default();
                recent.retain(|r| *r != rel);
                recent.insert(0, rel);
                recent.truncate(RECENT);
            }
        }
        self.open_file(path, line, false, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_are_named_from_the_root_or_its_real_path() {
        let root = Path::new("/w/link");
        let real = Path::new("/w/real");
        assert_eq!(relative(Path::new("/w/real/src/a.rs"), root, Some(real)).as_deref(), Some("src/a.rs"));
        assert_eq!(relative(Path::new("/w/link/b.rs"), root, Some(real)).as_deref(), Some("b.rs"));
        assert_eq!(relative(Path::new("/elsewhere/c.rs"), root, Some(real)), None);
        assert_eq!(relative(root, root, None), None);
    }
}
