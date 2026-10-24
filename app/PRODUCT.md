# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Primary: the author — a full-stack developer (Next.js / TypeScript background) running several coding agents in parallel on Windows, each in its own git worktree and real terminal session: Claude Code, Codex, Cursor, Gemini CLI, or a plain shell.

Later: other Windows developers who find Argus on GitHub. The design must hold up for someone who did not build it, but first-run polish is secondary to daily use for now.

## Product Purpose

Tell you which agent needs you.

Running one agent is fine — you watch it. Running five means four of them are sitting on a permission prompt you have not noticed while you stare at the fifth. Argus watches every session and says, at a glance, which are working, which need you, which are idle, and which are done — and lets you answer a blocked one without switching windows.

Success: no agent ever waits on you unnoticed, and answering one takes seconds.

## Positioning

- **Status comes from the screen the agent actually shows**, rendered through a headless terminal emulator and classified by per-agent rules — not by pattern-matching raw output, which breaks on every redraw.
- **Windows-native**, built on ConPTY, where the category's existing tools are macOS/Unix-first.
- **The UI is only a view.** Sessions live in a background engine; closing, crashing or restarting the window never touches an agent.
- **Agents are data.** Supporting a new agent is a JSON manifest, including the keys that approve or deny its prompts.

## Operating Context

Used both ways, equally:

- **Peripheral:** open on a second monitor while the developer works in an editor. They glance over when something changes, act, and look away. Must be readable at a glance from across the desk.
- **Up close:** switched to and worked inside — reading an agent's screen, typing into it, approving or denying prompts, spawning and killing sessions.

A Tauri desktop window (WebView2) on Windows 11, minimum 900×560. The engine is a separate process; the UI polls it several times a second. Typical load is several concurrent sessions (inferred from the product's framing of "five agents"; not confirmed as an upper bound).

## Capabilities and Constraints

Per session: an 8-character id (any unique prefix is accepted elsewhere, e.g. the CLI), the agent it was spawned from, its status and how long it has held it, the manifest rule that decided it, pid, and exit code.

Statuses: **working**, **needs you**, **idle**, **done**, plus **lost** — a session that was still running when a previous engine stopped and died with it. Lost is not done and must never read as finished.

Actions: spawn (agent + optional working directory), type into a zoomed pane's live terminal, send a line (Enter sent as its own keystroke), send a named key (Enter, Esc, Ctrl+C, arrows), approve / deny (the engine sends that agent's own keys from its manifest), kill, resume a finished or lost session (same id, same folder, same conversation; the engine says which sessions can be), and start the engine when it is not running.

The tiled screens are the emulated terminal as plain text, polled; a zoomed live pane is the real terminal, streamed in full colour.

Installed per user by a one-line PowerShell installer (Start menu shortcut, no admin), from a release zip.

Not yet built: stopping the engine from the UI, code signing, notifications beyond a taskbar flash and a count in the window title.

Terminology: session, agent, engine, manifest, rule, needs you, lost, resumable.

## Brand Commitments

Name: **Argus**. No logo, palette, or typeface has been committed; the icons in `src-tauri/icons/` are Tauri template placeholders, not brand assets.

## Evidence on Hand

- Real captured screens usable as demonstration content: Claude Code v2.1.142's TUI (welcome box, input box, spinner, permission flow), PowerShell, and cmd.exe — see the repository's tests and `scripts/demo.ps1`.
- No users, testimonials, metrics, or screenshots of third-party adoption exist. Do not invent them.

## Product Principles

1. **"Needs you" is the only call to action.** Everything else recedes until something is blocked.
2. **The screen is the truth.** Show what the agent shows, not an interpretation of it; status always names the rule that decided it.
3. **Closing the window costs nothing.** The UI is a view onto the engine, never the owner of an agent.
4. **Readable from across the desk, workable up close.** Both scenes are first-class.
5. **Honest states.** Lost is not done; "no rule matched" is not idle.
