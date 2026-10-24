# Shepherd fork

Rust/GPUI terminal-agent workspace app (herdr front end). This checkout is Kamran's fork; fork is the main line, upstream releases are merged in, only small Linux PRs go upstream (no Hyprland/Omarchy/Arch wording in upstream changes).

## Folders
- `shepherd` (this one): the only checkout, on `dev` (the working line). `main` mirrors upstream (`origin`, freethinkel/shepherd) and is only fast-forwarded; `fork` is smkamranqadri/shepherd. Upstream merged the Linux PRs #2 and #3, so dev's first Linux commits exist upstream under other hashes.
- `shepherd-local`: build folder. PKGBUILD builds `branch=dev` (webkit2gtk-4.1 and gtk3 are depends); rebuild with `makepkg -f`, Kamran runs `sudo pacman -U`. Keep only the latest `.zst`.
- Only branches: `dev` and `main`. Old feature branches were merged or superseded and deleted 2026-10-23 (`feat/right-sidebar` tip 995ad24; its packaging/arch/PKGBUILD was dropped). Git keeps their commits until gc.
- `fork` also has `linux-port`, `up-linux-pr1`, `up-linux-pr2` (the upstream PR branches, merged).
- Fork choices on upstream merge: sidebar tree by default with a "Sessions bar" toggle for upstream's bar; upstream's Markdown reader and update notice; upstream's usage events kept in code but off by default via the shepherd-local `/usr/bin/shepherd` wrapper (`DO_NOT_TRACK=1` unless set; since 2026-10-24); the tree keeps its own drag (`tree_drag.rs`), upstream's `reorder.rs` is used only for web pages.

## Rules
- Builds (start long ones with run_in_background from the outset; a foreground run moved to the background at 600 s got SIGKILLed twice, 2026-10-24): `ZIG=$(mise which zig) flock /tmp/claude-1000/shepherd-cargo.lock timeout 900 nice -n 19 cargo ... -j 3`.
- Gate: `cargo test --lib` plus clippy; re-run it before committing a builder's work.
- Never commit, push or deploy unasked. Commit trailer per the session reminder.
- Coordinator briefs, verifies and merges; builders write code. Propose harness/model/effort per agent and wait.
- Details: `~/.claude/projects/-home-mkamran-Repositories-tools-shepherd/plans/handoff-2026-10-19*.md`.
