---
name: Argus
description: A terminal multiplexer for coding agents — every real screen tiled at once, the bell naming the one that needs you.
colors:
  black: "#0c0c0c"
  white: "#cccccc"
  bright-white: "#f2f2f2"
  bright-black: "#767676"
  bright-yellow: "#f9f1a5"
  yellow: "#c19c00"
  green: "#13a10e"
  cyan: "#3a96dd"
  red: "#c50f1f"
  bright-red: "#e74856"
typography:
  chrome:
    fontFamily: "Argus Mono, Cascadia Mono, Consolas, monospace"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: "20px"
  chrome-strong:
    fontFamily: "Argus Mono, Cascadia Mono, Consolas, monospace"
    fontSize: "13px"
    fontWeight: 700
    lineHeight: "20px"
  screen:
    fontFamily: "Argus Mono, Cascadia Mono, Consolas, monospace"
    fontSize: "clamp(3px, min(100cqw / (cols * 0.6), 100cqh / (rows * 1.2)), 18px)"
    fontWeight: 400
    lineHeight: 1.2
rounded:
  none: "0px"
spacing:
  cell: "1ch"
  line: "20px"
  rule: "1px"
components:
  status-line:
    backgroundColor: "{colors.white}"
    textColor: "{colors.black}"
    typography: "{typography.chrome}"
    height: "{spacing.line}"
  status-line-prompt:
    backgroundColor: "{colors.bright-yellow}"
    textColor: "{colors.black}"
    typography: "{typography.chrome}"
    height: "{spacing.line}"
  status-line-message:
    backgroundColor: "{colors.black}"
    textColor: "{colors.bright-white}"
    typography: "{typography.chrome}"
    height: "{spacing.line}"
  window-entry:
    textColor: "{colors.black}"
    typography: "{typography.chrome}"
    padding: "0 1ch"
  window-entry-bell:
    backgroundColor: "{colors.black}"
    textColor: "{colors.bright-yellow}"
    typography: "{typography.chrome-strong}"
    padding: "0 1ch"
  pane-border:
    textColor: "{colors.white}"
    typography: "{typography.chrome}"
    height: "{spacing.line}"
  pane-border-bell:
    backgroundColor: "{colors.bright-yellow}"
    textColor: "{colors.black}"
    typography: "{typography.chrome}"
    height: "{spacing.line}"
  answer-key:
    backgroundColor: "{colors.black}"
    textColor: "{colors.bright-yellow}"
    typography: "{typography.chrome-strong}"
    rounded: "{rounded.none}"
    padding: "0 1ch"
  act-button:
    backgroundColor: "{colors.bright-yellow}"
    textColor: "{colors.black}"
    typography: "{typography.chrome}"
    rounded: "{rounded.none}"
    padding: "0 1ch"
  act-button-disabled:
    backgroundColor: "{colors.yellow}"
    textColor: "{colors.black}"
  key-chip:
    textColor: "{colors.bright-white}"
    typography: "{typography.chrome}"
    padding: "0 1ch"
  key-chip-hover:
    backgroundColor: "{colors.white}"
    textColor: "{colors.black}"
---

# Design System: Argus

## Overview

**Creative North Star: "The Multiplexer"**

Argus looks like the tool its users already live in: tmux, drawn in Windows Terminal's own colours and face. Every session is a pane showing its agent's real emulated screen, tiled edge to edge, with a one-row status line at the bottom. There is no app chrome around the panes — no sidebar, no cards, no toolbar. The window *is* the multiplexer.

The system is austere on purpose, because the screens are the content and the glance is the job. Colour is almost absent: the terminal's grey on near-black, a green rule for the current pane, cyan for a working status. One colour — bright yellow — is reserved for the places you can act, so that from across the desk a single yellow band says which agent is waiting and nothing else competes with it. Need decides size: a pane that needs you swells into the main tile, and zooming brings one screen to reading size while the chrome stays small.

The rejected look is the generic SaaS admin dashboard: a session sidebar with status dots beside a detail panel, soft cards, rounded corners and a blue accent. Argus's first draft was that, and it is the anti-reference.

**Key Characteristics:**
- One monospace face for everything, chrome and screen alike.
- Panes separated by 1px rules, never by gaps, shadows or cards.
- A reverse-video status line that turns yellow when it asks you something.
- Bright yellow only where an action is waiting.
- Panes keep their creation order; only the bell moves one.

