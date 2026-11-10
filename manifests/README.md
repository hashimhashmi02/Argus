# Agent manifests

One JSON file per kind of agent. Adding support for a new one should mean adding
a file here, not writing Rust — see
[DESIGN.md](../DESIGN.md#why-manifests-are-data).

Loaded and validated by [`argus-manifest`](../crates/manifest). Every file here
is checked at build time by that crate's tests, so a broken pattern or a
duplicate rule name fails CI rather than a session.

## Fields

| Field | Meaning |
|---|---|
| `id` | Stable identifier used on the command line and in the registry. |
| `display_name` | What the UI shows. |
| `launch.program` / `launch.args` | How to start it, fresh or resumed. |
| `launch.new_session_args` | Appended to `launch.args` for a fresh session only. |
| `launch.on.<platform>` | Per-platform overrides of the three fields above. See below. |
| `resume.args` | Appended to `launch.args`, in place of `new_session_args`, to reattach to previous work. Omit if the CLI cannot resume. |
| `env_scrub_prefixes` | Environment variables to strip before spawning, matched by prefix, case-insensitively. |
| `keys.approve` / `keys.deny` | What to send to answer a prompt. |
| `timing.idle_debounce_ms` | Override how long an idle screen must persist before the session is called idle. |
| `rules` | Screen-match rules, below. |

### Platforms

Most agents are one program with one name on every machine, so most manifests
say it once and leave `launch.on` out:

```json
"launch": { "program": "claude" }
```

Where it differs, `launch.on` holds one entry per platform, keyed by `windows`,
`macos`, `linux` — Rust's [`std::env::consts::OS`] values — or `unix`, which
answers for everything that is not Windows. The more specific key wins, and
each field is taken separately, so an entry that only renames the program does
not have to restate its arguments:

```json
"launch": {
  "on": {
    "windows": { "program": "powershell.exe", "args": ["-NoLogo"] },
    "macos": { "program": "/bin/zsh", "args": ["-i"] },
    "unix": { "program": "/bin/sh", "args": ["-i"] }
  }
}
```

Argument lists replace rather than merge: order carries meaning on a command
line, and merging could add but never remove. `"args": []` is therefore a real
answer — "no arguments here" — and not the same as leaving it out.

`launch.program` is optional, so a manifest can name programs only under `on`.
An agent with no program for the platform it finds itself on is **listed and
refused**, not hidden: it appears in `argus-cli agents` under a `WHERE` column
and struck through in the window's agent picker, and `spawn` says which key to
add. An agent hidden with no explanation sends you looking at your install
when the answer is a line of JSON.

A manifest that names no program anywhere is rejected at load. A misspelt key —
`windwos` — is not, because refusing unknown keys would refuse a manifest
written for a platform a newer Argus runs on; it shows up as an agent that
starts nowhere, and the spawn error prints the keys the manifest does have next
to the one it wanted.

A `rule` may name platforms the same way, with `only_on`:

```json
{ "name": "sh-prompt", "status": "idle", "pattern": "[$#%]\\s*$", "only_on": ["unix"] }
```

Rules are a priority-ordered list where one that does not match costs nothing,
so a manifest serving several shells could *mostly* just carry a rule for each.
What forces `only_on` is that the prompts are not equally distinctive. `zsh`
draws `host dir %`, so a rule that catches a real `zsh` prompt has to be loose
about everything before the sigil — which comes down to "the line ends with
`%`". Live on Windows, that would read output ending in `45%` as a prompt and
call a busy session idle.

**At least one rule must be unscoped.** A platform with no applicable rule
could never report a status, and it would fail silently: sessions that never
leave `working`. The catch-all rule a manifest needs anyway is exactly the one
to leave unscoped, and a manifest that scopes every rule is rejected at load.

Every pattern is compiled whether or not this machine will use it, so a typo in
a Windows-only rule fails to load on Linux too, rather than lying dormant.

[`std::env::consts::OS`]: https://doc.rust-lang.org/std/env/consts/constant.OS.html

### Resuming

`{session_uuid}` in any argument is replaced by a UUID Argus makes up when a
session starts and keeps in its state file, so that resuming can ask for the
same conversation back:

```json
"launch": { "program": "claude", "new_session_args": ["--session-id", "{session_uuid}"] },
"resume": { "args": ["--resume", "{session_uuid}"] }
```

Prefer this over "resume the latest" (`claude --continue`) wherever the CLI
allows it. Argus runs agents side by side, often in one folder, and "the
latest" would resume every one of them into the same conversation. A manifest
whose `resume.args` uses `{session_uuid}` but whose launch never does is
rejected at load time: nothing would ever have been given that id.

### Keys

Named, not raw bytes: `enter`, `escape`, `tab`, `space`, `backspace`, `up`,
`down`, `left`, `right`, `y`, `n`, or `ctrl-<letter>`. Anything else is sent as
literal text, so `"approve": "yes"` types the word.

Names rather than control characters because a raw escape byte in a config file
is invisible in an editor, does not survive being pasted anywhere, and is
illegal in a JSON string without an awkward unicode escape.

### Rules

```json
{
  "name": "permission-prompt",
  "status": "needs_you",
  "pattern": "Do you want to .*\\?",
  "priority": 100,
  "region": { "last_lines": 12 }
}
```

- `status` — one of `idle`, `working`, `needs_you`, `done`.
- `pattern` — a regular expression in the [`regex`](https://docs.rs/regex) crate's
  dialect. **No backreferences and no lookaround**: `(?<=foo)` and `\1` are
  compile errors, not silent misbehaviour. The trade is that matching is linear
  in the input, so a pathological rule cannot hang the daemon — which matters
  when the patterns are user-editable.
- `priority` — highest wins when several rules match. Ties go to the earlier rule
  in the file.
- `region` — `"screen"` (default) or `{ "last_lines": N }`. Prompts live at the
  bottom, so restricting a blocker rule to the last few lines is the cheapest
  defence against matching a question that was answered ten seconds ago but is
  still on screen.

Rules match against the *rendered screen*, never the raw byte stream. See
[DESIGN.md](../DESIGN.md#why-headless-terminal-emulation) for why that
distinction is the whole point.

## Writing rules that hold up

Two lessons from running real sessions, both learned the hard way in step 6:

**Every manifest needs a way to say *working*.** A screen no rule matches is
treated as *no new evidence*, so the status stays wherever it was. If a manifest
can only recognise idle, a session that reaches idle stays idle while it is busy.
For a shell, a lowest-priority catch-all is enough:

```json
{ "name": "busy", "status": "working", "pattern": "\\S", "priority": 1,
  "region": { "last_lines": 1 } }
```

That matches any visible character on the last line, so it covers a running
command, scrolling output, and programs that print as they go — and because it
is the lowest priority, any real prompt still wins.

**A prompt is only live at the bottom.** A question that has been answered is
still on screen, usually with a fresh prompt beneath it. A region of several
lines will keep matching it and report NEEDS YOU long after the user replied. For
a shell, `last_lines: 1` is right for every rule: only the last line of content
says what it is doing. TUIs that draw their input box at the bottom need a few
lines, but keep the region as tight as the prompt allows.

## A note on the patterns shipped here

`shell.json` is verified against real PowerShell sessions driven through the
engine: idle at a prompt, working during a command, NEEDS YOU at a `(y/n)`
question, and back to idle once answered.

`claude-code.json` is **partly** verified, against Claude Code v2.1.142 running
under the engine:

| Rule | Status |
|---|---|
| `input-box-idle` | Verified. The input box is a `❯` line (`>` in earlier releases) between two horizontal rules. |
| `busy-spinner` | Verified. The spinner is a glyph and a verb ending in `…` (`· Manifesting…`); the finished-turn summary (`✻ Churned for 2s`) has no ellipsis and correctly does not match. |
| `permission-prompt`, `trust-prompt`, `numbered-choice` | **Not yet seen live.** Reaching a permission prompt needs a logged-in `claude`, and the test session's login had expired. |
| `new_session_args` / `resume` | Verified: a session started with `--session-id`, lost in an engine crash and resumed with `--resume`, comes back showing the conversation it had. |

The idle rule has been wrong twice in ways no fixture would have caught: first
it expected a `│ >` bordered box that no longer exists, then Claude Code
changed its prompt from `>` to `❯`. Both times an idle Claude read as working
forever. Treat any misclassification as a bug in this file first, and check
against a real session:

```bash
.\scripts\demo.ps1 -WithClaude
```
