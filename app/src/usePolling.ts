import { useEffect, useState } from "react";

/**
 * Call `load` every `intervalMs`, keeping the latest result.
 *
 * Polling because the protocol has no subscription yet (that arrives with the
 * interactive terminal in step 8b). A request is never started while the
 * previous one is still in flight, so a slow engine is never buried under a
 * queue of identical calls.
 *
 * The in-flight flag lives inside the effect, not in a ref that outlives it.
 * It used to be a ref, and one request that never returned left it stuck at
 * "in flight" across every later restart of the poller -- so that poller
 * stopped for good while everything else kept working. Now `refresh()`, or any
 * change in `deps`, always starts a poller with a clean slate. (The commands
 * themselves are also bounded on the Rust side, so a hang cannot happen in the
 * first place; this is the second line of defence.)
 */
export function usePolling<T>(
  load: () => Promise<T>,
  intervalMs: number,
  deps: unknown[],
): { data: T | undefined; error: unknown; refresh: () => void } {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<unknown>();
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let cancelled = false;
    let inFlight = false;

    const run = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const value = await load();
        if (!cancelled) {
          setData(value);
          setError(undefined);
        }
      } catch (e) {
        if (!cancelled) setError(e);
      } finally {
        inFlight = false;
      }
    };

    run();
    const timer = setInterval(run, intervalMs);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, intervalMs, tick]);

  return { data, error, refresh: () => setTick((t) => t + 1) };
}
