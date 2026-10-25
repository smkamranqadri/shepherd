# Backlog (recorded 2026-10-24)

Kamran's list. Items 1–9 are done (`plan-sidebar-statusbar.md`, merged 2026-10-24); 10, 13–17, 19–21 are done (`plan-bundle-2026-10-25.md`, merged 2026-10-25); 23–24 are done (`plan-cmd-and-head.md`, merged into dev 470a038, 2026-10-25); 11, 12, 18, 22 are open.

## Remove
1. Sidebar search: the header magnifier, its search bar, and FilterSidebar.
2. The Workspaces | Agents toggle and the Agents view.
3. The "tabs in the sidebar" setting.
4. Jump to the agent that needs you (Ctrl+Shift+J, Shift+Cmd+J on macOS).

## Add or fix
5. Status bar: a Claude label and a Codex label, each with its 5-hour and weekly limit.
6. Focus mode: list every session with its workspaces that have a running agent. Today it filters to one session; it should not filter sessions.
7. Session row: remove the three-dot button. It pushes the green dot inward, so the dot no longer lines up with other rows.
8. Workspace with worktrees: remove the collapse chevron (worktree groups don't need folding). It indents the green dot the same way.
9. Sidebar foot (Settings, New workspace): add a border on its top edge, styled like the sidebar's right border.

14. Focus chip: with focus mode on, the chip draws on a row of its own under the sidebar header, taking vertical space. Put it in the header row, beside the ⋯ (Kamran, 2026-10-25).

15. Markdown preview: blocks overlap (paragraphs, list items, later sections drawn over the list), seen in Shepherd's README (Kamran, 2026-10-25, screenshots).
16. Markdown preview: images in raw HTML `<img>` tags show nothing, local SVG (`docs/assets/hero.svg`) and remote (shields.io, GitHub badge) alike; seen in `~/Repositories/side projects/lore-skill/README.md`. `picture()` in `src/ui/markdown.rs` takes local paths only.
17. Status bar usage: replace each window's ring with a named thin bar, `Claude 5h ▰▱▱▱▱ wk ▰▰▱▱▱`; percent in the popup (Kamran, 2026-10-25).
18. Overview page: a main-area page with every session's agents as cards, grouped by status (Needs you, Working, Done and resting) with a Group by choice; Kamran's mockup 2026-10-25. Its sidebar also showed Needs you and Search entries, which 1 and 4 removed.
19. Changes panel: after a Stage the panel takes ~5–20 s to show the new status while git updates at once (seen during the 9209847 live look).
20. Agent status during background work: when an agent ends its turn with a background task still running (Claude's run_in_background Bash, Monitor; Codex's equivalent), herdr reports it idle and the sidebar shows no work. Subagents are already covered outside Shepherd by `~/.claude/hooks/herdr-subagents.sh`; background shells are not (Kamran, 2026-10-25).
    Research 2026-10-25 (read-only, Claude Code 2.1.300, herdr 0.9.5, Codex 0.162.1): no Claude or Codex hook fires when a background shell or Monitor ends; PostToolUse(Bash) has `backgroundTaskId` (not seen live). Process tree tells them apart: every Bash call is a direct child of claude in its own session (setsid) with stdout on `/tmp/claude-$UID/<slug>/<session>/tasks/<id>.output`; MCP servers share claude's session. `herdr pane process-info` gives the agent's pid; Shepherd reads no pane pid today. Recommended: Shepherd counts live task shells of an idle agent (Linux /proc, macOS later) and shows working with N shells; a hook only as a fallback. Not verified: Monitor's process shape, `cmd &` in a foreground call, Codex's tree.
21. Notifications noisy and unclear (Kamran, 2026-10-25). Store `~/.local/state/byj-notification-center/store.json`, 500 kept over ~4.7 h: 274 Shepherd, 195 `notify-send` "Claude Code needs your attention" from a Notification hook in `~/.claude/settings.json` (there since before 2026-09-18, no session or workspace named), 25 of those within 5 s of a Shepherd one. Shepherd posts Done on every turn end, so an orchestrator woken by its workers repeats it (claude · herdr-orch "Done: Perms-column intent via kis:act" 43 times in ~3 h); 29 exact repeats within 60 s. Title gains "— session" only when that session is not shown, so one pane reads two ways.
22. herdr plugin `dan.pane-topic-sync` 0.3.0 (third party, `~/.config/herdr/plugins/github/dan.pane-topic-sync-1bfae7219dc1`) renames once, then freezes: pane w3:pG keeps label "Herdr shortcut sync planning" while its topic is "Remaining items for one cycle". Its state file `~/.local/state/herdr/plugins/dan.pane-topic-sync/pane-topic-sync-state.json` is shared by all five herdr sessions and pruned to the running session's ids (it held 0 panes, 0 tabs at 03:34); with `respect_manual_names` a label not in its history reads as a manual rename. Hypothesis, not reproduced.
23. Status bar: name Command Code `cmd` (bar only). Its numbers show (Kamran, 2026-10-25: "working now"). Plan `plan-cmd-and-head.md`.
24. Sidebar head: drawn only with a label or the spinner (focus on or off); the DEV badge moves into the titlebar after the toggle; a 1 px border under the sidebar's titlebar part (Kamran, 2026-10-25). Plan `plan-cmd-and-head.md`.
25. Background shell missed when it redirects its output (Kamran, 2026-10-25: menqal › school-next ran a shell, sidebar showed no working mark). Seen live on r432: claude pid 3426240 in pane w4:pM, herdr idle; its child bash 1842627 (own session, ~13 min) had fd 1 and 2 on `…/scratchpad/lf-gates.log` and `…/tasks/bjwdnj12j.output` only on fd 11 and 12. `src/shell/bg_shells.rs:164` reads `fd/1` alone, so the count was 0. Fix: count the shell when any of its fds points at a task output; test with fd 1 on a log and a high fd on the task file; mutation and live look in the sandbox.

## Later
13. Machine row (a saved SSH machine's header): remove its hover ⋯ like the session row's, the right-click menu staying (`sidebar_tree.rs` `tree-machine-menu-*`). Kamran has no SSH machines today (2026-10-25).
10. Agent usage for Command Code, as for Claude and Codex.
11. Port upstream's drag look (lift, placeholder, spring) onto the tree's drag (`tree_drag.rs`).
12. Earlier open items: `[[keys.command]]` add/remove, lone-prefix flush, Agents picker.

## Open after the bundle (2026-10-25)
- 17: settled 2026-10-25: Kamran finds the thin bars good.
- 16: images are left-aligned (`<p align="center">` not honored).
- 10: a Command Code plan without caps shows no label (the CLI shows monthly credits there); an organization account reads the personal plan.
- 20: Monitor and Codex background work not covered (Codex keeps its background terminals on pipes/ptys); the waiting pill, tab bar, status bar and command panel dots still use herdr's status.

## Open questions
- (Settled 2026-10-25: a narrow bar truncating the folder path to its icon beside both usage labels is accepted.)
- 12: is the Agents picker still wanted once the Agents view (2) is gone?
