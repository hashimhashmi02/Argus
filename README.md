# Argus

A native Windows orchestrator for coding agents.

Run several agents — Claude Code, Codex, Cursor, Gemini CLI, or a plain shell —
in parallel, each in its own git worktree and its own real terminal session, and
see at a glance which one is **working**, which **needs you**, which is **idle**,
and which is **done**.

Windows-native, written in Rust, built on [ConPTY](https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/).

> **Status: early.** Step 8a. The engine runs as a daemon, a desktop dashboard
> and `argus-cli` both drive it, and the session list survives an engine
> restart. The dashboard shows each agent's screen read-only; a fully
> interactive terminal is step 8b.
> See [DESIGN.md](DESIGN.md) for the architecture and the build order.

## Why

Running one agent is fine — you watch it. Running five means four of them are
sitting on a permission prompt you have not noticed while you stare at the fifth.
Argus tells you which window to look at.

Sessions live in a background daemon rather than in the UI process, so closing
the Argus window never kills a running agent.

## Try the current checkpoint

Requires the Rust MSVC toolchain (`rustup default stable-x86_64-pc-windows-msvc`)
and the Visual Studio Build Tools C++ workload for the linker.

### The desktop app

```powershell
cd app
npm install
npm run tauri dev
```

The first build compiles Tauri and takes a few minutes. If no engine is running,
the app offers to start one, detached, so closing the window never stops an
agent. Sessions are listed most-urgent first; select one to see its screen, and
answer it with **Approve** / **Deny** or by typing a line.

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
| `kill <id>` | Stop it |

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
