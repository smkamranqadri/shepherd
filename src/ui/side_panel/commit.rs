//! The Changes tab's commit box and the git it runs: a message over a Commit button (its ▾ offers Amend last
//! commit), the rows' stage, unstage and discard, and the headers' Stage All and Unstage All, whole files only
//! (`files::git_ops` builds and runs each). One runs at a time; each ends with git status asked again at once, which
//! refreshes the list, the shown diff and the gutters. A message is kept per root while Shepherd runs and cleared
//! by its commit; a merge under way starts it from `MERGE_MSG`. A commit has no cancel: killing git mid-commit would
//! leave `index.lock` behind.
use super::SidePanel;
use crate::files::git_ops::{self, Failure, Kind, Op};
use crate::files::status::{Change, Group};
use crate::shell::widgets::{menu_card, menu_choice, tip};
use crate::theme::Theme;
use gpui::{
    AnimationExt as _, AnyElement, ClickEvent, Context, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, PromptLevel, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_base::input::{Enter, InputEvent, Textarea, TextareaState};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const PLACEHOLDER: &str = "Message (⌘Enter to commit)";
#[cfg(not(target_os = "macos"))]
const PLACEHOLDER: &str = "Message (Ctrl+Enter to commit)";

/// What is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Commit,
    Other,
}

/// A failure under the button, for the root it happened in.
struct Shown {
    root: PathBuf,
    title: String,
    text: String,
    /// Kept when git was busy (`index.lock`): Retry runs it again.
    retry: Option<Op>,
}

/// The commit box's state, kept beside the Changes list.
#[derive(Default)]
pub(super) struct CommitBox {
    input: Option<Entity<TextareaState>>,
    _events: Vec<Subscription>,
    /// Each root's message while another root shows; none kept empty.
    drafts: Kept<String>,
    /// The root whose message the input holds.
    shown_for: Option<PathBuf>,
    /// Amending the last commit: the message that was in the box before, put back by Stop amending.
    amend: Option<String>,
    busy: Option<Busy>,
    error: Option<Shown>,
    /// The roots whose box was started from their merge's `MERGE_MSG`, so clearing it does not fill it again.
    merge_filled: Kept<()>,
    menu: Option<crate::ui::Popup<Point<Pixels>>>,
}

/// How many roots' drafts (and merge fills) are kept: the latest, a workspace's worth of repositories.
const MEMORY: usize = 32;

/// A value per root, the most recently put last, at most `MEMORY` of them (the oldest goes first).
struct Kept<T>(Vec<(PathBuf, T)>);

impl<T> Default for Kept<T> {
    fn default() -> Self {
        Kept(Vec::new())
    }
}

impl<T> Kept<T> {
    fn get(&self, root: &Path) -> Option<&T> {
        self.0.iter().find(|(r, _)| r == root).map(|(_, v)| v)
    }

    fn take(&mut self, root: &Path) -> Option<T> {
        let i = self.0.iter().position(|(r, _)| r == root)?;
        Some(self.0.remove(i).1)
    }

    fn put(&mut self, root: PathBuf, value: T) {
        self.take(&root);
        self.0.push((root, value));
        if self.0.len() > MEMORY {
            self.0.remove(0);
        }
    }
}

impl CommitBox {
    /// The shown root's box is left for another: its message is kept unless empty (an amend's is the one from
    /// before it, as Stop amending would give back).
    fn leave(&mut self, old: PathBuf, typed: String) {
        let kept = self.amend.take().unwrap_or(typed);
        if !kept.trim().is_empty() {
            self.drafts.put(old, kept);
        }
    }

    /// `committed` landed in `root`'s repository, `now` the shown box's text: whether the box empties. Only the root
    /// it was committed from leaves amending, and its text goes only if it is still what was committed (what was
    /// typed meanwhile stays); a root no longer shown loses its kept draft on the same terms.
    fn committed(&mut self, root: &Path, committed: &str, now: &str) -> bool {
        if self.shown_for.as_deref() == Some(root) {
            self.amend = None;
            return now == committed;
        }
        if self.drafts.get(root).is_some_and(|d| d == committed) {
            self.drafts.take(root);
        }
        false
    }
}

