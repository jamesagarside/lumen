import { createSignal, onCleanup, type Accessor } from "solid-js";
import type { Snapshot } from "./types";

const POLL_INTERVAL_MS = 1000;

export interface SnapshotStore {
  snapshot: Accessor<Snapshot | null>;
  error: Accessor<string | null>;
  start: () => void;
  stop: () => void;
}

export const createSnapshotStore = (url = "/snapshot"): SnapshotStore => {
  const [snapshot, setSnapshot] = createSignal<Snapshot | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  let timer: ReturnType<typeof setInterval> | null = null;
  let inflight: AbortController | null = null;

  const tick = async () => {
    inflight?.abort();
    const ctrl = new AbortController();
    inflight = ctrl;
    try {
      const res = await fetch(url, { signal: ctrl.signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as Snapshot;
      setSnapshot(data);
      setError(null);
    } catch (e) {
      if ((e as Error).name === "AbortError") return;
      setError((e as Error).message);
    }
  };

  const start = () => {
    if (timer) return;
    void tick();
    timer = setInterval(tick, POLL_INTERVAL_MS);
  };

  const stop = () => {
    if (timer) clearInterval(timer);
    timer = null;
    inflight?.abort();
  };

  onCleanup(stop);

  return { snapshot, error, start, stop };
};
