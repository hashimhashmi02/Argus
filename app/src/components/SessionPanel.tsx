import { useState } from "react";
import { api } from "../api";
import { duration, statusClass, statusLabel } from "../format";
import { usePolling } from "../usePolling";

interface Props {
  id: string;
  onError: (message: string) => void;
}

/** Quick keys, named the same way manifests and the CLI name them. */
const KEYS: [label: string, key: string][] = [
  ["Enter", "enter"],
  ["Esc", "escape"],
  ["Ctrl+C", "ctrl-c"],
  ["↑", "up"],
  ["↓", "down"],
];

/** One session: what it is doing, what is on its screen, and ways to answer it. */
export function SessionPanel({ id, onError }: Props) {
  const { data: view } = usePolling(() => api.readScreen(id), 400, [id]);
  const [line, setLine] = useState("");

  // Errors from actions are surfaced; errors from polling are not, because the
  // list above already says when the engine is unreachable.
  const act = (action: () => Promise<void>) => async () => {
    try {
      await action();
    } catch (err) {
      onError((err as { message?: string }).message ?? String(err));
    }
  };

  if (!view) return <section className="panel loading">Loading…</section>;

  const s = view.session;
  const live = s.status !== "done";
  const blocked = s.status === "needs_you";

  // Trailing blank rows are dropped: a fresh terminal is a few lines of content
  // above a screenful of nothing.
  let used = view.lines.length;
  while (used > 0 && view.lines[used - 1].trim() === "") used--;

  return (
    <section className="panel">
      <header className="panel-header">
        <span className={`badge ${statusClass(s)}`}>{statusLabel(s)}</span>
        <span className="panel-title">{s.agent}</span>
        <span className="panel-meta">
          {s.id} · for {duration(s.status_for_ms)}
          {s.rule && <> · rule <code>{s.rule}</code></>}
          {s.pid !== undefined && <> · pid {s.pid}</>}
        </span>
        <button className="danger" onClick={act(() => api.kill(id))} disabled={!live}>
          Kill
        </button>
      </header>

      {blocked && (
        <div className="attention">
          <span>This session is waiting for you.</span>
          <button className="approve" onClick={act(() => api.respond(id, true))}>
            Approve
          </button>
          <button className="deny" onClick={act(() => api.respond(id, false))}>
            Deny
          </button>
        </div>
      )}

      {s.lost && (
        <div className="notice">
          This session was still running when the engine last stopped, and its process went with
          it. Its screen was not kept.
        </div>
      )}

      <pre className="screen" aria-label="Session screen">
        {view.lines.slice(0, used).join("\n")}
      </pre>

      <form
        className="input-row"
        onSubmit={async (e) => {
          e.preventDefault();
          if (!line) return;
          await act(() => api.sendLine(id, line))();
          setLine("");
        }}
      >
        <input
          value={line}
          onChange={(e) => setLine(e.target.value)}
          placeholder={live ? "Type a line and press Enter to send it" : "This session has ended"}
          spellCheck={false}
          disabled={!live}
        />
        {KEYS.map(([label, key]) => (
          <button
            type="button"
            key={key}
            className="key"
            disabled={!live}
            onClick={act(() => api.sendKey(id, key))}
          >
            {label}
          </button>
        ))}
      </form>
    </section>
  );
}
