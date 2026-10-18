//! A file tab's view: the file, editable. Text within the editing limits (`MAX_EDIT` bytes, `MAX_EDIT_LINES` lines,
//! no line over `MAX_EDIT_LINE` characters) opens in gpui-base's code editor, highlighted as the viewer would
//! (`editor_highlight`); a markdown file shows its preview first and its source in the editor. Anything else (an
//! image, a PDF, binary, text past the limits) is the read-only viewer's (`ui::viewer`), drawn inside this view, with
//! a note when it is text that could not be edited here.
//!
//! The text is compared with the last version on disk on every change (its length first, then its chunks), so the
//! tab's dot goes when an undo comes back to it. A save (`files::save`, off the main thread) makes the version it
//! wrote the new baseline, not the buffer as it is when the save ends: typing during a save leaves the tab dirty.
//! A save that fails says why in a banner under the header.
//!
//! The file is watched on disk (its folder, through `notify`, for events naming it) and looked at when its tab
//! shows and when the window comes to the front. A change with no unsaved changes here is taken in place: the
//! buffer gets the disk's text as one edit (undo steps back to what it was), the cursor and scroll kept. With
//! unsaved changes a banner asks: Reload (the disk's text, theirs dropped, though undo still has it), Keep mine
//! (the next save writes over that version of the disk's, and only that one) or Compare (the disk's text against
//! the buffer, read-only, in the editor's place). A file deleted or moved away keeps its buffer, says so, and Save
//! anyway makes it again. Ctrl/Cmd+S over a newer version on disk asks Overwrite / Compare / Cancel; Save All and
//! closing a tab leave the banner instead.
//!
//! The gutter marks the lines changed against git's index, and those staged (HEAD to index) as outlines
//! (`files::line_changes`): its blobs are fetched when the file opens and when git's state may have moved
//! (`git_changed`), and the buffer is diffed against them a moment after typing stops.
use crate::bindings::SaveFile;
use crate::files::content::{self, FileKind};
use crate::files::highlight;
use crate::files::line_changes::{self, Hunk, LineChange, Mark};
use crate::files::runner::{self, Host, Limits};
use crate::files::save::{self, Baseline, LineEnding, SaveError, SaveMode, Seen};
use crate::shell::widgets::{tip, titlebar_button};
use crate::theme::Theme;
use crate::ui::diff_view::DiffView;
use crate::ui::editor_highlight::{SyntectHighlighter, editor_style};
use crate::ui::markdown::{Link, Links};
use crate::ui::review::Block;
use crate::ui::viewer::{FileViewer, ViewerEvent};
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, PromptLevel, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, div, point,
    prelude::*, px,
};
use gpui_base::input::{Editor, EditorState, InputEvent, InputHighlighter, Position, Rope, RopeExt, TabSize};
use futures::FutureExt;
use futures::future::{BoxFuture, LocalBoxFuture, Shared};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use syntect::parsing::SyntaxReference;
use tokio::runtime::Handle;

/// The largest file that opens editable; up to `content::MAX_TEXT` it opens read-only in the viewer.
pub const MAX_EDIT: u64 = 4 << 20;
pub const MAX_EDIT_LINES: usize = 200_000;
/// A line longer than this (minified code, data) keeps the file in the viewer unless "Edit anyway" is chosen.
pub const MAX_EDIT_LINE: usize = 20_000;
/// How long a read may take before the editor gives up on it (a hung mount).
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
/// How long after the last keystroke the gutter's bars are worked out again.
const GUTTER_AFTER: Duration = Duration::from_millis(300);
/// How long the gutter's diff may take before it settles for fewer, larger stretches.
const GUTTER_BUDGET: Duration = Duration::from_millis(250);
/// The padding left of the line numbers: the gutter's bars are drawn in it.
const GUTTER_PAD: f32 = 8.;
/// How long after the first event of a burst on disk (a checkout, a formatter's rename) the file is looked at.
const DISK_AFTER: Duration = Duration::from_millis(300);

pub enum EditorEvent {
    /// "Open in editor", at a line when there is one: the Shell shows the terminals and opens the file there.
    OpenInEditor { path: PathBuf, line: Option<u32> },
    /// A markdown link to another file in the root: the Shell opens it in a tab.
    OpenFile { path: PathBuf, line: Option<u32>, fragment: Option<String> },
    /// The unsaved-changes dot came or went, or a save started or ended: the tab bar draws again, and a preview is
    /// pinned on its first edit.
    Dirty(bool),
    /// The deleted file's banner's Close: the Shell closes the tab (asking first, with unsaved changes).
    Close,
}

/// Where the cursor and the scroll were, kept for the tab while its view is gone (dropped to save memory) and put
/// back when it is made again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub line: u32,
    pub column: u32,
    pub scroll_y: f32,
}

/// Whether the buffer (`len` bytes, as `chunks`) is exactly `saved`: its length first, then chunk by chunk.
pub fn same_text<'a>(len: usize, chunks: impl IntoIterator<Item = &'a str>, saved: &[u8]) -> bool {
    if len != saved.len() {
        return false;
    }

    let mut at = 0;
    for chunk in chunks {
        let end = at + chunk.len();
        if saved.get(at..end) != Some(chunk.as_bytes()) {
            return false;
        }
        at = end;
    }
    at == saved.len()
}

/// Unsaved changes: the text last written (or as opened) against the buffer, and the save under way.
#[derive(Debug, Default)]
pub struct Tracker {
    /// Its bytes: the baseline's own when they are the text as edited (no BOM, \n line ends), so they are kept once.
    saved: Arc<Vec<u8>>,
    dirty: bool,
    /// The save under way, by its number: only its answer counts.
    saving: Option<u64>,
    saves: u64,
}

impl Tracker {
    pub fn new(saved: Arc<Vec<u8>>) -> Tracker {
        Tracker { saved, ..Tracker::default() }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn saving(&self) -> bool {
        self.saving.is_some()
    }

    /// The buffer changed: true when that turned the dot on or off.
    pub fn changed<'a>(&mut self, len: usize, chunks: impl IntoIterator<Item = &'a str>) -> bool {
        let dirty = !same_text(len, chunks, &self.saved);
        std::mem::replace(&mut self.dirty, dirty) != dirty
    }

    /// The buffer is about to take the disk's text in place (`saved`): that is the saved text now, which the buffer's
    /// next change is compared with.
    pub fn reloaded(&mut self, saved: Arc<Vec<u8>>) {
        self.saved = saved;
    }

    /// A save starts; its number, or `None` while another runs.
    pub fn start_save(&mut self) -> Option<u64> {
        if self.saving.is_some() {
            return None;
        }
        self.saves += 1;
        self.saving = Some(self.saves);
        Some(self.saves)
    }

    /// Save `ticket` ended, having written `written` (`None`: it failed). What it wrote is the new saved text, and the
    /// buffer as it is now is compared with that: typing since the save started keeps the dot. False for an answer
    /// to a save that is not the one under way.
    pub fn finish_save<'a>(&mut self, ticket: u64, written: Option<Arc<Vec<u8>>>, len: usize, chunks: impl IntoIterator<Item = &'a str>) -> bool {
        if self.saving != Some(ticket) {
            return false;
        }
        self.saving = None;
        if let Some(written) = written {
            self.saved = written;
            self.dirty = !same_text(len, chunks, &self.saved);
        }
        true
    }
}

/// What the background read hands back.
enum Loaded {
    Edit(Box<Editable>),
    /// The viewer's: not text, or text past the limits (`note` says which, when it is text).
    View(Option<Note>),
}

/// A file that opens in the editor, read and worked out off the main thread.
struct Editable {
    /// The text with its line ends made `\n` (a save writes the file's own ending back).
    text: String,
    baseline: Baseline,
    syntax: Option<&'static SyntaxReference>,
    /// Why it opens read-only, if it does.
    readonly: Option<&'static str>,
    blocks: Option<Vec<Block>>,
    real_root: Option<PathBuf>,
    hard_tabs: bool,
}

/// Why text opened in the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Note {
    LongLines,
    TooLarge,
}

#[cfg(unix)]
unsafe extern "C" {
    fn access(path: *const std::ffi::c_char, mode: std::ffi::c_int) -> std::ffi::c_int;
}

/// Whether this user may write `path` (the kernel's answer: mode bits, ACLs, a read-only mount).
#[cfg(unix)]
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: a NUL-terminated path, read by access(2) only
    unsafe { access(c.as_ptr(), 2) == 0 }
}

/// Off unix: the file's read-only flag.
#[cfg(not(unix))]
fn writable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| !m.permissions().readonly())
}

/// Reads `path` for editing (blocking: the runtime's blocking pool runs it). `force` opens text with long lines too.
fn read_for_edit(path: &Path, root: Option<&Path>, force: bool) -> Loaded {
    let Ok(meta) = std::fs::metadata(path) else { return Loaded::View(None) };
    if !meta.is_file() {
        return Loaded::View(None);
    }
    if meta.len() > MAX_EDIT {
        // text the viewer shows, too large to edit; anything else is the viewer's to say
        let head = std::fs::File::open(path).ok().and_then(|f| {
            let mut head = Vec::new();
            std::io::Read::read_to_end(&mut std::io::Read::take(f, 8192), &mut head).ok().map(|_| head)
        });
        let text = head.is_some_and(|h| matches!(content::kind_of(path, &h, meta.len()), FileKind::Text | FileKind::Markdown));
        return Loaded::View(text.then_some(Note::TooLarge));
    }
    let Ok(baseline) = Baseline::read(path) else { return Loaded::View(None) };
    let head = &baseline.bytes[..baseline.bytes.len().min(8192)];
    let kind = content::kind_of(path, head, baseline.bytes.len() as u64);
    if !matches!(kind, FileKind::Text | FileKind::Markdown) {
        return Loaded::View(None);
    }
    let Some(text) = content::decode(&baseline.bytes) else { return Loaded::View(None) };
    if text.len() > MAX_EDIT_LINES {
        return Loaded::View(Some(Note::TooLarge));
    }
    if !force && text.lines().any(|l| l.len() > MAX_EDIT_LINE && l.chars().count() > MAX_EDIT_LINE) {
        return Loaded::View(Some(Note::LongLines));
    }
    let readonly = if !writable(path) {
        Some("You can't write to this file")
    } else {
        let root = root.or(path.parent()).unwrap_or(Path::new("/"));
        match save::contained(root, path) {
            Err(SaveError::OutsideRoot) => Some("Outside the workspace's folder"),
            _ => None,
        }
    };
    let syntax = highlight::syntax_for(path, text.line(0));
    if let Some(syntax) = syntax {
        // its contexts load on first use: here, not on the main thread when the editor makes its highlighter
        let _ = highlight::start(syntax);
    }
    let blocks = (kind == FileKind::Markdown && text.text.len() <= crate::ui::viewer::MAX_PREVIEW).then(|| crate::ui::review::markdown(&text.text));
    let real_root = blocks.as_ref().and(root).and_then(|r| r.canonicalize().ok());
    let sample = || text.lines().take(2000);
    let hard_tabs = sample().filter(|l| l.starts_with('\t')).count() > sample().filter(|l| l.starts_with("  ")).count();
    Loaded::Edit(Box::new(Editable { text: lf(text.text), baseline, syntax, readonly, blocks, real_root, hard_tabs }))
}

