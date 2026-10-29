import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow, UserAttentionType } from "@tauri-apps/api/window";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { api, isUiError, type SessionInfo } from "./api";
import { usePolling } from "./usePolling";
import { blocked, layout, layoutKey, morph, reduceView, type Action, type View } from "./layout";
import { Pane, type TerminalHooks } from "./components/Pane";
import type { TerminalControl } from "./components/Terminal";
import {
  StatusLine,
  type Command,
  type Engine,
  type Message,
  type Prompt,
} from "./components/StatusLine";
import "./App.css";

/** How long a pane must have been asking before `y` or `n` will answer it. */
const ARM_MS = 500;
/** How long "answer sent" holds before the prompt comes back if nothing changed. */
const ANSWER_HOLD_MS = 4000;

export default function App() {
  const sessions = usePolling(() => api.listSessions(), 700, []);
  const notRunning = isUiError(sessions.error) && sessions.error.kind === "not_running";
  // Keyed on `notRunning`, so the agent list is fetched again the moment the
  // engine becomes reachable rather than on the next slow poll.
  const agents = usePolling(() => api.listAgents(), 5000, [notRunning]);

  const [prompt, setPrompt] = useState<Prompt>();
  const [message, setMessage] = useState<Message>();
  const [answered, setAnswered] = useState<Record<string, number>>({});
  const [starting, setStarting] = useState(false);
  // The zoomed pane's live terminal: whether the keyboard is in it, and
  // whether its prefix key is waiting for a command.
  const [typing, setTyping] = useState(false);
  const [prefix, setPrefix] = useState(false);
  const terminal = useRef<TerminalControl>({ wantFocus: false });

  // The view (list, current pane, zoom) changes through `apply`, which runs
  // any change that moves a pane as a view transition. `viewRef` is always the
  // latest state: a transition applies it a frame later, and by then another
  // change may have landed on top.
  const [view, setView] = useState<View>({ list: [] });
  const viewRef = useRef(view);
  const apply = useCallback((action: Action) => {
    const prev = viewRef.current;
    const next = reduceView(prev, action);
    if (next === prev) return;
    viewRef.current = next;
    if (layoutKey(next) !== layoutKey(prev)) morph(() => setView(viewRef.current));
    else setView(next);
  }, []);

  const promptOpen = prompt !== undefined;
  useEffect(() => {
    apply({ type: "data", list: notRunning ? [] : (sessions.data ?? []), hold: promptOpen });
  }, [sessions.data, notRunning, promptOpen, apply]);

  const engine: Engine = notRunning
    ? { state: "down" }
    : sessions.error
      ? { state: "error", message: errorText(sessions.error) }
      : { state: "up" };

  const { list, current, zoomed } = view;
  const currentSession = list.find((s) => s.id === current);
  const say = useCallback((text: string, error = false) => setMessage({ text, error }), []);
  const fail = useCallback((err: unknown) => say(errorText(err), true), [say]);

  // tmux clears a message after a few seconds; so does this.
  useEffect(() => {
    if (!message) return;
    const t = setTimeout(() => setMessage(undefined), 6000);
    return () => clearTimeout(t);
  }, [message]);

  // Draw attention when a session newly needs you: the title carries the
  // count, and the taskbar button flashes.
  const needYou = list.filter(blocked).length;
  const lastNeedYou = useRef(0);
  useEffect(() => {
    const win = getCurrentWindow();
    win.setTitle(needYou > 0 ? `(${needYou}) Argus` : "Argus").catch(() => {});
    if (needYou > lastNeedYou.current) {
      win.requestUserAttention(UserAttentionType.Critical).catch(() => {});
    }
    lastNeedYou.current = needYou;
  }, [needYou]);

  // And a toast for each session that newly needs you, unless this window is
  // already in front -- then the pane turning yellow is enough.
  const notified = useRef<Set<string>>(new Set());
  useEffect(() => {
    const now = new Set(list.filter(blocked).map((s) => s.id));
    const fresh = list.filter((s) => now.has(s.id) && !notified.current.has(s.id));
    notified.current = now;
    if (fresh.length === 0) return;
    (async () => {
      if (await getCurrentWindow().isFocused()) return;
      let allowed = await isPermissionGranted();
      if (!allowed) allowed = (await requestPermission()) === "granted";
      if (!allowed) return;
      for (const s of fresh) {
        const where = s.branch ? ` on ${s.branch}` : "";
        sendNotification({
          title: `${list.indexOf(s)}:${s.agent}${where} needs you`,
          body: s.rule ? `Waiting on a prompt (${s.rule}).` : "Waiting on a prompt.",
        });
      }
    })().catch(() => {});
  }, [list]);

  // `y` and `n` answer the current pane, but only once its prompt has been on
  // screen for a moment: a key pressed as a bell moves you must not answer a
  // question you never saw.
  const armedAt = useRef(0);
  const currentBlocked = currentSession !== undefined && blocked(currentSession);
  useEffect(() => {
    armedAt.current = performance.now();
  }, [current, currentBlocked]);

  // An answer is pending until the engine reports a new status, or for a few
  // seconds, after which the prompt comes back so a lost keystroke is visible.
  const pending = (s: SessionInfo) =>
    blocked(s) && answered[s.id] !== undefined && Date.now() - answered[s.id] < ANSWER_HOLD_MS;
  useEffect(() => {
    setAnswered((a) => {
      const keep = Object.fromEntries(
        Object.entries(a).filter(([id]) => list.some((s) => s.id === id && blocked(s))),
      );
      return Object.keys(keep).length === Object.keys(a).length ? a : keep;
    });
  }, [list]);

  const respond = useCallback(
    async (id: string, approve: boolean) => {
      setAnswered((a) => ({ ...a, [id]: Date.now() }));
      try {
        await api.respond(id, approve);
      } catch (err) {
        setAnswered((a) => {
          const rest = { ...a };
          delete rest[id];
          return rest;
        });
        fail(err);
      }
    },
    [fail],
  );

  const startEngine = useCallback(async () => {
    if (starting) return;
    setStarting(true);
    say("starting the engine");
    try {
      await api.startEngine();
      sessions.refresh();
      agents.refresh();
    } catch (err) {
      fail(err);
    } finally {
      // The engine needs a moment to bind its pipe; the next poll shows it.
      setTimeout(() => setStarting(false), 1500);
    }
  }, [starting, say, fail, sessions, agents]);

  const resume = useCallback(
    async (id: string) => {
      say(`resuming ${id}`);
      try {
        await api.resume(id);
        setMessage(undefined);
        sessions.refresh();
      } catch (err) {
        fail(err);
      }
    },
    [say, fail, sessions],
  );

  const confirm = (p: Prompt) => {
    if (p.kind === "kill") api.kill(p.id).catch(fail);
    if (p.kind === "stop") {
      say("stopping the engine");
      api
        .stopEngine()
        .then(() => sessions.refresh())
        .catch(fail);
    }
  };

  const toggleZoom = useCallback(
    (id: string) => apply(viewRef.current.zoomed ? { type: "unzoom" } : { type: "zoom", id }),
    [apply],
  );

  const run = useCallback(
    (command: Command) => {
      const live = currentSession !== undefined && currentSession.status !== "done";
      switch (command) {
        case "zoom":
          if (current) apply({ type: "zoom", id: current });
          break;
        case "unzoom":
          blurTerminal();
          apply({ type: "unzoom" });
          break;
        case "type":
          // Into the live terminal, zooming to it first if need be.
          if (!live) break;
          if (zoomed === currentSession.id && terminal.current.focus) {
            terminal.current.focus();
          } else {
            terminal.current.wantFocus = true;
            apply({ type: "zoom", id: currentSession.id });
          }
          break;
        case "detach":
          blurTerminal();
          break;
        case "new":
          if (!notRunning) setPrompt({ kind: "new" });
          break;
        case "send":
          if (live) setPrompt({ kind: "send", id: currentSession.id });
          break;
        case "kill":
          if (live) setPrompt({ kind: "kill", id: currentSession.id });
          break;
        case "resume":
          // No confirmation: it starts the agent again, in the folder and
          // conversation it had -- nothing is lost by doing it by mistake.
          if (currentSession?.resumable) resume(currentSession.id);
          break;
        case "start":
          startEngine();
          break;
        case "stop":
          if (!notRunning) setPrompt({ kind: "stop" });
          break;
      }
    },
    [apply, current, currentSession, notRunning, resume, startEngine, zoomed],
  );

  // The key typed after the terminal's prefix (Ctrl+B), as tmux binds them.
  const prefixCommand = (key: string) => {
    if ((key === "y" || key === "n") && currentSession && currentBlocked) {
      // A deliberate two-key chord, so no arming delay -- and the keyboard
      // stays in the terminal.
      if (!pending(currentSession)) respond(currentSession.id, key === "y");
      return;
    }
    const index = Number.parseInt(key, 10);
    if (!Number.isNaN(index) && index < list.length) {
      blurTerminal();
      apply({ type: "select", id: list[index].id });
      return;
    }
    switch (key) {
      case "d":
      case "Escape":
        run("detach");
        break;
      case "z":
        run("unzoom");
        break;
      case "c":
        blurTerminal();
        run("new");
        break;
      case "x":
        blurTerminal();
        run("kill");
        break;
      case ":":
        blurTerminal();
        run("send");
        break;
    }
  };

  const terminalHooks: TerminalHooks = {
    control: terminal,
    onTyping: setTyping,
    onPrefix: setPrefix,
    onCommand: prefixCommand,
    onError: fail,
  };

  // The keyboard, tmux-style but without the prefix: this window has nothing
  // else to type into. Keys typed into a prompt's input never reach here.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (e.ctrlKey || e.altKey || e.metaKey || e.isComposing) return;
      if (target.closest("input, textarea, select")) return;

      if (prompt?.kind === "kill" || prompt?.kind === "stop") {
        if (e.key === "y") {
          setPrompt(undefined);
          confirm(prompt);
        } else if (e.key === "n" || e.key === "Escape") {
          setPrompt(undefined);
        } else return;
        e.preventDefault();
        return;
      }
      if (prompt && e.key === "Escape") {
        setPrompt(undefined);
        e.preventDefault();
        return;
      }

      const index = Number.parseInt(e.key, 10);
      if (!Number.isNaN(index) && index < list.length) {
        apply({ type: "select", id: list[index].id });
      } else if (e.key === "Enter" || e.key === "z") {
        // Enter on a focused button presses it; only a bare Enter zooms.
        if (target.closest("button")) return;
        if (current) toggleZoom(current);
      } else if (e.key === "Escape") {
        if (message) setMessage(undefined);
        else apply({ type: "unzoom" });
      } else if (e.key === "ArrowRight" || e.key === "ArrowDown" || e.key === "o") {
        apply({ type: "cycle", by: 1 });
      } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
        apply({ type: "cycle", by: -1 });
      } else if ((e.key === "y" || e.key === "n") && currentSession && currentBlocked) {
        if (pending(currentSession) || performance.now() - armedAt.current < ARM_MS) return;
        respond(currentSession.id, e.key === "y");
      } else if (e.key === "c") {
        run("new");
      } else if (e.key === "x") {
        run("kill");
      } else if (e.key === "r") {
        run("resume");
      } else if (e.key === ":") {
        run("send");
      } else if (e.key === "i") {
        run("type");
      } else if (e.key === "s" && notRunning) {
        run("start");
      } else if (e.key === "Q") {
        run("stop");
      } else {
        return;
      }
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // Screen readers hear the bell too.
  const announce = list
    .filter(blocked)
    .map((s) => `${list.indexOf(s)} ${s.agent} needs you`)
    .join(". ");

  const arrangement = layout(view);
  const pane = (s: SessionInfo) => (
    <Pane
      key={s.id}
      session={s}
      index={list.indexOf(s)}
      current={s.id === current}
      zoomed={s.id === zoomed}
      answered={pending(s)}
      terminal={terminalHooks}
      onSelect={() => s.id !== viewRef.current.current && apply({ type: "select", id: s.id })}
      onToggleZoom={() => toggleZoom(s.id)}
      onRespond={(approve) => respond(s.id, approve)}
      onKey={(key) => api.sendKey(s.id, key).catch(fail)}
    />
  );

  return (
    <div className="mux">
      <main className="panes">
        {notRunning ? (
          <NoServer starting={starting} onStart={startEngine} />
        ) : arrangement.kind === "empty" && engine.state === "error" && !sessions.data ? (
          <EngineError message={engine.message} />
        ) : arrangement.kind === "empty" ? (
          <NoSessions loading={sessions.data === undefined} onNew={() => run("new")} />
        ) : arrangement.kind === "zoomed" ? (
          pane(arrangement.pane)
        ) : arrangement.kind === "tiled" ? (
          <div className="tiled">
            {arrangement.rows.map((row, r) => (
              <div className="tile-row" key={r}>
                {row.map(pane)}
              </div>
            ))}
          </div>
        ) : (
          <div className="main-vertical">
            <div className="main-tile">{pane(arrangement.main)}</div>
            <div className="tiled side">
              {arrangement.rows.map((row, r) => (
                <div className="tile-row" key={r}>
                  {row.map(pane)}
                </div>
              ))}
            </div>
          </div>
        )}
      </main>

      <StatusLine
        list={list}
        current={current}
        zoomed={zoomed}
        engine={engine}
        agents={agents.data ?? []}
        prompt={prompt}
        message={message}
        typing={typing}
        prefix={prefix}
        onSelect={(id) => apply({ type: "select", id })}
        onCommand={run}
        onCancel={() => setPrompt(undefined)}
        onConfirm={() => {
          if (prompt) confirm(prompt);
          setPrompt(undefined);
        }}
        onSend={(id, text) => {
          setPrompt(undefined);
          api.sendLine(id, text).catch(fail);
        }}
        onSpawn={async (agent, cwd, branch) => {
          setPrompt(undefined);
          if (branch.trim()) say(`making a worktree for ${branch.trim()}`);
          try {
            const session = await api.spawn(
              agent,
              cwd.trim() || undefined,
              branch.trim() || undefined,
            );
            setMessage(undefined);
            apply({ type: "select", id: session.id });
            sessions.refresh();
          } catch (err) {
            fail(err);
          }
        }}
        onDismiss={() => setMessage(undefined)}
      />

      <div className="sr-only" aria-live="polite">
        {announce}
      </div>
    </div>
  );
}

