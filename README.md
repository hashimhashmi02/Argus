# Argus

A native Windows orchestrator for coding agents.

Run several agents — Claude Code, Codex, Cursor, Gemini CLI, or a plain shell —
in parallel, each in its own git worktree and its own real terminal session, and
see at a glance which one is **working**, which **needs you**, which is **idle**,
and which is **done**.

Windows-native, written in Rust, built on [ConPTY](https://devblogs.microsoft.com/commandline/windows-command-line-introducing-the-windows-pseudo-console-conpty/).

> **Status: early.** Step 1 of 8. The PTY layer works; there is no UI yet.
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
cargo run -p argus-pty --bin pty-smoke
```

This spawns a real shell on a pseudo-console and streams its raw output —
escape sequences and all. Type `exit` to leave.

The output is deliberately unreadable in places. That is the point: parsing
status out of a raw byte stream is unreliable, which is why step 2 puts a
headless terminal emulator in front of it. See
[DESIGN.md](DESIGN.md#why-headless-terminal-emulation).

## Acknowledgements

Inspired by [Zeus](https://github.com/nnayz/zeus), a macOS-only project in the
same problem space, whose public design docs and bug history informed several
decisions here. Argus is an independent Windows-native implementation — not a
fork or a port, and no Zeus code is reused.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE),
at your option.
