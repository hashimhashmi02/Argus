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

const RANK: Record<Status, number> = { needs_you: 0, working: 1, idle: 2, done: 3 };

/**
 * Most urgent first: blocked sessions at the top, the longest-waiting first,
 * then working, idle, finished. The list exists to answer "which one should I
 * look at", so it is ordered by that question rather than by creation.
 */
export function byUrgency(sessions: SessionInfo[]): SessionInfo[] {
  return [...sessions].sort(
    (a, b) => RANK[a.status] - RANK[b.status] || b.status_for_ms - a.status_for_ms,
  );
}
