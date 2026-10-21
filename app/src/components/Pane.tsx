import { api, type SessionInfo } from "../api";
import { paneStatus, statusClass, statusLabel } from "../format";
import { blocked, keepFocus } from "../layout";
import { usePolling } from "../usePolling";
import { Screen } from "./Screen";

interface Props {
  session: SessionInfo;
  index: number;
  current: boolean;
  zoomed: boolean;
  /** An answer was sent and the engine has not reported the result yet. */
  answered: boolean;
  onSelect: () => void;
  onToggleZoom: () => void;
  onRespond: (approve: boolean) => void;
  onKey: (key: string) => void;
}

/** Named keys, labelled as tmux's send-keys names them, sent as manifests name them. */
const KEYS: [label: string, key: string][] = [
  ["Enter", "enter"],
  ["Escape", "escape"],
  ["C-c", "ctrl-c"],
  ["Up", "up"],
  ["Down", "down"],
];

/**
 * One session as a tmux pane: a border line carrying its fixed furniture
 * (index and agent, id, status and age, rule -- always in that order), then
 * its real screen. A pane that needs you gets the only bright yellow in the
 * window, top and bottom, because that is where you act.
 */
export function Pane(props: Props) {
  const { session: s, index, current, zoomed, answered } = props;
  const dead = s.status === "done";
  const bell = blocked(s);

  // The pane you are looking at is polled fastest; a dead one barely at all,
  // since its screen can no longer change.
  const interval = dead ? 5000 : current || zoomed ? 400 : 1000;
  const { data: view } = usePolling(() => api.readScreen(s.id), interval, [s.id]);

  const classes = [
    "pane",
    `is-${statusClass(s)}`,
    dead && !s.lost && s.exit_code !== undefined && s.exit_code !== 0 && "is-failed",
    current && "is-current",
    zoomed && "is-zoomed",
  ].filter(Boolean);

  return (
    <section
      className={classes.join(" ")}
      style={{ viewTransitionName: `pane-${s.id}` }}
      aria-label={`${index} ${s.agent}: ${statusLabel(s)}`}
      aria-current={current || undefined}
      onClick={props.onSelect}
      onDoubleClick={props.onToggleZoom}
    >
      <header className="pane-head" title={s.pid === undefined ? undefined : `pid ${s.pid}`}>
        <span className="tick" />
        <span className="seg seg-name">
          {index} {s.agent}
        </span>
        <span className="tick" />
        <span className="seg">{s.id}</span>
        <span className="tick" />
        <span className="seg seg-status">{paneStatus(s)}</span>
        <span className="tick" />
        <span className="seg">{s.rule ?? "-"}</span>
        <span className="fill" />
      </header>

      {view && view.cols > 0 ? (
        <Screen view={view} cursor={current && !dead} />
      ) : s.lost ? (
        <p className="pane-note">
          This session was still running when the engine last stopped, and its process went with
          it. Its screen was not kept.
        </p>
      ) : dead && view ? (
        <p className="pane-note">
          This session ended before the engine last restarted. Its screen was not kept.
        </p>
      ) : (
        <div className="screen-fit" />
      )}

      {zoomed && !dead && (
        <div className="pane-keys" onDoubleClick={(e) => e.stopPropagation()}>
          <span className="pane-keys-label">send-keys</span>
          {KEYS.map(([label, key]) => (
            <button
              type="button"
              className="key"
              key={key}
              onMouseDown={keepFocus}
              onClick={() => props.onKey(key)}
            >
              {label}
            </button>
          ))}
        </div>
      )}

      {bell && (
        <div className="pane-prompt" onDoubleClick={(e) => e.stopPropagation()}>
          {answered ? (
            <span>answer sent, waiting for {s.agent}</span>
          ) : (
            <>
              <span>approve?</span>
              <button
                type="button"
                className="answer"
                onMouseDown={keepFocus}
                onClick={() => props.onRespond(true)}
              >
                y
              </button>
              <button
                type="button"
                className="answer"
                onMouseDown={keepFocus}
                onClick={() => props.onRespond(false)}
              >
                n
              </button>
            </>
          )}
        </div>
      )}
    </section>
  );
}
