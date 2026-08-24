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

## A note on the patterns shipped here

The rules in `claude-code.json` are a starting point written from the shape of
the CLI, not from captured transcripts. They are structurally correct — right
statuses, right priorities, right regions — but the exact wording will need
tuning against real sessions once step 4 can run an agent under Argus and record
what its screens actually look like. Treat any misclassification as a bug in
this file first.

`shell.json` is different: its prompts are stable and already verified against
the PTY layer, which makes it the useful manifest for testing the machinery
itself.
