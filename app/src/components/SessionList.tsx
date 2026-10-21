import type { SessionInfo } from "../api";
import { byUrgency, duration, statusClass, statusLabel } from "../format";

interface Props {
  sessions: SessionInfo[];
  selected?: string;
  onSelect: (id: string) => void;
}

/** Every session, most urgent first. */
export function SessionList({ sessions, selected, onSelect }: Props) {
  if (sessions.length === 0) {
    return <p className="empty">No sessions yet. Start one above.</p>;
  }

  return (
    <ul className="session-list">
      {byUrgency(sessions).map((s) => (
        <li key={s.id}>
          <button
            className={`session-row ${s.id === selected ? "selected" : ""}`}
            onClick={() => onSelect(s.id)}
          >
            <span className={`badge ${statusClass(s)}`}>{statusLabel(s)}</span>
            <span className="session-agent">{s.agent}</span>
            <span className="session-age">{duration(s.status_for_ms)}</span>
            <span className="session-id">{s.id}</span>
          </button>
        </li>
      ))}
    </ul>
  );
}
