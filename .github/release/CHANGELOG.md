# Changelog

All notable user-facing changes to Crab are listed here, newest first.

## Unreleased

## 1.0.0 - 2026-11-16

- New Agent Chat panel in the activity bar. Chat with Claude Code, Codex or OpenCode using the CLIs you already have installed and signed in; no API keys needed.
- Agent chats support slash commands, `@` file mentions and `$` skills, with a menu that completes them as you type. Files, commands and skills picked from the menus show up as badges.
- Attach images by pasting, dropping or using the paperclip button, and download images from chats.
- The chat sidebar has pins, custom sections, an archive, search and grouping by project. Chats can be renamed, forked and restored on restart, and open in a side panel with multiple tabs.
- Subagents and background commands can be observed from the bar under the chat box. Search inside a chat with the Find bar.
- Choose permission modes and effort from the model picker. Claude models use their largest context window by default.
- Sound and desktop notifications when an agent finishes, needs you or hits an error. Notifications are off by default.
- Voice dictation in the chat box, using a local speech model that downloads on first use.
- Enable Agent Chat setting hides the agent panel and everything agent-related. Individual agents can be hidden from the new chat menu.
- Write commit messages with agents from the Git panel.
- Add "VS Code Dark Modern" and "VS Code Light Modern" themes, now the default, with VS Code's terminal colors. Default fonts and sizes match VS Code. Crab Light and Crab Dark themes are also included.
- Symbols is the default icon theme. Inter at 350 weight is the interface font.
- Problems tab opens as a table in the bottom panel, with filters, severity filters and a tree view.
- Terminal panel lists terminals in a side list, with command status dots (zsh), a Ports tab, and tabs that follow the title set by the running program.
- Right-click the panel tab row to show or hide tabs, move the panel, hide it, or switch between icons and labels.
- Status bar shows the git branch, indentation, and "Ln 1, Col 1", with a menu to convert indentation.
- Project panel gains an "Open Editors" section, New File, New Folder, Refresh and Collapse All buttons, git status letters and error counts, and a Get Info item. Copied files can be pasted into other windows and apps. Delete always moves to the Trash.
- Easy switching between multiple open projects in a window, with a dot while an agent is working or needs you.
- Search has its own panel with VS Code-style input boxes and a Toggle Search Details button.
- Git panel: multi-select, "Pull From" to pull any branch from any remote, expandable commits in History, and the `git.graph_date_format` setting.
- New `project_panel.open_mode`, `activity_bar.position` and `activity_bar.icon_size` settings.
- Add Vietnamese (Tiếng Việt) as an interface language. Choose it in settings or set `"ui_language": "vi"`.
- Support for Intel Macs, and smaller downloads on macOS, Linux and Windows.
- Bracket pair colorization is on by default, and Quick open and the command palette open near the top of the window.
- Syntax highlighting for XML and SVG files.
- Many fixes to agent chat, the Windows installer, scrolling and theme colors.
