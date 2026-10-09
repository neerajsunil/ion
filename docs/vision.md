# Vision

> **Ion: the lightweight IDE built around your terminal agent.**
> Your harness does the coding. Ion lets you watch it, review it and steer it,
> and stays out of the way otherwise.

## The problem

More and more code is written by agents running in a terminal: Claude Code,
Codex, Gemini CLI and whatever comes next. The developer's job is moving from
typing code to directing and reviewing it.

Today's editors don't fit that workflow:

- **Cursor, Windsurf and Zed** each push their own built-in agent. That means
  lock-in, and paying twice for AI you already have in your terminal.
- **VS Code** works with terminal agents, but it is heavy, and the terminal and
  change review are afterthoughts.

## What Ion is

Ion is the IDE you keep open next to your agent. It doesn't try to be the AI.
It is the best place to work alongside whichever one you choose.

1. **Terminal first.** The terminal is where the agent lives: GPU-rendered,
   with tabs and splits, and badges when an agent finishes or needs input.
2. **Live changes.** Open files reload as the agent edits them, and the file
   tree and gutter show what changed.
3. **Review and control.** Every file the agent touched, as diffs. Revert any
   hunk, checkpoint before a task, roll back after.
4. **Context bridge.** Send a selection or `file:line` into the agent's prompt,
   click paths in terminal output, and support harness IDE integrations where
   they exist.
5. **Minimal and efficient.** Near-zero idle usage, so the machine's resources
   go to agents, builds and tests.

## Non-goals

Ion deliberately does **not** include:

- a built-in agent loop, chat UI, or model/provider integrations
- an extension marketplace
- a debugger
- real-time collaboration

LSP support may come later, and only if users ask for it.

## Principles

- **Minimal.** Every feature has to earn its place. When in doubt, leave it out.
- **Harness-agnostic.** Work with any terminal agent through plain, standard
  mechanisms (text, escape codes, files, git) before any harness-specific
  integration.
- **Efficient by design.** Idle means ~0% CPU: no polling, no redraw loops, no
  timers that run while nothing happens. Under load, scale out across all
  cores, then go back to sleep. See [performance.md](performance.md).
- **Modern platforms only.** Windows 11 x64 today and Apple Silicon next. No
  legacy OS or hardware support.
