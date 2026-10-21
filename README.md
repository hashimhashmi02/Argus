# Argus

A native Windows orchestrator for coding agents.

Run several agents — Claude Code, Codex, Cursor, Gemini CLI, or a plain shell —
in parallel, each in its own git worktree and its own real terminal session, and
see at a glance which one is **working**, which **needs you**, which is **idle**,
and which is **done**.

Windows-native, written in Rust, built on [ConPTY](https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/).

> **Status: early.** Step 6 of 8. The engine runs as a daemon and `argus-cli`
> drives it end to end. Sessions do not yet survive an engine restart (step 7),
> and there is no UI.
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

Start the engine in one terminal:

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
