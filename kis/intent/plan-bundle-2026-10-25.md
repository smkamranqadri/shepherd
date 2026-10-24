# Plan: bundle 2026-10-25 (backlog 10, 13–17, 19–21)

Approved scope 2026-10-25 ("follow your recommendation, use sonnet instead"). Source: `kis/intent/backlog.md`. Mode: Phase (four builders, two waves, disjoint files, one worktree and branch each).
Status: planned, review fixes applied.

## Decisions (Kamran, 2026-10-25)
- Builders: Claude Code in herdr panes, Sonnet. A medium effort; B, C, D high.
- Two waves: A + B first; C + D branch off `dev` after A is merged (D edits A's files and `src/shell.rs` after it).
- 17: each window becomes a named thin bar, `Claude 5h ▰▱▱▱▱ wk ▰▰▱▱▱`; the percent moves to the popup.
- 10: Command Code gets a usage label like Claude and Codex, if a usage source exists.
- 20: Shepherd detects an agent's live background shells from the process tree; no hook. Sidebar only (not the status bar).
- 21: no notification replacement (gpui's sender has no `replaces_id`) and no repeat window (Kamran: the orchestrator flow that repeated Done has changed). Only the title: it names the session always.

## Out of scope
- 18 Overview page (own cycle), 22 `dan.pane-topic-sync` (third party; local test first), 11, 12.
- Notification replacement or de-duplication beyond today's `tell` (re-check the store after a day without the removed `notify-send` hook).
- macOS process detection for 20 (Linux `/proc` only; macOS keeps today's behavior); SSH machines' panes.
- Any push, package build or install (Kamran runs those).

## File ownership
- A: `src/shell/sidebar.rs` (header), `src/shell/sidebar_focus.rs`, `src/shell/sidebar_tree.rs` (machine row), `src/shell.rs` titlebar cluster (~2080–2110).
- B: `src/ui/markdown.rs`, `src/ui/viewer.rs`, `src/ui/review.rs` (its markdown tag stripping ~865–890), `src/files/content.rs` (read only unless limits must be shared).
- C: `src/ui/statusbar.rs`, `src/settings.rs`, `src/ui/settings.rs`, `src/ui/side_panel/changes.rs`, `src/ui/side_panel/commit.rs`.
- D (off `dev` after A): `src/shell/attention.rs`, `src/shell/sync.rs`, `src/shell/watchers.rs`, `src/herdr/client.rs`, a new process module, the sidebar's status drawing (`src/shell/sidebar.rs` ~328, `src/shell/sidebar_tree.rs` dots, `src/shell/widgets.rs` `status_dot`), `src/shell.rs` attention caller (~1029). No `statusbar.rs`, no settings files.
- A file outside a builder's list is a stop: report, do not edit.

## Setup (every builder)
- Worktree `/home/mkamran/Repositories/tools/shepherd-wt/<name>`, branch `feat/<name>` off `dev` (wave 1 at this plan's commit or later; wave 2 after A's merge); names `sidebar-chip` (A), `markdown` (B), `statusbar-changes` (C), `agent-status` (D). The builder owns it alone.
- Every cargo command exactly as written, run_in_background, output to `.kis-logs/` in the worktree (untracked), read in full:
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo clippy --all-targets -j 3 -- -D warnings`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo test --lib -j 3`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo test --test model -j 3`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo build --bin shepherd -j 3`
- Gate per commit = those four, all green, plus at least one mutation per guard (literal edit, the named test fails, `cmp`-restored).
- Never run rustfmt or `cargo fmt`. Fixtures use fictional names. No push, no merge, no app launch.
- An investigation item (15, 16, 19, 10, 20) whose cause or source differs from this plan stops with a report before code.

## Coordinator verification
- Per builder commit: re-run the four gate commands in its worktree, read each mutation, trial-merge with `git merge-tree`, merge into `dev` one commit at a time.
- After each wave's merges: re-run the gate on merged `dev` in the main checkout, then build the live-look binary into its own target dir so no builder overwrites it: `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target-live flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo build --bin shepherd -j 3`, record its path, mtime and `git rev-parse HEAD`, and run it via `shepherd-dev sandbox` with that binary; screenshots as proof.

## A — sidebar (`sidebar-chip`)
- [ ] 14. Focus chip: today it is in `sidebar_header` (`src/shell/sidebar.rs` 491–517), a row under the titlebar; the ⋯ is in the titlebar at the sidebar's right edge (`src/shell.rs` ~2086, `sidebar-view`). Move the chip into the titlebar beside that ⋯; the sidebar_header row it used is gone (the place's spinner keeps a home). With the dev build's DEV badge in the titlebar, chip, badge and ⋯ do not overlap down to the width where the ⋯ hides (`TITLEBAR_CLUSTER + 32`); below it the chip hides with it or shortens to an icon.
- [ ] 13. Machine row: no hover ⋯ (`sidebar_tree.rs` `tree-machine-menu-*` ~784–868), as the session row; right-click opens the same menu.
- Acceptance: tests that with focus on the titlebar holds the chip at a left edge before the ⋯ (rendered bounds), the sidebar has no chip row (header height as with focus off), and focus off shows no chip; a fit test at the narrowest width with DEV on; machine row: no ⋯ element, the right-click menu's items equal today's, and its dots' x equals a session row's. Gate green.

## B — markdown (`markdown`)
- [ ] 15. Overlap: reproduce with Shepherd's `README.md` in the reader at a narrow and a wide width; find the cause (block height, wrap width, or text layout in zui); fix so each block takes its laid-out height.
- [ ] 16. HTML images. The pinned zeron-markdown keeps inline and block HTML as text (`parser.rs` ~574, ~716) and Shepherd strips its tags before rendering (`src/ui/review.rs` ~865–890, `src/ui/viewer.rs` ~315); the reader never sets `Look.image` (`viewer.rs` ~943), so no image draws, markdown or HTML. Do: in the reader only, turn `<img src alt width>` (also inside `<p>`/`<a>`, as in `~/Repositories/side projects/lore-skill/README.md`) into image runs before stripping; wire `Look.image` through the reader. Local paths resolve against the document (SVG included). Remote `https` images: fetched with the existing `ureq` dependency, off the UI thread, once per URL, cached on disk under Shepherd's cache dir, capped by the viewer's image limits (`src/files/content.rs` 12–19) in bytes and decoded size; alt text until loaded and on failure. `width` caps the drawn width. No fetch for non-https or outside the reader.
- Acceptance: the root cause's own guard test for 15 (fails with the old code); reader-parse tests for `<img>` inline, in a block and nested in `<p><a>`; a markdown `![]()` image renders; a remote image shows alt text, then the image from cache on a second open without a request, alt text on a failed or oversize fetch; `width` caps the width. Gate green. Live: lore-skill's README shows hero.svg, how-it-works.svg and the badges; Shepherd's README has no overlap.

## C — status bar and Changes (`statusbar-changes`), wave 2
- [ ] 17. Usage label: per window a short name and a thin 5-segment bar (`5h ▰▱▱▱▱`, `wk ▰▰▱▱▱`, Codex's 30-day `30d`), filled segments colored by `usage_color`; the bar drops `donut` and the percent (`src/ui/statusbar.rs` ~961); the popup (`limit_row`) is unchanged.
- [ ] 10. Command Code: find its plan-limit source (installed: `~/.local/bin/commandcode`, `~/.commandcode/`); if one exists, add `UsageAgent::CommandCode` with its toggle in Usage settings (default on, an old settings file without it loads with it on), label and popup as the others; if none, report and skip.
- [ ] 19. Stage lag: Stage already asks for an immediate refresh (`commit.rs` ~247, `changes.rs` ~395, ~450), so reproduce the 5–20 s lag first and find the real cause (e.g. a status request already in flight answering with the old tree); fix so the panel shows the new rows right after Stage, Unstage and Discard.
- Acceptance: segment count per percent (0, 1, 20, 50, 99, 100), segment colors at 69/70/90, window names, no percent or ring in the bar, popup rows unchanged; Command Code parse test from a real (redacted) response plus settings default and round-trip, or the skip report; a test that the rows after Stage/Unstage/Discard show the new state with a status request already in flight. Gate green. Live: a Stage shows within 1 s.

## D — agent status and notifications (`agent-status`), wave 2
- [ ] 20. Background shells: for each local (not SSH) agent pane at rest (Idle, or herdr's unseen Done), get its pid (`herdr pane process-info` / API `pane.process_info`; confirm it is reachable from `src/herdr/client.rs`), count its direct children in a process session other than the agent's whose stdout is a `/tmp/claude-*/…/tasks/*.output` file (Linux `/proc`); with one or more, the sidebar shows the pane as working with `N shells` (row and workspace dots). A bounded timer (every few seconds, off the UI thread) runs the scan without waiting for a herdr event, so a shell's end clears the mark with no event. Results are keyed by session, pane and pid, and stale ones dropped. Blocked wins over the mark. Monitor and Codex: investigate; covered if they share the shape, else reported as not covered.
- [ ] 21. Title: a notification's title names the session always (`claude · herdr-orch — side-projects`), the shown session's too (`src/shell.rs` ~1029 passes `None` today; `attention::message`).
- Acceptance: tests on a fake `/proc` tree (MCP child in the agent's session: not counted; task shell in its own session with a tasks output: counted; exited, or a reused pid: not counted; SSH pane: not scanned); mark shown for Idle and Done, not over Blocked; workspace dot aggregates; cleared on a timer tick with no herdr event; title test for shown and other sessions. Gate green. Live: an idle Claude with a background `sleep` reads working with 1 shell and goes idle when it ends.

## Risks and assumptions
- 15's cause is not known yet; the fix may be in zui/gpui text layout, outside `markdown.rs` (a stop: zui is a git dependency).
- 16 remote fetch adds network from the reader; capped, timed out, https only.
- 10 may have no usage endpoint; then it is skipped.
- 20 rests on Claude Code 2.1.300's process shape (research 2026-10-25); a change in it makes the scan a no-op, not a wrong status.
- Builders share one cargo lock and target dir: builds queue; the live look uses `target-live`.

## Agents
- Builders A–D: Claude Code (herdr pane each), Sonnet; A medium, B/C/D high.
- Plan review: Codex CLI 0.162.1, gpt-6.1-sol, high, read-only (herdr tab w3:tE).

## Review
- Reviewed: 2026-10-25 by codex gpt-6.1-sol (high); plan hash 1984c9e70c14
- [fixed] File ownership incomplete (A/D both need sidebar.rs and shell.rs, C/D statusbar.rs) - ownership section added; D branches after A, drops the status bar; C owns the settings files
- [fixed] Linux notifications cannot pass replaces_id through gpui's sender - replacement dropped by Kamran (skip repeats only), then repeat window dropped too (flow changed); 21 is the title only
- [fixed] Shared target dir lets the live look run a builder's binary; no merged-tree gate - coordinator re-runs the gate on merged dev and builds the live binary into target-live
- [fixed] Focus chip is already in sidebar_header, the ⋯ is in the titlebar (shell.rs) - 14 moves it into the titlebar with rendered-bounds, row-gone and fit checks
- [fixed] 16 misses the HTML-loss path (parser keeps HTML as text, review.rs strips tags) - reader-only img conversion before stripping, nested p/a test
- [fixed] Reader never sets Look.image - wiring it added, markdown images tested too; reuse ureq
- [fixed] Image acceptance checked only the placeholder; no decoded-size limits - cache, failure, oversize, width checks; content.rs limits reused
- [fixed] Stage already refreshes at once, a call-presence test would pass with the lag - reproduce first, test rows with a request in flight
- [fixed] Sync loop has no periodic wakeup - bounded timer independent of herdr events, test clearing with no event
- [fixed] SSH panes' pids are not local - local sessions only, keyed by session/pane/pid, stale dropped, tests for remote and pid reuse
- [fixed] 20 covered only Claude's shape and only Idle - Idle and unseen Done, Blocked wins, Monitor/Codex as investigation outcomes
- [fixed] 21 window undefined, no stored body, shown-session caller passes None - window dropped; title from the shown session's caller tested
- [fixed] Acceptance gaps (machine-dot alignment, usage colors/percent/popup, Command Code default-on) - added
