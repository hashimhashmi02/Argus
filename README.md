# Argus

A native Windows orchestrator for coding agents.

Run several coding agents in parallel, each in its own real terminal session and,
if you like, its own git worktree, and see at a glance which one is **working**,
which **needs you**, which is **idle**, and which is **done**.

Claude Code and PowerShell work out of the box. Any other terminal agent is one
JSON manifest away: how to launch it, how to tell its screens apart, and which
keys answer its prompts. See [manifests/](manifests/README.md).

Windows-native, written in Rust, built on [ConPTY](https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/).

> **Status: 0.2.** The engine runs as a daemon, and both the desktop app and
> `argus-cli` drive it. Each agent can get a git worktree on a branch of its
> own. The session list survives an engine restart, and a session lost in a
> crash can be resumed back into the same conversation. The app tiles every
> agent's screen, puts the one that needs you up front, and sends a Windows
> notification when you're looking elsewhere. A zoomed pane is the agent's real
> terminal, live and typeable.
> See [DESIGN.md](DESIGN.md) for the architecture and the build order.

## Why

Running one agent is fine — you watch it. Running five means four of them are
sitting on a permission prompt you have not noticed while you stare at the fifth.
Argus tells you which window to look at.

Sessions live in a background daemon rather than in the UI process, so closing
the Argus window never kills a running agent.

## Install

With npm, on Windows 10 or 11:

```powershell
npm install -g argus-agents
```

```powershell
argus
```

`argus` opens the desktop app, and `argus-cli` and `argus-engine` land on your
PATH too. The package holds no binaries of its own: installing it downloads
the matching release zip from GitHub and checks it against that release's
`SHA256SUMS` before unpacking. If npm skips install scripts, that happens the
first time you run one of them instead. `npm uninstall -g argus-agents` removes
it.

Or without Node, one line in PowerShell:

```powershell
irm https://raw.githubusercontent.com/hashimhashmi02/Argus/main/install.ps1 | iex
```

It downloads the latest release, checks it against the release's `SHA256SUMS`,
unpacks it into `%LOCALAPPDATA%\Programs\Argus`, puts `argus-cli` and
`argus-engine` on your user PATH, and adds **Argus** to the Start menu. No admin
rights, nothing outside your user profile. Windows 10 or 11, x64; the window
needs the WebView2 runtime, which Windows 11 already has.

A specific version, or to uninstall (your session history in
`%LOCALAPPDATA%\Argus` stays unless you add `-Purge`):

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/hashimhashmi02/Argus/main/install.ps1))) -Version 0.2.0
```

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/hashimhashmi02/Argus/main/install.ps1))) -Uninstall
```

The installer will not upgrade or uninstall while the engine is running from
the install folder, because stopping the engine stops every agent session it
owns. It tells you how to stop it when you are ready.

The binaries are not code-signed yet, so Windows SmartScreen may ask before the
first launch.

## Build from source

Requires the Rust MSVC toolchain (`rustup default stable-x86_64-pc-windows-msvc`)
and the Visual Studio Build Tools C++ workload for the linker.

To build the same zip a release ships, and install from it:

```powershell
.\scripts\package.ps1
```

```powershell
.\install.ps1 -FromZip dist\argus-0.2.0-x86_64-pc-windows-msvc.zip
```

A release is that zip and its `SHA256SUMS`, published under a version tag that
matches the version in `Cargo.toml` — which is what the one-line installer
downloads:

```powershell
gh release create v0.2.0 dist\argus-0.2.0-x86_64-pc-windows-msvc.zip dist\SHA256SUMS --title "Argus v0.2.0" --generate-notes
```

Then the npm package, whose version must match. It downloads that release
when installed, so publish it after the release exists:

```powershell
cd npm
```

```powershell
npm publish
```

To try the npm package against a local build first, point it at the zip:

```powershell
$env:ARGUS_ZIP = "$PWD\dist\argus-0.2.0-x86_64-pc-windows-msvc.zip"; npm install -g .\npm
```

### The desktop app

```powershell
cd app
npm install
npm run tauri dev
```

The first build compiles Tauri and takes a few minutes. If no engine is running,
the app offers to start one, detached, so closing the window never stops an
agent.

The window works like tmux. Every session is a pane showing its real screen,
and the status line along the bottom flags each one: `!` needs you, `#` working,
`~` idle. A session that needs you takes the left of the window in yellow.

| Key | Does |
|---|---|
| `0`–`9`, arrows | Pick a pane |
| `Enter` / `Esc` | Zoom the pane to the full window / tile every pane again |
| `y` / `n` | Approve or deny the pane that needs you |
| `i` | Type into the pane's live terminal, zooming to it first |
| `:` | Type one line into the pane |
| `c` | Start a new session |
| `x` | Kill the pane's session, after asking |
| `r` | Resume the pane's finished or lost session |
| `Q` | Stop the engine and every session, after asking |

`c` asks for an agent, an optional working folder (`-c`), and an optional
worktree branch (`-b`). With a branch, the agent runs in a git worktree of that
folder's repository instead of the folder itself. A pane on a worktree shows
its branch in magenta.

When a session starts needing you and the Argus window isn't in front, Windows
shows a notification, the taskbar button flashes, and the window title counts
how many are waiting.

A zoomed pane is the agent's real terminal, in full colour. Press `i` or click
it, and everything you type goes to the agent, Esc and Ctrl+C included. To get
back out, use the tmux prefix, Ctrl+B, followed by a command key:

