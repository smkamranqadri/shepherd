//! A workspace's file tabs: Shepherd's own, drawn after the workspace's herdr tabs and never told to herdr. One of
//! them may be the one shown in place of the panes (`active`); `None` is the terminals, of the herdr tab Shepherd
//! shows. Opening follows VS Code's preview tabs: a single click opens a preview (its label in italics) that the
//! next single click replaces while it has no unsaved changes; a double-click, Enter, or a double-click on the tab
//! pins it. Pure: the Shell keeps the views and asks these what to show.
use super::status::Group;
use std::path::{Path, PathBuf};

/// The most file tabs a workspace keeps: opening one more closes the leftmost one that is neither shown nor dirty.
pub const MAX_TABS: usize = 40;

/// A file tab's id, unique for the run (never reused, so a late answer for a closed tab finds nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

/// What a file tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    File,
    /// The diff of the file's change in a Changes group (its id is `diff:{group}:{path}`): never kept for the next
    /// run, as its change may be gone by then.
    Diff(Group),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTab {
    pub id: TabId,
    pub path: PathBuf,
    pub kind: TabKind,
    /// A preview: replaced by the next file opened as one, unless it was pinned (or has unsaved changes) first.
    pub preview: bool,
}

impl FileTab {
    /// The tab's label: the file's name, and a diff's group after it.
    pub fn label(&self) -> String {
        let name = self.path.file_name().map_or_else(|| self.path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
        match self.kind {
            TabKind::File => name,
            TabKind::Diff(group) => format!("{name} ({})", diff_label(group)),
        }
    }
}

/// What a diff tab's label says of its group.
fn diff_label(group: Group) -> &'static str {
    match group {
        Group::Conflicts => "merge",
        Group::Staged => "staged",
        Group::Changes => "changes",
        Group::Untracked => "new",
    }
}

/// One workspace's file tabs, in the strip's order, and the one shown (`None`: the terminals).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceTabs {
    pub files: Vec<FileTab>,
    pub active: Option<TabId>,
}

/// A slot in a workspace's tab strip: one of its herdr tabs (by herdr's id), then its file tabs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StripEntry {
    Herdr(String),
    File(TabId),
}

impl WorkspaceTabs {
    pub fn get(&self, id: TabId) -> Option<&FileTab> {
        self.files.iter().find(|t| t.id == id)
    }

    /// The tab shown in place of the panes, if one is.
    pub fn shown(&self) -> Option<&FileTab> {
        self.get(self.active?)
    }

    /// Shows `path` and returns its tab: the tab it already has (pinned now unless opened as a preview), else the
    /// preview it replaces (a preview opened while one without unsaved changes is up), else a new tab right of the
    /// shown one (or last). `next_id` hands out ids.
    pub fn open(&mut self, path: PathBuf, kind: TabKind, preview: bool, is_dirty: impl Fn(TabId) -> bool, next_id: &mut u64) -> TabId {
        if let Some(tab) = self.files.iter_mut().find(|t| t.path == path && t.kind == kind) {
            tab.preview &= preview;
            self.active = Some(tab.id);
            return tab.id;
        }
        if preview && let Some(tab) = self.files.iter_mut().find(|t| t.preview && !is_dirty(t.id)) {
            (tab.path, tab.kind) = (path, kind);
            self.active = Some(tab.id);
            return tab.id;
        }
        *next_id += 1;
        let id = TabId(*next_id);
        let at = self.active.and_then(|a| self.files.iter().position(|t| t.id == a)).map_or(self.files.len(), |i| i + 1);
        self.files.insert(at, FileTab { id, path, kind, preview });
        self.active = Some(id);
        id
    }

    /// Over `MAX_TABS`: closes the leftmost tabs that are neither `keep`, shown nor dirty until it fits, and
    /// returns them (their views go).
    pub fn trim(&mut self, keep: TabId, is_dirty: impl Fn(TabId) -> bool) -> Vec<TabId> {
        let mut gone = Vec::new();
        while self.files.len() > MAX_TABS {
            let Some(at) = self.files.iter().position(|t| t.id != keep && Some(t.id) != self.active && !is_dirty(t.id)) else { break };
            gone.push(self.files.remove(at).id);
        }
        gone
    }

