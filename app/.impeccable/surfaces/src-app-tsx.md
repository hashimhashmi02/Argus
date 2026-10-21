---
version: 1
slug: "src-app-tsx"
primary_target: "src/App.tsx"
related_targets: ["src/components/SessionList.tsx","src/components/SessionPanel.tsx","src/App.css"]
---

# Surface: the Argus window

**Mode:** Operate. The visitor completes a task: notice which agent needs them, answer it, get back to work.

**Audience and job:** the developer running several coding agents at once. Two scenes, both first-class: a glance at a second monitor from arm's length, and close work inside one agent's screen.

**Task and content:** every session's live emulated screen (plain text plus cursor); status working / needs you / idle / done / lost, how long it has held, the rule that decided it; actions spawn, send a line, send a key, approve / deny, kill, start the engine.

**Constraints:** Tauri desktop window, minimum 900×560. The screen arrives as monochrome text. The protocol is polled, not pushed. Must never read as a generic SaaS admin.

**Memorable moment:** the pane that rang the bell swells to the main tile; Enter zooms it full-window, y answers it, Esc tiles it back.

**Unresolved:** interactive typing inside the screen (step 8b) will live in the zoomed pane.

## Direction contract

THESIS: A terminal multiplexer for agents: every real screen tiled at once, the bell flag naming the one that needs you. Refuses sidebar-plus-detail.

OWN-WORLD: Campbell ANSI on #0C0C0C, Cascadia Mono throughout, 1px box-drawing rules, reverse-video status line. Bright yellow only where you can act. No cards, shadows, radii, glow.

STORY: Glance, see which pane rang. Zoom it, read the real screen, answer y/n, tile back. Finished panes stay, marked.

FIRST VIEWPORT: Panes fill the window edge to edge. A bell switches to main-vertical: the blocked pane takes the left 60%. One status row: session, window list flagged ! # ~, engine, clock.

FORM: tmux multiplexer, candidate 5 of 7, seed key 8180ea78. Signature: zoom morph, confirm-before prompts.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
