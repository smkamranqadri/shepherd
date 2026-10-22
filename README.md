<h1 align="center"><img src="docs/icon.png" width="48" height="48" align="absmiddle" alt=""> Shepherd</h1>

When five Claude Code or Codex agents run at once, one of them is always waiting on you, and you find out by cycling through panes. Shepherd lists every agent with its status and how long it has waited, notifies you when one needs an answer, and lets you approve it from the notification.

It's a native app for macOS and Linux on top of [herdr](https://herdr.dev), which runs your shells and agents in workspaces, tabs and panes. Shepherd shows the same session with a sidebar and real tabs.

It's written in Rust on [GPUI](https://github.com/zeronsh/zui) and styled after [Zeron](https://github.com/zeronsh/zeron). The terminal is [libghostty-vt](https://github.com/ghostty-org/ghostty) with its own glyph renderer.

https://github.com/user-attachments/assets/9b3f5fae-8eee-4697-89b4-37d232250117

## What it does

- The sidebar lists workspaces. A repo's worktrees fold under it with their branch, changes against main and PR state. Right-click creates, opens or removes a worktree.
- The sidebar lists every machine, its herdr sessions and their workspaces in one tree. The sessions not shown are kept fresh in the background, a click on one of their workspaces goes there, and a session's menu starts, stops or restarts it.
- The agents list at the bottom shows every agent herdr detects in any workspace, with its status (working, blocked, done, idle), the task in its title and how long it has waited. Click one to jump to its pane; click the list's header to fold it down to the header, and again to open it.
- The Workspaces | Agents toggle at the sidebar's top swaps the tree for every session's agents, under their machine and session headers with the count waiting in each; the toggle counts what the Dock does. The choice is kept, and the agents list at the bottom comes back as it was with Workspaces.
- When an agent you can't see starts waiting or finishes, macOS shows a notification and the Dock icon counts it until you look at that tab. Approve on the notification answers the agent without opening Shepherd.
- Tabs show the agent or shell running in them. Drag tabs to reorder them. Drag a pane by its grip onto another pane to split or swap, or onto a tab to move it there.
- Dividers between panes snap to the cell grid herdr lays out, and the terminal reflows while you drag.
- The terminal draws ligatures, Kitty images and box drawing on its own. Shepherd reads scrollback from herdr once and scrolls it locally. `⌘`-click opens links, files, folders and images.
- herdr's own `[keys]` work in Shepherd, and its popups float over the panes.
- `⌥⌘B` opens the right panel: the workspace folder's files (to view or edit in a tab), its git changes and diffs, where whole files are staged, unstaged, discarded and committed (or the last commit amended), a search in its files, Go to file (`⌘P`: a few letters of a file's name, its open tabs first, `name:12` at a line), and the branch's PR (gh) or MR (glab) with its description, checks or pipeline jobs and their logs. The PR tab works for SSH machines too.
- Unsaved changes in a file tab are kept as a draft until you save them or let them go, so a crash or a quick quit doesn't lose them; they come back the next time the file opens. Drafts are plain text in `drafts/` beside the settings file, readable by your user only, and last up to 30 days unopened. A home backup can copy them unencrypted, even for files from an encrypted or removable volume; files that look like secrets (`.env`, keys, certificates, `.ssh`, `.gnupg`) get no draft.
- `⌘K` opens a command panel with every action, workspace and tab of the session shown. `⇧⌘A` opens the agents panel and Workspaces the workspaces panel, which list every session's, going there as a click in the sidebar does; a session out of reach is listed from what it last said, marked so.
- Hold `⌘` for a moment and each tab shows its number; hold `⌃` and each workspace does (Ctrl and Ctrl+Shift on Linux). Settings can turn the hints off.
- A status bar along the bottom, off until General settings turn it on, shows the branch, its PR and CI, the current folder, the focused agent's tokens and your Claude and Codex plan limits. Switch branch is in the command panel either way, and Usage settings show the limits.
- Web pages (Settings → Web pages) list local tools, such as a dev server or a dashboard, above the workspaces. A click shows the page in the system web view in place of the tabs and panes, and it keeps its state while hidden. A page can be listed in one place only, and opened on the machine shown, through an SSH forward.
- Settings cover fonts, themes, key bindings and usage. They also edit herdr's own `config.toml` for notifications and integrations, keeping your comments.
- The Shortcuts page is the one place for keys: every herdr action (`[keys]`, its prefix key included), every herdr custom command (`[[keys.command]]`) and every Shepherd action, in one searchable list. A change to a herdr key is written to herdr's `config.toml` (comments kept, a copy of the file made the first time as `config.toml.shepherd-bak`) and herdr reloads it; Shepherd follows the file whoever edits it, no restart. Reset puts the default back by removing just that line. A prefix chord (`prefix+n`) works in a pane as in herdr: the prefix key, then the key. A herdr action Shepherd has no equivalent of is shown as herdr only and said so when pressed.

