# Plan: `cmd` in the bar, no empty sidebar head (backlog 23, 24)

Approved scope 2026-10-25 ("approve"). Source: `kis/intent/backlog.md` 23, 24. Mode: Standard (one builder, one worktree and branch).

## Decisions (Kamran, 2026-10-25)
- 23: Command Code's numbers show now ("working now"; live API 2026-10-25 08:11 UTC: fiveHour used 0.0028 of cap 14, weekly 0.0028 of 35, so the bars are empty because the account has used almost nothing). What is left is the name: `cmd` in the status bar only; Settings keeps "Command Code"; the popup (which names no agent) is unchanged; Claude and Codex unchanged.
- 24: the sidebar head row draws only when it has something in it (the place label or the connecting spinner), whether focus mode is on or off. Today it draws empty whenever the tree names the sessions (`listed`), leaving 14 px under the 38 px titlebar.
- 24: the DEV badge moves out of the head row into the titlebar, right after the sidebar toggle (dev builds only; it hides with the toggle on the Settings page, `on_page`, `src/shell.rs` ~1793, and stays on a web page as the toggle does). macOS keeps `TOGGLE_LEFT = 88`.
- 24: a 1 px `theme.border` line under the sidebar's part of the titlebar, like the status bar's `border_t_1` (`src/ui/statusbar.rs` ~978). It belongs to the sidebar column (`src/shell/sidebar.rs` ~472–487, which today reserves the titlebar with `pt(TITLEBAR_HEIGHT)`): the reserved 38 px becomes a box with a bottom border, the column's full width (`sidebar_width - 1`, meeting the right border), outside the swiped list, so it folds and resizes with the column and shows in the floating peek card too (`src/shell.rs` ~2019, 6 px in). Its place is 38 px below the column's top, not a window y.

## Out of scope
- Short names for Claude or Codex; other titlebar or main-area changes; backlog 18, 22 and the rest.
- Any push, package build or install (Kamran runs those).

## Files (the builder's own)
- `src/ui/statusbar.rs` (bar label ~1030, its tests), wherever `UsageAgent` is defined (a bar-only name beside `label()`).
- `src/shell/sidebar.rs` (`sidebar_head`, `sidebar_header`, the column ~460–500), `src/shell.rs` (toggle ~2075, `TITLEBAR_CLUSTER` ~77, `is_dev_build` ~1317), `src/shell/sidebar_focus.rs` (`view_button_left`, `chip_box`, tests ~408–500), `src/shell/tab_bar.rs` ~727 and `src/shell/web.rs` ~631 (cluster pad), `src/shell/widgets.rs` (`dev_badge`).
- A file outside this list is a stop: report, do not edit.

