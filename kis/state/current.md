# State (2026-10-24)
Stamp: dev 7ba0a71 80b5e82c9d5c

Branch: `dev` @ 7ba0a71 (upstream v1.3.5 merged); pushed to fork; installed as shepherd-local 1.3.5.r403 (running app has DO_NOT_TRACK=1, checked 2026-10-24).
Task: none active. shepherd-local PKGBUILD now installs a DO_NOT_TRACK=1 wrapper at /usr/bin/shepherd (binary in /usr/lib/shepherd-local).
Blocker: none. Untested live: v1.3.5 web-page menu and drag, Open in editor from the reader, SSH machine files, bar swipe/drag with several sessions.
Proof: merge 7ba0a71: clippy --all-targets clean; cargo test --lib 918 passed, tests/model.rs 30 passed (2026-10-24).
Next: Kamran picks. Planned: port upstream's lift/placeholder/spring drag look onto tree_drag. Open: `[[keys.command]]` add/remove, lone-prefix flush, Agents picker.
