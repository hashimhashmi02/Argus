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
| `launch.program` / `launch.args` | How to start it. |
| `resume.args` | Appended to the launch command to reattach to previous work. Omit if the CLI cannot resume. |
| `env_scrub_prefixes` | Environment variables to strip before spawning, matched by prefix, case-insensitively. |
| `keys.approve` / `keys.deny` | What to send to answer a prompt. |
| `timing.idle_debounce_ms` | Override how long an idle screen must persist before the session is called idle. |
| `rules` | Screen-match rules, below. |

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

The rules in `claude-code.json` are still a starting point written from the
shape of the CLI rather than from captured sessions. They are structurally
correct but unverified, and the lessons above probably apply to them too. Check
one with:

```bash
cargo run -p argus-cli -- spawn claude-code
```

Treat any misclassification as a bug in this file first.
