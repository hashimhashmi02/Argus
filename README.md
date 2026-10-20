# Argus

A native Windows orchestrator for coding agents.

Run several agents — Claude Code, Codex, Cursor, Gemini CLI, or a plain shell —
in parallel, each in its own git worktree and its own real terminal session, and
see at a glance which one is **working**, which **needs you**, which is **idle**,
and which is **done**.

Windows-native, written in Rust, built on [ConPTY](https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/).

> **Status: early.** Step 5 of 8. The engine runs as a daemon, owns live agent
> sessions, and serves a control API over a named pipe. There is no command-line
> client yet (step 6), sessions do not survive an engine restart (step 7), and
> there is no UI.
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

Start the engine:

```bash
cargo run -p argus-engine
```

It loads every manifest in `manifests/` and listens on
`\\.\pipe\argus-<your username>`. There is no client binary yet — that is step
6 — but the protocol is plain length-prefixed JSON, so anything that can open a
named pipe can drive it. A request looks like this:

```json
{"version":1,"method":"spawn","agent":"shell"}
```

See [DESIGN.md](DESIGN.md#the-wire-protocol) for the framing and
`crates/protocol` for every request and reply.

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