## Tasks
- [x] 1. `cmd`: the status bar draws `cmd` for Command Code through a bar-only name; `label()` stays "Command Code". A test renders the production status bar element (not a helper that calls the name itself) and finds `cmd`, `Claude`, `Codex` in it; another keeps `label()` "Command Code" for Settings. Mutation: the bar back to `label()` fails the render test.
- [x] 2. Head row: drawn only with a label or the spinner, decided in one function the caller (`sidebar_header` plus the spinner append ~461, 487) uses. Tests through that function, all four cases: label only, spinner only, both, neither (no `sidebar-head` bounds). The label shows when `!listed`: one session with focus off, or the sessions bar; focus mode heads even a lone session (`sidebar_focus.rs` ~46, `sidebar_tree.rs` ~692), so no label there. Mutation: the head drawn when empty fails the neither case.
- [x] 3. DEV badge in the titlebar after the toggle. The badge gets a fixed width and height (set on the element, no intrinsic text size) and a fixed gap after the toggle, both shared constants. The cluster's width becomes a function of `badge: bool` (production passes `is_dev_build()`, which caches its first value, so tests pass the flag and never set the env var); every `TITLEBAR_CLUSTER` use (chip_box, view_button_left, tab_bar ~727, web ~631) takes it. The titlebar's badge, chip and ⋯ are built by one function production calls and the test renders (the test `Bar` today rebuilds them by hand, `sidebar_focus.rs` ~415–427). Tests, both flags: the rendered badge's measured bounds never overlap the chip or ⋯ (full, icon only, gone as the sidebar narrows); without the badge today's widths hold; the tab bar's pad clears the badge's measured right edge with the sidebar closed. Mutations: the cluster without the badge fails the overlap test; the badge not drawn fails a presence test.
- [x] 4. Border: the 38 px titlebar box at the top of the sidebar column with a 1 px `theme.border` bottom border (decision above). A test renders the column at two widths and finds the box (debug selector) 38 px high, as wide as the column, at its top. Mutations: the border removed (the box's border width 0) fails it; the box dropped fails it.

## Gate (every commit)
Each cargo command run_in_background, output to `.kis-logs/` in the worktree (untracked), read in full:
- `ZIG=$(mise which zig) CARGO_TARGET_DIR=/home/mkamran/Repositories/tools/shepherd/target flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo clippy --all-targets -j 3 -- -D warnings`
- `... cargo test --lib -j 3`, `... cargo test --test model -j 3`, `... cargo build --bin shepherd -j 3` (same prefix).
- Mutations, each failing a named test, then `cmp`-restored: the ones named in tasks 1–4.
- Never run rustfmt or `cargo fmt`. No push, no merge, no app launch by the builder.

## Coordinator verification
- Re-run the gate in the worktree, read each mutation, merge into `dev`, re-run the gate on `dev`.
- Live look: build into `target-live`, run via `shepherd-dev sandbox`, screenshots of: debug build (badge after the toggle, focus on and off, narrow sidebar, sidebar closed with tabs clear of the badge); `SHEPHERD_DEV_BADGE=0` (no badge, the first session row right under the line, several sessions and one session, the spinner while connecting); the bar reading `cmd`; the line meeting the sidebar's right border at the corner; the line in the floating peek card; the Settings page (no toggle, no badge) and a web page (toggle and badge stay).

## Acceptance
- The bar reads `cmd 5h … wk …`; Settings says "Command Code"; the popup is unchanged.
- With the release look (no badge) and several sessions, no empty row between the titlebar line and the first session row, focus on or off; the label row stays when the tree names no session (one session with focus off, or the sessions bar).
- Dev builds show DEV after the toggle and nothing overlaps it at any sidebar width or with the sidebar closed.
- The line spans the sidebar only, docked or floating.

## Review
- Reviewed: 2026-10-25 by codex gpt-6.1-sol (medium); plan hash ce74058ab6d2
- [fixed] "hides with the toggle on a web page": `on_page` is the Settings page (src/shell.rs ~1793); web pages keep the toggle - decision and live look name Settings and web pages
- [fixed] the popup names no agent (statusbar.rs ~504–524) - acceptance says the popup is unchanged, Settings keeps "Command Code"
- [fixed] focus mode heads a lone session, so "label row with one session" was wrong - label row tied to `!listed` (one session with focus off, or the sessions bar)
- [fixed] badge width was intrinsic (text, 6 px margin) - fixed width, height and gap as shared constants; macOS TOGGLE_LEFT kept
- [fixed] is_dev_build() caches its first value, so tests cannot flip it - geometry takes `badge: bool`, production passes is_dev_build()
- [fixed] test helpers bypass production wiring (windows() calls label(), Bar rebuilds the titlebar) - tests render the production bar and one shared titlebar builder, badge bounds measured
- [fixed] border placement underspecified - the column's reserved 38 px becomes a bordered box, full column width, outside the swiped list
- [fixed] the floating peek card renders the same sidebar 6 px in - border placed relative to the column, peek card in the live look and acceptance
- [fixed] no mutations for border or badge, spinner check optional - mutations for border removed, box dropped, badge not drawn; four label/spinner cases through the caller's function; spinner required in the live look