/** The engine is not running: tmux's own words, then the one thing to do about it. */
function NoServer({ starting, onStart }: { starting: boolean; onStart: () => void }) {
  return (
    <div className="notice">
      <p>no server running</p>
      <p className="notice-body">
        The engine is the background process that owns every agent session.
        <br />
        It keeps running when this window closes, so your agents do too.
      </p>
      <p>
        <button type="button" className="act" onClick={onStart} disabled={starting}>
          <b>s</b> {starting ? "starting the engine" : "start the engine"}
        </button>
      </p>
    </div>
  );
}

/**
 * The engine is there but not answering properly, and nothing has been listed
 * yet. Every poll is a retry, so there is nothing to press -- only the reason.
 */
function EngineError({ message }: { message: string }) {
  return (
    <div className="notice" role="alert">
      <p>the engine is not answering</p>
      <p className="notice-body">{message}</p>
      <p className="notice-body">
        Argus keeps asking, several times a second. If this does not clear and the engine was
        started from this window, its log is %LOCALAPPDATA%\Argus\engine.log.
      </p>
    </div>
  );
}

/** No sessions yet: the key table, as `tmux list-keys` would print it. */
function NoSessions({ loading, onNew }: { loading: boolean; onNew: () => void }) {
  if (loading) return <div className="notice" />;
  const keys: [key: string, command: string, does: string][] = [
    ["c", "new-window", "start a session from any agent the engine knows"],
    ["0-9", "select-pane", "make a pane current; arrows and o cycle"],
    ["enter", "resize-pane -Z", "zoom the current pane to the full window"],
    ["esc", "", "tile every pane again"],
    ["y n", "", "approve or deny the pane that needs you"],
    ["i", "", "type into the current pane's live terminal"],
    ["C-b d", "detach", "stop typing, from inside the terminal"],
    ["C-b z", "", "stop typing and tile every pane again"],
    [":", "send-keys", "type one line into the current pane"],
    ["x", "kill-pane", "stop the current session, after asking"],
    ["r", "respawn-pane", "start a finished or lost session again, where it left off"],
    ["Q", "kill-server", "stop the engine and every session, after asking"],
  ];
  return (
    <div className="notice">
      <p>no sessions</p>
      <table className="keys">
        <tbody>
          {keys.map(([key, command, does]) => (
            <tr key={key}>
              <th scope="row">{key}</th>
              <td>{command}</td>
              <td>{does}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p>
        <button type="button" className="act" onClick={onNew}>
          <b>c</b> new-window
        </button>
      </p>
    </div>
  );
}

/** Take the keyboard out of the live terminal, if it is in there. */
function blurTerminal() {
  const active = document.activeElement;
  if (active instanceof HTMLElement && active.closest(".terminal-host")) active.blur();
}

function errorText(err: unknown): string {
  return isUiError(err) ? err.message : ((err as { message?: string })?.message ?? String(err));
}
