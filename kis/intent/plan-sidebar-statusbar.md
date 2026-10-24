# Plan: sidebar cleanup, focus mode, status bar limits (backlog 1–9)

Approved scope 2026-10-24. Source: `kis/intent/backlog.md` items 1–9. Mode: Phase (one builder, four phases, one commit each).
Status: done 2026-10-24; merged into dev as cbd3661, 3284d7b, 46fe9fa, b65eb3f.

## Decisions (Kamran, 2026-10-24)
- Delete the code of 1–4, not hide it: all of it is fork-only (absent from upstream v1.3.5, `origin/main` 5abcb57), so deleting cuts merge conflicts.
- Search removal is the left sidebar's only; the right panel's Search tab (`src/ui/side_panel/search.rs`) stays.
- NextNeedsYou goes everywhere, herdr's `prefix+o` (`open_notification_target`) included: in Shepherd that chord does nothing.
- Focus mode keeps its name (fork-only since 597dc5c, no upstream conflict). It lists every session with its header, each with only the workspaces that have an agent in any status; a session with none is hidden.
- Sessions bar on: the tree still shows the shown session alone; focus on as well filters that session to workspaces with an agent.
- No tree reordering (workspaces, worktrees, sessions) while focus mode is on.
- Status bar: one label per agent, a ring and percent per window: `Claude ◔ 12% ◑ 40%   Codex ◑ 55% ◕ 71%`.
- Usage settings: a toggle switch per agent (Claude, Codex; both on) replaces the one-agent choice (`usage_agent`).

## Out of scope
- Backlog 10–12 (Command Code usage, drag look port, `[[keys.command]]`, lone-prefix flush, Agents picker).
- The right panel's Search tab; upstream's `reorder.rs`; the Sessions bar's own behavior beyond the focus combination above.
- Any push, package build or install (Kamran runs those).

## Setup
- Worktree via `herdr worktree create` (branch `feat/sidebar-statusbar` off `dev` at ef63532 or later); the builder owns it alone.
- Every cargo command is run exactly as written below, with run_in_background, output to a log file in the worktree's `.kis-logs/` (untracked) read in full:
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo clippy --all-targets -j 3 -- -D warnings`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo test --lib -j 3`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo test --test model -j 3`
  - `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo build --bin shepherd -j 3` (the binary used for the live look; report its path and mtime)
- Gate per phase = those four commands, all green.
- Never run rustfmt or `cargo fmt`: the repo has no rustfmt config and rustfmt rewrites untouched files (63 hunks on `src/ui/statusbar.rs` alone, checked 2026-10-24). Match the surrounding style by hand; `git diff` must show only intended lines.
- Builder commits each phase on the branch with the session's trailer; the coordinator re-runs the gate, trial-merges (`git merge-tree`) and merges into `dev` one phase at a time. No push.

## Phase 1 — removals (items 1–4)
- [x] 1. Left sidebar search: delete the header magnifier, the search bar, `FilterSidebar` (action, binding, command-panel entry, icon map) and the search half of `src/shell/sidebar_search.rs`. Keep focus mode (`toggle_focus_mode`, `ToggleFocusMode`, header menu and Focus chip); if the file becomes focus-only, rename it for that. Drag, fold display and Workspace1–9 numbering lose their "while searching" branches.
- [x] 2. Agents view: delete `SidebarView`, `sidebar_view`, `ToggleAgentsView`, the Workspaces | Agents toggle, `agents_view_rows`/`agents_view_scroll`, the agents branch of `sidebar_layout`, the Agents menu (`agents_menu`, its sort/compact items, its `popup_open` entry in `src/shell/hints.rs`) and every consumer only it had (`AgentSort` and attention helpers only if nothing else reads them), plus the General "Show agents" toggle and the `show_agents` field.
- [x] 3. Tabs in the sidebar: delete `tabs_in_sidebar` (field, Settings toggle, `tabs_in_sidebar_shown` and its draw path in `src/shell/tab_bar.rs`).
- [x] 4. Next agent that needs you: delete `src/shell/needs_you.rs`, `NextNeedsYou` (action, chords `ctrl-shift-j`/`cmd-shift-j`, command-panel entry and icon), `needs_you_last`. In `src/herdr/keymap.rs`, keep `open_notification_target` known to Shepherd's table (so `actions()` does not re-add it as `HerdrOnly`) but with no app action, so its chord, default or customized, runs nothing; keep the real defaults fixture and the table-completeness tests passing.
- Acceptance:
  - Production code: `grep -rn 'FilterSidebar\|SidebarView\|ToggleAgentsView\|tabs_in_sidebar\|NextNeedsYou\|needs_you\|show_agents\|agents_menu' src` matches only inside `#[cfg(test)]` modules (builder lists each remaining hit with its test name).
  - Tests: an old settings file with `sidebar_view: "agents"`, `show_agents`, `tabs_in_sidebar: true` and a `NextNeedsYou` chord loads through `Settings::load` without error and binds nothing; `ctrl-shift-j` fires no action; `prefix+o` with herdr's default and a customized chord fires no app action and is not reported HerdrOnly.
  - Gate green.