/// Why Commit is disabled, or `None` when it is not: a run under way, a conflict in the list, no message, or
/// nothing staged (unless amending, which may only reword, or changes outside the root, where staged ones may be).
pub(super) fn commit_blocked(message: &str, staged: usize, outside_root: usize, conflicts: usize, amend: bool, busy: bool) -> Option<&'static str> {
    if busy {
        Some("Git is running")
    } else if conflicts > 0 {
        Some("Resolve conflicts first")
    } else if message.trim().is_empty() {
        Some("Type a commit message")
    } else if staged == 0 && outside_root == 0 && !amend {
        Some("Stage changes to commit them")
    } else {
        None
    }
}

/// A 14 px spinner turning once every 900 ms.
fn spinner(theme: &Theme, id: &'static str) -> impl IntoElement {
    gpui::svg()
        .path("icons/spinner.svg")
        .size(px(14.))
        .flex_none()
        .text_color(theme.text_faint)
        .with_animation(id, gpui::Animation::new(std::time::Duration::from_millis(900)).repeat(), |svg, t| {
            svg.with_transformation(gpui::Transformation::rotate(gpui::percentage(t)))
        })
}

/// A row's or a header's 20 px action: a muted icon, a wash on hover, dimmed while git runs.
pub(super) fn action_button(theme: &Theme, id: impl Into<gpui::ElementId>, icon: &'static str, label: impl Into<SharedString>, busy: bool) -> gpui::Stateful<gpui::Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .size(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .when(busy, |el| el.opacity(0.4))
        .when(!busy, |el| el.cursor_pointer().hover(move |s| s.bg(hover)))
        .child(gpui::svg().path(icon).size(px(14.)).text_color(theme.text_muted))
        .tooltip(tip(label.into(), None))
}