| After Ctrl+B | Does |
|---|---|
| `d` | Stop typing |
| `z` | Stop typing and tile every pane again |
| `y` / `n` | Answer the pane that needs you |
| `0`–`9` | Pick a pane |
| Ctrl+B | Send a real Ctrl+B to the agent |

With text selected in the terminal, Ctrl+C copies it, and Ctrl+V pastes.

Everything also works with the mouse: click a pane or its status-line entry,
double-click to zoom, and click `y` / `n` in a waiting pane.

### The demo script

The quickest way to see the engine on its own is the narrated demo. It builds everything, runs a
private engine against a scratch state file, and walks through three shells at
once, answering a blocked one from the CLI, and surviving a crash:

```powershell
.\scripts\demo.ps1
```

Add `-WithClaude` for a fourth act that drives a real Claude Code session. That
sends one small prompt on your Claude account, and needs `claude` logged in.

To drive it by hand instead, start the engine in one terminal:

```bash
cargo run -p argus-engine
```

It loads every manifest in `manifests/` and listens on a named pipe for your
user. Then drive it from another:

```bash
cargo run -p argus-cli -- spawn shell
```

```bash
cargo run -p argus-cli -- list
```

```text
ID        AGENT  STATUS     FOR  PID    RULE
c91c1b08  shell  NEEDS YOU  2s   17776  confirm-prompt
ecec9277  shell  working    3s   28056  busy
496b768b  shell  idle       5s   27032  powershell-prompt
```

The most urgent session is always at the top. The rest of the commands, with
ids accepted by any unique prefix:

| Command | Does |
|---|---|
| `screen <id>` | Show the session's screen exactly as you would see it |
| `send <id> <text...>` | Type into it and press Enter (`--no-enter`, or `--key escape` to send a key) |
| `resize <id> <cols> <rows>` | Change its terminal size |
| `watch <id>` | Mirror its terminal here, live and in colour, read-only, until it ends or Ctrl+C |
| `kill <id>` | Stop it |
| `resume <id>` | Start a finished or lost session again, same id, same folder, same conversation |
| `shutdown` | Stop every session, then the engine. They are recorded as finished, not lost |

### Worktrees

Two agents editing one checkout trip over each other's changes. Give each one a
branch and Argus runs it in a [git worktree](https://git-scm.com/docs/git-worktree)
of its own:

```bash
cargo run -p argus-cli -- spawn claude-code --cwd C:\src\shop-api --worktree feature/login
```

```bash
cargo run -p argus-cli -- spawn claude-code --cwd C:\src\shop-api --worktree fix/cart-total
```

The branch is created from the repository's `HEAD` if it doesn't exist yet.
Worktrees go in `%LOCALAPPDATA%\Argus\worktrees\<repo>\<branch>`, outside
your repository, so they never show up there as untracked files. A second session
on the same branch shares its worktree, and the branch your main checkout is on
runs in the main checkout. Killing a session leaves its worktree and everything
the agent wrote. Merge the branch as usual, then remove the worktree with
`git worktree remove`. `list` gains a `BRANCH` column as soon as any session has
one.

Add `--json` to any command for the engine's raw reply. Exit codes are 0 for
success, 1 if the engine refused, 2 for bad usage, and 3 if no engine is
running.

The engine remembers its sessions in `%LOCALAPPDATA%\Argus\state.json`, so the
list is still there after a restart. The sessions themselves stop with the
engine, so any that were still running come back as `lost`:

```text
ID        AGENT  STATUS    FOR  PID  RULE
00efdcf5  shell  done (0)  4s   -    -
b90097bc  shell  lost      0s   -    -
```

A lost session whose agent can resume is marked `*` in the list, and
`resume <id>` brings it back under the same id. For Claude Code that is the same
conversation, not just the latest one in the folder: Argus starts every Claude
session with a conversation id of its own and asks for that id back. See
[DESIGN.md](DESIGN.md#resuming-a-session).

If the state file is ever unreadable, it is moved aside — never overwritten — and
the engine says where it went.

The protocol underneath is plain length-prefixed JSON, so anything that can open
a named pipe can drive the engine; see
[DESIGN.md](DESIGN.md#the-wire-protocol).

To watch a single session's status in-process, without the engine:

```bash
cargo run -p argus-session --example watch
```

Spawns the agent described by `manifests/shell.json`, forwards your typing to
it, and prints every status transition with the manifest rule responsible:

```text
[  0.00s] Working   rule=-
[  3.16s] Idle      rule=powershell-prompt
[  3.20s] Done      rule=-  exit=0
```

Pass a different manifest to watch another agent:

```bash
cargo run -p argus-session --example watch -- manifests/claude-code.json
```

Earlier checkpoints are still there, one layer at a time. The emulated screen
alone, redrawn as it changes:

```bash
cargo run -p argus-terminal-emu --example screen
```

And the raw byte stream with no emulator in front of it, which is the clearest
argument for why the emulator exists:

```bash
cargo run -p argus-pty --bin pty-smoke
```

The raw output is deliberately unreadable in places. Characters get overwritten
in place, text is printed and then erased, and words arrive one letter at a time
between colour changes. Matching a status out of that is unreliable; matching it
out of the rendered screen is not. See
[DESIGN.md](DESIGN.md#why-headless-terminal-emulation).

## Acknowledgements

Inspired by [Zeus](https://github.com/nnayz/zeus), a macOS-only project in the
same problem space, whose public design docs and bug history informed several
decisions here. Argus is an independent Windows-native implementation — not a
fork or a port, and no Zeus code is reused.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE),
at your option.