/// `text` with its line ends made `\n`, as the editor holds it.
fn lf(text: String) -> String {
    if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text }
}

/// The saved text for the tracker: the baseline's own bytes when they are `text` (no BOM, `\n` ends), so they are kept
/// once.
fn saved_text(baseline: &Baseline, text: &str) -> Arc<Vec<u8>> {
    if baseline.bytes.as_slice() == text.as_bytes() { baseline.bytes.clone() } else { Arc::new(text.as_bytes().to_vec()) }
}

/// The buffer is about to take `file`'s text in place (a reload that keeps undo): the baseline and the tracker's saved
/// text become the file's first, so the change the buffer then hears compares clean against them.
fn rebase(tracker: &mut Tracker, baseline: &mut Baseline, file: &Editable) {
    *baseline = file.baseline.clone();
    tracker.reloaded(saved_text(baseline, &file.text));
}

/// What a look at the disk found, against the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disk {
    /// The baseline's bytes (touched, maybe, but not changed).
    Same,
    /// Another version, by its length and digest.
    Changed(Seen),
    /// Nothing there, or no file: deleted, or moved away.
    Gone,
}

/// What the editor does about a look at the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDisk {
    Nothing,
    /// The disk is as opened: a banner about it goes.
    Clear,
    /// No unsaved changes: the buffer takes the disk's text in place.
    Reload,
    /// Unsaved changes: the banner asks what to do.
    Banner,
    Deleted,
}

/// What to do about `disk`, with unsaved changes or not (`dirty`), when the user chose to keep their text over the
/// version `kept`: that version again asks nothing, any other does.
pub fn decide(dirty: bool, disk: Disk, kept: Option<Seen>) -> OnDisk {
    match disk {
        Disk::Same => OnDisk::Clear,
        Disk::Gone => OnDisk::Deleted,
        Disk::Changed(_) if !dirty => OnDisk::Reload,
        Disk::Changed(seen) if kept == Some(seen) => OnDisk::Nothing,
        Disk::Changed(_) => OnDisk::Banner,
    }
}

/// Looks at the file on disk against `b` (blocking); `None` when it can't tell (an error reading it).
fn look(path: &Path, b: &Baseline) -> Option<Disk> {
    let gone = |e: &std::io::Error| matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput);
    match save::changed_on_disk(path, b) {
        Ok(false) => Some(Disk::Same),
        // a folder or a FIFO in its place is no file either
        Ok(true) => match save::fingerprint(path) {
            Ok(seen) => Some(Disk::Changed(seen)),
            Err(e) if gone(&e) => Some(Disk::Gone),
            Err(_) => None,
        },
        Err(e) if gone(&e) => Some(Disk::Gone),
        Err(_) => None,
    }
}

/// `save::save`, but a save that finds the disk changed writes over it when the disk holds the version the user
/// chose to keep their text over (`kept`), and only then: checked again at the rename, as any save.
fn save_kept(root: &Path, path: &Path, text: &str, b: &Baseline, mode: SaveMode, kept: Option<Seen>) -> Result<Baseline, SaveError> {
    let saved = save::save(root, path, text, b, mode);
    let (Err(SaveError::Conflict), Some(kept), SaveMode::Safe | SaveMode::InPlace) = (&saved, kept, mode) else { return saved };
    match Baseline::read(path) {
        Ok(disk) if Seen::of(&disk.bytes) == kept => save::save(root, path, text, &b.over(&disk), mode),
        _ => saved,
    }
}

/// The disk's version of `path` as the editor would hold it, for a Compare: `None` when it is not text.
fn disk_text(path: &Path) -> Option<String> {
    let b = Baseline::read(path).ok()?;
    let text = content::decode(&b.bytes)?;
    Some(lf(text.text))
}

/// A save that failed, or the disk's news, as the banner under the header says it.
#[derive(Debug, Clone, PartialEq)]
enum Banner {
    Failed(String),
    /// The file changed on disk (to `disk`) while the buffer has unsaved changes: Compare, Keep mine or Reload.
    /// `save`: a save found it so and wrote nothing (Overwrite then stands for Keep mine).
    Changed { disk: Seen, save: bool },
    /// The file was deleted or moved away on disk: Save anyway makes it again; Close closes the tab.
    Deleted,
}

/// The open file in the editor.
struct Edit {
    state: Entity<EditorState>,
    baseline: Baseline,
    tracker: Tracker,
    readonly: Option<&'static str>,
    /// A markdown file's blocks, for its preview.
    blocks: Option<Vec<Block>>,
    real_root: Option<PathBuf>,
    _events: Subscription,
    /// The window coming to the front looks at the disk.
    _activation: Subscription,
}

enum Body {
    Loading,
    Edit(Box<Edit>),
    View { viewer: Entity<FileViewer>, note: Option<Note>, _events: Subscription },
}

/// Where to go once the file is in.
#[derive(Debug, Clone)]
enum Goto {
    Line(u32),
    Fragment(String),
    /// A search match: its line (1-based) and its bytes in the line.
    Select(u32, Range<usize>),
}

/// git's view of the file, for the gutter: the index's blob, and what is staged (HEAD to index, worked out once when
/// they came, so HEAD's blob is not kept), by their object ids.
#[derive(Default, Clone)]
struct Blobs {
    index: Option<(String, Arc<String>)>,
    head_id: Option<String>,
    staged: Arc<Vec<Hunk>>,
}

/// A save under way, as its callers await it: whether the file was written. Dropping it leaves the save running.
pub type Saving = Shared<LocalBoxFuture<'static, bool>>;

pub struct FileEditor {
    focus: FocusHandle,
    rt: Handle,
    path: PathBuf,
    root: Option<PathBuf>,
    /// Bumped by every open: a load, save or diff that started for the file before is dropped.
    generation: u64,
    body: Body,
    /// Markdown shows as its preview, not its source.
    preview: bool,
    preview_scroll: ScrollHandle,
    goto: Option<Goto>,
    restore: Option<Place>,
    banner: Option<Banner>,
    /// The user agreed to save this file in place (a hard-linked file, another owner's): every save does, without
    /// asking again, while the tab shows the file.
    in_place: bool,
    /// The save under way, and the one more asked for meanwhile (Ctrl/Cmd+S again), started when it ends.
    saving: Option<Saving>,
    queued: Option<SaveMode>,
    /// The bytes of the save under way reaching the disk, which quitting waits for (no gpui task to drive it then).
    writing: Option<Shared<BoxFuture<'static, ()>>>,
    /// The gutter's bars and what they are worked out from.
    gutter: Arc<Vec<LineChange>>,
    blobs: Blobs,
    /// Numbers the gutter's diffs: only the latest one's answer is drawn.
    gutter_seq: u64,
    gutter_wait: Option<Task<()>>,
    gutter_fetch: Option<Task<()>>,
    gutter_diff: Option<Task<()>>,
    /// git changed while the tab was out of sight: the blobs are fetched again when it shows.
    gutter_stale: bool,
    /// The watch on the file's folder (its watcher lives in the task), the look at the disk under way, and the read
    /// of a reload in place (apart, so a look never cancels a Reload asked for).
    disk_watch: Option<Task<()>>,
    disk_check: Option<Task<()>>,
    disk_reload: Option<Task<()>>,
    /// The version on disk the user chose to keep their text over ("Keep mine"): the next save writes over it.
    kept: Option<Seen>,
    /// The disk's text against the buffer, shown in the text's place.
    compare: Option<Entity<DiffView>>,
    /// The save under way was asked for by Ctrl/Cmd+S: a newer version on disk is asked about, not bannered.
    explicit: bool,
    _theme: Subscription,
}

impl EventEmitter<EditorEvent> for FileEditor {}

impl Focusable for FileEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if let (Body::Edit(_), Some(compare)) = (&self.body, &self.compare) {
            return compare.read(cx).focus_handle(cx);
        }
        match &self.body {
            Body::Edit(e) if !self.preview_shown() => e.state.read(cx).focus_handle(cx),
            Body::View { viewer, .. } => viewer.read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        }
    }
}

