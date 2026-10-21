// Display rules, kept identical to argus-cli's render.rs so the CLI and the UI
// never disagree about what a session is doing or which one comes first.

import type { SessionInfo, Status } from "./api";

/** `45s`, `4m12s`, `1h04m`. */
export function duration(ms: number): string {
  const secs = Math.floor(ms / 1000);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m${String(secs % 60).padStart(2, "0")}s`;
  return `${Math.floor(secs / 3600)}h${String(Math.floor((secs % 3600) / 60)).padStart(2, "0")}m`;
}

/** What a session's status is called. `lost` wins over `done`: it did not finish. */
export function statusLabel(s: SessionInfo): string {
  switch (s.status) {
    case "needs_you":
      return "Needs you";
    case "working":
      return "Working";
    case "idle":
      return "Idle";
    case "done":
      if (s.lost) return "Lost";
      return s.exit_code === undefined ? "Done" : `Done (${s.exit_code})`;
  }
}

/** A CSS class per visual state. Lost is its own look, not a kind of done. */
export function statusClass(s: SessionInfo): string {
  return s.status === "done" && s.lost ? "lost" : s.status;
}

/**
 * The window flag in the status line, borrowed from tmux: `!` is a bell (it
 * needs you), `#` is activity (working), `~` is silence (idle). A finished
 * session carries none -- its pane border says how it ended.
 */
export function flag(s: SessionInfo): string {
  switch (s.status) {
    case "needs_you":
      return "!";
    case "working":
      return "#";
    case "idle":
      return "~";
    case "done":
      return "";
  }
}

/**
 * The status as a pane border spells it. A finished pane uses tmux's own word
 * for one whose process has gone: it is dead, with the exit status.
 */
export function paneStatus(s: SessionInfo): string {
  if (s.status === "done") {
    if (s.lost) return "lost";
    return s.exit_code === undefined ? "dead" : `dead, status ${s.exit_code}`;
  }
  return `${statusLabel(s).toLowerCase()} ${duration(s.status_for_ms)}`;
}

const RANK: Record<Status, number> = { needs_you: 0, working: 1, idle: 2, done: 3 };

/**
 * Most urgent first: blocked sessions at the top, the longest-waiting first,
 * then working, idle, finished. Used to pick which blocked pane takes the main
 * tile; the panes themselves keep their creation order, so nothing moves under
 * your eye except the one that rang.
 */
export function byUrgency(sessions: SessionInfo[]): SessionInfo[] {
  return [...sessions].sort(
    (a, b) => RANK[a.status] - RANK[b.status] || b.status_for_ms - a.status_for_ms,
  );
}
