# Backlog (recorded 2026-10-24)

Kamran's list. Items 1–9 are done (`plan-sidebar-statusbar.md`, merged 2026-10-24); 10–12 are not planned.

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

13. Machine row: remove its ⋯ too, like the session row (Kamran, 2026-10-25); the right-click menu stays.

## Later
10. Agent usage for Command Code, as for Claude and Codex.
11. Port upstream's drag look (lift, placeholder, spring) onto the tree's drag (`tree_drag.rs`).
12. Earlier open items: `[[keys.command]]` add/remove, lone-prefix flush, Agents picker.

## Open questions
- (Settled 2026-10-25: a narrow bar truncating the folder path to its icon beside both usage labels is accepted.)
- 12: is the Agents picker still wanted once the Agents view (2) is gone?
