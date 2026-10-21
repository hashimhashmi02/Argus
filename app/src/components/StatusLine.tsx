import { useEffect, useState } from "react";
import type { AgentInfo, SessionInfo } from "../api";
import { flag } from "../format";
import { keepFocus } from "../layout";

/** A question the status line is asking, as tmux's command prompt would. */
export type Prompt =
  | { kind: "send"; id: string }
  | { kind: "kill"; id: string }
  | { kind: "new" };

export interface Message {
  text: string;
  error: boolean;
}

export type Engine = { state: "up" } | { state: "down" } | { state: "error"; message: string };

/** What a hint in the status line runs when clicked: the same as its key. */
export type Command = "zoom" | "unzoom" | "new" | "send" | "kill" | "start";

interface Props {
  list: SessionInfo[];
  current?: string;
  zoomed?: string;
  engine: Engine;
  agents: AgentInfo[];
  prompt?: Prompt;
  message?: Message;
  onSelect: (id: string) => void;
  onCommand: (command: Command) => void;
  onCancel: () => void;
  onConfirm: () => void;
  onSend: (id: string, text: string) => void;
  onSpawn: (agent: string, cwd: string) => void;
  onDismiss: () => void;
}

/**
 * The one row at the bottom, in reverse video. Normally it is tmux's status
 * line: the window list with its flags, then the engine and the clock. When
 * there is a question -- send what, kill which, start what -- the whole row
 * becomes that question, in the yellow tmux uses for its prompts.
 */
export function StatusLine(props: Props) {
  const { list, prompt, message } = props;
  const name = (id: string) => {
    const i = list.findIndex((s) => s.id === id);
    return i < 0 ? id : `${i}:${list[i].agent}`;
  };

  if (prompt?.kind === "send") {
    return <SendPrompt target={name(prompt.id)} {...props} id={prompt.id} />;
  }
  if (prompt?.kind === "kill") {
    return (
      <footer className="status is-prompt" role="alertdialog" aria-label="Confirm kill">
        <span className="status-text">
          kill-pane {name(prompt.id)} {prompt.id}? (y/n)
        </span>
        <button type="button" className="answer" onClick={props.onConfirm}>
          y
        </button>
        <button type="button" className="answer" onClick={props.onCancel}>
          n
        </button>
      </footer>
    );
  }
  if (prompt?.kind === "new") {
    return <NewPrompt {...props} />;
  }
  if (message) {
    return (
      <footer
        className={`status is-message ${message.error ? "is-error" : ""}`}
        role={message.error ? "alert" : "status"}
        onClick={props.onDismiss}
      >
        <span className="status-text">{message.text}</span>
      </footer>
    );
  }

  return (
    <footer className="status">
      <span className="status-session">[argus]</span>
      <nav className="windows" aria-label="Sessions">
        {list.map((s, i) => (
          <button
            type="button"
            key={s.id}
            className={`window ${s.status === "needs_you" ? "is-bell" : ""}`}
            aria-current={s.id === props.current || undefined}
            onMouseDown={keepFocus}
            onClick={() => props.onSelect(s.id)}
          >
            {i}:{s.agent}
            {s.id === props.current && "*"}
            {s.id === props.zoomed && "Z"}
            {flag(s)}
          </button>
        ))}
      </nav>
      <Hints {...props} />
      <EngineState engine={props.engine} />
      <Clock />
    </footer>
  );
}

function Hints({ engine, list, zoomed, current, onCommand }: Props) {
  const hints: [key: string, label: string, command: Command][] = [];
  const live = list.some((s) => s.id === current && s.status !== "done");
  if (engine.state === "down") {
    hints.push(["s", "start engine", "start"]);
  } else {
    if (zoomed) hints.push(["esc", "tile", "unzoom"]);
    else if (current) hints.push(["enter", "zoom", "zoom"]);
    hints.push(["c", "new", "new"]);
    if (live) hints.push([":", "send", "send"], ["x", "kill", "kill"]);
  }
  return (
    <span className="hints">
      {hints.map(([key, label, command]) => (
        <button
          type="button"
          className="hint"
          key={command}
          onMouseDown={keepFocus}
          onClick={() => onCommand(command)}
        >
          <b>{key}</b> {label}
        </button>
      ))}
    </span>
  );
}

function EngineState({ engine }: { engine: Engine }) {
  if (engine.state === "up") return <span className="engine">engine up</span>;
  if (engine.state === "down") return <span className="engine is-down">no server</span>;
  // Not "down": the engine answered, or half-answered, and every poll retries.
  return (
    <span className="engine is-down is-error" title={engine.message}>
      engine: {engine.message}, retrying
    </span>
  );
}

/** `14:32 21-Oct-26`, tmux's default right-hand clock. */
function Clock() {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 5000);
    return () => clearInterval(t);
  }, []);
  const hm = now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
  const month = now.toLocaleString("en", { month: "short" });
  const date = `${String(now.getDate()).padStart(2, "0")}-${month}-${String(now.getFullYear()).slice(2)}`;
  return (
    <time className="clock" dateTime={now.toISOString()}>
      {hm} {date}
    </time>
  );
}

function SendPrompt(props: Props & { id: string; target: string }) {
  const [text, setText] = useState("");
  return (
    <form
      className="status is-prompt"
      onSubmit={(e) => {
        e.preventDefault();
        if (text) props.onSend(props.id, text);
        else props.onCancel();
      }}
    >
      <label className="status-text" htmlFor="send-line">
        send-keys -t {props.target}
      </label>
      <input
        id="send-line"
        className="prompt-input"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => e.key === "Escape" && props.onCancel()}
        autoFocus
        spellCheck={false}
        autoComplete="off"
      />
      <span className="prompt-hint">enter send line · esc cancel</span>
    </form>
  );
}

function NewPrompt(props: Props) {
  const { agents } = props;
  const [at, setAt] = useState(0);
  const [cwd, setCwd] = useState("");
  const chosen = agents[at];

  return (
    <form
      className="status is-prompt"
      onSubmit={(e) => {
        e.preventDefault();
        if (chosen) props.onSpawn(chosen.id, cwd);
      }}
    >
      <span className="status-text">new-window</span>
      <span className="agents" role="radiogroup" aria-label="Agent">
        {agents.length === 0 && <span>no agents loaded</span>}
        {agents.map((a, i) => (
          <button
            type="button"
            role="radio"
            aria-checked={i === at}
            key={a.id}
            className="agent"
            onMouseDown={keepFocus}
            onClick={() => setAt(i)}
          >
            {a.id}
          </button>
        ))}
      </span>
      <label className="status-text" htmlFor="new-cwd">
        -c
      </label>
      <input
        id="new-cwd"
        className="prompt-input"
        value={cwd}
        placeholder="working directory (optional)"
        onChange={(e) => setCwd(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") props.onCancel();
          // Up and Down pick the agent, so the whole prompt works from the keyboard.
          if ((e.key === "ArrowUp" || e.key === "ArrowDown") && agents.length > 0) {
            e.preventDefault();
            const by = e.key === "ArrowDown" ? 1 : -1;
            setAt((i) => (i + by + agents.length) % agents.length);
          }
        }}
        autoFocus
        spellCheck={false}
        autoComplete="off"
      />
      <span className="prompt-hint">up/down agent · enter start · esc cancel</span>
    </form>
  );
}