impl FileEditor {
    pub fn new(rt: Handle, cx: &mut Context<Self>) -> FileEditor {
        let theme = cx.observe_global::<Theme>(|e: &mut FileEditor, cx| {
            if let Body::Edit(edit) = &e.body {
                let style = editor_style(Theme::of(cx));
                edit.state.update(cx, |s, cx| {
                    s.set_editor_style(style);
                    cx.notify();
                });
            }
        });
        FileEditor {
            focus: cx.focus_handle(),
            rt,
            path: PathBuf::new(),
            root: None,
            generation: 0,
            body: Body::Loading,
            preview: true,
            preview_scroll: ScrollHandle::new(),
            goto: None,
            restore: None,
            banner: None,
            in_place: false,
            saving: None,
            queued: None,
            writing: None,
            gutter: Arc::default(),
            blobs: Blobs::default(),
            gutter_seq: 0,
            gutter_wait: None,
            gutter_fetch: None,
            gutter_diff: None,
            gutter_stale: false,
            disk_watch: None,
            disk_check: None,
            disk_reload: None,
            kept: None,
            compare: None,
            explicit: false,
            _theme: theme,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The editor's text state, while the file is open editable: what a find bar searches and selects in.
    pub(crate) fn editor_state(&self) -> Option<&Entity<EditorState>> {
        match &self.body {
            Body::Edit(e) => Some(&e.state),
            _ => None,
        }
    }

    /// Markdown's preview is on screen (in the editor, or the viewer's).
    pub fn shows_markdown(&self, cx: &App) -> bool {
        match &self.body {
            Body::Edit(e) => e.blocks.is_some() && self.preview,
            Body::View { viewer, .. } => viewer.read(cx).shows_markdown(),
            Body::Loading => false,
        }
    }

    fn preview_shown(&self) -> bool {
        matches!(&self.body, Body::Edit(e) if e.blocks.is_some() && self.preview)
    }

    pub fn is_dirty(&self) -> bool {
        matches!(&self.body, Body::Edit(e) if e.tracker.is_dirty())
    }

    /// A save is under way: quitting waits for it.
    pub fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    /// The save under way, to wait for.
    pub fn saving(&self) -> Option<Saving> {
        self.saving.clone()
    }

    /// Quitting without asking (the Dock's Quit, logging out): the write under way, and a save of unsaved changes
    /// started now off the main thread, as one future the app's quit waits on (for as long as it lets it). Nothing
    /// in the view is updated: it is going.
    // ponytail: E4's drafts keep what a quit this short cannot write
    pub fn flush(&self, cx: &App) -> Option<BoxFuture<'static, ()>> {
        let writing = self.writing.clone();
        let save = match &self.body {
            Body::Edit(e) if e.tracker.is_dirty() && e.readonly.is_none() && self.saving.is_none() => {
                let rope: Rope = e.state.read(cx).text().clone();
                let (root, path, baseline) = (self.save_root(), self.path.clone(), e.baseline.clone());
                let mode = if self.in_place { SaveMode::InPlace } else { SaveMode::Safe };
                let kept = self.kept;
                let job = self.rt.spawn_blocking(move || {
                    if let Err(err) = save_kept(&root, &path, &rope_text(&rope), &baseline, mode, kept) {
                        eprintln!("shepherd: quitting, couldn't save {}: {err}", path.display());
                    }
                });
                Some(job)
            }
            _ => None,
        };
        if writing.is_none() && save.is_none() {
            return None;
        }
        Some(
            async move {
                if let Some(writing) = writing {
                    writing.await;
                }
                if let Some(save) = save {
                    let _ = save.await;
                }
            }
            .boxed(),
        )
    }

    /// The cursor and the scroll, to put back if the view is made again.
    pub fn place(&self, cx: &App) -> Option<Place> {
        let state = self.editor_state()?.read(cx);
        let at = state.cursor_position();
        Some(Place { line: at.line, column: at.character, scroll_y: f32::from(state.scroll_offset().y) })
    }

    /// Shows `path` (at `line`, 1-based) in place of what was shown, or, when it is the file shown (with its edits),
    /// only goes to `line`. `restore` puts the cursor and scroll back as they were (a view made again).
    pub fn open(&mut self, path: PathBuf, line: Option<u32>, root: Option<PathBuf>, restore: Option<Place>, window: &mut Window, cx: &mut Context<Self>) {
        // the file shown, or still being read (another match in it, say): the read goes on, and `go_to_line` keeps
        // the line for when it is in
        if path == self.path {
            if let Some(line) = line {
                self.go_to_line(line, window, cx);
            }
            return;
        }
        self.load(path, line.map(Goto::Line), root, restore, false, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn load(&mut self, path: PathBuf, goto: Option<Goto>, root: Option<PathBuf>, restore: Option<Place>, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        // the view had the keyboard (a reload, Edit anyway): it keeps it while the file loads, and the new state takes
        // it once in (`loaded`)
        if self.focus_handle(cx).contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        self.release(Some(window), cx);
        self.generation += 1;
        // the choice to save in place is the file's: another file asks again
        self.in_place &= path == self.path;
        (self.path, self.root, self.body) = (path.clone(), root.clone(), Body::Loading);
        (self.goto, self.restore, self.banner) = (goto, restore, None);
        (self.saving, self.queued) = (None, None);
        (self.preview, self.preview_scroll) = (true, ScrollHandle::new());
        (self.gutter, self.blobs, self.gutter_stale) = (Arc::default(), Blobs::default(), false);
        (self.kept, self.explicit) = (None, false);
        let generation = self.generation;
        let job = self.rt.spawn(async move {
            let read = tokio::task::spawn_blocking(move || read_for_edit(&path, root.as_deref(), force));
            // ponytail: a read that never ends keeps its blocking thread; the viewer then says so
            tokio::time::timeout(LOAD_TIMEOUT, read).await.ok().and_then(Result::ok).unwrap_or(Loaded::View(None))
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(loaded) = job.await {
                let _ = this.update_in(cx, |e, window, cx| e.loaded(generation, loaded, window, cx));
            }
        })
        .detach();
        cx.notify();
    }

    fn loaded(&mut self, generation: u64, loaded: Loaded, window: &mut Window, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        // the view had the keyboard while it loaded: what it shows now takes it
        let focused = self.focus.is_focused(window);
        match loaded {
            Loaded::Edit(file) => self.edit(*file, window, cx),
            Loaded::View(note) => self.view(note, window, cx),
        }
        if focused {
            let handle = self.focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// The file in the editor.
    fn edit(&mut self, file: Editable, window: &mut Window, cx: &mut Context<Self>) {
        let Editable { text, baseline, syntax, readonly, blocks, real_root, hard_tabs } = file;
        // the bytes on disk are the text as edited (no BOM, `\n` ends): one copy for both
        let saved = saved_text(&baseline, &text);
        let style = editor_style(Theme::of(cx));
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(true)
                .folding(false)
                .soft_wrap(false)
                .tab_size(TabSize { tab_size: 4, hard_tabs });
            // ponytail: its find bar is gpui-component's; Shepherd's own comes with E3
            state.set_searchable(false, cx);
            state.set_editor_style(style);
            state.set_editor_paddings(gpui::Edges { left: px(GUTTER_PAD), top: px(4.), ..Default::default() });
            state.set_value(text, window, cx);
            if let Some(syntax) = syntax {
                let factory: gpui_base::input::InputHighlighterFactory =
                    Rc::new(move |_: &str| Some(Box::new(SyntectHighlighter::new(syntax)) as Box<dyn InputHighlighter>));
                state.set_highlighter_factory(factory, cx);
            }
            state.set_readonly(readonly.is_some(), cx);
            state
        });
        let events = cx.subscribe_in(&state, window, |e: &mut FileEditor, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                e.changed(cx);
            }
        });
        let activation = cx.observe_window_activation(window, |e: &mut FileEditor, window, cx| {
            if window.is_window_active() {
                e.check_disk(false, window, cx);
            }
        });
        let markdown = blocks.is_some();
        let tracker = Tracker::new(saved);
        self.body = Body::Edit(Box::new(Edit { state, baseline, tracker, readonly, blocks, real_root, _events: events, _activation: activation }));
        self.watch_disk(window, cx);
        match self.goto.take() {
            Some(Goto::Line(line)) => self.go_to_line(line, window, cx),
            Some(Goto::Fragment(fragment)) => self.go_to_fragment(&fragment, window, cx),
            Some(Goto::Select(line, cols)) => self.select_in_line(line, cols, window, cx),
            None => {
                if let Some(place) = self.restore.take() {
                    self.put_back(place, cx);
                    // a markdown file's source was where the cursor was
                    self.preview = !markdown || place.line == 0;
                }
            }
        }
        self.fetch_blobs(cx);
    }

    /// The file in the viewer, read again by it.
    fn view(&mut self, note: Option<Note>, window: &mut Window, cx: &mut Context<Self>) {
        let viewer = cx.new(|cx| FileViewer::new(self.rt.clone(), cx));
        let events = cx.subscribe(&viewer, |_: &mut FileEditor, _, event: &ViewerEvent, cx| match event {
            ViewerEvent::OpenInEditor { path, line } => cx.emit(EditorEvent::OpenInEditor { path: path.clone(), line: *line }),
            ViewerEvent::OpenFile { path, line, fragment } => cx.emit(EditorEvent::OpenFile { path: path.clone(), line: *line, fragment: fragment.clone() }),
        });
        let line = match &self.goto {
            Some(Goto::Line(line) | Goto::Select(line, _)) => Some(*line),
            _ => None,
        };
        let (path, root) = (self.path.clone(), self.root.clone());
        viewer.update(cx, |v, cx| v.open(path, line, root, window, cx));
        if let Some(Goto::Fragment(fragment)) = self.goto.take() {
            viewer.update(cx, |v, cx| v.go_to(fragment, cx));
        }
        self.body = Body::View { viewer, note, _events: events };
    }

    /// Lets go of what it holds outside itself: the viewer's image and highlight, the gutter's git and diff.
    pub fn release(&mut self, window: Option<&mut Window>, cx: &mut App) {
        if let Body::View { viewer, .. } = &self.body {
            viewer.update(cx, |v, cx| v.release(window, cx));
        }
        (self.gutter_wait, self.gutter_fetch, self.gutter_diff) = (None, None, None);
        (self.disk_watch, self.disk_check, self.disk_reload) = (None, None, None);
        if let Some(compare) = self.compare.take() {
            compare.update(cx, |v, _| v.release());
        }
    }

    /// Out of sight: the viewer's image leaves gpui's cache.
    pub fn hide(&mut self, window: Option<&mut Window>, cx: &mut App) {
        if let Body::View { viewer, .. } = &self.body {
            viewer.update(cx, |v, cx| v.evict_image(window, cx));
        }
    }

    /// Back in sight: the gutter catches up with git if it moved meanwhile, and the disk is looked at.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.gutter_stale) {
            self.fetch_blobs(cx);
        }
        self.check_disk(false, window, cx);
    }

    /// git's state may have moved (a Changes poll, a watch on `.git`): the gutter's blobs are fetched again, now if
    /// `shown`, else when the tab next shows.
    pub fn git_changed(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown {
            self.fetch_blobs(cx);
        } else {
            self.gutter_stale = true;
        }
    }

    /// The buffer changed: the dot, the pin of a preview, and the gutter a moment after typing stops.
    fn changed(&mut self, cx: &mut Context<Self>) {
        let Body::Edit(e) = &mut self.body else { return };
        let text = e.state.read(cx).text();
        if e.tracker.changed(text.len(), text.chunks()) {
            cx.emit(EditorEvent::Dirty(e.tracker.is_dirty()));
            cx.notify();
        }
        self.gutter_wait = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GUTTER_AFTER).await;
            let _ = this.update(cx, |e, cx| e.diff_gutter(cx));
        }));
    }

    fn go_to_line(&mut self, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        match &self.body {
            Body::Loading => self.goto = Some(Goto::Line(line)),
            Body::View { viewer, .. } => {
                let (viewer, path, root) = (viewer.clone(), self.path.clone(), self.root.clone());
                viewer.update(cx, |v, cx| v.open(path, Some(line), root, window, cx));
            }
            Body::Edit(e) => {
                let state = e.state.clone();
                if self.preview_shown() {
                    self.preview = false;
                    if self.focus.is_focused(window) {
                        state.update(cx, |s, cx| s.focus(window, cx));
                    }
                }
                state.update(cx, |s, cx| {
                    let offset = s.text().line_start_offset((line.saturating_sub(1) as usize).min(s.text().lines_len().saturating_sub(1)));
                    s.set_selected_range(offset..offset, cx);
                });
                cx.notify();
            }
        }
    }

    /// Selects `cols` (byte offsets in the line) of `line` (1-based), a search match, scrolled into view; once the
    /// text is in when it is not yet. The viewer, which has no selection, only goes to the line.
    pub fn select_in_line(&mut self, line: u32, cols: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        match &self.body {
            Body::Loading => self.goto = Some(Goto::Select(line, cols)),
            Body::View { .. } => self.go_to_line(line, window, cx),
            Body::Edit(_) => {
                self.go_to_line(line, window, cx);
                let Body::Edit(e) = &self.body else { return };
                e.state.update(cx, |s, cx| {
                    let row = (line.saturating_sub(1) as usize).min(s.text().lines_len().saturating_sub(1));
                    let (start, end) = (s.text().line_start_offset(row), s.text().line_end_offset(row));
                    let from = (start + cols.start).min(end);
                    let to = (start + cols.end).clamp(from, end);
                    s.set_selected_range(from..to, cx);
                });
                cx.notify();
            }
        }
    }

    /// The buffer, while it has changes not saved: what a search looks in rather than the file on disk.
    pub fn unsaved_text(&self, cx: &App) -> Option<Rope> {
        match &self.body {
            Body::Edit(e) if e.tracker.is_dirty() => Some(e.state.read(cx).text().clone()),
            _ => None,
        }
    }

    /// A markdown link's `#heading` or `#L12`, once the text is in.
    pub fn go_to(&mut self, fragment: String, window: &mut Window, cx: &mut Context<Self>) {
        match &self.body {
            Body::Loading => self.goto = Some(Goto::Fragment(fragment)),
            Body::View { viewer, .. } => viewer.update(cx, |v, cx| v.go_to(fragment, cx)),
            Body::Edit(_) => self.go_to_fragment(&fragment, window, cx),
        }
    }

    fn go_to_fragment(&mut self, fragment: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Edit(e) = &self.body else { return };
        if let Some(line) = crate::ui::markdown::fragment_line(fragment) {
            self.go_to_line(line, window, cx);
        } else if let Some(ix) = e.blocks.as_deref().and_then(|b| crate::ui::markdown::anchor_block(b, fragment)) {
            self.preview = true;
            self.preview_scroll.scroll_to_top_of_item(ix + 1);
            cx.notify();
        }
    }

    fn put_back(&mut self, place: Place, cx: &mut Context<Self>) {
        let Body::Edit(e) = &self.body else { return };
        e.state.update(cx, |s, cx| {
            let offset = s.text().position_to_offset(&Position::new(place.line, place.column));
            s.set_selected_range(offset..offset, cx);
            s.set_scroll_offset(point(px(0.), px(place.scroll_y)), cx);
        });
    }

    /// A link in the preview: a heading or a line here is gone to, another file in the root is the Shell's to open.
    fn follow(&mut self, link: Link, window: &mut Window, cx: &mut Context<Self>) {
        match link {
            Link::Anchor(anchor) => self.go_to_fragment(&anchor, window, cx),
            Link::File { path, fragment } if path == self.path => {
                if let Some(fragment) = fragment {
                    self.go_to_fragment(&fragment, window, cx);
                }
            }
            Link::File { path, fragment } => {
                let line = fragment.as_deref().and_then(crate::ui::markdown::fragment_line);
                cx.emit(EditorEvent::OpenFile { path, line, fragment: fragment.filter(|_| line.is_none()) });
            }
            Link::External(_) | Link::Disabled(_) => {}
        }
    }

    fn set_preview(&mut self, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview == preview {
            return;
        }
        let had = self.focus_handle(cx).contains_focused(window, cx) || self.focus.is_focused(window);
        self.preview = preview;
        if had {
            let handle = self.focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// The folder a save checks the file is inside: the workspace's, else the file's own.
    fn save_root(&self) -> PathBuf {
        self.root.clone().or_else(|| self.path.parent().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("/"))
    }

    /// Saves the buffer off the main thread; the task answers whether it was written (callers may share it). While
    /// one runs, one more is queued (typing since is saved too) and the task answers for that one. `None` when there
    /// is nothing to save it from (read-only, not text).
    pub fn save(&mut self, mode: SaveMode, window: &mut Window, cx: &mut Context<Self>) -> Option<Saving> {
        self.save_as(mode, false, window, cx)
    }

    /// `save`; `explicit` (Ctrl/Cmd+S) asks about a newer version on disk rather than leaving the banner.
    fn save_as(&mut self, mode: SaveMode, explicit: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<Saving> {
        let Body::Edit(e) = &self.body else { return None };
        if e.readonly.is_some() {
            return None;
        }
        if let Some(current) = self.saving.clone() {
            self.queued = Some(mode);
            self.explicit |= explicit;
            let follow = cx.spawn_in(window, async move |this, cx| {
                let first = current.await;
                // the queued one started as that one ended (`saved`), unless that one failed or left nothing to save
                match this.update(cx, |e, _| e.saving.clone()).ok().flatten() {
                    Some(next) => next.await,
                    None => first && this.update(cx, |e, _| !e.is_dirty()).unwrap_or(false),
                }
            });
            return Some(follow.boxed_local().shared());
        }
        self.explicit = explicit;
        self.start_save(mode, window, cx)
    }

    fn start_save(&mut self, mode: SaveMode, window: &mut Window, cx: &mut Context<Self>) -> Option<Saving> {
        // a file agreed to be saved in place always is
        let mode = match (mode, self.in_place) {
            (SaveMode::Safe, true) => SaveMode::InPlace,
            (SaveMode::Overwrite, true) => SaveMode::OverwriteInPlace,
            (mode, _) => mode,
        };
        let root = self.save_root();
        let Body::Edit(e) = &mut self.body else { return None };
        let ticket = e.tracker.start_save()?;
        let rope: Rope = e.state.read(cx).text().clone();
        let (baseline, path, generation, kept) = (e.baseline.clone(), self.path.clone(), self.generation, self.kept);
        // the answer comes from a runtime task, not the blocking thread itself (gpui's test scheduler hears tasks
        // woken from its own thread only)
        let job = self.rt.spawn(async move {
            let save = tokio::task::spawn_blocking(move || {
                let text = rope_text(&rope);
                let saved = save_kept(&root, &path, &text, &baseline, mode, kept);
                (Arc::new(text), saved)
            });
            save.await.ok()
        });
        let job = job.map(|done| done.ok().flatten()).boxed().shared();
        self.writing = Some(job.clone().map(|_| ()).boxed().shared());
        cx.emit(EditorEvent::Dirty(e.tracker.is_dirty()));
        cx.notify();
        let (tx, rx) = futures::channel::oneshot::channel();
        cx.spawn_in(window, async move |this, cx| {
            let done = job.await.ok_or_else(|| SaveError::Io("The save did not finish".into()));
            let ok = this.update_in(cx, |e, window, cx| e.saved(generation, ticket, mode, done, window, cx)).unwrap_or(false);
            let _ = tx.send(ok);
        })
        .detach();
        let saving = rx.map(|ok| ok.unwrap_or(false)).boxed_local().shared();
        self.saving = Some(saving.clone());
        Some(saving)
    }

    #[allow(clippy::type_complexity)]
    fn saved(&mut self, generation: u64, ticket: u64, mode: SaveMode, done: Result<(Arc<String>, Result<Baseline, SaveError>), SaveError>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if generation != self.generation {
            return false;
        }
        (self.saving, self.writing) = (None, None);
        let queued = self.queued.take();
        let explicit = std::mem::take(&mut self.explicit);
        let Body::Edit(e) = &mut self.body else { return false };
        let (written, result) = match done {
            Ok((text, Ok(baseline))) => {
                // what was written is the new saved text, kept once when it is the bytes on disk
                let written = if baseline.bytes.as_slice() == text.as_bytes() { baseline.bytes.clone() } else { Arc::new(text.as_bytes().to_vec()) };
                (Some(written), Ok(baseline))
            }
            Ok((_, Err(err))) | Err(err) => (None, Err(err)),
        };
        let text = e.state.read(cx).text();
        if !e.tracker.finish_save(ticket, written, text.len(), text.chunks()) {
            return false;
        }
        let dirty = e.tracker.is_dirty();
        cx.emit(EditorEvent::Dirty(dirty));
        cx.notify();
        let in_place = matches!(mode, SaveMode::InPlace | SaveMode::OverwriteInPlace);
        let result = match result {
            Ok(baseline) => {
                e.baseline = baseline;
                Ok(())
            }
            Err(err) => Err(err),
        };
        match result {
            Ok(()) => {
                // what was kept over is written over
                (self.banner, self.kept) = (None, None);
                // typed during it and asked again: one more
                if let Some(mode) = queued.filter(|_| dirty) {
                    self.explicit = explicit;
                    let _ = self.start_save(mode, window, cx);
                } else {
                    // anything written meanwhile by another program is heard now, not only at its next event
                    self.check_disk(false, window, cx);
                }
                true
            }
            Err(SaveError::Conflict) if explicit => {
                self.ask_overwrite(window, cx);
                false
            }
            Err(SaveError::Conflict) => {
                self.check_disk(true, window, cx);
                false
            }
            Err(SaveError::Deleted) => {
                self.banner = Some(Banner::Deleted);
                false
            }
            Err(err @ (SaveError::HardLinked(_) | SaveError::FolderNotWritable | SaveError::OwnerDiffers)) if !in_place => {
                self.banner = None;
                self.ask_in_place(err, mode, window, cx);
                false
            }
            Err(err) => {
                self.banner = Some(Banner::Failed(err.to_string()));
                false
            }
        }
    }

    /// A safe save can't be made here (a hard-linked file, another owner's, a folder Shepherd can't write to): asks
    /// to write the file where it is instead.
    fn ask_in_place(&mut self, err: SaveError, mode: SaveMode, window: &mut Window, cx: &mut Context<Self>) {
        let detail = format!("{err}. Saving in place rewrites the file where it is: its links and owner stay, but a crash midway can leave it half written.");
        let answer = window.prompt(PromptLevel::Warning, "Save in place?", Some(&detail), &["Save in Place", "Cancel"], cx);
        let next = if mode == SaveMode::Overwrite { SaveMode::Overwrite } else { SaveMode::Safe };
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |e, window, cx| {
                    if e.generation == generation {
                        // asked once per file: its next saves are in place too
                        e.in_place = true;
                        let _ = e.save(next, window, cx);
                    }
                });
            }
        })
        .detach();
    }

    /// Ctrl/Cmd+S found a newer version on disk: Overwrite it, Compare the two, or Cancel (the banner stays).
    fn ask_overwrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let detail = "The file changed on disk since you opened it. Overwrite writes your text over it; Compare shows the two side by side.";
        let answer = window.prompt(PromptLevel::Warning, "Overwrite the newer version on disk?", Some(detail), &["Overwrite", "Compare", "Cancel"], cx);
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await.ok();
            let _ = this.update_in(cx, |e, window, cx| {
                if e.generation != generation {
                    return;
                }
                match answer {
                    Some(0) => e.overwrite(window, cx),
                    Some(1) => {
                        e.check_disk(true, window, cx);
                        e.compare(window, cx);
                    }
                    _ => e.check_disk(true, window, cx),
                }
            });
        })
        .detach();
    }

    /// The banner's Overwrite and the deleted one's Save anyway: the buffer is written over what changed on disk,
    /// or where the file was.
    fn overwrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self.save(SaveMode::Overwrite, window, cx);
    }

    /// The banner's Keep mine: the version on disk it names is the one the next save writes over; another change
    /// on disk asks again.
    fn keep_mine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Banner::Changed { disk, .. }) = self.banner {
            (self.kept, self.banner) = (Some(disk), None);
        }
        self.end_compare(window, cx);
        cx.notify();
    }

    /// The banner's Reload: the disk's text in place, the unsaved changes dropped (undo still has them).
    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_compare(window, cx);
        self.reload_in_place(true, window, cx);
    }

    /// Reads the file again and puts its text in the buffer as one edit, the cursor and scroll kept; `discard`
    /// drops unsaved changes, else a buffer edited meanwhile is left alone (and the banner asks). Text that no
    /// longer opens editable (binary now, too large) goes to the viewer.
    fn reload_in_place(&mut self, discard: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (path, root, generation) = (self.path.clone(), self.root.clone(), self.generation);
        let job = self.rt.spawn(async move {
            let read = tokio::task::spawn_blocking(move || read_for_edit(&path, root.as_deref(), false));
            tokio::time::timeout(LOAD_TIMEOUT, read).await.ok().and_then(Result::ok)
        });
        self.disk_reload = Some(cx.spawn_in(window, async move |this, cx| {
            let Ok(Some(loaded)) = job.await else { return };
            let _ = this.update_in(cx, |e, window, cx| {
                if e.generation != generation {
                    return;
                }
                match loaded {
                    Loaded::Edit(file) => e.put_in_place(*file, discard, window, cx),
                    Loaded::View(_) => e.reload_now(window, cx),
                }
            });
        }));
    }

    fn put_in_place(&mut self, file: Editable, discard: bool, window: &mut Window, cx: &mut Context<Self>) {
        let place = self.place(cx);
        let Body::Edit(e) = &mut self.body else { return };
        // a save began meanwhile (it looks again when it ends), or typing did and nothing is to be dropped
        if e.tracker.saving() || (!discard && e.tracker.is_dirty()) {
            return self.check_disk(false, window, cx);
        }
        // the baseline and the saved text first: the buffer's change below is compared with them
        rebase(&mut e.tracker, &mut e.baseline, &file);
        let Editable { text, readonly, blocks, real_root, .. } = file;
        (e.readonly, e.blocks, e.real_root) = (readonly, blocks, real_root);
        e.state.update(cx, |s, cx| {
            s.set_readonly(readonly.is_some(), cx);
            s.replace_all(text, window, cx);
            if let Some(place) = place {
                // the cursor where it was, on a line the text still has
                let last = s.text().lines_len().saturating_sub(1) as u32;
                let offset = s.text().position_to_offset(&Position::new(place.line.min(last), place.column));
                s.set_selected_range(offset..offset, cx);
                s.set_scroll_offset(point(px(0.), px(place.scroll_y)), cx);
            }
        });
        if matches!(self.banner, Some(Banner::Changed { .. } | Banner::Deleted)) {
            self.banner = None;
        }
        self.kept = None;
        // the dot now, not only when the buffer's change is heard
        self.changed(cx);
        cx.notify();
    }

    /// Reads the file again from scratch (as opening it): for text that no longer opens editable in place.
    fn reload_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let place = self.place(cx);
        let (path, root) = (self.path.clone(), self.root.clone());
        self.load(path, None, root, place, false, window, cx);
        cx.emit(EditorEvent::Dirty(false));
    }

    /// The banner's Compare: the disk's text (left) against the buffer (right) in the text's place, read again
    /// when the disk changes while it shows.
    fn compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Edit(e) = &self.body else { return };
        let rope: Rope = e.state.read(cx).text().clone();
        let (path, generation) = (self.path.clone(), self.generation);
        let read = path.clone();
        let job = self.rt.spawn(async move {
            let texts = tokio::task::spawn_blocking(move || disk_text(&read).map(|disk| (disk, rope_text(&rope))));
            tokio::time::timeout(LOAD_TIMEOUT, texts).await.ok().and_then(Result::ok).flatten()
        });
        let rt = self.rt.clone();
        cx.spawn_in(window, async move |this, cx| {
            let texts = job.await.ok().flatten();
            let _ = this.update_in(cx, |e, window, cx| {
                if e.generation != generation {
                    return;
                }
                let Some((disk, mine)) = texts else {
                    return crate::ui::toast::error(cx, "Can't compare: the file on disk isn't text");
                };
                let (disk, mine) = (Arc::new(disk), Arc::new(mine));
                if let Some(view) = &e.compare {
                    view.update(cx, |v, cx| v.set_texts(disk, mine, cx));
                    return;
                }
                let had = e.focus_handle(cx).contains_focused(window, cx);
                let view = cx.new(|cx| DiffView::compare(rt, path, disk, mine, ("On disk", "Yours"), cx));
                e.compare = Some(view);
                if had {
                    let handle = e.focus_handle(cx);
                    window.focus(&handle, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Back from a Compare to the text.
    fn end_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.compare.take() else { return };
        let had = view.read(cx).focus_handle(cx).contains_focused(window, cx);
        view.update(cx, |v, _| v.release());
        if had {
            let handle = self.focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// Watches the file's folder (not its tree) for events naming the file, the watch made on the runtime's
    /// blocking pool (a dead mount can hang it); a burst is heard `DISK_AFTER` after its first event, on the
    /// runtime, and the editor looks at the disk then. The watch goes with the editor's task (the runtime's sees its
    /// channel close). A folder that can't be watched leaves the looks to the tab showing and the window coming to
    /// the front.
    fn watch_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(dir), Some(name)) = (self.path.parent().map(PathBuf::from), self.path.file_name().map(std::ffi::OsString::from)) else { return };
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (out, mut heard) = tokio::sync::mpsc::unbounded_channel::<()>();
        self.rt.spawn(async move {
            let make = tokio::task::spawn_blocking(move || {
                use ::notify::event::{AccessKind, AccessMode};
                use ::notify::{EventKind, RecursiveMode, Watcher};
                let mut watcher = ::notify::recommended_watcher(move |event: ::notify::Result<::notify::Event>| {
                    let named = |ev: &::notify::Event| ev.paths.iter().any(|p| p.file_name() == Some(&name));
                    let ours = match &event {
                        // an open or a read (the editor's own looks among them) changes nothing; a write's close does
                        Ok(ev) if matches!(ev.kind, EventKind::Access(AccessKind::Close(AccessMode::Write))) => named(ev),
                        Ok(ev) if matches!(ev.kind, EventKind::Access(_)) => false,
                        // a rescan names nothing
                        Ok(ev) => ev.paths.is_empty() || named(ev),
                        Err(_) => true,
                    };
                    if ours {
                        let _ = tx.send(());
                    }
                })
                .ok()?;
                watcher.watch(&dir, RecursiveMode::NonRecursive).ok()?;
                Some(watcher)
            });
            let Ok(Ok(Some(watcher))) = tokio::time::timeout(LOAD_TIMEOUT, make).await else { return };
            // the watch lasts as long as this loop
            let _watcher = watcher;
            loop {
                tokio::select! {
                    got = events.recv() => if got.is_none() { return },
                    _ = out.closed() => return,
                }
                tokio::time::sleep(DISK_AFTER).await;
                while events.try_recv().is_ok() {}
                if out.send(()).is_err() {
                    return;
                }
            }
        });
        let generation = self.generation;
        self.disk_watch = Some(cx.spawn_in(window, async move |this, cx| {
            while heard.recv().await.is_some() {
                let looked = this.update_in(cx, |e, window, cx| {
                    if e.generation == generation {
                        e.check_disk(false, window, cx);
                    }
                });
                if looked.is_err() {
                    return;
                }
            }
        }));
    }

    /// Looks at the file on disk off the main thread and acts on it (`decide`): not while a save runs (it looks when
    /// it ends). `after_save`: a save found the disk changed, which the banner says.
    fn check_disk(&mut self, after_save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Edit(e) = &self.body else { return };
        if e.tracker.saving() {
            return;
        }
        let (path, baseline, generation) = (self.path.clone(), e.baseline.clone(), self.generation);
        let checked = baseline.bytes.clone();
        let job = self.rt.spawn(async move {
            let look = tokio::task::spawn_blocking(move || look(&path, &baseline));
            tokio::time::timeout(LOAD_TIMEOUT, look).await.ok().and_then(Result::ok).flatten()
        });
        self.disk_check = Some(cx.spawn_in(window, async move |this, cx| {
            let Ok(Some(disk)) = job.await else { return };
            let _ = this.update_in(cx, |e, window, cx| e.on_disk(generation, &checked, disk, after_save, window, cx));
        }));
    }

    fn on_disk(&mut self, generation: u64, checked: &Arc<Vec<u8>>, disk: Disk, after_save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Edit(e) = &self.body else { return };
        // another file since, or a save began or ended since the look: that one looks again
        if generation != self.generation || e.tracker.saving() || !Arc::ptr_eq(&e.baseline.bytes, checked) {
            return;
        }
        let dirty = e.tracker.is_dirty();
        match decide(dirty, disk, self.kept) {
            OnDisk::Nothing => {}
            OnDisk::Clear => {
                self.kept = None;
                if matches!(self.banner, Some(Banner::Changed { .. } | Banner::Deleted)) {
                    self.banner = None;
                    cx.notify();
                }
            }
            OnDisk::Reload => self.reload_in_place(false, window, cx),
            OnDisk::Banner => {
                let Disk::Changed(seen) = disk else { return };
                let save = after_save || matches!(self.banner, Some(Banner::Changed { save: true, .. }));
                let banner = Some(Banner::Changed { disk: seen, save });
                if self.banner != banner {
                    self.banner = banner;
                    cx.notify();
                }
            }
            OnDisk::Deleted => {
                if self.banner != Some(Banner::Deleted) {
                    self.banner = Some(Banner::Deleted);
                    cx.notify();
                }
            }
        }
        // a Compare showing takes the disk's new text
        if self.compare.is_some() && matches!(disk, Disk::Changed(_)) {
            self.compare(window, cx);
        }
    }

    fn open_in_editor(&mut self, cx: &mut Context<Self>) {
        let line = self.place(cx).map(|p| p.line + 1);
        cx.emit(EditorEvent::OpenInEditor { path: self.path.clone(), line });
    }

    /// Fetches the index's and HEAD's blobs of the file in one `git cat-file --batch`, in the file's folder.
    fn fetch_blobs(&mut self, cx: &mut Context<Self>) {
        let (Body::Edit(_), Some(dir), Some(name)) = (&self.body, self.path.parent(), self.path.file_name().and_then(|n| n.to_str())) else { return };
        // the batch protocol is a line per object
        if name.contains('\n') {
            return;
        }
        let mut cmd = runner::git_cmd(dir, &["cat-file", "--batch"]);
        cmd.stdin = Some(format!(":./{name}\nHEAD:./{name}\n").into_bytes());
        let limits = Limits { timeout: Duration::from_secs(5), max_bytes: 2 * MAX_EDIT as usize + 4096 };
        let job = self.rt.spawn(async move { runner::run(&Host::Local, cmd, limits).await });
        let generation = self.generation;
        self.gutter_fetch = Some(cx.spawn(async move |this, cx| {
            let answers = match job.await {
                Ok(Ok(out)) if !out.truncated => line_changes::parse_batch(&out.stdout),
                _ => Vec::new(),
            };
            let mut answers = answers.into_iter();
            let text = |a: Option<Option<(String, Vec<u8>)>>| a.flatten().map(|(id, bytes)| (id, String::from_utf8_lossy(&bytes).into_owned()));
            let (index, head) = (text(answers.next()), text(answers.next()));
            let ids = (index.as_ref().map(|(id, _)| id.clone()), head.as_ref().map(|(id, _)| id.clone()));
            // nothing moved in git: the bars stand
            let same = this.update(cx, |e, _| (e.blobs.index.as_ref().map(|(id, _)| id.clone()), e.blobs.head_id.clone()) == ids);
            if same.unwrap_or(true) {
                return;
            }
            let blobs = cx
                .background_spawn(async move {
                    let staged = index.as_ref().map(|(_, i)| line_changes::staged(head.as_ref().map(|(_, h)| h.as_str()), i, Instant::now() + GUTTER_BUDGET));
                    Blobs { index: index.map(|(id, i)| (id, Arc::new(i))), head_id: ids.1, staged: Arc::new(staged.unwrap_or_default()) }
                })
                .await;
            let _ = this.update(cx, |e, cx| {
                if e.generation == generation {
                    e.blobs = blobs;
                    e.diff_gutter(cx);
                }
            });
        }));
    }

    /// Works the gutter's bars out again on the background executor: the buffer against the blobs.
    fn diff_gutter(&mut self, cx: &mut Context<Self>) {
        let Body::Edit(e) = &self.body else { return };
        let Some(index) = self.blobs.index.as_ref().map(|(_, b)| b.clone()) else {
            if !self.gutter.is_empty() {
                self.gutter = Arc::default();
                cx.notify();
            }
            return;
        };
        let staged = self.blobs.staged.clone();
        let rope: Rope = e.state.read(cx).text().clone();
        self.gutter_seq += 1;
        let seq = self.gutter_seq;
        let job = cx.background_spawn(async move {
            line_changes::line_changes(&staged, Some(&index), &rope_text(&rope), Instant::now() + GUTTER_BUDGET)
        });
        self.gutter_diff = Some(cx.spawn(async move |this, cx| {
            let changes = job.await;
            let _ = this.update(cx, |e, cx| {
                if e.gutter_seq == seq && *e.gutter != changes {
                    e.gutter = Arc::new(changes);
                    cx.notify();
                }
            });
        }));
    }

    /// Keys while the preview has the keyboard (the editor handles its own): copy and scrolling.
    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) || !self.preview_shown() {
            return;
        }
        let (key, m) = (e.keystroke.key.as_str(), &e.keystroke.modifiers);
        let primary = if cfg!(target_os = "macos") { m.platform && !m.control } else { m.control && !m.platform };
        if crate::platform::terminal_chord(key, m) == Some("c") || (primary && !m.alt && key == "c") {
            if let Some(text) = crate::ui::markdown::selected_text() {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            }
        } else if m.modified() {
            return;
        } else {
            let handle = &self.preview_scroll;
            let line = px(Theme::of(cx).terminal_font_size * 1.5);
            let page = handle.bounds().size.height - line * 2.;
            let (offset, max) = (handle.offset(), handle.max_offset());
            let y = match key {
                "up" => offset.y + line * 3.,
                "down" => offset.y - line * 3.,
                "pageup" => offset.y + page,
                "pagedown" => offset.y - page,
                "home" => px(0.),
                "end" => -max.y,
                _ => return,
            };
            handle.set_offset(point(offset.x, y.clamp(-max.y, px(0.))));
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn header(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let icon = crate::files::icons::material_icon(&name, false, false);
        let tag = |label: SharedString| {
            div().flex_none().px(px(5.)).rounded(px(4.)).border_1().border_color(theme.border).text_size(px(10.5)).text_color(theme.text_faint).child(label)
        };
        let Body::Edit(e) = &self.body else { return div().into_any_element() };
        let ending = match e.baseline.ending {
            LineEnding::Lf => "LF",
            LineEnding::Crlf => "CRLF",
            LineEnding::Cr => "CR",
        };
        let toggle = e.blocks.is_some().then(|| {
            let choice = |id: &'static str, label: &'static str, on: bool, preview: bool| {
                let hover = theme.hover;
                div()
                    .id(id)
                    .px(px(8.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .text_color(if on { theme.text } else { theme.text_muted })
                    .when(on, |el| el.bg(theme.selected))
                    .when(!on, |el| el.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |e: &mut FileEditor, _: &ClickEvent, window, cx| e.set_preview(preview, window, cx)))
                    .child(label)
            };
            div()
                .flex()
                .flex_none()
                .gap(px(2.))
                .p(px(2.))
                .rounded(px(7.))
                .border_1()
                .border_color(theme.border)
                .child(choice("editor-preview", "Preview", self.preview, true))
                .child(choice("editor-source", "Source", !self.preview, false))
        });
        let path = self.path.to_string_lossy().into_owned();
        let readonly = e.readonly;
        div()
            .h(px(36.))
            .flex_none()
            .pl(px(12.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.wash(0.04))
            .font_family(theme.font_ui.clone())
            .text_size(px(12.5))
            .child(gpui::img(icon).size(px(16.)).flex_none().object_fit(gpui::ObjectFit::Contain))
            .child(div().min_w_0().truncate().text_color(theme.text).child(crate::ui::viewer::shown_path(&self.path, self.root.as_deref())))
            .child(tag(ending.into()))
            .when(e.baseline.mixed, |el| el.child(tag("Mixed line ends".into())))
            .when_some(readonly, |el, why| el.child(div().id("editor-readonly").child(tag("Read-only".into())).tooltip(tip(why, None))))
            .when(e.tracker.saving(), |el| el.child(div().text_size(px(11.5)).text_color(theme.text_faint).child("Saving…")))
            .child(div().flex_1())
            .children(toggle)
            .child(
                titlebar_button(theme, "editor-open-in-editor", "icons/solar/pen-new-square.svg")
                    .tooltip(tip("Open in editor", None))
                    .on_click(cx.listener(|e, _: &ClickEvent, _, cx| e.open_in_editor(cx))),
            )
            .child(
                titlebar_button(theme, "editor-copy-path", "icons/solar/copy.svg")
                    .tooltip(tip("Copy path", None))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone())))),
            )
            .into_any_element()
    }

    /// A button in a banner or note.
    fn button(theme: &Theme, id: &'static str) -> gpui::Stateful<gpui::Div> {
        let hover = theme.hover;
        div()
            .id(id)
            .flex_none()
            .px(px(10.))
            .h(px(22.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .border_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .text_color(theme.text)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
    }

    /// The strip under the header for a save's trouble or the disk's changes.
    fn banner(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let banner = self.banner.as_ref()?;
        let strip = div()
            .flex_none()
            .min_h(px(32.))
            .px(px(12.))
            .py(px(4.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.warning.opacity(0.12))
            .font_family(theme.font_ui.clone())
            .text_size(px(12.5));
        Some(match banner {
            Banner::Changed { save, .. } => {
                let said = if *save { "The file changed on disk, so your save was not written." } else { "The file changed on disk." };
                let keep = if *save {
                    Self::button(theme, "editor-overwrite").child("Overwrite").on_click(cx.listener(|e, _: &ClickEvent, window, cx| e.overwrite(window, cx)))
                } else {
                    Self::button(theme, "editor-keep-mine").child("Keep mine").on_click(cx.listener(|e, _: &ClickEvent, window, cx| e.keep_mine(window, cx)))
                };
                let compare = if self.compare.is_some() {
                    Self::button(theme, "editor-back").child("Back to editor").on_click(cx.listener(|e, _: &ClickEvent, window, cx| e.end_compare(window, cx)))
                } else {
                    Self::button(theme, "editor-compare").child("Compare").on_click(cx.listener(|e, _: &ClickEvent, window, cx| e.compare(window, cx)))
                };
                strip
                    .child(div().flex_1().min_w_0().text_color(theme.text).child(said))
                    .child(compare)
                    .child(keep)
                    .child(
                        Self::button(theme, "editor-reload")
                            .child("Reload")
                            .tooltip(tip("Take the disk's version; undo brings yours back", None))
                            .on_click(cx.listener(|e, _: &ClickEvent, window, cx| e.discard(window, cx))),
                    )
                    .into_any_element()
            }
            Banner::Deleted => strip
                .child(div().flex_1().min_w_0().text_color(theme.text).child("Deleted or moved on disk."))
                .child(Self::button(theme, "editor-save-anyway").child("Save anyway").on_click(cx.listener(|e, _: &ClickEvent, window, cx| e.overwrite(window, cx))))
                .child(Self::button(theme, "editor-close").child("Close").on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(EditorEvent::Close))))
                .into_any_element(),
            Banner::Failed(why) => strip
                .child(div().flex_1().min_w_0().text_color(theme.text).child(format!("Couldn't save: {why}")))
                .child(Self::button(theme, "editor-dismiss").child("Dismiss").on_click(cx.listener(|e, _: &ClickEvent, _, cx| {
                    e.banner = None;
                    cx.notify();
                })))
                .into_any_element(),
        })
    }

    /// The note over the viewer when text did not open editable.
    fn note(&self, note: Note, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let strip = div()
            .flex_none()
            .h(px(32.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(theme.border)
            .font_family(theme.font_ui.clone())
            .text_size(px(12.5))
            .text_color(theme.text_muted);
        match note {
            Note::LongLines => strip
                .child(div().flex_1().min_w_0().truncate().child(format!("A line is longer than {} characters: shown read-only.", group(MAX_EDIT_LINE))))
                .child(Self::button(theme, "editor-edit-anyway").child("Edit anyway").on_click(cx.listener(|e, _: &ClickEvent, window, cx| {
                    let (path, root) = (e.path.clone(), e.root.clone());
                    e.load(path, None, root, None, true, window, cx);
                })))
                .into_any_element(),
            Note::TooLarge => strip
                .child(div().flex_1().min_w_0().truncate().child(format!(
                    "Files over {} or {} lines open read-only.",
                    content::human_size(MAX_EDIT),
                    group(MAX_EDIT_LINES)
                )))
                .into_any_element(),
        }
    }

    /// The editor, with the gutter's bars over its padding.
    fn code(&self, e: &Edit, theme: &Theme) -> AnyElement {
        let font = gpui::font(theme.font_terminal.clone());
        let line_h = px((theme.terminal_font_size * theme.terminal_line_height).round());
        let (state, changes) = (e.state.clone(), self.gutter.clone());
        let colors = (theme.success, theme.accent, theme.danger);
        let bars = canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                if changes.is_empty() {
                    return;
                }
                let s = state.read(cx);
                let (Some(rows), Some(line_h)) = (s.visible_row_range(), s.line_height()) else { return };
                let first = s.text().line_start_offset(rows.start);
                let Some(top) = s.range_to_bounds(&(first..first)).map(|b| b.top()) else { return };
                let x = s.input_bounds().left() - px(GUTTER_PAD) + px(2.);
                window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                    for change in line_changes::visible(&changes, rows.clone()) {
                        let color = match change.mark {
                            Mark::Added => colors.0,
                            Mark::Modified => colors.1,
                            Mark::Deleted => colors.2,
                        };
                        let y = top + line_h * (change.rows.start as f32 - rows.start as f32);
                        let bar = match change.mark {
                            // a notch on the line between, where the lines went
                            Mark::Deleted => gpui::Bounds::new(point(x, y - px(2.)), gpui::size(px(5.), px(4.))),
                            _ => gpui::Bounds::new(point(x, y), gpui::size(px(3.), line_h * change.rows.len() as f32)),
                        };
                        if change.staged {
                            window.paint_quad(gpui::outline(bar, color, gpui::BorderStyle::Solid));
                        } else {
                            window.paint_quad(gpui::fill(bar, color));
                        }
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font(font)
            .text_size(px(theme.terminal_font_size))
            .line_height(line_h)
            .text_color(theme.terminal.foreground)
            .child(Editor::new(&e.state))
            .child(bars)
            .into_any_element()
    }

    /// The strips under the header, in order: a save's trouble, the disk's changes.
    fn banners(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = self.banner(theme, cx).into_iter().collect();
        // a Compare with no banner to leave it by (the disk went back, or Keep mine came from elsewhere)
        if self.compare.is_some() && !matches!(self.banner, Some(Banner::Changed { .. })) {
            let strip = div()
                .flex_none()
                .min_h(px(32.))
                .px(px(12.))
                .py(px(4.))
                .flex()
                .items_center()
                .gap(px(8.))
                .border_b_1()
                .border_color(theme.border)
                .font_family(theme.font_ui.clone())
                .text_size(px(12.5))
                .child(div().flex_1().min_w_0().text_color(theme.text_muted).child("The file on disk against your text."))
                .child(Self::button(theme, "editor-back").child("Back to editor").on_click(cx.listener(|e, _: &ClickEvent, window, cx| e.end_compare(window, cx))));
            out.push(strip.into_any_element());
        }
        out
    }

    /// What sits between the banners and the text (a find bar): none yet.
    fn overlays(&self, _theme: &Theme, _cx: &mut Context<Self>) -> Vec<AnyElement> {
        Vec::new()
    }

    /// The text: a Compare, markdown's preview, or the editor.
    fn body_element(&self, e: &Edit, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if let Some(compare) = &self.compare {
            compare.clone().into_any_element()
        } else if self.preview_shown() {
            // a press in the preview gives it the keyboard (its selection is ui::markdown's)
            let preview = self.preview_body(e, theme, cx);
            div()
                .size_full()
                .on_mouse_down(MouseButton::Left, cx.listener(|e, _, window, cx| window.focus(&e.focus, cx)))
                .child(preview)
                .into_any_element()
        } else {
            self.code(e, theme)
        }
    }

    fn preview_body(&self, e: &Edit, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let blocks = e.blocks.as_deref().unwrap_or_default();
        let links = match &e.real_root {
            Some(root) => {
                let this = cx.entity().downgrade();
                Links::local(self.path.clone(), root.clone(), move |link, window, cx| {
                    let _ = this.update(cx, |e, cx| e.follow(link, window, cx));
                })
            }
            None => Links::web(),
        };
        crate::ui::viewer::markdown_preview("editor-md", blocks, &links, &self.preview_scroll, theme)
    }
}

/// The rope as one string (off the main thread: a 4 MB copy).
fn rope_text(rope: &Rope) -> String {
    let mut text = String::with_capacity(rope.len());
    for chunk in rope.chunks() {
        text.push_str(chunk);
    }
    text
}

/// `n` with thousands apart: 200,000.
fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl Render for FileEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &Theme::of(cx).clone();
        let root = div()
            .id("file-editor")
            .track_focus(&self.focus)
            .key_context("FileEditor")
            .on_key_down(cx.listener(|e, ev: &KeyDownEvent, window, cx| e.key(ev, window, cx)))
            // the editor's text and a markdown preview's alike
            .on_action(cx.listener(|e, _: &SaveFile, window, cx| {
                let _ = e.save_as(SaveMode::Safe, true, window, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.terminal.background);
        match &self.body {
            Body::Loading => root.into_any_element(),
            Body::View { viewer, note, .. } => {
                let note = note.map(|n| self.note(n, theme, cx));
                root.children(note).child(div().flex_1().min_h_0().child(viewer.clone())).into_any_element()
            }
            Body::Edit(e) => {
                let header = self.header(theme, cx);
                let banners = self.banners(theme, cx);
                let overlays = self.overlays(theme, cx);
                let body = self.body_element(e, theme, cx);
                root.child(header)
                    .children(banners)
                    .children(overlays)
                    .child(div().flex_1().min_h_0().overflow_hidden().child(body))
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_is_compared_by_length_then_chunks() {
        assert!(same_text(5, ["ab", "c", "de"], b"abcde"));
        assert!(!same_text(5, ["ab", "c", "dx"], b"abcde"));
        assert!(!same_text(4, ["ab", "cd"], b"abcde"), "lengths differ");
        assert!(same_text(0, std::iter::empty(), b""));
        // chunks that claim the length but run short or long
        assert!(!same_text(5, ["ab"], b"abcde"));
        assert!(!same_text(5, ["abcdef"], b"abcde"));
    }

    #[test]
    fn undo_back_to_the_saved_text_clears_the_dot() {
        let mut t = Tracker::new(Arc::new("fn main() {}\n".into()));
        assert!(!t.is_dirty());
        assert!(t.changed(14, ["fn main() {x}\n"]), "the first edit turns it on");
        assert!(t.is_dirty());
        assert!(!t.changed(15, ["fn main() {xy}\n"]), "still dirty: no flip");
        assert!(t.changed(13, ["fn main() {", "}\n"]), "undone to the saved text, in other chunks");
        assert!(!t.is_dirty());
    }

    #[test]
    fn typing_during_a_save_stays_dirty() {
        let mut t = Tracker::new(Arc::new("a".into()));
        t.changed(2, ["ab"]);
        let ticket = t.start_save().expect("a save");
        assert!(t.saving());
        assert_eq!(t.start_save(), None, "one at a time");
        // typed while the save of "ab" ran
        t.changed(3, ["abc"]);
        assert!(t.finish_save(ticket, Some(Arc::new(b"ab".to_vec())), 3, ["abc"]));
        assert!(t.is_dirty(), "only what was written is the baseline");
        assert!(!t.saving());
        // undo to what the save wrote: clean
        assert!(t.changed(2, ["ab"]));
        assert!(!t.is_dirty());
        // a save of the buffer as it is ends clean
        t.changed(3, ["abc"]);
        let ticket = t.start_save().unwrap();
        assert!(t.finish_save(ticket, Some(Arc::new(b"abc".to_vec())), 3, ["abc"]));
        assert!(!t.is_dirty());
        // a failed save leaves the dot and the old baseline
        t.changed(4, ["abcd"]);
        let ticket = t.start_save().unwrap();
        assert!(t.finish_save(ticket, None, 4, ["abcd"]));
        assert!(t.is_dirty());
        assert!(t.changed(3, ["abc"]), "the baseline is still the last written");
        // a late answer to an older save counts for nothing
        assert!(!t.finish_save(ticket, Some(Arc::new(b"zzz".to_vec())), 3, ["abc"]));
        assert!(!t.is_dirty());
    }

    #[test]
    fn text_within_the_limits_opens_editable() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, body: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, body).unwrap();
            p
        };
        let ok = file("a.rs", b"fn a() {}\r\nfn b() {}\r\n");
        match read_for_edit(&ok, Some(dir.path()), false) {
            Loaded::Edit(e) => {
                assert_eq!(e.text, "fn a() {}\nfn b() {}\n", "line ends as \\n; the baseline keeps CRLF");
                assert_eq!(e.baseline.ending, LineEnding::Crlf);
                assert!(e.syntax.is_some());
                assert_eq!(e.readonly, None);
            }
            Loaded::View(_) => panic!("editable"),
        }
        let long = file("min.js", format!("{}\n", "x".repeat(MAX_EDIT_LINE + 1)).as_bytes());
        assert!(matches!(read_for_edit(&long, Some(dir.path()), false), Loaded::View(Some(Note::LongLines))));
        assert!(matches!(read_for_edit(&long, Some(dir.path()), true), Loaded::Edit(_)), "Edit anyway");
        let binary = file("b.bin", b"\0\x01\x02");
        assert!(matches!(read_for_edit(&binary, Some(dir.path()), false), Loaded::View(None)));
        // outside the workspace's folder: read-only
        let other = tempfile::tempdir().unwrap();
        match read_for_edit(&ok, Some(other.path()), false) {
            Loaded::Edit(e) => assert!(e.readonly.is_some()),
            Loaded::View(_) => panic!("editable"),
        }
    }

    #[test]
    fn the_disk_s_changes_are_decided_by_dirt_and_what_was_kept() {
        let (v1, v2) = (Seen { len: 3, hash: 1 }, Seen { len: 3, hash: 2 });
        use OnDisk::*;
        let table = [
            // (dirty, disk, kept) -> what happens
            (false, Disk::Same, None, Clear),
            (true, Disk::Same, Some(v1), Clear),
            (false, Disk::Changed(v1), None, Reload),
            (false, Disk::Changed(v1), Some(v1), Reload),
            (true, Disk::Changed(v1), None, Banner),
            (true, Disk::Changed(v1), Some(v1), Nothing),
            (true, Disk::Changed(v2), Some(v1), Banner),
            (false, Disk::Gone, None, Deleted),
            (true, Disk::Gone, Some(v1), Deleted),
        ];
        for (dirty, disk, kept, want) in table {
            assert_eq!(decide(dirty, disk, kept), want, "dirty {dirty}, {disk:?}, kept {kept:?}");
        }
    }

    #[test]
    fn a_look_at_the_disk_tells_same_changed_and_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        assert_eq!(look(&path, &b), Some(Disk::Same));
        std::fs::write(&path, "two\n").unwrap();
        assert_eq!(look(&path, &b), Some(Disk::Changed(Seen::of(b"two\n"))));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(look(&path, &b), Some(Disk::Gone), "deleted");
        // a folder in its place is no file
        std::fs::create_dir(&path).unwrap();
        assert_eq!(look(&path, &b), Some(Disk::Gone));
        std::fs::remove_dir(&path).unwrap();
        // back as it was: the same
        std::fs::write(&path, "one\n").unwrap();
        assert_eq!(look(&path, &b), Some(Disk::Same));
    }

    #[test]
    fn a_save_writes_over_the_version_kept_and_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let b = Baseline::read(&path).unwrap();
        std::fs::write(&path, "theirs\n").unwrap();
        // nothing kept: a conflict
        assert_eq!(save_kept(dir.path(), &path, "mine\n", &b, SaveMode::Safe, None), Err(SaveError::Conflict));
        // their version kept: written over
        let kept = Some(Seen::of(b"theirs\n"));
        let saved = save_kept(dir.path(), &path, "mine\n", &b, SaveMode::Safe, kept).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");
        assert_eq!(*saved.bytes, b"mine\n");
        // a newer version than the one kept: a conflict again
        std::fs::write(&path, "theirs again\n").unwrap();
        assert_eq!(save_kept(dir.path(), &path, "mine 2\n", &saved, SaveMode::Safe, kept), Err(SaveError::Conflict));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs again\n");
        // an Overwrite needs nothing kept
        save_kept(dir.path(), &path, "mine 2\n", &saved, SaveMode::Overwrite, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine 2\n");
    }

    #[test]
    fn a_reload_in_place_leaves_the_tracker_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn a() {}\r\n").unwrap();
        let Loaded::Edit(old) = read_for_edit(&path, Some(dir.path()), false) else { panic!("editable") };
        let mut baseline = old.baseline.clone();
        let mut tracker = Tracker::new(saved_text(&baseline, &old.text));
        // unsaved changes, then the disk's new text taken in place
        tracker.changed(5, ["mine\n"]);
        assert!(tracker.is_dirty());
        std::fs::write(&path, "fn b() {}\r\n").unwrap();
        let Loaded::Edit(file) = read_for_edit(&path, Some(dir.path()), false) else { panic!("editable") };
        rebase(&mut tracker, &mut baseline, &file);
        assert_eq!(*baseline.bytes, b"fn b() {}\r\n", "the disk's version is the baseline");
        // the buffer's change to that text is heard after: clean, the dot going
        assert!(tracker.changed(file.text.len(), [file.text.as_str()]));
        assert!(!tracker.is_dirty());
        // undo back to what was typed: dirty again
        assert!(tracker.changed(5, ["mine\n"]));
    }

    /// A runtime driven on the test's thread (`settle`): gpui's test scheduler hears tasks woken from there only.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    /// Runs the window's tasks and the runtime's until `done` holds.
    fn settle(rt: &tokio::runtime::Runtime, cx: &mut gpui::VisualTestContext, mut done: impl FnMut(&mut gpui::VisualTestContext) -> bool) {
        for _ in 0..500 {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            rt.block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
        }
        panic!("never settled");
    }

    /// A window with an editor on `path` (in `root`), once its text is in.
    fn editor_on(cx: &mut gpui::TestAppContext, rt: &tokio::runtime::Runtime, root: &Path, path: &Path) -> (Entity<FileEditor>, &'static mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::settings::Settings::default().theme(gpui::WindowAppearance::Dark));
        });
        let handle = rt.handle().clone();
        let (editor, cx) = cx.add_window_view(|_, cx| FileEditor::new(handle, cx));
        let cx: &'static mut gpui::VisualTestContext = Box::leak(Box::new(cx.clone()));
        let (path, root) = (path.to_path_buf(), root.to_path_buf());
        editor.update_in(cx, |e, window, cx| e.open(path, None, Some(root), None, window, cx));
        settle(rt, cx, |cx| editor.read_with(cx, |e, _| matches!(e.body, Body::Edit(_))));
        (editor, cx)
    }

    fn text_of(editor: &Entity<FileEditor>, cx: &mut gpui::VisualTestContext) -> String {
        editor.read_with(cx, |e, cx| e.editor_state().map(|s| rope_text(s.read(cx).text())).unwrap_or_default())
    }

    #[gpui::test]
    fn a_clean_buffer_takes_the_disk_s_change_in_place_and_undo_steps_back(cx: &mut gpui::TestAppContext) {
        let rt = &runtime();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let (editor, cx) = editor_on(cx, &rt, dir.path(), &path);
        std::fs::write(&path, "one\nTWO\nthree\n").unwrap();
        editor.update_in(cx, |e, window, cx| e.check_disk(false, window, cx));
        settle(rt, cx, |cx| text_of(&editor, cx) == "one\nTWO\nthree\n");
        editor.read_with(cx, |e, _| {
            assert!(!e.is_dirty(), "the disk's text is the saved text");
            assert_eq!(e.banner, None);
            let Body::Edit(edit) = &e.body else { panic!("editable") };
            assert_eq!(*edit.baseline.bytes, b"one\nTWO\nthree\n");
        });
        // the reload was one edit: undo brings the old text back, unsaved against the disk's
        editor.update_in(cx, |e, window, cx| {
            let handle = e.focus_handle(cx);
            window.focus(&handle, cx);
        });
        cx.run_until_parked();
        cx.dispatch_action(gpui_base::input::Undo);
        settle(rt, cx, |cx| text_of(&editor, cx) == "one\ntwo\n");
        editor.read_with(cx, |e, _| assert!(e.is_dirty()));
    }

    #[gpui::test]
    fn a_write_heard_by_the_watch_is_taken_in_and_a_neighbour_s_is_not(cx: &mut gpui::TestAppContext) {
        let rt = &runtime();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let (editor, cx) = editor_on(cx, rt, dir.path(), &path);
        // the watch is made on the blocking pool: a moment for it
        let mut rounds = 0;
        settle(rt, cx, |_| {
            rounds += 1;
            rounds > 20
        });
        let looks = |cx: &mut gpui::VisualTestContext| editor.read_with(cx, |e, _| e.disk_check.is_some());
        assert!(!looks(cx), "nothing looked yet");
        std::fs::write(dir.path().join("b.txt"), "other\n").unwrap();
        let mut rounds = 0;
        settle(rt, cx, |_| {
            rounds += 1;
            rounds > 100
        });
        assert!(!looks(cx), "another file's write is not this one's");
        // renamed over it, as an editor saves
        std::fs::write(dir.path().join("a.tmp"), "two\n").unwrap();
        std::fs::rename(dir.path().join("a.tmp"), &path).unwrap();
        settle(rt, cx, |cx| text_of(&editor, cx) == "two\n");
        editor.read_with(cx, |e, _| assert!(!e.is_dirty()));
    }

    #[gpui::test]
    fn unsaved_changes_ask_and_keep_mine_writes_over_that_version_only(cx: &mut gpui::TestAppContext) {
        let rt = &runtime();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let (editor, cx) = editor_on(cx, &rt, dir.path(), &path);
        editor.update_in(cx, |e, window, cx| {
            let state = e.editor_state().unwrap().clone();
            state.update(cx, |s, cx| s.replace_all("mine\n", window, cx));
        });
        settle(rt, cx, |cx| editor.read_with(cx, |e, _| e.is_dirty()));
        std::fs::write(&path, "theirs\n").unwrap();
        editor.update_in(cx, |e, window, cx| e.check_disk(false, window, cx));
        settle(rt, cx, |cx| editor.read_with(cx, |e, _| e.banner.is_some()));
        editor.read_with(cx, |e, _| assert_eq!(e.banner, Some(Banner::Changed { disk: Seen::of(b"theirs\n"), save: false })));
        assert_eq!(text_of(&editor, cx), "mine\n", "the buffer is left alone");
        // Keep mine: the same version again asks nothing; the save writes over it
        editor.update_in(cx, |e, window, cx| e.keep_mine(window, cx));
        editor.update_in(cx, |e, window, cx| e.check_disk(false, window, cx));
        let mut rounds = 0;
        settle(rt, cx, |_| {
            rounds += 1;
            rounds > 20
        });
        editor.read_with(cx, |e, _| assert_eq!(e.banner, None));
        let saved = editor.update_in(cx, |e, window, cx| e.save(SaveMode::Safe, window, cx)).unwrap();
        settle(rt, cx, |cx| editor.read_with(cx, |e, _| !e.is_saving()));
        assert!(futures::executor::block_on(saved));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");
        // deleted: the buffer stays, and says so
        std::fs::remove_file(&path).unwrap();
        editor.update_in(cx, |e, window, cx| e.check_disk(false, window, cx));
        settle(rt, cx, |cx| editor.read_with(cx, |e, _| e.banner.is_some()));
        editor.read_with(cx, |e, _| assert_eq!(e.banner, Some(Banner::Deleted)));
        assert_eq!(text_of(&editor, cx), "mine\n");
        // back as saved: the banner goes
        std::fs::write(&path, "mine\n").unwrap();
        editor.update_in(cx, |e, window, cx| e.check_disk(false, window, cx));
        settle(rt, cx, |cx| editor.read_with(cx, |e, _| e.banner.is_none()));
    }

    #[test]
    fn numbers_group_by_thousands() {
        assert_eq!(group(200_000), "200,000");
        assert_eq!(group(20_000), "20,000");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000_000), "1,000,000");
    }
}
