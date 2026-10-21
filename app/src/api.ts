// Typed wrappers around the Rust commands in src-tauri/src/commands.rs.
//
// The webview never talks to the engine directly -- only through these. The
// shapes below mirror argus-protocol's JSON exactly (snake_case and all), because
// that is what serde sends. If they drift, the place to look is
// crates/protocol/src/message.rs, which is the source of truth.

import { invoke } from "@tauri-apps/api/core";

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
  spawn: (agent: string, cwd?: string) => invoke<SessionInfo>("spawn_session", { agent, cwd }),
  sendLine: (id: string, text: string) => invoke<void>("send_line", { id, text }),
  sendKey: (id: string, key: string) => invoke<void>("send_key", { id, key }),
  respond: (id: string, approve: boolean) => invoke<void>("respond", { id, approve }),
  kill: (id: string) => invoke<void>("kill_session", { id }),
  startEngine: () => invoke<void>("start_engine"),
};