## Colors

The Campbell ANSI palette — Windows Terminal's default — used as a terminal uses it: mostly white-on-black, with colour carrying status.

### Primary
- **Bell Yellow** (bright-yellow): everything you can act on right now — the border band and footer of a pane that needs you, the status line while it is a prompt, the bell entry in the window list (as yellow text in a black notch), and the single button on an empty or engine-less window.
- **Pressed Yellow** (yellow): the same action while it is in progress (the start-the-engine button while the engine is starting).

### Secondary
- **Active Green** (green): the current pane's border rule and its index and agent name — tmux's active-pane border.
- **Working Cyan** (cyan): the status words of a session that is working.

### Tertiary
- **Exit Red** (bright-red): a pane that died with a non-zero status, a lost session's status, the engine's "no server" and error segments. Readable red on black.
- **Lost Rule** (red): the broken border of a lost session. Used only for that rule, never for text — it is under 4.5:1 on black.

### Neutral
- **Terminal Black** (black): the ground of every pane and the window behind them; the text of every yellow or light band.
- **Terminal White** (white): screen text, border text, and the status line's reverse-video ground.
- **Bright White** (bright-white): pane names and messages — one step up from screen text, for the labels that identify.
- **Rule Grey** (bright-black): pane rules between columns, inactive pane borders, scrollbars. A line colour only: at 4.3:1 it is too dim for text.

### Named Rules
**The Bell Rule.** Bright yellow appears only where an action is waiting. If nothing can be pressed or answered, nothing is yellow.

**The Lines-Not-Text Rule.** Rule Grey and Lost Rule draw lines; they never colour text. Text stays at Terminal White or brighter, so every label clears 4.5:1.

## Typography

**Display Font:** none — there is no display voice.
**Body Font:** Argus Mono, Cascadia Mono self-hosted (with the installed Cascadia Mono, then Consolas)
**Label/Mono Font:** the same.

**Character:** Cascadia Mono is the Windows Terminal face, so the chrome reads as part of the terminal rather than as an app wrapped around it. The installed copy is preferred where Windows has one, for its full glyph coverage; the bundled Latin, Latin Extended and box-drawing subsets keep the face identical everywhere else.

