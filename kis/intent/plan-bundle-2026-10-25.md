# Plan: bundle 2026-10-25 (backlog 10, 13–17, 19–21)

Approved scope 2026-10-25 ("follow your recommendation, use sonnet instead"). Source: `kis/intent/backlog.md`. Mode: Phase (four builders, two waves, disjoint files, one worktree and branch each).
Status: planned, not reviewed.

## Decisions (Kamran, 2026-10-25)
- Builders: Claude Code in herdr panes, Sonnet. A medium effort; B, C, D high.
- Two waves: A + B first, then C + D. D merges after A (its status mark is drawn in A's sidebar files).
- C starts only after Kamran's live check of the hover fix (9209847) in r412, since 19 is in the same Changes panel.
- 17: each window becomes a named thin bar, `Claude 5h ▰▱▱▱▱ wk ▰▰▱▱▱`; the percent moves to the popup.
- 10: Command Code gets a usage label like Claude and Codex, if a usage source exists.
- 20: Shepherd detects an idle agent's live background shells from the process tree; no hook.
- 21 (Shepherd side): a pane's new notification replaces its previous one; a repeat Done with the same body inside the window is skipped; the title is the same whichever session is shown. No "Waiting only" setting.

## Out of scope
- 18 Overview page (own cycle), 22 `dan.pane-topic-sync` (third party; local test first), 11, 12.
- The `notify-send` hook in `~/.claude/settings.json` (Kamran removes it).
- macOS process detection for 20 (Linux `/proc` only; macOS keeps today's behavior).
- Any push, package build or install (Kamran runs those).

## Setup (every builder)
- Worktree `/home/mkamran/Repositories/tools/shepherd-wt/<name>`, branch `feat/<name>` off `dev` at the plan's commit or later; names `sidebar-chip` (A), `markdown` (B), `statusbar-changes` (C), `agent-status` (D). The builder owns it alone.
- Every cargo command exactly as written, run_in_background, output to `.kis-logs/` in the worktree (untracked), read in full:
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo clippy --all-targets -j 3 -- -D warnings`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo test --lib -j 3`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo test --test model -j 3`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo build --bin shepherd -j 3`
- Gate per commit = those four, all green, plus at least one mutation per guard (literal edit, the named test fails, `cmp`-restored).
- Never run rustfmt or `cargo fmt`. Fixtures use fictional names. No push, no merge, no app launch: the coordinator re-runs the gate, trial-merges with `git merge-tree`, merges into `dev` one commit at a time and does the live look in `shepherd-dev sandbox`.
- An investigation item (15, 16 remote, 19, 10, 20) whose cause or source differs from this plan stops with a report before code.

## A — sidebar (`sidebar-chip`)
- [ ] 14. Focus chip: drawn in the sidebar header row beside the ⋯ (`src/shell/sidebar.rs` ~490, `sidebar_focus.rs`), not on a row of its own; the row it used is gone. With the dev build's DEV badge in the same header, both fit at the sidebar's narrowest width without overlap (chip text may shorten to an icon).
- [ ] 13. Machine row: no hover ⋯ (`sidebar_tree.rs` `tree-machine-menu-*`, ~787), as the session row; right-click opens the same menu. The machine row's status dots line up with the session rows'.
- Acceptance: tests for chip placement (focus on: header holds the chip, no extra row; off: no chip) and the machine row (no ⋯ element, right-click menu items unchanged). Gate green.

## B — markdown (`markdown`)
- [ ] 15. Overlap: find why blocks draw over each other in the reader (`src/ui/markdown.rs`, `src/ui/viewer.rs`); fix so each block takes its laid-out height. Repro: Shepherd's `README.md` in the reader at a narrow and a wide panel width.
- [ ] 16. HTML images: `<img src=… alt=… width=…>` in markdown (inline and in an HTML block, as in `~/Repositories/side projects/lore-skill/README.md`) draws like `![]()`: local paths resolve against the document (SVG included); remote `https` images are fetched once in the background, cached on disk, size-capped, and show their alt text until loaded or on failure. `width` caps the drawn width. Nothing fetched for a document outside the reader.
- Acceptance: a test where a long paragraph and a list's laid-out heights match their measured heights (or the root cause's own guard); parser tests for `<img>` in both positions; a test that a remote image shows alt text before it loads. Gate green. Live: lore-skill's README shows hero.svg, how-it-works.svg and the badges; Shepherd's README has no overlap.

## C — status bar and Changes (`statusbar-changes`), after Kamran's hover check
- [ ] 17. Usage label: per window a short name and a thin 5-segment bar (`5h ▰▱▱▱▱`, `wk ▰▰▱▱▱`, Codex's 30-day `30d`), segment color by `usage_color`; the bar drops `donut` and the percent (`src/ui/statusbar.rs` ~961); the popup (`limit_row`) keeps the ring and gains nothing.
- [ ] 10. Command Code: find its plan-limit source (it is installed: `~/.local/bin/commandcode`, `~/.commandcode/`); if one exists, add `UsageAgent::CommandCode` with its toggle in Usage settings (default on), label and popup as the others; if none, report and skip.
- [ ] 19. Stage lag: find why the Changes panel shows a Stage's result 5–20 s late while git is updated at once; fix so the panel refreshes right after its own Stage/Unstage/Discard.
- Acceptance: tests for the bar's segment count per percent (0, 1, 20, 50, 99, 100) and window names; Command Code parse test from a real (redacted) response, or the skip report; a test that a Stage triggers a refresh at once. Gate green. Live: bar reads at a glance; a Stage shows within 1 s.

## D — agent status and notifications (`agent-status`), merges after A
- [ ] 20. Background shells: for each idle agent pane, get its pid (`herdr pane process-info` / API `pane.process_info`), count its direct children in a process session other than the agent's whose stdout is a `/tmp/claude-*/…/tasks/*.output` file (Linux `/proc`); with one or more, the sidebar row and the status bar read working with `N shells`. Polled off the UI thread, no more than every few seconds. Linux only; a clean no-op elsewhere.
- [ ] 21. Notifications: a pane's new notification replaces its previous one (freedesktop `replaces_id`); a Done with the same pane and body as the last one inside the window is skipped even when the agent worked in between; the title names the session always (`claude · herdr-orch — side-projects`). Change `src/shell/attention.rs` `tell`/`message` and the sender.
- Acceptance: a test on a fake `/proc` tree (MCP child in the agent's session: not counted; task shell in its own session with a tasks output: counted; exited: not counted); tests for replace and skip in `Attention` and the stable title. Gate green. Live: an idle Claude with a background `sleep` reads working with 1 shell and goes idle when it ends; an orchestrator woken several times leaves one notification.

## Risks and assumptions
- 15's cause is not known yet; the fix may be in zui/gpui text layout, outside `markdown.rs`.
- 16 remote fetch adds network from the reader; capped size, timeout, no fetch for non-https.
- 10 may have no usage endpoint; then it is skipped.
- 20 rests on Claude Code 2.1.300's process shape (research 2026-10-25); Monitor, `cmd &` and Codex not verified; a change in Claude's layout makes it a no-op, not a wrong status.
- Three or four builders share one cargo lock: builds queue; wall time grows, CPU stays bounded.

## Verification method
- Coordinator re-runs each builder's gate in its worktree, reads each mutation, trial-merges, merges one at a time, then one live look per wave in `shepherd-dev sandbox` with screenshots.

## Agents
- Builders A–D: Claude Code (herdr pane each), Sonnet; A medium, B/C/D high.
- Plan review: a different CLI (proposed: Codex, read-only).
