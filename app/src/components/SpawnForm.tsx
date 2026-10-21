import { useState } from "react";
import { api, type AgentInfo } from "../api";

interface Props {
  agents: AgentInfo[];
  onSpawned: (id: string) => void;
  onError: (message: string) => void;
}

/** Start a new session from any agent the engine knows. */
export function SpawnForm({ agents, onSpawned, onError }: Props) {
  const [agent, setAgent] = useState("");
  const [cwd, setCwd] = useState("");
  const [busy, setBusy] = useState(false);

  const chosen = agent || agents[0]?.id || "";

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!chosen) return;
    setBusy(true);
    try {
      const session = await api.spawn(chosen, cwd || undefined);
      onSpawned(session.id);
    } catch (err) {
      onError((err as { message?: string }).message ?? String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="spawn-form" onSubmit={submit}>
      <select value={chosen} onChange={(e) => setAgent(e.target.value)} disabled={busy}>
        {agents.map((a) => (
          <option key={a.id} value={a.id}>
            {a.display_name}
          </option>
        ))}
      </select>
      <input
        value={cwd}
        onChange={(e) => setCwd(e.target.value)}
        placeholder="Working directory (optional)"
        spellCheck={false}
        disabled={busy}
      />
      <button type="submit" disabled={busy || !chosen}>
        {busy ? "Starting…" : "New session"}
      </button>
    </form>
  );
}
