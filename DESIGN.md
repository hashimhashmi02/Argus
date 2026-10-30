# Argus — Design

This document is the source of truth for why Argus is built the way it is. It
should be updated when a decision changes, and it should explain *why* rather
than restate what the code already says.

## What Argus is

A native Windows orchestrator for coding agents. It runs several agents —
Claude Code, Codex, Cursor, Gemini CLI, or a plain shell — in parallel, each in
its own git worktree and its own real terminal session, and it reports what each
one is actually doing right now: **working**, **needs-you**, **idle**, or
**done**.

The problem it solves is attention. Running one agent is fine; you watch it.
Running five means four of them are blocked on a permission prompt you have not
noticed while you stare at the fifth. The job of Argus is to tell you which
window to look at.

### Relationship to Zeus

Argus is inspired by [Zeus](https://github.com/nnayz/zeus), a macOS-only project
in the same problem space. I read the public architecture and roadmap
documentation of Zeus for design ideas, including its bug history, which is where
several of the hardening rules below come from.

This is **not a fork or a port**. No Zeus code is copied or translated. Zeus
targets macOS and Unix PTYs; Argus is written from scratch for Windows and
ConPTY, which changes enough at the systems layer that the two implementations
have little in common below the concept level. Argus is dual-licensed
MIT / Apache-2.0.

### Secondary goal

This project is also how I am learning systems-level Rust — PTY handling,
process lifecycle, binary protocols, async I/O — coming from a Next.js /
TypeScript / Postgres background. That is a real constraint on the code, not a
footnote. Clear and idiomatic beats clever and terse; a comment explaining why a
systems-level decision was made is worth more than the three lines it saved.

## Architecture

### The daemon/client split

Argus is two processes, not one:

- **`argus-engine`** — a daemon that owns every live session. It runs detached
  from any UI.
- **clients** — `argus-cli` today, a Tauri app later. They connect, ask
  questions, and send input.

This split is the single most important structural decision in the project, and
it exists for one reason: **closing the UI must never kill a running agent.** An
agent halfway through a refactor should survive the user quitting the app, the
UI crashing, or the window manager doing something unhelpful. If sessions lived
inside the UI process, every one of those events would be a destroyed workspace.
Once sessions live in a separate process, the UI becomes what it should be — a
view — and can be closed, restarted, or replaced freely.

The cost is an IPC boundary and everything that comes with it: a wire format, a
protocol version, reconnection. That cost is paid deliberately.

### Crates

Each crate has one responsibility, and the dependency direction is strictly
downward — nothing low-level knows about anything above it.

| Crate | Responsibility |
|---|---|
| `crates/pty` | `PtyProcess` trait + ConPTY implementation. Knows nothing about agents or status. |
| `crates/terminal-emu` | Raw PTY bytes into an emulated screen grid, via `alacritty_terminal`. |
| `crates/manifest` | JSON per-agent definitions: launch, resume, env scrubbing, approve/deny keys, screen-match rules. |
| `crates/reducer` | Pure state machine. `reduce(state, screen_match, hook_signal, now) -> state`. No I/O, no async. |
| `crates/session` | One live agent: ties pty + terminal-emu + manifest + reducer together on a `tokio` task. |
| `crates/registry` | The set of known sessions + persistence to disk. |
| `crates/protocol` | Shared RPC request/response types. Zero I/O — types only. |
| `crates/control-server` | Named pipe server exposing the protocol. The whole external API of the daemon. |
| `crates/engine` | The daemon binary that wires the above together. |
| `cli/` | A small client for driving the engine by hand. |
| `manifests/` | The actual agent JSON files. |
| `app/` | Tauri UI. Does not exist yet, on purpose. |

## The decisions that matter

### Why a PTY, not a pipe

A program connected to a pipe knows it is not talking to a terminal, and every
interactive CLI changes behaviour accordingly: no colour, no spinners, prompts
skipped or failing, no window size. A pseudo-terminal makes the child believe it
is attached to a real console, so it behaves exactly as it does for a human.
The entire premise of Argus is watching what the user would have seen, so
anything less than a real PTY makes the observations wrong.

### What ConPTY actually does (found in step 1)

Three things about ConPTY are not obvious from the Unix mental model, and all
three cost real debugging time before the first checkpoint worked. They are
written down here because every layer above `crates/pty` inherits them.

**1. ConPTY interrogates the terminal before anything runs.** It sends
`ESC[6n` — Device Status Report, "where is the cursor?" — to the terminal side,
and withholds output until it gets an answer. This is ConPTY itself, not the
shell: swapping PowerShell for `cmd.exe` does not avoid it. A client that never
answers sees six bytes of output and then nothing, forever, which looks exactly
like a hung child. Argus is the terminal here, so Argus has to answer.

**2. Input written before that handshake completes is silently discarded.** No
error, no short write — the bytes simply never reach the child. A human never
notices, because a human waits for a prompt before typing. Anything automated
does notice, immediately and confusingly. Nothing may be written to a session
until it has produced output at least once.

**3. Child exit and PTY EOF are separate events.** On a Unix pty the child
exiting closes the slave and the reader gets EOF for free. On Windows the output
pipe belongs to the *pseudo-console*, not to the child, so it stays open after
the child is gone and a blocked reader waits forever. `ClosePseudoConsole` is
what ends the stream, and only the owner of the master can call it. So the
sequence has to be explicit: observe the exit, *then* close the console, *then*
join the reader. Getting this backwards deadlocks — as does dropping the master
while a writer still holds the input pipe, which is why the fields of
`ConPtyProcess` are declared in the order they must be destroyed.

### Why headless terminal emulation

Status detection runs against a *rendered screen*, never the raw byte stream.

Agent CLIs redraw themselves constantly — cursor moves, line clears, spinner
frames, in-place rewrites. So the bytes carrying "Do you want to allow this
edit?" onto the screen may be interleaved with, or later erased by, bytes that
have nothing to do with it. Matching regexes against that stream produces false
positives on text that was wiped a millisecond later, and false negatives on
text assembled from several separate writes.

Feeding the bytes through `alacritty_terminal` — the parser and grid, with no
renderer attached — yields the same screen a human would see. Matching against
that is the difference between status detection that works and status detection
that works in the demo.

Here is PowerShell echoing the word `echo`, captured from the step 1 smoke test:

```text
^[[93me^[[?25h^[[m^[[93m^Hecho ^[[37mhello-from-powershell
```

The letter `e` is printed, the cursor is moved back over it, then `echo` is
printed on top — with four colour changes along the way. A regex for `echo`
against those bytes matches something the user never saw in that form. Run it
through the emulator and it renders as `echo hello-from-powershell`, which is
what was on screen. `crates/terminal-emu/tests/screen.rs` asserts exactly that,
along with the two symmetrical failure modes: text that was printed and then
erased must *not* match, and text assembled from several separate writes must.

### A terminal is not a passive sink

The emulator does not just consume bytes; it produces them. The far end asks the
terminal questions — where is the cursor, what is your size, what colour is
index 4 — and blocks waiting for answers. Step 1 discovered this when ConPTY's
startup `ESC[6n` went unanswered and the shell never started.

So `TerminalEmulator::advance` returns the bytes a real terminal would have
written back, and the caller is obliged to send them to the PTY. It does not
perform the write itself, because owning a PTY handle would make the crate
untestable and would put a dependency edge in the wrong direction. The emulator
stays a function from `&[u8]` to a screen plus a reply; where the bytes came
from is `argus-session`'s problem in step 4.

This is also why step 1's hardcoded `ESC[1;1R` reply could be deleted. A raw
byte pipe genuinely does not know where the cursor is. The emulator does.

### Why the reducer is pure, and why `now` is a parameter

`reduce` takes the current time as an argument and never reads the clock.

That one constraint means every debounce and anti-flicker path is testable by
advancing a fake timestamp instead of sleeping. Timing tests that sleep are slow
and, worse, flaky — they encode an assumption about scheduler latency that fails
on a loaded machine. Injected time removes the sleeps and the flakiness together.

The reducer also owns three pieces of judgement that are easy to get wrong and
hard to debug once buried in async code:

- **Debounce.** One stray idle frame must not flip a working session to idle.
  Agents go quiet mid-thought all the time.
- **Blocker arbitration.** When several signals fire at once, something must
  decide which one the user is told about.
- **Subagent isolation.** A subagent finishing must not drag the parent session
  to idle. The parent is still working.

Keeping all of this in a pure function with no I/O means it can be reasoned
about, and tested, in isolation. `crates/reducer` has no dependencies at all,
and its whole test suite runs in microseconds because nothing in it sleeps.

Two rules fell out of writing it that are worth stating explicitly:

- **No match is not idle.** A frame where no rule fired is an *absence of
  evidence*, so it changes nothing — including any debounce already in
  progress. Treating it as idleness would make a session that alternates
  between "idle" and "nothing matched" flicker forever.
- **`Done` is absorbing.** A dead session's final screen is still sitting in
  the emulator's grid, still matching whatever rule it matched a moment before
  the exit. Without a guard, a finished agent argues itself back to "working"
  forever.

### Why manifests are data

Adding support for a new agent should mean adding a JSON file, not writing Rust.
Every agent-specific behaviour that would otherwise become a `match` arm inside
`session` — how to launch it, how to resume it, which key approves a prompt,
which regex means "blocked" — belongs in the manifest instead. The alternative is
a growing pile of per-agent special cases in code that only I can extend. The
rules are also the part most likely to need tuning, since CLIs change their
wording between releases, so keeping them editable is the difference between a
fix taking a minute and taking a release.

See [manifests/README.md](manifests/README.md) for the format. Three decisions
in it that are not obvious:

- **Keys are names, not bytes.** `"approve": "enter"`, not `"approve": "\r"`. A
  literal control character in a hand-edited config file is invisible in an
  editor, does not survive being pasted anywhere, and is illegal in a JSON
  string without an awkward unicode escape.
- **Unknown fields are ignored, not rejected.** A manifest written for a newer
  Argus must still load on an older one. The cost is that a typo is silently
  dropped, which is why everything the schema *does* understand is validated
  strictly, at load time, against a filename.
- **Rules carry a region.** Prompts live at the bottom of the screen, so
  restricting a blocker rule to the last few lines is the cheapest defence
  against matching a question that was answered ten seconds ago but is still
  visible.

Patterns are `regex`-crate dialect: RE2-style, so no backreferences and no
lookaround. That is a feature, not a limitation to work around — matching stays
linear in the input, so a pathological rule in user-editable data cannot hang
the daemon.

### How a session runs

`crates/session` is the only place that knows the whole pipeline exists:

```text
ConPTY bytes -> emulator -> screen -> manifest rules -> reducer -> status
                    |                                      ^
                    +---- write-backs to the PTY           |
                                           hook signals ---+
```

One `tokio` task per session, waking on three things: PTY output, a client
command, or a timer tick. Status is re-evaluated on every wakeup.

**The tick is the design, not a detail.** A debounce is a claim about elapsed
time, and elapsed time passes whether or not the child says anything. A loop
that only woke on bytes would pin a session at "working" from the moment it
genuinely went quiet — the one case the debounce exists to resolve.

**This is where the clock lives.** The reducer never reads one. The session
task holds a single `Instant` and hands the reducer a `Timestamp`. Everything
below this layer is deterministic.

**Reads and writes take different routes.** Commands travel an `mpsc` channel
because they must be ordered and applied by the task that owns the PTY. Status
comes from a `watch` channel holding the latest snapshot, which never waits on
the task. A UI listing a dozen sessions reads every one of them on each repaint,
and those reads must not queue behind a busy session's input.

**The step 1 lessons are properties of the session, not caller obligations.**
The blocking reader is on its own OS thread feeding a *bounded* channel, so a
flood of output backpressures the child rather than growing a queue. Input sent
before the child's first output is held, not lost to the unfinished handshake.
On exit the task observes it, closes the console, then drains to EOF. And
because the task alone writes to the PTY — keystrokes and emulator replies
alike — no mutex is needed anywhere.

**Dropping a `Session` does not kill the child.** That is the daemon/client
split expressed as a type: a session survives whatever was watching it.

### Live tests find what fixtures cannot

Step 3's manifest tests fed rules hand-written arrays of lines, and they all
passed. The first live session test failed immediately: a rule restricted to
`last_lines: 3` never matched an idle shell. A real emulated screen is padded
with blank rows until it fills — a fresh `cmd.exe` is three lines of content
above twenty-seven empty ones — and "the last three lines" was being counted
from the bottom of the grid. No fixture had trailing blanks, so no fixture could
have caught it.

Regions now count back from the last line with content. The broader lesson is
why `crates/session/tests/live.rs` drives a real process: the composition has
failure modes that none of its parts have on their own.

### What the client found

Building `argus-cli` before any UI was meant to flush out problems while they
were cheap, and it found four in one step — none of which any earlier test had
seen, because each only shows up when the whole system runs together.

- **A protocol gap.** The most useful thing a session list can say is "needs
  you *for four minutes*". The client cannot compute that: it knows when a
  status began relative to spawn, but not when spawn was. The engine now reports
  `status_for_ms`. Added without a version bump, deliberately, because an
  additive field with a default is misread by nobody — and that, not "did a
  struct change", is the test for whether a change needs one.
- **A busy shell read as idle.** `shell.json` had rules for idle and none for
  working. Once a shell reached idle, a long command left a screen that matched
  nothing, and the reducer correctly treats that as no new evidence — so the
  session stayed idle for the whole command.
- **An answered question still read as NEEDS YOU.** The question stayed within
  the bottom four lines after it was answered.
- **A typed pipe path did not survive the shell.** Git Bash rewrote
  `\\.\pipe\name` before the program saw it, so "engine not running" came back
  as "path not found" depending on which shell ran the command. Both binaries
  now accept a bare name.

The two manifest bugs share one cause and one fix: **for a shell, only the last
line of content says what it is doing.** A bare prompt there is idle, a question
there needs you, and anything else there is busy. A lowest-priority rule
matching any visible character expresses "anything else" without listing every
command that could be running. The general lesson for manifest authors is in
[manifests/README.md](manifests/README.md).

### Why a named pipe

Named pipes are the native Windows IPC primitive and they carry an ACL, so
access control is the job of the operating system rather than something Argus
invents. A TCP socket on localhost would be portable and would also mean any
process on the machine can talk to the daemon.

The pipe is `\\.\pipe\argus-<username>`: one per Windows user, so two people on
one machine each get their own engine.

**This API can spawn programs as the user**, so two properties are set
explicitly rather than inherited from library defaults:

- **Remote clients are rejected.** A named pipe is reachable over SMB as
  `\\host\pipe\name` unless told otherwise, and remote access to an API that
  runs programs is remote code execution.
- **The first instance must be ours.** If another process already owns the
  name, the engine refuses to start rather than becoming one more instance of an
  impostor's pipe.

Not yet done: a client verifying the server process's identity via
`GetNamedPipeServerProcessId`. Until then, a process that creates the pipe
*before* the engine starts can impersonate it. Worth closing before Argus is
trusted on a shared machine.

### The wire protocol

Length-prefixed JSON: a 4-byte big-endian length, then that many bytes of JSON.
Framing is needed because a byte-mode pipe delivers a stream with no message
boundaries; two writes can arrive as one read or be split across three, and
"one read = one message" code works on an idle machine and corrupts messages
under load.

`argus-protocol` is **sans-I/O**: its decoder never reads from anything. Bytes go
in however they arrived; complete messages come out. Server and client share one
implementation, and the awkward cases — a header split across reads, two frames
in one read — are tested with byte literals rather than a real pipe and luck.

Three rules the codec enforces:

- **A 1 MiB frame limit, checked from the header alone**, before a byte of body
  is buffered. Otherwise the length prefix is an allocation request from
  whoever is on the other end of the pipe.
- **An oversized frame is fatal for the connection; a malformed payload is
  not.** After refusing to read a frame there is no way to find where the next
  one starts. But a frame whose length was valid and whose JSON was not has
  already been consumed whole, so the client gets an error reply and keeps its
  connection.
- **Every request carries `PROTOCOL_VERSION`.** Sharing the protocol crate
  prevents drift between binaries built from the same source — and only those.
  An `argus-cli` from last month talking to this week's engine was not, and
  nothing in the type system can see it, so the engine refuses a mismatch by
  name instead of misreading the bytes.

The JSON is deliberately readable — `{"version":1,"method":"list"}` — so it can
be written by hand from any language. Step 5 was verified that way, from
PowerShell on .NET's `NamedPipeClientStream` with the framing done by hand.

### Watching a session

`watch` is the one call that streams, added in step 8b for the interactive
terminal. The engine answers `watching` and then sends event frames on that
connection until the session ends or the client hangs up:

```text
{"event":"repaint","cols":120,"rows":30,"data":"\u001b[0m...\u001b[3;12H"}
{"event":"output","data":"Reply from 127.0.0.1: bytes=32 time<1ms TTL=128\r\n"}
{"event":"ended"}
```

A watching connection is only for watching; the client opens another to type.
Mixing the two would let a request's answer queue behind a backlog of output.

**A client that arrives late needs a starting point.** The bytes that drew the
current screen were sent long ago, to nobody. So a watch starts with a
*repaint*: terminal output, generated from the emulator's grid, that turns a
blank terminal into a copy of the session's. It reproduces the colours and
attributes, and two things that are easy to forget:

- **The cursor's pen.** This is a colour that has been set but not used yet.
  Without it, the agent's next words come out in the wrong colour.
- **The input modes.** An agent that switched on application cursor keys
  expects Up as `ESC O A`. A client that never saw the switch would keep
  sending `ESC [ A`.

The tests check this by feeding a repaint into a fresh emulator and requiring
the copy's own repaint to match the original's. That covers everything the
screen text cannot show.

**The repaint and the stream must meet exactly.** Both are taken inside the
session task, between two reads from the child, so no byte is lost between them
and none arrives twice. The stream carries text, and a read can end in the
middle of a character, so each read's unfinished tail is held back and joined to
the next. That happens even when nobody is watching: a watcher who arrives
between the two halves of a character needs the first half too.

**A slow client gets a fresh start, not a backlog.** Only a small queue of events
waits for each client. When it is full, the engine stops reading the session's
broadcast for that client, which then falls behind and is told it lagged. The
engine watches again for it, which starts from a new repaint. Output that has
already scrolled away is not worth replaying.

**A client that leaves is noticed even when the session is silent.** The server
reads the watching connection while it streams, purely to see it close. Without
that read, a client that left an idle agent would only be discovered at the next
write, which could be hours away.

Not reproduced by a repaint: the scroll region, saved cursor and character set.
A TUI relying on a scroll region it set before the watcher arrived may draw a
few lines in the wrong place until its next full redraw.

### Session ids

Eight hex characters, accepted by any unique prefix the way git accepts a short
hash, because people type them. An ambiguous prefix lists the candidates; an
empty one matches nothing rather than everything. Ids mix the clock and process
id with a counter so they do not repeat across engine restarts, which matters
once step 7 persists them.

### Surviving a restart

The engine remembers its sessions in `%LOCALAPPDATA%\Argus\state.json` and
restores them on startup. **The list survives a restart; the sessions do not.**

That second half was checked, not assumed. When the engine dies — even from a
hard kill with no chance to clean up — its pseudo-consoles close and every child
process goes with them. So a session that was working when the engine stopped is
not paused somewhere waiting to be reattached; it is gone. It comes back listed as
**`lost`**, not `done`: "done" would claim it finished. `lost` is a separate flag
on the wire, not a fifth `Status`, because `Status` is the reducer's judgement of
a *live* session, and no live session is ever lost.

What the file stores is only what still means something once the engine is gone.
There is no pid, because a reused pid would point at a stranger. There is no
screen, because it would be most of the file and stale by definition. Times are
wall-clock Unix milliseconds — the one place Argus uses the wall clock — because
they must mean the same thing to a different process on a different day.

Rules the state file enforces, each with a test against a real file:

- **Unparseable: quarantine.** The file is renamed to `state.corrupt-<ms>.json`
  and the engine starts empty. Quarantine names are timestamped so a second
  corruption keeps the first one's evidence. If even the rename fails, the engine
  refuses to start rather than continue toward an overwrite.
- **Unreadable: error.** A file locked by another process says nothing about its
  contents. Quarantining it would lose history by a different route.
- **Unknown fields survive.** Every persisted struct carries a flattened map of
  fields this build does not understand, written back unchanged.
- **Writes are atomic.** The engine writes a temp file, calls `sync_all`, then
  renames it over the old one. A crash mid-save leaves the old file or the new
  one, never a corrupt file Argus made itself.
- **Unchanged means unwritten.** The persister wakes every second, so a status's
  start time is stored as creation time plus a fixed offset. "Now minus time in
  status" would drift by a millisecond per call, and an idle engine would rewrite
  the file forever.

Two orderings protect the file from a second engine, which would otherwise read
the first engine's *live* sessions, decide they were lost in a crash, and write
that down:

1. The pipe is bound before the state file is read, so a second engine on the
   same pipe fails before it looks.
2. The engine holds `state.json.lock` open with no sharing for its whole life.
   Windows enforces share modes in the kernel and releases them when a process
   dies, so a crash never leaves a stale lock behind.

### Resuming a session

A lost session — or one that simply exited — can be started again with
`resume`. It comes back **under the same id, in the same place in the list**,
running the agent's own resume command in the folder it ran in. A second entry
for the same work would leave a dead row beside a live one, and nothing would
say which to look at.

**By conversation id, not "the latest".** The obvious resume for Claude Code is
`claude --continue`, and it is wrong here: it reopens *the most recent
conversation in the current folder*. Argus runs agents side by side, often in
one folder, so three lost Claude sessions resumed that way would all reopen the
same conversation. Instead the manifest names the conversation:

```json
"launch": { "program": "claude", "new_session_args": ["--session-id", "{session_uuid}"] },
"resume": { "args": ["--resume", "{session_uuid}"] }
```

Argus makes up a UUID when the session starts, passes it in, and keeps it in
the state file as `agent_session`. `new_session_args` is a separate list from
`launch.args` because Claude refuses `--session-id` together with `--resume`
(checked: *"--session-id can only be used with --continue or --resume if
--fork-session is also specified"*). A manifest that resumes by
`{session_uuid}` without ever launching with it is rejected when it loads.

**The folder is recorded even when none was asked for.** Spawn stores the
engine's own working directory, made absolute, when the request names none.
Claude Code files conversations by folder, and "wherever the engine was
started" means something else to the next engine.

**Refused rather than guessed:** an agent with no `resume`, and sessions from
before step 9, which have no recorded folder or conversation id. Resuming those
in the wrong folder, or as "whatever was last", could quietly bring back
someone else's work. `SessionInfo.resumable` carries the engine's verdict, so a
client only has to show it.

**Two resumes at once start one process.** The registry claims the entry
before anything is spawned and releases it when the new process is attached or
the spawn fails. Without the claim, both callers would start the agent and the
loser's process would run on with no handle pointing at it. Resuming a session
that is already running returns it unchanged, as `kill` does for one that has
already stopped.

A resumed entry drops what it was restored as — it is no longer lost — but
keeps the fields from a newer Argus that this build did not understand, and its
persisted status times count from the new process rather than from creation.

Checked live with Claude Code v2.1.142 (`demo.ps1 -WithClaude`, act 10): a
session started with `--session-id`, lost in a hard-killed engine, and resumed,
came back under its own id showing the prompt it had been sent before the
crash. The same run found that Claude Code now draws its input line as `❯`
rather than `>`, which had left every idle Claude reading as working; the idle
rule accepts both.

Not covered: an agent that was lost before it wrote its conversation down.
`claude --resume <id>` then finds nothing, and the pane shows Claude's own
error. Argus does not read an agent's private files to check beforehand.

### The UI

`app/` is a Tauri v2 app with a React + TypeScript frontend, and it is **only a
client**. It owns no sessions. Closing it, or it crashing, changes nothing for
any agent.

**The webview never touches the pipe.** It calls Rust commands in
`app/src-tauri`, and those talk to the engine through the same `Client` as the
CLI. The pipe can spawn programs as the user, and exposing it to JavaScript
would put that power one XSS away from anything the UI renders. Here, the
webview can only do what a command allows. The UI also inherits everything
already learned in Rust — framing, version checks, Enter as its own keystroke —
rather than re-learning it in TypeScript.

**Approving is the engine's job, not the UI's.** The `respond` method says
*approve* or *deny*; the engine sends the keys from that agent's manifest. A UI
that hardcoded "press Enter" would be right for one agent and wrong for the rest.

**Every engine call from the UI is bounded** by a timeout. Without one, a single
request that never returned stalled a poller permanently — the list froze on
"no sessions" while the CLI listed four.

**The window is a terminal multiplexer, not a dashboard.** Every session is a
pane showing its real emulated screen, tiled tmux-style, with a status line
flagging each one (`!` needs you, `#` working, `~` idle). A session that needs
you takes the left 60% of the window; Enter zooms a pane to full size, `y` or `n`
answers it, Esc tiles back. Panes keep their creation order, so nothing moves
under your eye except the one that rang. The first version was a session list
beside one selected screen, which meant reading a list to find the blocked
agent; tiling every screen means the screen itself is what you glance at. The
visual system — palette, type, the rule that yellow appears only where you can
act — is recorded in [app/DESIGN.md](app/DESIGN.md).

**The zoomed pane is the real terminal.** Tiles poll the text-only screen,
which is enough for a glance. A zoomed live session is shown in xterm.js,
started from the engine's `watch`: it gets every byte the agent prints, in full
colour, and everything typed there goes straight to the agent. Its events reach
the webview over a Tauri channel. The Rust side adds one event of its own,
`disconnected`, for a stream that stopped without the session ending, so the
webview can tell "finished" from "lost contact" and try again.

Three details shape it:

- **The session's terminal is resized to fit the pane,** as tmux sizes a window
  to its client. The tiles then show it at that size, scaled down.
- **Keystrokes are sent strictly one at a time.** Each one is an engine call, and
  two calls in flight together could arrive in the wrong order. Text typed
  while one call is in flight is queued and sent together. Anything containing
  a control character is sent on its own, so a TUI never reads Enter as part of
  a paste.
- **Getting out needs a key no agent uses.** Esc and Ctrl+C belong to the agent
  while you type. So, as in tmux, there is a prefix: Ctrl+B, then `d` to stop
  typing, `z` to tile back, `y` / `n` to answer, or a pane number. Ctrl+B
  pressed twice sends a real Ctrl+B.

A bell that rings while you are zoomed into another pane is held back, not
dropped. Your keyboard stays where it was. When you tile back, the waiting
pane becomes current, so the next `y` answers it.

### Starting the engine: Job objects

When the engine is not running, the UI starts it detached. The first attempt
used `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, and the engine died anyway
the moment the UI exited.

The reason is a Windows **Job object**. A child process inherits its parent's
job no matter how detached it is from the console, and when a job set to
kill-on-close closes, every process in it dies. `cargo run`, and so
`tauri dev`, runs programs in exactly such a job, so Ctrl+C can clean everything
up. The engine is now started with `CREATE_BREAKAWAY_FROM_JOB`, which is the only
way a child outlives a job.

Breaking away is not always allowed: a job can forbid it, and then
`CreateProcess` fails outright. The UI falls back to starting the engine inside
the job, with a warning, because a working engine that dies with its launcher
beats no engine. Launched from the Start menu or Explorer, the UI is in no job,
and the breakaway is a no-op.

Verified in the real app: start the engine from the UI, run three shells (one
mid-command), close the window — the engine and all three keep running — and
reopen the UI straight onto them.

### Installing

A release is **one zip**, built by `scripts/package.ps1` and published by the
`Release` workflow when a tag matching the workspace version is pushed
(`git tag v0.2.0 && git push origin v0.2.0`); a tag that disagrees with
`Cargo.toml` fails the run before anything is built. The same workflow builds
the zip without publishing on a pull request that touches the packaging. It
holds `Argus.exe`
(the app), `argus-engine.exe`, `argus-cli.exe` and `manifests\`, side by side,
with a `SHA256SUMS` beside it. `install.ps1` is the installer, run as
`irm …/install.ps1 | iex`:

- **Per user, no admin.** It unpacks into `%LOCALAPPDATA%\Programs\Argus`, adds
  that folder to the *user* PATH, and adds a Start menu shortcut. Nothing is
  written outside the user's profile, so nothing needs elevation — and an
  installer that does not ask for admin cannot do anything that needed it.
- **Checked before it is unpacked.** The zip is hashed and compared with the
  release's `SHA256SUMS`; a mismatch stops the install.
- **It never stops the engine.** Upgrading means replacing files the running
  engine holds open, and stopping the engine stops every agent session it
  owns. So the installer refuses while anything runs from the install folder,
  and says how to stop it. Choosing when to lose live sessions is the user's
  call.
- **It only deletes what it made.** The install folder carries a marker file,
  and an existing folder without one is refused rather than replaced, so a
  mistyped `-InstallDir` cannot wipe something else. Uninstalling leaves the
  session history in `%LOCALAPPDATA%\Argus` unless asked to `-Purge` it.
- **PATH is edited as text, in the registry.** The obvious call,
  `[Environment]::SetEnvironmentVariable("Path", …, "User")`, reads
  `%USERPROFILE%\…` entries already expanded and writes the value back as a
  plain string, freezing every one of them — found by testing this installer
  against a real profile. It reads the raw value, keeps its registry type,
  adds or removes only its own entry, and broadcasts the change so the next
  terminal sees it. An install and uninstall leave the value byte-identical.
- **Everything is inside a function.** Piped into `iex`, a script has no path
  and cannot take arguments, so the file defines `Install-Argus` and calls it
  with `@args`. The same file then works piped, as a script block with
  arguments, and run from a clone.

Two things the installed layout needed from the code. The engine looks for
`manifests\` beside its own executable before the working directory, so
`argus-engine` works from any folder. And the app starts the engine in the
user's home: launched from the Start menu, the app's own working directory is
wherever Explorer chose, and a session spawned without a folder is recorded as
running in the engine's.

The same zip is also published to npm, as `argus-agents`, for people who
already have Node. The package holds no binaries: its `postinstall` downloads the
release named after its own version, checks it against `SHA256SUMS` exactly as
`install.ps1` does, and unpacks it with Windows' own `Expand-Archive`, so the
package has no dependencies to audit. `argus`, `argus-cli` and `argus-engine`
are small Node launchers that run the unpacked programs. Because the version is
the link between the two, `package.ps1` refuses to build when `npm\package.json`
disagrees with `Cargo.toml`.

### Worktrees

Two agents in one checkout edit the same files and trip over each other's
changes. `spawn` takes an optional branch, and the engine runs that agent in a
git worktree on the branch instead of in the folder it was given:

- **One worktree per branch.** If any worktree already has the branch checked
  out, that is where the session runs. That covers one Argus made for an
  earlier session, and the main checkout itself. Git would refuse a second
  checkout of the branch anyway, and the person asked to work on that branch.
  A branch that doesn't exist is created from `HEAD`.
- **Outside the repository.** New worktrees go under `worktrees\<repo>\<branch>`
  beside the state file. Inside the repository they would show up as untracked
  folders in the main checkout. The path is recorded in Windows form
  (`C:\src`, not git's `C:/src`), so the same folder is always the same string.
- **Kept after the session.** Killing an agent does not delete what it wrote.
  Removing a worktree is `git worktree remove`, after the branch is merged or
  abandoned. That's a decision about the work, which Argus doesn't make.
- **Recorded with the session.** The branch is persisted beside the folder, so
  the list still shows it after a restart, and a resume lands back in the
  worktree because that's the session's folder.

Git runs under hardening rule 1, mostly: no stdin, `GIT_TERMINAL_PROMPT=0`, no
credential dialog, no console window, and a 60-second deadline. The user's git
config is kept, though. A worktree checked out without their `core.autocrlf`
would not match the line endings of their main checkout.

### Stopping the engine

`shutdown` kills every live session, answers, and then stops the engine. The
sessions are killed first, not left to die with the engine, so they are
remembered as finished with an exit code rather than as `lost`. `lost` means
the engine went away unexpectedly, and a deliberate stop is not that. They stay
resumable where their agent allows it. The stop signal is sent a moment after
the reply, so the reply reaches the client before the process starts to exit.
The CLI sends it as `argus-cli shutdown`; the window asks first, on `Q` or a
click on "engine up".

## Hardening rules

These are not hypothetical. They come from the bug history of Zeus and from the
failure modes of the primitives involved.

1. **Git subprocesses must never inherit stdin, must have a bounded timeout, and
   should avoid inheriting ambient user/system git config.** A `git` command that
   can prompt is a `git` command that can hang forever — silently, holding a
   lock, with no output explaining itself.

2. **Blocking work never runs on the async cooperative pool.** PTY reads and
   named-pipe `accept()` genuinely block. Parked on a tokio worker they starve
   every other task, and on a low-core machine that can be the whole runtime.
   Use `spawn_blocking` or a dedicated thread. This rule shows up as early as
   `crates/pty`, where the reader is deliberately kept off the `PtyProcess`
   trait so that nobody can call it from a task by accident.

3. **The persisted state file is a compatibility surface.** Other processes and
   future versions of Argus have to keep reading it, so deserialization is
   permissive and preserves unknown fields — explicitly *not*
   `#[serde(deny_unknown_fields)]`. A file that fails to parse is **quarantined**
   (renamed aside), never treated as empty and overwritten; silently starting
   fresh on a parse error destroys the session history of the user at exactly the
   moment they need it most. Implemented in step 7 — see
   [Surviving a restart](#surviving-a-restart).

4. **Spawned agents do not inherit the identity of Argus.** Environment variables
   are scrubbed by prefix before spawn, so credentials and identity meant for the
   orchestrator never reach the thing being orchestrated. Prefix matching is
   case-insensitive, because Windows environment variable names are.

## Build order

Each step is a working, runnable checkpoint — not a half-finished layer.

| # | Step | State |
|---|---|---|
| 1 | `pty`: spawn a shell via ConPTY, stream raw bytes | **done** |
| 2 | `terminal-emu`: same stream through `alacritty_terminal`, print the grid | **done** |
| 3 | `reducer` + a hand-written `claude-code.json`, with fake-clock unit tests | **done** |
| 4 | `session`: steps 1-3 on a `tokio` task, exposing status/send/resize/kill | **done** |
| 5 | `protocol` + `control-server`: RPC types and a named pipe server | **done** |
| 6 | `cli`: spawn / list / screen / kill against the running engine | **done** |
| 7 | `registry` persistence: the session list survives an engine restart | **done** |
| 8a | `app/`: Tauri UI — every screen tiled live, approve/deny, spawn, kill | **done** |
| 8b | `app/`: interactive terminal (xterm.js) over a streaming `watch` RPC | **done** |
| 9 | `resume`: a lost or finished session started again, same id, same conversation | **done** |
| 10 | Packaging: a release zip, and a one-line `install.ps1` that installs it | **done** |
| 11 | Worktrees per session, `shutdown`, notifications, and `npm install -g argus-agents` | **done** |

The UI is last on purpose. A UI built on an unstable protocol locks in the
mistakes of that protocol.

### On `portable-pty` vs raw ConPTY

`crates/pty` uses `portable-pty` (the crate behind WezTerm) rather than calling
`CreatePseudoConsole` through raw Win32 FFI. That is a sequencing decision, not a
permanent one: the goal is a working system first, and reimplementing the PTY
layer against raw ConPTY is a worthwhile exercise *once everything above it works
and can prove the replacement behaves identically*. The `PtyProcess` trait exists
partly to keep that door open — which is also why its signatures name types owned
by Argus instead of types owned by `portable-pty`.