    /// Closes `id`. Closing the shown tab shows its right neighbour, else its left one, else the terminals.
    /// Returns the tab shown after.
    pub fn close(&mut self, id: TabId) -> Option<TabId> {
        let Some(at) = self.files.iter().position(|t| t.id == id) else { return self.active };
        self.files.remove(at);
        if self.active == Some(id) {
            self.active = self.files.get(at).or_else(|| at.checked_sub(1).and_then(|i| self.files.get(i))).map(|t| t.id);
        }
        self.active
    }

    /// The workspace's root moved: its tabs of files under none of `roots` (the new root as written and resolved)
    /// close, but not one with unsaved changes. Returns the ones closed.
    pub fn close_outside(&mut self, roots: &[PathBuf], is_dirty: impl Fn(TabId) -> bool) -> Vec<TabId> {
        let gone: Vec<TabId> = self.files.iter().filter(|t| !roots.iter().any(|r| t.path.starts_with(r)) && !is_dirty(t.id)).map(|t| t.id).collect();
        for id in &gone {
            self.close(*id);
        }
        gone
    }

    /// Keeps `id` open past the next preview.
    pub fn pin(&mut self, id: TabId) {
        if let Some(tab) = self.files.iter_mut().find(|t| t.id == id) {
            tab.preview = false;
        }
    }

    /// Puts the tabs remembered from the last run (`files`, pinned, in order) before the ones opened since, unless
    /// already open, and shows the one that was shown then if nothing is shown now.
    pub fn restore(&mut self, files: &[PathBuf], active: Option<&Path>, next_id: &mut u64) {
        let mut restored = Vec::new();
        for path in files {
            if self.files.len() + restored.len() >= MAX_TABS {
                break;
            }
            if self.files.iter().chain(&restored).any(|t: &FileTab| &t.path == path && t.kind == TabKind::File) {
                continue;
            }
            *next_id += 1;
            restored.push(FileTab { id: TabId(*next_id), path: path.clone(), kind: TabKind::File, preview: false });
        }
        restored.append(&mut self.files);
        self.files = restored;
        if self.active.is_none() {
            self.active = active.and_then(|a| self.files.iter().find(|t| t.path == a && t.kind == TabKind::File)).map(|t| t.id);
        }
    }

    /// What is kept for the next run: the pinned file tabs, and the shown one if it is among them (no previews).
    pub fn remembered(&self) -> (Vec<PathBuf>, Option<PathBuf>) {
        let pinned = |t: &&FileTab| !t.preview && t.kind == TabKind::File;
        let files = self.files.iter().filter(pinned).map(|t| t.path.clone()).collect();
        (files, self.shown().filter(pinned).map(|t| t.path.clone()))
    }
}

/// A workspace's tab strip: its herdr tabs, then its file tabs.
pub fn strip(herdr: &[String], files: &[FileTab]) -> Vec<StripEntry> {
    herdr.iter().cloned().map(StripEntry::Herdr).chain(files.iter().map(|t| StripEntry::File(t.id))).collect()
}

