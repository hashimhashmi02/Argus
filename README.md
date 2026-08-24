# Argus

A native Windows orchestrator for coding agents.

Run several agents — Claude Code, Codex, Cursor, Gemini CLI, or a plain shell —
in parallel, each in its own git worktree and its own real terminal session, and
see at a glance which one is **working**, which **needs you**, which is **idle**,
and which is **done**.

Windows-native, written in Rust, built on [ConPTY](https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/).

> **Status: early.** Step 3 of 8. The PTY layer, headless terminal emulation,
> the status state machine and the agent manifests all work — but nothing has
> wired them together into a live session yet, and there is no UI.
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

```bash
cargo run -p argus-terminal-emu --example screen
```

Spawns a real shell on a pseudo-console, feeds its output through a headless
terminal emulator, and redraws the emulated screen whenever it changes. Type
`exit` to leave.

To see why that emulator is necessary, run the step 1 checkpoint instead — the
same stream, unparsed:

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
