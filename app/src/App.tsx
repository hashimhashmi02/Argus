import { useEffect, useRef, useState } from "react";
import { getCurrentWindow, UserAttentionType } from "@tauri-apps/api/window";
import { api, isUiError } from "./api";
import { usePolling } from "./usePolling";
import { SessionList } from "./components/SessionList";
import { SessionPanel } from "./components/SessionPanel";
import { SpawnForm } from "./components/SpawnForm";
import "./App.css";

export default function App() {
  const sessions = usePolling(() => api.listSessions(), 700, []);
  const notRunning = isUiError(sessions.error) && sessions.error.kind === "not_running";
  // Keyed on `notRunning`, so the agent list is fetched again the moment the
  // engine becomes reachable -- otherwise the picker sits empty until the next
  // slow poll, which is what the first version did.
  const agents = usePolling(() => api.listAgents(), 5000, [notRunning]);
  const [selected, setSelected] = useState<string>();
  const [toast, setToast] = useState<string>();
  const [starting, setStarting] = useState(false);

  // Any other polling failure is shown, not swallowed. A list that silently
  // stops updating is worse than one that says why.
  const pollError =
    !notRunning && sessions.error
      ? isUiError(sessions.error)
        ? sessions.error.message
        : String(sessions.error)
      : undefined;

  const list = notRunning ? [] : (sessions.data ?? []);
  const needYou = list.filter((s) => s.status === "needs_you").length;

  // Draw attention when a session newly needs you -- the whole point of Argus
  // is not having to watch every window. The taskbar button flashes, and the
  // title carries the count so it is visible from anywhere.
  const lastNeedYou = useRef(0);
  useEffect(() => {
    const win = getCurrentWindow();
    win.setTitle(needYou > 0 ? `(${needYou}) Argus` : "Argus").catch(() => {});
    if (needYou > lastNeedYou.current) {
      win.requestUserAttention(UserAttentionType.Critical).catch(() => {});
    }
    lastNeedYou.current = needYou;
  }, [needYou]);

  // Auto-hide error toasts.
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(undefined), 6000);
    return () => clearTimeout(t);
  }, [toast]);

  const startEngine = async () => {
    setStarting(true);
    try {
      await api.startEngine();
      sessions.refresh();
      agents.refresh();
    } catch (err) {
      setToast((err as { message?: string }).message ?? String(err));
    } finally {
      // Leave the button disabled briefly: the engine needs a moment to bind
      // its pipe, and the next poll will flip the banner once it has.
      setTimeout(() => setStarting(false), 1500);
    }
  };

  return (
    <div className="app">
      <header className="topbar">
        <h1>Argus</h1>
        {notRunning ? (
          <span className="engine down">Engine not running</span>
        ) : pollError ? (
          <span className="engine down" title={pollError}>
            Engine error: {pollError}
          </span>
        ) : (
          <span className="engine up">Engine connected</span>
        )}
        {needYou > 0 && <span className="need-count">{needYou} need you</span>}
      </header>

      {notRunning ? (
        <main className="offline">
          <p>The engine is the background process that owns every agent session.</p>
          <p>It keeps running when this window closes, so your agents do too.</p>
          <button onClick={startEngine} disabled={starting}>
            {starting ? "Starting…" : "Start the engine"}
          </button>
        </main>
      ) : (
        <main className="layout">
          <aside className="sidebar">
            <SpawnForm
              agents={agents.data ?? []}
              onSpawned={(id) => {
                setSelected(id);
                sessions.refresh();
              }}
              onError={setToast}
            />
            <SessionList sessions={list} selected={selected} onSelect={setSelected} />
          </aside>
          {selected && list.some((s) => s.id === selected) ? (
            <SessionPanel key={selected} id={selected} onError={setToast} />
          ) : (
            <section className="panel empty-panel">Select a session to see its screen.</section>
          )}
        </main>
      )}

      {toast && (
        <div className="toast" role="alert" onClick={() => setToast(undefined)}>
          {toast}
        </div>
      )}
    </div>
  );
}