/// The entry after (or before) `at`, round the strip's end; `at` itself when it is not in the strip but the strip
/// has entries, its first.
pub fn step(strip: &[StripEntry], at: &StripEntry, forward: bool) -> Option<StripEntry> {
    let n = strip.len();
    let Some(i) = strip.iter().position(|e| e == at) else { return strip.first().cloned() };
    Some(strip[if forward { (i + 1) % n } else { (i + n - 1) % n }].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn paths(t: &WorkspaceTabs) -> Vec<(&str, bool)> {
        t.files.iter().map(|f| (f.path.to_str().unwrap(), f.preview)).collect()
    }

    #[test]
    fn a_preview_is_replaced_while_clean_and_pinned_by_a_second_open() {
        let (mut t, mut n) = (WorkspaceTabs::default(), 0);
        let a = t.open(p("a"), TabKind::File, true, |_| false, &mut n);
        let b = t.open(p("b"), TabKind::File, true, |_| false, &mut n);
        assert_eq!(a, b, "the preview's slot is reused");
        assert_eq!(paths(&t), [("b", true)]);
        // dirty: the next preview opens beside it
        let c = t.open(p("c"), TabKind::File, true, |id| id == b, &mut n);
        assert_ne!(c, b);
        assert_eq!(paths(&t), [("b", true), ("c", true)]);
        // opened again for good (a double-click, Enter): pinned, not a new tab
        assert_eq!(t.open(p("c"), TabKind::File, false, |_| false, &mut n), c);
        assert_eq!(paths(&t), [("b", true), ("c", false)]);
        // a preview of an open pinned file leaves it pinned
        t.open(p("c"), TabKind::File, true, |_| false, &mut n);
        assert_eq!(paths(&t), [("b", true), ("c", false)]);
        t.pin(b);
        let d = t.open(p("d"), TabKind::File, true, |_| false, &mut n);
        assert_eq!(paths(&t), [("b", false), ("c", false), ("d", true)]);
        assert_eq!(t.active, Some(d));
    }

    #[test]
    fn new_tabs_open_right_of_the_shown_one() {
        let (mut t, mut n) = (WorkspaceTabs::default(), 0);
        let a = t.open(p("a"), TabKind::File, false, |_| false, &mut n);
        t.open(p("b"), TabKind::File, false, |_| false, &mut n);
        t.active = Some(a);
        t.open(p("c"), TabKind::File, false, |_| false, &mut n);
        assert_eq!(paths(&t), [("a", false), ("c", false), ("b", false)]);
        t.active = None;
        t.open(p("d"), TabKind::File, false, |_| false, &mut n);
        assert_eq!(paths(&t).last(), Some(&("d", false)));
    }

    #[test]
    fn closing_shows_the_right_neighbour_then_the_left_then_the_terminals() {
        let (mut t, mut n) = (WorkspaceTabs::default(), 0);
        let a = t.open(p("a"), TabKind::File, false, |_| false, &mut n);
        let b = t.open(p("b"), TabKind::File, false, |_| false, &mut n);
        let c = t.open(p("c"), TabKind::File, false, |_| false, &mut n);
        t.active = Some(b);
        assert_eq!(t.close(b), Some(c));
        assert_eq!(t.close(c), Some(a), "no right neighbour: the left one");
        assert_eq!(t.close(a), None, "the last: back to the terminals");
        // closing a tab not shown leaves the shown one
        let d = t.open(p("d"), TabKind::File, false, |_| false, &mut n);
        let e = t.open(p("e"), TabKind::File, false, |_| false, &mut n);
        assert_eq!(t.close(d), Some(e));
        assert_eq!(t.close(TabId(999)), Some(e));
    }

    #[test]
    fn a_full_workspace_closes_its_leftmost_clean_tab() {
        let (mut t, mut n) = (WorkspaceTabs::default(), 0);
        let ids: Vec<TabId> = (0..MAX_TABS).map(|i| t.open(p(&i.to_string()), TabKind::File, false, |_| false, &mut n)).collect();
        assert!(t.trim(ids[0], |_| false).is_empty());
        let new = t.open(p("new"), TabKind::File, false, |_| false, &mut n);
        assert_eq!(t.trim(new, |id| id == ids[0]), [ids[1]], "the dirty first tab stays");
        assert_eq!(t.files.len(), MAX_TABS);
    }

    #[test]
    fn a_new_root_closes_the_clean_tabs_outside_it() {
        let (mut t, mut n) = (WorkspaceTabs::default(), 0);
        let a = t.open(p("/old/a"), TabKind::File, false, |_| false, &mut n);
        let b = t.open(p("/new/b"), TabKind::File, false, |_| false, &mut n);
        let c = t.open(p("/old/c"), TabKind::File, false, |_| false, &mut n);
        let d = t.open(p("/real/d"), TabKind::File, false, |_| false, &mut n);
        t.active = Some(a);
        assert_eq!(t.close_outside(&[p("/new"), p("/real")], |id| id == c), [a]);
        assert_eq!(t.files.iter().map(|f| f.id).collect::<Vec<_>>(), [b, c, d], "dirty c stays");
        assert_eq!(t.active, Some(b), "the shown one's right neighbour shows");
    }

    #[test]
    fn the_strip_walks_across_both_kinds_and_wraps() {
        let files = [FileTab { id: TabId(7), path: p("a"), kind: TabKind::File, preview: false }];
        let s = strip(&["t1".into(), "t2".into()], &files);
        assert_eq!(s, [StripEntry::Herdr("t1".into()), StripEntry::Herdr("t2".into()), StripEntry::File(TabId(7))]);
        assert_eq!(step(&s, &StripEntry::Herdr("t2".into()), true), Some(StripEntry::File(TabId(7))));
        assert_eq!(step(&s, &StripEntry::File(TabId(7)), true), Some(StripEntry::Herdr("t1".into())));
        assert_eq!(step(&s, &StripEntry::Herdr("t1".into()), false), Some(StripEntry::File(TabId(7))));
        assert_eq!(step(&s, &StripEntry::Herdr("gone".into()), true), Some(StripEntry::Herdr("t1".into())));
        assert_eq!(step(&[], &StripEntry::Herdr("t1".into()), true), None);
    }

    #[test]
    fn restored_tabs_go_first_and_skip_ones_open_already() {
        let (mut t, mut n) = (WorkspaceTabs::default(), 0);
        let opened = t.open(p("b"), TabKind::File, true, |_| false, &mut n);
        t.restore(&[p("a"), p("b"), p("c")], Some(Path::new("c")), &mut n);
        assert_eq!(paths(&t), [("a", false), ("c", false), ("b", true)]);
        assert_eq!(t.active, Some(opened), "what was opened meanwhile stays shown");
        let mut fresh = WorkspaceTabs::default();
        fresh.restore(&[p("a"), p("c")], Some(Path::new("c")), &mut n);
        assert_eq!(fresh.shown().map(|t| t.path.clone()), Some(p("c")));
        let ids: Vec<_> = fresh.files.iter().chain(&t.files).map(|f| f.id).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "ids stay unique across workspaces");
    }

    #[test]
    fn only_pinned_tabs_are_remembered() {
        let (mut t, mut n) = (WorkspaceTabs::default(), 0);
        t.open(p("a"), TabKind::File, false, |_| false, &mut n);
        t.open(p("b"), TabKind::File, true, |_| false, &mut n);
        assert_eq!(t.remembered(), (vec![p("a")], None), "a shown preview is not");
        t.open(p("a"), TabKind::File, true, |_| false, &mut n);
        assert_eq!(t.remembered(), (vec![p("a")], Some(p("a"))));
    }

    #[test]
    fn diff_tabs_are_never_kept_and_sit_beside_their_file() {
        let (mut t, mut n) = (WorkspaceTabs::default(), 0);
        let file = t.open(p("a"), TabKind::File, false, |_| false, &mut n);
        let staged = t.open(p("a"), TabKind::Diff(Group::Staged), false, |_| false, &mut n);
        let changes = t.open(p("a"), TabKind::Diff(Group::Changes), false, |_| false, &mut n);
        assert_eq!(t.files.len(), 3, "a file and each of its groups' diffs are tabs of their own");
        assert_ne!(staged, changes);
        assert_eq!(t.remembered(), (vec![p("a")], None), "a pinned diff, shown, is not kept");
        t.active = Some(file);
        assert_eq!(t.remembered(), (vec![p("a")], Some(p("a"))));
        // a preview diff replaced by a preview of a file, in place
        let preview = t.open(p("b"), TabKind::Diff(Group::Untracked), true, |_| false, &mut n);
        assert_eq!(t.open(p("c"), TabKind::File, true, |_| false, &mut n), preview);
        assert_eq!(t.get(preview).map(|f| f.kind), Some(TabKind::File));
        assert_eq!(t.get(changes).map(FileTab::label).as_deref(), Some("a (changes)"));
        // restoring brings back files only
        let mut fresh = WorkspaceTabs::default();
        fresh.restore(&[p("a")], None, &mut n);
        assert!(fresh.files.iter().all(|f| f.kind == TabKind::File));
    }
}