impl SidePanel {
    /// The message input, made once.
    fn commit_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextareaState> {
        if let Some(input) = &self.commit.input {
            return input.clone();
        }
        let theme = Theme::of(cx).clone();
        let input = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx).auto_grow(2, 8).placeholder(PLACEHOLDER);
            s.set_editor_style(crate::ui::input_style(&theme));
            s
        });
        // Commit's enabled state follows the message
        let typed = cx.subscribe_in(&input, window, |_: &mut SidePanel, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        });
        self.commit._events = vec![typed];
        self.commit.input = Some(input.clone());
        input
    }

    fn commit_message(&self, cx: &gpui::App) -> String {
        self.commit.input.as_ref().map(|i| i.read(cx).value().to_string()).unwrap_or_default()
    }

    fn set_commit_message(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.commit_input(window, cx);
        input.update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
    }

    /// Keeps the input on the shown root's message, and starts a merge's from its `MERGE_MSG`.
    fn sync_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.changes_root() else { return };
        if self.commit.shown_for.as_ref() != Some(&root) {
            if let Some(old) = self.commit.shown_for.take() {
                let typed = self.commit_message(cx);
                self.commit.leave(old, typed);
            }
            self.commit.amend = None;
            let back = self.commit.drafts.take(&root).unwrap_or_default();
            self.set_commit_message(&back, window, cx);
            self.commit.shown_for = Some(root.clone());
        }
        let Some(repo) = self.changes_answer().map(|a| a.repo.clone()) else { return };
        if !repo.merging && repo.picking.is_none() {
            self.commit.merge_filled.take(&root);
        } else if let Some(msg) = repo.merge_msg.filter(|_| self.commit.amend.is_none() && self.commit.merge_filled.get(&root).is_none()) {
            self.commit.merge_filled.put(root, ());
            if self.commit_message(cx).trim().is_empty() {
                self.set_commit_message(&msg, window, cx);
            }
        }
    }

    /// Runs `op` in the shown root's repository unless one is running; the list asked again after it.
    pub(super) fn run_git_op(&mut self, op: Op, window: &mut Window, cx: &mut Context<Self>) {
        if self.commit.busy.is_some() {
            return;
        }
        let Some(root) = self.changes_root() else { return };
        self.commit.busy = Some(if matches!(op, Op::Commit { .. }) { Busy::Commit } else { Busy::Other });
        self.commit.error = None;
        let task = self.rt.spawn(git_ops::run(root.clone(), op.clone()));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| Err(Failure { kind: Kind::Other, text: e.to_string() }));
            let _ = this.update_in(cx, |p, window, cx| p.land_git_op(root, op, result, window, cx));
        })
        .detach();
    }

    fn land_git_op(&mut self, root: PathBuf, op: Op, result: Result<(), Failure>, window: &mut Window, cx: &mut Context<Self>) {
        self.commit.busy = None;
        match result {
            Ok(()) if let Op::Commit { message, .. } = &op => {
                let now = self.commit_message(cx);
                if self.commit.committed(&root, message, &now) {
                    self.set_commit_message("", window, cx);
                }
            }
            Ok(()) => {}
            Err(failure) => {
                let retry = (failure.kind == Kind::IndexLocked).then(|| op.clone());
                self.commit.error = Some(Shown { root, title: failure.title(&op), text: failure.text, retry });
            }
        }
        self.refresh_changes_now(cx);
        cx.notify();
    }

    /// Commit (or Amend), Ctrl+Enter's too, when `commit_blocked` lets it.
    fn try_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.commit_message(cx);
        let Some(answer) = self.changes_answer() else { return };
        let s = &answer.status;
        let count = |g| s.changes.iter().filter(|c| c.group == g).count();
        let amend = self.commit.amend.is_some();
        if commit_blocked(&message, count(Group::Staged), s.outside_root, count(Group::Conflicts), amend, self.commit.busy.is_some()).is_none() {
            self.run_git_op(Op::Commit { message, amend }, window, cx);
        }
    }

    /// Amend last commit: the box takes the last commit's message to edit; what was typed comes back on Stop amending.
    fn start_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.changes_root() else { return };
        if self.commit.amend.is_some() || self.changes_answer().is_none_or(|a| a.repo.initial) {
            return;
        }
        let task = self.rt.spawn(git_ops::last_message(root.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|e| Err(Failure { kind: Kind::Other, text: e.to_string() }));
            let _ = this.update_in(cx, |p, window, cx| {
                if p.commit.shown_for.as_ref() != Some(&root) || p.commit.amend.is_some() {
                    return;
                }
                match result {
                    Ok(message) => {
                        p.commit.amend = Some(p.commit_message(cx));
                        p.set_commit_message(&message, window, cx);
                        if let Some(input) = p.commit.input.clone() {
                            input.update(cx, |s, cx| s.focus(window, cx));
                        }
                    }
                    Err(f) => p.commit.error = Some(Shown { root, title: "Couldn't read the last commit".into(), text: f.text, retry: None }),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stop_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(before) = self.commit.amend.take() {
            self.set_commit_message(&before, window, cx);
            cx.notify();
        }
    }

    /// A row's discard: asked first, always, as it cannot be undone (`git_ops::confirm_discard`).
    pub(super) fn ask_discard(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        if self.git_busy() {
            return;
        }
        let (Some(path), Some(root)) = (self.change_path(&change), self.changes_root()) else { return };
        let rel = path.strip_prefix(&root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| change.path.clone());
        let unsaved = self.unsaved.as_ref().is_some_and(|buffers| buffers(cx).iter().any(|(p, _)| *p == path));
        let Ok(confirm) = git_ops::confirm_discard(&change, &rel, unsaved) else { return };
        let answer = window.prompt(PromptLevel::Warning, &confirm.message, Some(&confirm.detail), &git_ops::DISCARD_ANSWERS, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(answer) = answer.await else { return };
            if let Some(op) = confirm.answered(answer) {
                let _ = this.update_in(cx, |p, window, cx| p.run_git_op(op, window, cx));
            }
        })
        .detach();
    }

    /// A conflict's Mark resolved: staged, after a yes when its file still has conflict markers.
    pub(super) fn mark_resolved(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        if self.git_busy() {
            return;
        }
        let Some(path) = self.change_path(&change).filter(|_| change.openable()) else {
            self.run_git_op(Op::Stage(change), window, cx);
            return;
        };
        let read = self.rt.spawn(tokio::fs::read(path));
        cx.spawn_in(window, async move |this, cx| {
            let marked = matches!(read.await, Ok(Ok(bytes)) if git_ops::has_conflict_markers(&String::from_utf8_lossy(&bytes)));
            let Ok(answer) = this.update_in(cx, |_, window, cx| {
                marked.then(|| {
                    let message = "This file still has conflict markers. Mark it resolved anyway?";
                    window.prompt(PromptLevel::Warning, message, Some("Its markers would be committed as they are."), &["Mark Resolved", "Cancel"], cx)
                })
            }) else {
                return;
            };
            if let Some(answer) = answer
                && answer.await != Ok(0)
            {
                return;
            }
            let _ = this.update_in(cx, |p, window, cx| p.run_git_op(Op::Stage(change), window, cx));
        })
        .detach();
    }

    /// A stage, discard or commit is running: the rows' and headers' buttons are dimmed and do nothing.
    pub(super) fn git_busy(&self) -> bool {
        self.commit.busy.is_some()
    }

    /// The box over the groups: the message, a rebase's warning, the amend note, Commit ▾ and the last failure.
    pub(super) fn render_commit(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        crate::ui::reap_popup(&mut self.commit.menu, window);
        self.sync_commit(window, cx);
        let input = self.commit_input(window, cx);
        let answer = self.changes_answer()?;
        let s = &answer.status;
        let count = |g| s.changes.iter().filter(|c| c.group == g).count();
        let (staged, conflicts, outside, repo) = (count(Group::Staged), count(Group::Conflicts), s.outside_root, answer.repo.clone());
        let amend = self.commit.amend.is_some();
        let busy = self.commit.busy;
        let message = self.commit_message(cx);
        let blocked = commit_blocked(&message, staged, outside, conflicts, amend, busy.is_some());
        let field = div()
            .id("commit-message")
            .mx(px(12.))
            .px(px(8.))
            .py(px(5.))
            .rounded(px(6.))
            .bg(theme.wash(0.06))
            .text_size(px(12.5))
            .capture_action(cx.listener(|p, e: &Enter, window, cx| {
                // Ctrl+Enter (Cmd+Enter) commits rather than add a line
                if e.secondary {
                    cx.stop_propagation();
                    p.try_commit(window, cx);
                }
            }))
            .child(Textarea::new(&input));
        let note = |text: String, color| div().mx(px(12.)).text_size(px(11.5)).text_color(color).child(text);
        let rebase = repo.rebasing.then(|| note("A rebase is under way: this commit goes into it. Continue the rebase in a terminal.".into(), theme.warning));
        let picking = repo.picking.map(|what| note(format!("A {what} stopped on a conflict: this commit concludes it."), theme.warning));
        let amending = amend.then(|| {
            div()
                .mx(px(12.))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(11.5))
                .child(div().flex_1().min_w_0().truncate().text_color(theme.text_muted).child("Amending the last commit"))
                .child(
                    div()
                        .id("commit-stop-amend")
                        .flex_none()
                        .text_color(theme.accent)
                        .cursor_pointer()
                        .child("Stop amending")
                        .on_click(cx.listener(|p, _: &ClickEvent, window, cx| p.stop_amend(window, cx))),
                )
        });
        let label = match (busy, amend) {
            (Some(Busy::Commit), _) => "Committing…",
            (_, true) => "Amend",
            (_, false) => "Commit",
        };
        let enabled = blocked.is_none();
        let (fg, bg) = (theme.surface, theme.accent);
        let hover = theme.accent.opacity(0.85);
        let why = blocked.map(str::to_string).unwrap_or_else(|| {
            if outside > 0 { "Commits everything staged in the repository, outside this folder too".to_string() } else { "Commit what is staged".to_string() }
        });
        let button = div()
            .id("commit-button")
            .flex_1()
            .min_w_0()
            .h(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .rounded_l(px(6.))
            .when(repo.initial, |el| el.rounded_r(px(6.)))
            .bg(bg)
            .text_color(fg)
            .text_size(px(12.5))
            .font_weight(gpui::FontWeight::MEDIUM)
            .when(!enabled, |el| el.opacity(0.5))
            .when(enabled, |el| el.cursor_pointer().hover(move |s| s.bg(hover)))
            .when(busy == Some(Busy::Commit), |el| el.child(spinner(theme, "commit-spinner")))
            .child(label)
            .tooltip(tip(why, None))
            .on_click(cx.listener(|p, _: &ClickEvent, window, cx| p.try_commit(window, cx)));
        // no ▾ before the first commit: there is nothing to amend
        let more = (!repo.initial).then(|| {
            div()
                .id("commit-more")
                .w(px(26.))
                .h(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_r(px(6.))
                .border_l_1()
                .border_color(theme.surface.opacity(0.25))
                .bg(bg)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(gpui::svg().path("icons/alt-arrow-down.svg").size(px(12.)).text_color(fg))
                .tooltip(tip("More commit actions", None))
                .on_mouse_down(MouseButton::Left, cx.listener(|p, e: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    p.commit.menu = Some(crate::ui::Popup::new(e.position));
                    cx.notify();
                }))
        });
        let buttons = div().mx(px(12.)).flex().child(button).children(more);
        let error = self.commit.error.as_ref().filter(|e| Some(&e.root) == self.commit.shown_for.as_ref()).map(|e| self.commit_error(theme, e, cx));
        let menu = self.commit_menu(theme, amend, cx);
        Some(
            div()
                .flex_none()
                .pb(px(8.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(field)
                .children(rebase)
                .children(picking)
                .children(amending)
                .child(buttons)
                .children(error)
                .children(menu)
                .into_any_element(),
        )
    }

    /// The last failure: git's words, Copy, Dismiss, and Retry when git was busy.
    fn commit_error(&self, theme: &Theme, shown: &Shown, cx: &mut Context<Self>) -> AnyElement {
        let copied = format!("{}\n{}", shown.title, shown.text);
        let retry = shown.retry.clone();
        let link = |id: &'static str, label: &'static str| div().id(id).text_color(theme.text_muted).cursor_pointer().hover(|s| s.underline()).child(label);
        div()
            .mx(px(12.))
            .p(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme.danger.opacity(0.5))
            .bg(theme.danger.opacity(0.08))
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(px(11.5))
            .child(div().text_color(theme.danger).font_weight(gpui::FontWeight::MEDIUM).child(shown.title.clone()))
            .when(!shown.text.is_empty(), |el| el.child(div().font_family(theme.font_terminal.clone()).text_color(theme.text_muted).child(shown.text.clone())))
            .child(
                div()
                    .pt(px(2.))
                    .flex()
                    .gap(px(12.))
                    .children(retry.map(|op| {
                        link("commit-error-retry", "Retry").on_click(cx.listener(move |p, _: &ClickEvent, window, cx| p.run_git_op(op.clone(), window, cx)))
                    }))
                    .child(link("commit-error-copy", "Copy").on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone()));
                    })))
                    .child(link("commit-error-dismiss", "Dismiss").on_click(cx.listener(|p, _: &ClickEvent, _, cx| {
                        p.commit.error = None;
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }

    /// Commit ▾'s menu, while it is up.
    fn commit_menu(&self, theme: &Theme, amend: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.commit.menu.as_ref()?;
        let close = |p: &mut SidePanel| {
            if let Some(m) = p.commit.menu.as_mut() {
                m.close()
            }
        };
        let card = menu_card(theme, "commit-menu", 200.)
            .on_mouse_down_out(cx.listener(move |p, _, _, cx| {
                close(p);
                cx.notify();
            }))
            .child(menu_choice(theme, "commit-menu-amend", "icons/solar/pen.svg", "Amend last commit", amend).on_click(cx.listener(move |p, _: &ClickEvent, window, cx| {
                close(p);
                if amend {
                    p.stop_amend(window, cx);
                } else {
                    p.start_amend(window, cx);
                }
                cx.notify();
            })));
        let anchored = gpui::anchored().position(menu.value).snap_to_window_with_margin(px(8.)).child(crate::ui::menu_motion("commit-menu-in", menu, card));
        Some(gpui::deferred(anchored).with_priority(1).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_waits_for_a_message_something_staged_and_no_conflicts() {
        assert_eq!(commit_blocked("Fix it", 1, 0, 0, false, false), None);
        assert_eq!(commit_blocked("  \n ", 1, 0, 0, false, false), Some("Type a commit message"));
        assert_eq!(commit_blocked("Fix it", 0, 0, 0, false, false), Some("Stage changes to commit them"));
        // amending may only reword; staged changes outside the root may be what is committed
        assert_eq!(commit_blocked("Fix it", 0, 0, 0, true, false), None);
        assert_eq!(commit_blocked("Fix it", 0, 3, 0, false, false), None);
        assert_eq!(commit_blocked("Fix it", 2, 0, 1, false, false), Some("Resolve conflicts first"));
        assert_eq!(commit_blocked("Fix it", 2, 0, 1, true, false), Some("Resolve conflicts first"));
        assert_eq!(commit_blocked("Fix it", 1, 0, 0, false, true), Some("Git is running"));
    }

    fn shown(root: &str) -> CommitBox {
        CommitBox { shown_for: Some(PathBuf::from(root)), ..Default::default() }
    }

    /// A commit empties only its own root's box, only of what it committed, and ends only that root's amend.
    #[test]
    fn a_commit_clears_what_it_committed_and_only_in_its_own_root() {
        let mut b = shown("/tmp/fictional-a");
        b.amend = Some("before".into());
        assert!(b.committed(Path::new("/tmp/fictional-a"), "Fix it", "Fix it"));
        assert_eq!(b.amend, None);
        // typed while the commit ran: kept
        assert!(!b.committed(Path::new("/tmp/fictional-a"), "Fix it", "Fix it, and more"));
        // committed in a root no longer shown: the shown root's amend stays, the other's kept draft goes if it was that
        let mut b = shown("/tmp/fictional-b");
        b.amend = Some("before".into());
        b.drafts.put("/tmp/fictional-a".into(), "Fix it".into());
        b.drafts.put("/tmp/fictional-c".into(), "Other".into());
        assert!(!b.committed(Path::new("/tmp/fictional-a"), "Fix it", "Amended text"));
        assert_eq!(b.amend.as_deref(), Some("before"));
        assert_eq!(b.drafts.get(Path::new("/tmp/fictional-a")), None);
        assert!(!b.committed(Path::new("/tmp/fictional-c"), "Fix it", ""));
        assert_eq!(b.drafts.get(Path::new("/tmp/fictional-c")).map(String::as_str), Some("Other"));
    }

    /// An empty message is not kept as a draft, an amend keeps the message from before it, and only `MEMORY` roots are
    /// remembered, the oldest dropped first.
    #[test]
    fn drafts_skip_empty_messages_and_stay_bounded() {
        let mut b = CommitBox::default();
        b.leave("/tmp/fictional-a".into(), "  \n".into());
        assert_eq!(b.drafts.0.len(), 0);
        b.amend = Some("before amending".into());
        b.leave("/tmp/fictional-a".into(), "the last commit's message".into());
        assert_eq!(b.drafts.get(Path::new("/tmp/fictional-a")).map(String::as_str), Some("before amending"));
        for n in 0..MEMORY + 5 {
            b.leave(format!("/tmp/fictional-{n}").into(), format!("draft {n}"));
        }
        assert_eq!(b.drafts.0.len(), MEMORY);
        assert_eq!(b.drafts.get(Path::new("/tmp/fictional-a")), None);
        assert_eq!(b.drafts.get(Path::new("/tmp/fictional-4")), None);
        assert_eq!(b.drafts.get(Path::new("/tmp/fictional-5")).map(String::as_str), Some("draft 5"));
        // put again: moved to the newest, not doubled
        b.drafts.put("/tmp/fictional-5".into(), "again".into());
        assert_eq!(b.drafts.0.len(), MEMORY);
        assert_eq!(b.drafts.0.last().map(|(_, v)| v.as_str()), Some("again"));
    }
}