## Phase 2 — rows and foot (items 7–9)
- [x] 7. Session row: delete the ⋯ button (`tree-session-menu` in `src/shell/sidebar_tree.rs`); right-click still opens the session menu. The status dot sits at the row's right padding like workspace rows.
- [x] 8. Worktree group: delete its fold chevron and fold toggle (the group header's `fold_chevron` in `src/shell/sidebar.rs`, `toggle_fold` for groups, the folded count via `count_badge` in `src/shell/widgets.rs` if nothing else uses it); groups always draw open; saved worktree folds (`folded`) no longer affect drawing or Workspace1–9 numbering (`src/shell/sidebar_tree.rs` near 562). Session and machine folds stay.
- [x] 9. Sidebar foot (`footer` in `src/shell/sidebar.rs`): a top border with the same color and width as the sidebar's right border.
- Acceptance:
  - Tests: with legacy worktree folds loaded, the group's kids are drawn and Workspace1–9 target exactly the rendered rows in order; session and machine fold still toggle.
  - Live look (screenshot): no ⋯ on session rows and no chevron on worktree groups; right-click on a session opens its menu; the status dots of session rows, plain workspace rows and worktree-group rows share one x position (±1 px); the foot's top border matches the right border's color and width.
  - Gate green.

## Phase 3 — focus mode (item 6)
- [x] 6. One selector, shared by rendering and Workspace1–9 numbering, decides the rows in focus mode: every session with its header; under each, only workspaces with an agent in any status (working, waiting, done, idle); a worktree group shows its parent and the worktrees with one, the parent also when only it has one; sessions with none hidden; no agent anywhere → an empty-state line. A lone session keeps its header in focus mode. Sessions bar on → only the shown session, filtered the same way. Tree drag is off while focus is on. Update `sidebar_focus`'s and `focus_mode()`'s doc comments (`focus_mode()` no longer means "shown session alone"; split it into what each caller needs).
- Acceptance: unit tests through the shared selector: each agent status kept; two sessions with agents both listed; a session without agents absent; a workspace without an agent absent; a kid-only agent → parent + that kid; a parent-only agent → parent alone; empty state; lone session keeps its header; Sessions bar on with focus on and off; focus off identical to before; numbering equals rendered order; drag refused while focus is on. Gate green; live look with several sessions.

## Phase 4 — status bar limits (item 5)
- [x] 5a. Settings: replace `usage_agent` with a show flag per agent (Claude, Codex; both default on); toggled independently and persisted. An old file with `usage_agent` loads through `Settings::load` with both on. Usage settings page: a toggle switch per agent (never a checkbox).
- [x] 5b. `src/ui/statusbar.rs`: on the right end, for each shown agent with limits, `<Name>` followed by a ring and percent per window (Claude: 5-hour, Weekly; Codex: what the plan gives, one ring for a 30-day-only plan). An agent not signed in or failing its endpoint draws nothing.
- [x] 5c. Popup plumbing: `Item::Usage` carries the agent; `on_usage` passes it with the bounds (`src/shell.rs` near 1788), the usage popup (`src/shell/popups.rs` near 892) stores it, `usage_card` takes it; only the clicked label is lit as open.
- Acceptance: tests for windows per agent (two, one, none → no label), both-on default, independent toggles round-tripping through save/load, legacy `usage_agent`, and the popup opening the clicked agent's limits; screenshot of the bar with both agents signed in, matching the agreed look, and with one toggled off. Gate green.

## Files likely involved
`src/shell/sidebar_search.rs`, `src/shell/sidebar.rs`, `src/shell/sidebar_tree.rs`, `src/shell/needs_you.rs`, `src/shell/tree_drag.rs`, `src/shell/tab_bar.rs`, `src/shell/popups.rs`, `src/shell/hints.rs`, `src/shell/widgets.rs`, `src/shell.rs`, `src/settings.rs`, `src/bindings.rs`, `src/herdr/keymap.rs`, `src/ui/command_panel.rs`, `src/ui/settings.rs`, `src/ui/statusbar.rs`.

## Risks and assumptions
- Search state feeds focus, drag and numbering; removing it can break them silently. Mitigation: the Phase 1 and 3 tests plus a live look at drag and focus.
- Item 5 edits upstream files (`statusbar.rs`, `ui/settings.rs`, `usage_agent`, `shell.rs`, `popups.rs`): small conflicts on later upstream merges, accepted.
- The worktree's debug build beside the installed Shepherd is assumed to attach to the same herdr server harmlessly; if not, the live look waits for Kamran's install.
- Tests pinning removed behavior (`ctrl-shift-j`, `FilterSidebar`, Agents view) are rewritten to assert the removal, never deleted to make the gate pass.

## Verification method
- Builder: gate per phase, logs read in full, a report per phase in the worktree (`.kis-report-phase<N>.md`, untracked) with the commit hash, gate counts, acceptance evidence and the mutation table.
- Mutation: every new or changed guard in a phase is listed in the report; each is broken by a literal edit, its specific test is shown failing, and the file is restored from a saved copy and proven byte-identical (`cmp`). The coordinator re-runs at least one per phase.
- Coordinator: re-runs the gate on the branch, trial-merges, merges one phase at a time into `dev`, runs the built debug binary, and takes screenshots (rows aligned, foot border, focus mode with several sessions, status bar with both agents and one off, drag still works outside focus).

## Agents
- Builder: Claude Code, Opus 5.5, high effort, one Herdr tab (`opus: sidebar+statusbar`, agent `sidebar-statusbar`) in its worktree workspace, brief as a file.
- Plan reviewer: Codex gpt-6.1-sol, high effort, read-only sandbox.

## Review
- Reviewed: 2026-10-24 by codex gpt-6.1-sol (high); plan hash baba7bd7057f
- [fixed] Focus mode and Sessions bar share focus_mode() - Decisions + Phase 3: bar keeps the shown session alone, focus filters it; focus_mode() split per caller; tests for bar with focus on and off.
- [fixed] Deleting the prefix+o mapping re-adds open_notification_target as HerdrOnly - Phase 1 task 4 keeps it known with no app action; tests for default and customized chords.
- [fixed] Saved worktree folds still drive Workspace1-9 numbering - Phase 2 task 8 and a numbering test with legacy folds.
- [fixed] Drops among focus-filtered rows - Decision: no tree reordering in focus mode (owner); Phase 3 test.
- [fixed] usage_card needs agent plumbing - Phase 4 task 5c names Item::Usage, on_usage, popups.rs, lit state, and a test.
- [fixed] Agents menu and count helper consumers missing - Phase 1 task 2 names agents_menu and hints.rs; Phase 2 task 8 names count_badge.
- [fixed] Zero-match grep conflicts with negative tests - Phase 1 acceptance limits it to production code, test hits listed.
- [fixed] Phase 2 acceptance misses menu access and absent controls - live-look checks added, fold tests for session and machine.
- [fixed] Focus acceptance omits statuses, parent-only, empty state, lone session - all added through one shared selector.
- [fixed] Phase 4 checks miss defaults, independence, persistence, popup selection - added, with Settings::load for legacy.
- [fixed] Gate does not build the debug binary - cargo build --bin shepherd added; path and mtime reported.
- [fixed] rustfmt follows child modules - stronger: rustfmt is banned (no config; 63 hunks on an untouched file); style by hand.
- [fixed] -j 3 lands after clippy's -- and target sharing is prose - gate commands written out in full.
- [fixed] One mutation per phase is short of the rule - every new or changed guard mutated, cmp-restored, coordinator re-runs one per phase.
