//! Quick Open (`QuickOpen`): the command panel over the shown workspace's files (`files::quick_open`), its file tabs
//! first. Each root's listing is kept for the app's windows and shown at once; one older than `FRESH` is listed
//! again behind it.
use super::*;
use crate::files::quick_open as qo;
use crate::files::runner::Host;
use crate::files::tabs::TabKind;
use crate::ui::command_panel::{FilesState, Kind, Panel};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A listing younger than this is used as it is.
const FRESH: Duration = Duration::from_secs(10);
/// The roots whose listings are kept (200,000 paths are some 10 MB); the oldest goes first.
const KEPT_ROOTS: usize = 4;
/// The files opened from Quick Open this run, per root.
const RECENT: usize = 20;
/// How long the root's real path may take (a hung mount).
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);

struct Listed {
    paths: Arc<Vec<Box<str>>>,
    capped: bool,
    at: Instant,
    /// The root with its symlinks resolved, as file tabs keep their paths.
    real_root: PathBuf,
}

/// Every root's listing and its recent files, shared by the windows.
#[derive(Default)]
struct Listings {
    roots: HashMap<PathBuf, Listed>,
    recent: HashMap<PathBuf, Vec<String>>,
}

impl gpui::Global for Listings {}

/// Stops a listing on the runtime when dropped.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `path` from `root` (or its real path) with `/` between folders; `None` outside it.
fn relative(path: &Path, root: &Path, real_root: Option<&Path>) -> Option<String> {
    let rel = real_root.and_then(|r| path.strip_prefix(r).ok()).or_else(|| path.strip_prefix(root).ok())?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

impl Shell {
    /// `QuickOpen`: opens Quick Open over the shown workspace's files, or closes it when it is open.
    pub(super) fn quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = |shell: &Self, cx: &App| shell.command_panel.as_ref().filter(|p| p.is_open() && p.value.0.read(cx).kind == Kind::Files).map(|p| p.value.0.clone());
        if let Some(panel) = shown(self, cx) {
            panel.update(cx, |p, _| p.stop_listing());
        }
        self.toggle_command_panel(Kind::Files, window, cx);
        let Some(panel) = shown(self, cx) else { return };
        let context = self.panel_context(window, cx);
        if context.remote {
            return panel.update(cx, |p, cx| p.files_unavailable(FilesState::Remote, cx));
        }
        let Some(root) = context.root.map(PathBuf::from) else {
            return panel.update(cx, |p, cx| p.files_unavailable(FilesState::NoFolder, cx));
        };
        let listings = cx.default_global::<Listings>();
        let listed = listings.roots.get(&root);
        let real_root = listed.map(|l| l.real_root.clone());
        let stale = listed.is_none_or(|l| l.at.elapsed() >= FRESH);
        let kept = listed.map(|l| (l.paths.clone(), l.capped));
        let recent = listings.recent.get(&root).cloned().unwrap_or_default();
        // the shown tab, then the strip's order
        let open: Vec<String> = self
            .shown_tabs_key()
            .and_then(|key| self.file_tabs.get(&key))
            .map(|tabs| {
                let files = tabs.files.iter().filter(|t| t.kind == TabKind::File);
                let (active, rest): (Vec<_>, Vec<_>) = files.partition(|t| Some(t.id) == tabs.active);
                active.into_iter().chain(rest).filter_map(|t| relative(&t.path, &root, real_root.as_deref())).collect()
            })
            .unwrap_or_default();
        panel.update(cx, |p, cx| p.files_root(root.clone(), open, recent, kept, cx));
        if stale {
            self.list_files(root, &panel, cx);
        }
    }

    /// Lists `root`'s files on the runtime; the answer is kept for every window and handed to `panel`. The listing
    /// stops when the panel closes first.
    fn list_files(&mut self, root: PathBuf, panel: &Entity<Panel>, cx: &mut Context<Self>) {
        let asked = root.clone();
        let job = self.rt.spawn(async move {
            let rg = crate::ui::statusbar::tool("rg");
            let listed = qo::list(&Host::Local, &asked, &rg).await;
            let resolve = asked.clone();
            let real = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::task::spawn_blocking(move || std::fs::canonicalize(&resolve))).await;
            let real = match real {
                Ok(Ok(Ok(real))) => real,
                _ => asked,
            };
            (listed, real)
        });
        panel.update(cx, |p, _| p.hold(Box::new(AbortOnDrop(job.abort_handle()))));
        let panel = panel.downgrade();
        cx.spawn(async move |this, cx| {
            // stopped: the panel closed
            let Ok((listed, real_root)) = job.await else { return };
            let _ = this.update(cx, |_, cx| {
                let shown = listed.map(|l| {
                    let paths = Arc::new(l.paths);
                    let listings = cx.default_global::<Listings>();
                    if !listings.roots.contains_key(&root)
                        && listings.roots.len() >= KEPT_ROOTS
                        && let Some(oldest) = listings.roots.iter().min_by_key(|(_, l)| l.at).map(|(r, _)| r.clone())
                    {
                        listings.roots.remove(&oldest);
                    }
                    listings.roots.insert(root, Listed { paths: paths.clone(), capped: l.capped, at: Instant::now(), real_root });
                    (paths, l.capped)
                });
                let _ = panel.update(cx, |p, cx| p.files_listed(shown, cx));
            });
        })
        .detach();
    }

    /// A file picked in Quick Open: opened pinned (the keyboard goes to it), at `line`, and remembered for its root.
    pub(super) fn open_quick_file(&mut self, path: PathBuf, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = self.panel_context(window, cx).root.map(PathBuf::from) {
            let real_root = cx.try_global::<Listings>().and_then(|l| l.roots.get(&root)).map(|l| l.real_root.clone());
            if let Some(rel) = relative(&path, &root, real_root.as_deref()) {
                let recent = cx.default_global::<Listings>().recent.entry(root).or_default();
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
