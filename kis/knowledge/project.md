# Shepherd fork

Rust/GPUI terminal-agent workspace app (herdr front end). This checkout is Kamran's fork; fork is the main line, upstream releases are merged in, only small Linux PRs go upstream (no Hyprland/Omarchy/Arch wording in upstream changes).

## Folders
- `shepherd` (this one): the only checkout, on `dev` (the working line). `main` mirrors upstream (`origin`, freethinkel/shepherd) and is only fast-forwarded; `fork` is smkamranqadri/shepherd. Upstream merged the Linux PRs #2 and #3, so dev's first Linux commits exist upstream under other hashes.
- `shepherd-local`: build folder. PKGBUILD builds `branch=dev` (webkit2gtk-4.1 and gtk3 are depends); rebuild with `makepkg -f`, Kamran runs `sudo pacman -U`. Keep only the latest `.zst`.
- Only branches: `dev` and `main`. Old feature branches were merged or superseded and deleted 2026-10-23 (`feat/right-sidebar` tip 995ad24; its packaging/arch/PKGBUILD was dropped). Git keeps their commits until gc.
- `fork` also has `linux-port`, `up-linux-pr1`, `up-linux-pr2` (the upstream PR branches, merged).
- Fork choices on upstream merge: sidebar tree by default with a "Sessions bar" toggle for upstream's bar; upstream's Markdown reader and update notice; upstream's usage events kept in code but off by default via the shepherd-local `/usr/bin/shepherd` wrapper (`DO_NOT_TRACK=1` unless set; since 2026-10-24); the tree keeps its own drag (`tree_drag.rs`), upstream's `reorder.rs` is used only for web pages. Removed in the fork (2026-10-24): the sidebar search, the Agents view and upstream's bottom Agents section with its menu and settings (`show_agents`, `agent_sort`, `compact_agents`, `attention_agents`), tabs in the sidebar, NextNeedsYou (herdr's `prefix+o` is caught and does nothing), worktree-group folds; `usage_agent` became `usage_show` (a switch per agent). These overlap upstream code: expect conflicts in settings.rs, sidebar.rs, popups.rs, statusbar.rs, shell.rs, ui/settings.rs.

## Rules
- Builds (start long ones with run_in_background from the outset; a foreground run moved to the background at 600 s got SIGKILLed twice, 2026-10-24): `ZIG=$(mise which zig) flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo ... -j 3`.
- Gate: `cargo test --lib` plus clippy; re-run it before committing a builder's work.
- Never commit, push or deploy unasked. Commit trailer per the session reminder.
- Coordinator briefs, verifies and merges; builders write code. Propose harness/model/effort per agent and wait.
- Live looks run the debug build through `~/.local/bin/shepherd-dev` (outside the repo): `sandbox` (own herdr server, sockets and settings copy under `~/.cache/shepherd-dev`, never the real ones), `seed` (fictional repos and sessions), `mock` (upstream's `docs/mock` fixture, untried), `herdr <args>`, `stop`, `reset`. A copy of `sleep` named `claude` in a pane reads as an agent to herdr.
- Load-sensitive tests: `files::runner::tests::a_dropped_receiver_kills_a_silent_child` and `ui::side_panel::changes::tests::a_commit_during_a_rebase_goes_into_it` fail at load ~10; re-run before blaming a change.
- Never run rustfmt: no config, it rewrites untouched files.
- gpui (zui): never show a child by toggling `display` from `hover`/`group_hover` (`.hidden().group_hover(.., |s| s.flex())`): hover is known only by paint, prepaint skipped the hidden children, and paint panics `must call prepaint before paint`. Fade with `opacity` (keeps its space and its clicks) or hold the hovered row in state via `on_hover` (Changes panel, 9209847).
- Live-look input on Hyprland (Lua config): keys `wtype -M ctrl -M alt g -m alt -m ctrl`; focus `hyprctl dispatch 'hl.dsp.focus({ window = "pid:N" })'`; pointer `hl.dsp.cursor.move({ x =, y = })`; click `hl.dsp.send_key_state({ mods = "", key = "mouse:272", state = "down" })` then `up`; screenshot `grim -g "x,y wxh"`. Kamran's own mouse overrides it: check `hyprctl cursorpos` before a click, since a click lands wherever the pointer is.
