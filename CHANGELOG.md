# Changelog (Linux)

## 0.7.0

- **Click to continue.** Selecting a session restored at launch now starts its agent and continues the conversation, without pressing ▶. This includes the session that was selected when omux quit. Settings › Sessions › Continue a session when you select it turns this off.
- **Open Terminal Here.** Right-click a session for a plain terminal in the same project and folder (its worktree, for a worktree session).

## 0.6.0

- **Sessions are named after their conversation.** Claude Code gives each conversation a short title, such as "Expo build plan". omux now uses it as the session's name instead of the first words of the first prompt, and keeps it current. A title set with `/rename` wins. A name you gave a session yourself is never replaced.
- Search results show these titles too.

## 0.5.0

- **Search inside conversations.** The Conversations sheet (Ctrl+Alt+O, or the clock button) searches the whole text of every Claude Code conversation, not only its first prompt. Case and accents don't matter, so `kopek` finds `köpek`. Each result shows the text around the match.
  - omux keeps the text in `~/.cache/omux/conversations.json` and reads only what was added since the last search. The first read happens in the background shortly after launch.
- **Ask Claude.** Type a question such as "the conversation where we ported omux to Linux" and press Ask Claude. Claude Haiku reads a short digest of each conversation (folder, age and your messages) and picks the matching ones by meaning. It runs `claude -p` with no tools and saves no session.

## 0.4.0

- **Show and order sessions.** A new button at the top of the sidebar:
  - **All Conversations, Latest First:** every session in one list, without project headers, ordered by its latest activity. Each row names its project.
  - **Order Projects › Latest Activity First:** projects, and the sessions in them, ordered by their latest activity. Manual keeps the order you drag them into.
  - Latest activity is the last write to the Claude Code conversation, or the last status change. The list re-sorts by itself, but not while you hover over it or rename something.
  - Next and previous session follow the order on screen.

## 0.3.0

- **Skip permission prompts.** A new switch in Settings › Sessions. When it is on, Claude Code sessions start with `--dangerously-skip-permissions`, and Codex sessions run without approvals or a sandbox. It is off by default and applies to sessions started after you turn it on.

## 0.2.0

- **Your Claude Code conversations are already there.** At the first launch, omux adds every Claude Code conversation on the computer to the sidebar: one project per folder, newest first. The new projects start collapsed. Nothing runs until you select a session, and selecting one continues its conversation. Main menu › Import Claude Code Conversations adds conversations started since then.

## 0.1.0

First Linux release. It covers the features of omux 0.6.1 for macOS:

- **Sessions:**
  - Projects and sessions in a sidebar. Sessions can be renamed, colored, dragged and moved between projects.
  - Claude Code sessions keep their pinned conversation. Restart and Continue resume it.
  - Live status (working, needs you, done) from Claude Code's OSC 7501 reports.
  - Allow and Deny from the sidebar and the header.
  - Subagents with their transcripts.
- **Codex:** managed sessions on a private app-server per session, with status, approvals, subagents, usage limits and recent conversations.
- **Worktrees:** worktree sessions with a setup command, and close, merge, push and clean-up flows. Pull request status and checks come from `gh`.
- **Panels:**
  - Files: mentioned files and previews.
  - Changes: the diff and review notes sent to the agent.
  - Browser: local servers, plus an element picker that adds the element to the prompt.
- **Notifications and attention:**
  - Desktop notifications for sessions that need you or finish, and for programs that send OSC 9 or OSC 777. Choose banner and sound, or sound only.
  - The count of sessions needing attention shows on the dock icon.
- **Input:** paste an image as a file path, and paste or drop files as shell-escaped paths. Ctrl+click opens paths and URLs.
- **Settings and desktop:**
  - 11 themes and a choice of terminal font. Settings, a welcome sheet and a keyboard shortcuts list.
  - One instance. `omux <folder>` and Files › Open With open folders as projects.
  - `.deb`, a tarball with `install.sh`, a desktop entry and AppStream metadata.