## Install

Download `Shepherd-<version>-universal.dmg` from [Releases](https://github.com/freethinkel/shepherd/releases), open it and drag Shepherd to Applications. One build runs on Apple silicon and Intel Macs. It's signed and notarized, so Gatekeeper opens it without a prompt. The same app is also there as a zip.

### Linux

The same release has Shepherd for x86_64 and aarch64 as an `.rpm`, a `.deb` and a `tar.gz`:

```sh
sudo dnf install ./shepherd-<version>-1.<arch>.rpm      # Fedora
sudo apt install ./shepherd_<version>-1_<arch>.deb       # Debian, Ubuntu
tar -xzf Shepherd-<version>-linux-<arch>.tar.gz -C ~/.local   # anywhere else: bin, .desktop file and icon
```

It draws with Vulkan, so it needs a Vulkan driver (`mesa-vulkan-drivers`, which the packages recommend). Web pages need WebKitGTK 4.1 (`libwebkit2gtk-4.1-0`), which the packages depend on; with the tar.gz, install it yourself, or Shepherd won't start. On Wayland a web page opens in the browser. Linux gets no Dock badge, Secure Keyboard Entry or self-update: release builds only say in the sidebar that a new release is out, and a click opens its page. Settings live in `~/.config/shepherd`. Clicking a notification asks the compositor to raise the window. Some do, others only mark it for attention. The notification sound comes from the freedesktop sound theme, played by `canberra-gtk-play`, or by `paplay` if that's missing.
Settings → Appearance can make the window translucent (your compositor decides whether it's blurred), or hide Shepherd's window buttons when the window manager has its own or tiles windows. Right-clicking the titlebar opens the window manager's window menu.

### Requirements

Shepherd needs macOS 14 or later, or Linux. It talks to the herdr server over `~/.config/herdr/herdr.sock` and starts the server if nothing answers there. If herdr isn't installed, Shepherd downloads the build for your system from herdr.dev, checks its SHA-256 and puts it in `~/.local/bin`, as herdr's `install.sh` does. You can point it at another binary or socket in Settings.

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
| `⌘F`, `⌥⌘F`, `⌃G` | `Ctrl+F`, `Ctrl+H`, `Ctrl+G` | Find, replace, go to line (in the editor; a pane's keys stay the shell's) |
| `↩`, `⇧↩` or `F3`, `⇧F3`, `⌘G`, `⇧⌘G`, `⎋` | `Enter`, `Shift+Enter` or `F3`, `Shift+F3`, `Esc` | Next match, previous match, close the find bar |
| `⌘C`, `⌘V`, `⌘F` | `Ctrl+Shift+C`, `Ctrl+Shift+V`, `Ctrl+Shift+F` | Copy, paste, find in a pane |
| `⌘K`, `⇧⌘A` | `Ctrl+Shift+P`, `Ctrl+Shift+A` | Command panel, agents panel |
| `⇧⌘J` | `Ctrl+Shift+J` | Next agent that needs you: blocked first, then done, the longest wait first, in any session; again for the one after |
| `⌘=`, `⌘-`, `⌘0` | `Ctrl+=`, `Ctrl+-`, `Ctrl+0` | Text size up, down, default |
| `⌘,` | `Ctrl+,` | Settings |
| `⌘/` | `Ctrl+Shift+/` | Keyboard shortcuts: every action and its keys, searchable by either |

On Linux the app's chords follow Ghostty's: Ctrl with a letter stays the shell's, Ctrl+click opens a link, and the
right panel's chords take Ctrl+Alt beside its toggle. Settings can make Ctrl+C and Ctrl+V copy and paste in the panes
too (Ctrl+C still sends ^C with nothing selected).

You can rebind any of them in Settings.

## Build

You need a recent stable Rust toolchain, Xcode's command line tools and [Zig 0.16](https://ziglang.org/download/), which builds libghostty-vt. Put `zig` on your `PATH` and run with `ZIG=zig`, or point `ZIG` at the binary. The path in `.cargo/config.toml` only fits one machine.

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

`make app` signs with the Developer ID certificate in your keychain if it finds one, and ad hoc otherwise. CI makes the releases. Bumping `version` in `Cargo.toml` on `main` builds, signs, notarizes and publishes one.

## License

MIT, see [LICENSE](LICENSE). Code and assets taken from Zeron (MIT) and the Geist fonts (SIL OFL 1.1) are listed in [assets/NOTICE](assets/NOTICE).
