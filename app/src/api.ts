// Typed wrappers around the Rust commands in src-tauri/src/commands.rs.
//
// The webview never talks to the engine directly -- only through these. The
// shapes below mirror argus-protocol's JSON exactly (snake_case and all), because
// that is what serde sends. If they drift, the place to look is
// crates/protocol/src/message.rs, which is the source of truth.

import { Channel, invoke } from "@tauri-apps/api/core";

export type Status = "idle" | "working" | "needs_you" | "done";

export interface SessionInfo {
  id: string;
  agent: string;
  status: Status;
  since_ms: number;
  status_for_ms: number;
  rule?: string;
  pid?: number;
  exit_code?: number;
  /** Running when a previous engine stopped; its process went with it. */
  lost?: boolean;
  /** Finished, and `resume` would start it again under the same id. */
  resumable?: boolean;
  /** The folder the agent runs in. */
  cwd?: string;
  /** The branch of the git worktree Argus made for it, if any. */
  branch?: string;
}

export interface ScreenView {
  session: SessionInfo;
  cols: number;
  rows: number;
  cursor_row: number;
  cursor_col: number;
  lines: string[];
}

export interface AgentInfo {
  id: string;
  display_name: string;
}

/**
 * What a watch delivers: the engine's events (argus-protocol's `WatchEvent`),
 * plus `disconnected`, added by the Rust side when the stream stops without
 * the session having ended.
 */
export type WatchMessage =
  | { event: "repaint"; cols: number; rows: number; data: string }
  | { event: "output"; data: string }
  | { event: "ended" }
  | { event: "disconnected" };

/** What a failed command rejects with. `kind` says what the UI should do. */
export interface UiError {
  kind: "not_running" | "rpc" | "other";
  code?: string;
  message: string;
}

export function isUiError(value: unknown): value is UiError {
  return typeof value === "object" && value !== null && "kind" in value && "message" in value;
}

export const api = {
  listSessions: () => invoke<SessionInfo[]>("list_sessions"),
  listAgents: () => invoke<AgentInfo[]>("list_agents"),
  readScreen: (id: string) => invoke<ScreenView>("read_screen", { id }),
  spawn: (agent: string, cwd?: string, worktree?: string) =>
    invoke<SessionInfo>("spawn_session", { agent, cwd, worktree }),
  sendLine: (id: string, text: string) => invoke<void>("send_line", { id, text }),
  sendText: (id: string, text: string) => invoke<void>("send_text", { id, text }),
  sendKey: (id: string, key: string) => invoke<void>("send_key", { id, key }),
  resize: (id: string, cols: number, rows: number) =>
    invoke<void>("resize_session", { id, cols, rows }),
  /** Start a watch; resolves to the number `unwatch` takes. */
  watch: (id: string, onMessage: (message: WatchMessage) => void) => {
    const onEvent = new Channel<WatchMessage>();
    onEvent.onmessage = onMessage;
    return invoke<number>("watch_session", { id, onEvent });
  },
  unwatch: (watch: number) => invoke<void>("unwatch_session", { watch }),
  respond: (id: string, approve: boolean) => invoke<void>("respond", { id, approve }),
  kill: (id: string) => invoke<void>("kill_session", { id }),
  resume: (id: string) => invoke<SessionInfo>("resume_session", { id }),
  startEngine: () => invoke<void>("start_engine"),
  stopEngine: () => invoke<void>("stop_engine"),
};