### Hierarchy
- **Screen** (400, fitted: the largest size at which the session's full grid fits its pane, between 3px and 18px, 1.2): agent screens. A tile shows the whole screen in miniature; zoomed, it rises to reading size and stops.
- **Chrome** (400, 13px, 20px): pane borders, the status line, prompts, notices.
- **Chrome Strong** (700, 13px, 20px): the name of a pane that needs you, the bell entry, answer keys, key names in hints.

### Named Rules
**The One Face Rule.** Every glyph in the window is Cascadia Mono. Hierarchy comes from scale (a fitted screen against fixed 13px chrome), weight (700 for what needs you) and colour — never from a second family.

**The Fixed Furniture Rule.** A pane border always reads `─ index agent ─ id ─ status age ─ rule ───`, in that order, with `-` for a missing rule. The eye learns one position per fact.

## Layout

The window is a column: panes fill everything above a single 20px status row. Panes run edge to edge with no outer margin.

- **Tiled** (nothing needs you): panes in creation order, `ceil(√n)` to a row; a short last row stretches its panes.
- **Main-vertical** (something needs you): the blocked pane takes the left 60%; the rest stack on the right, in two columns past four.
- **Zoomed**: one pane fills the window.

Columns are separated by a 1px Rule Grey gap; rows are separated by each pane's own top border line, so there is never a double rule. Inside a pane the screen sits 1ch from the left rule. Every row of chrome is exactly one 20px line, so the whole window keeps the rhythm of a terminal grid.

The window's minimum is 900×560. At that size a six-pane tile is a miniature, not reading text; the borders, flags and the bell's yellow carry the glance, and zoom carries the reading.

### Named Rules
**The Stable Panes Rule.** Panes keep their creation order and index. Only a bell moves a pane, and only into the main tile.

## Elevation & Depth

Flat. There are no shadows anywhere. Depth is expressed the way a terminal multiplexer expresses it: by size (main tile, zoom) and by line weight and colour of the border. A blocked pane draws a 1px Bell Yellow outline inside its edge and sits above its neighbours so that outline is never covered.

### Named Rules
**The No-Shadow Rule.** Nothing casts a shadow, glows, or floats. If something needs to stand out, it gets the bell's colour or the main tile's size.

## Shapes

Rectangles only (radius 0 everywhere, buttons included). Borders are 1px rules. A finished pane's border goes dashed — solid means alive, broken means its process has gone — and a lost session's dashed rule turns red.

## Components

### Status line
The tmux status row: `[argus]`, the window list, contextual key hints, the engine's state and a `HH:MM dd-Mon-yy` clock, in reverse video (Terminal Black on Terminal White).
- **Window list:** `index:agent` plus tmux flags — `*` current, `Z` zoomed, `!` bell (needs you), `#` activity (working), `~` silence (idle); a finished session carries none. A bell entry is reversed again into a black notch with bold Bell Yellow text.
- **As a prompt:** the whole row turns Bell Yellow and becomes the question — `send-keys -t 0:shell` with an input, `kill-pane 1:shell 1a2b3c4d? (y/n)`, or `new-window` with the agents as a radio row (the chosen one in a black notch) and `-c` for the working directory. Hints name keys in words (`enter`, `esc`, `up/down`), as tmux does.
- **As a message:** a black row in Bright White (or Exit Red for an error) for six seconds, or until clicked.
- **While typing in a live terminal:** the hints become `typing · C-b d stop typing · C-b z tile`, since every other key now belongs to the agent. A pressed prefix shows as a `^B` notch, black with bold Bell Yellow, because the next key is a command waiting to be given.
- **Hover:** underline with a 0.2em offset. **Focus:** 1px black outline.

### Pane
- **Border line:** a 20px row of segments joined by 1ch rules and ended by a rule to the edge. Inactive: Rule Grey. Current: Active Green. Finished: dashed. Needs you: the whole row is a Bell Yellow band in black text with a bold name.
- **Screen (tiled):** the fitted grid, the cursor drawn as a reverse-video cell in the current pane only.
- **Terminal (zoomed, live):** the agent's real terminal in xterm.js, 14px Cascadia Mono on the Campbell theme, so the agent's own colours sit in the same palette as the chrome. Agent output keeps whatever colours the agent chose, yellow included: that is the agent's screen, not Argus's chrome, and the Bell Rule governs the chrome. The cursor is a solid block while you type and an outline when the keyboard is elsewhere, which is the cue for whether keys go to the agent.
- **Footers:** a pane that needs you ends in a Bell Yellow row, `approve? y n`, whose answer keys are black notches with bold yellow letters; once answered it reads `answer sent, waiting for <agent>` until the engine reports a new status. A zoomed live pane adds a `send-keys` row above it: `Enter Escape C-c Up Down`, reversing to Terminal White on hover.

### Notices
Whole-window text for the states with no panes, set like terminal output at the top left: `no server running` with one Bell Yellow button (`s start the engine`), `no sessions` with a `list-keys` table, `the engine is not answering` with the error and where its log is.

### Zoom morph (signature)
Zooming, tiling back, and a bell's pane swelling into the main tile are one view transition. Each pane has its own transition name and animates box to box in 180ms on `cubic-bezier(0.2, 0, 0, 1)`; the snapshots are not stretched but clipped and anchored top-left, so it reads as a terminal being resized. Skipped entirely under reduced motion.

## Do's and Don'ts

### Do:
- **Do** keep Bright Yellow for actions that are waiting: the bell pane's border band and footer, prompts, the one button on an empty window.
- **Do** separate panes with 1px rules — the column gap and each pane's top border — and nothing else.
- **Do** spell keys as tmux names them (`enter`, `esc`, `C-c`, `up/down`) in hints and key rows.
- **Do** keep every chrome row one 20px line of 13px Cascadia Mono.
- **Do** keep only screen text selectable; chrome is `user-select: none`.
- **Do** mark finished panes in place — dashed border, `dead, status N` or `lost` — rather than removing them.

### Don't:
- **Don't** add a sidebar, cards, rounded corners, shadows or glows; the generic SaaS admin dashboard is the anti-reference.
- **Don't** reorder panes by urgency; the bell moves one pane into the main tile and nothing else moves.
- **Don't** set text in Rule Grey or Lost Rule; they are line colours.
- **Don't** introduce a second typeface or a display size; hierarchy is fitted screen against fixed chrome.
- **Don't** use Unicode symbols as icons for keys or actions.
