<h1 align="center"><img src="docs/icon.png" width="48" height="48" align="absmiddle" alt=""> Shepherd</h1>

A native macOS window for [herdr](https://herdr.dev). herdr runs your shells and coding agents in workspaces, tabs and panes. Shepherd shows the same session with a sidebar, real tabs and a list of every agent, so you can see which one is waiting on you without cycling through panes.

It's written in Rust on [GPUI](https://github.com/zeronsh/zui) and styled after [Zeron](https://github.com/zeronsh/zeron). The terminal is [libghostty-vt](https://github.com/ghostty-org/ghostty) with its own glyph renderer.

![Shepherd with a split tab, the workspace sidebar and four agents](docs/screenshot.png)

## What it does

- The sidebar lists workspaces. A repo's worktrees fold under it with their branch, changes against main and PR state. Right-click creates, opens or removes a worktree.
- The sidebar lists every machine, its herdr sessions and their workspaces in one tree. The sessions not shown are kept fresh in the background, a click on one of their workspaces goes there, and a session's menu starts, stops or restarts it.
- The agents list at the bottom shows every agent herdr detects in any workspace, with its status (working, blocked, done, idle), the task in its title and how long it has waited. Click one to jump to its pane; click the list's header to fold it down to the header, and again to open it.
- When an agent you can't see starts waiting or finishes, macOS shows a notification and the Dock icon counts it until you look at that tab. Approve on the notification answers the agent without opening Shepherd.
- Tabs show the agent or shell running in them. Drag tabs to reorder them. Drag a pane by its grip onto another pane to split or swap, or onto a tab to move it there.
- Dividers between panes snap to the cell grid herdr lays out, and the terminal reflows while you drag.
- The terminal draws ligatures, Kitty images and box drawing on its own. Scrollback is read from herdr once and scrolled locally. `⌘`-click opens links, files, folders and images.
- herdr's own `[keys]` work in Shepherd, and its popups float over the panes.
- `⌥⌘B` opens the right panel: the workspace folder's files (to view or edit in a tab), its git changes and diffs, a search in its files, and the branch's PR (gh) or MR (glab) with its description, checks or pipeline jobs and their logs. The PR tab works for SSH machines too.
- `⌘K` opens a command panel with every action, workspace and tab of the session shown. `⇧⌘A` opens the agents panel and Workspaces the workspaces panel, which list every session's, going there as a click in the sidebar does; a session out of reach is listed from what it last said, marked so.
- Hold `⌘` for a moment and each tab shows its number; hold `⌃` and each workspace does (Ctrl and Ctrl+Shift on Linux). Settings can turn the hints off.
- The status bar shows the branch, its PR and CI, the current folder, the focused agent and your Claude and Codex plan limits.
- Settings cover fonts, themes, key bindings and usage. They also edit herdr's own `config.toml` for notifications and integrations, keeping your comments.

## Install

Download `Shepherd-<version>-universal.dmg` from [Releases](https://github.com/freethinkel/shepherd/releases), open it and drag Shepherd to Applications. One build runs on Apple silicon and Intel Macs. It's signed and notarized, so Gatekeeper opens it without a prompt. The same app is also there as a zip.

### Linux

The same release has Shepherd for x86_64 and aarch64 as an `.rpm`, a `.deb` and a `tar.gz`:

```sh
sudo dnf install ./shepherd-<version>-1.<arch>.rpm      # Fedora
sudo apt install ./shepherd_<version>-1_<arch>.deb       # Debian, Ubuntu
tar -xzf Shepherd-<version>-linux-<arch>.tar.gz -C ~/.local   # anywhere else: bin, .desktop file and icon
```

It draws with Vulkan, so it needs a Vulkan driver (`mesa-vulkan-drivers`, which the packages recommend). There's no
Dock badge, Secure Keyboard Entry or self-update on Linux; settings live in `~/.config/shepherd`. A release build made
with `SHEPHERD_UPDATE_CHECK` set says when a new version is out and opens its page; other builds never check. A click
on a notification requests activation for the window; compositors may grant it or mark the window for attention. The
notification sound is the freedesktop sound theme's, played by `canberra-gtk-play`, or `paplay` where that is missing.
Settings → Appearance can make the window translucent (whether it is blurred depends on your compositor), or hide its
own buttons when the window manager provides its own controls or tiles windows. A right-click on the titlebar opens
the window manager's window menu.

### Requirements

Shepherd needs macOS 14 or later (or Linux) and herdr. It talks to the herdr server over `~/.config/herdr/herdr.sock`, so herdr has to be installed and running. You can point it at another binary or socket in Settings.

## Keys

| macOS | Linux | Action |
|---|---|---|
| `⌘T`, `⇧⌘W` | `Ctrl+Shift+T`, `Ctrl+Alt+W` | New tab, close tab |
| `⌘D`, `⇧⌘D` | `Ctrl+Shift+O`, `Ctrl+Shift+E` | Split right, split down |
| `⌘W` | `Ctrl+Shift+W` | Close the pane, or the file tab |
| `⌥⌘` + arrows | `Ctrl+Alt` + arrows | Focus the pane in that direction |
| `⇧⌘↩` | `Ctrl+Shift+Enter` | Zoom the pane |
| `⌘1` … `⌘9`, `⇧⌘[`, `⇧⌘]`, `⌃⇥`, `⌃⇧⇥` | `Ctrl+1` … `Ctrl+9`, `Ctrl+PgUp`, `Ctrl+PgDn`, `Ctrl+Tab`, `Ctrl+Shift+Tab` | Go to a tab, previous or next tab (terminal and file tabs, from any view) |
| `⌃1` … `⌃9`, `⌘↑`, `⌘↓` | `Ctrl+Shift+1` … `9`, `Ctrl+Shift+Up`, `Ctrl+Shift+Down` | Go to a workspace (in sidebar order), previous or next workspace |
| `⌃⌘↑`, `⌃⌘↓` | `Ctrl+Shift+PgUp`, `Ctrl+Shift+PgDn` | Previous or next session or machine |
| | `Ctrl+Shift+S` | Switch session or machine (arrows and Enter pick, the next one is preselected; ●N counts its agents waiting or done) |
| `⇧⌘H` | `Ctrl+Shift+H` | Fold or unfold the shown session's workspaces in the sidebar |
| `⇧⌘N` | `Ctrl+Shift+N` | New workspace, in a folder you pick (on a remote machine, the focused pane's) |
| `⌘B`, `⌥⌘B` | `Ctrl+Shift+B`, `Ctrl+Alt+B` | Toggle the sidebar, the right panel |
| `⇧⌘E`, `⌥⌘G`, `⇧⌘F`, `⌘P` | `Ctrl+Alt+E`, `Ctrl+Alt+G`, `Ctrl+Alt+F`, `Ctrl+Alt+P` | Files, Changes, Search in files, Go to file |
| `⌘S`, `⌥⌘S` | `Ctrl+S`, `Ctrl+Alt+S` | Save the file, save all (in a file tab) |
| `⌘C`, `⌘V`, `⌘F` | `Ctrl+Shift+C`, `Ctrl+Shift+V`, `Ctrl+Shift+F` | Copy, paste, find in a pane |
| `⌘K`, `⇧⌘A` | `Ctrl+Shift+P`, `Ctrl+Shift+A` | Command panel, agents panel |
| `⇧⌘J` | `Ctrl+Shift+J` | Next agent that needs you: blocked first, then done, the longest wait first, in any session; again for the one after |
| `⌘=`, `⌘-`, `⌘0` | `Ctrl+=`, `Ctrl+-`, `Ctrl+0` | Text size up, down, default |
| `⌘,` | `Ctrl+,` | Settings |
| `⌘/` | `Ctrl+Shift+/` | Keyboard shortcuts: every action and its keys, searchable by either |

On Linux the app's chords follow Ghostty's: Ctrl with a letter stays the shell's, Ctrl+click opens a link, and the
right panel's chords take Ctrl+Alt beside its toggle. Settings can make Ctrl+C and Ctrl+V copy and paste in the panes
too (Ctrl+C still sends ^C with nothing selected).

Every binding can be changed in Settings.

## Build

You need a recent stable Rust toolchain, Xcode's command line tools and [Zig 0.16](https://ziglang.org/download/), which builds libghostty-vt. Put `zig` on your `PATH` and run with `ZIG=zig`, or point `ZIG` at the binary: `.cargo/config.toml` holds a local path that only fits one machine.

```sh
make run      # debug build, run as Shepherd.app
make test
make lint     # clippy, warnings as errors
make app      # target/release/Shepherd.app
make install  # copy it to /Applications
make dist     # universal zip and dmg in target/universal
make linux    # Linux tar.gz, deb and rpm in target/linux-dist, built in Docker
make help     # everything else
```

`make app` signs with the Developer ID certificate in your keychain if it finds one, and ad hoc otherwise. Releases come from CI: bumping `version` in `Cargo.toml` on `main` builds, signs, notarizes and publishes a new release.

## License

MIT, see [LICENSE](LICENSE). Code and assets taken from Zeron (MIT) and the Geist fonts (SIL OFL 1.1) are listed in [assets/NOTICE](assets/NOTICE).
