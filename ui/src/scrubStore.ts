import { createSignal, onCleanup, type Accessor } from "solid-js";
import type { Snapshot } from "./types";

/**
 * How often we re-fetch `/scrub/window` while the user is scrubbed back in
 * time. This keeps the timeline's right edge ("now") moving forward at a
 * pace that feels live without spamming the daemon.
 */
const WINDOW_POLL_MS = 1000;

/**
 * Re-fetch the scrubbed snapshot every second while parked at a fixed
 * playhead. The visual effect is "looping playback at playhead" — the rate
 * window is centred on `t`, and the response reflects whatever flows have
 * since *aged out of* or *aged into* that window. For a long-dead moment
 * the response is stable; for a near-live moment edges throb as fresh
 * traffic enters the rate window.
 */
const SCRUB_POLL_MS = 1000;

export interface ScrubWindow {
  /** Server-side wall clock (Unix epoch seconds). */
  nowSecs: number;
  /** Oldest buffered arrival, or null if the buffer is empty. */
  earliestSecs: number | null;
  /** Configured upper bound on retention (seconds). */
  maxWindowSecs: number;
}

export type ScrubMode =
  | { kind: "live" }
  | { kind: "scrubbed"; tSecs: number };

export interface ScrubStore {
  /** Current scrubber window bounds (poll-refreshed). */
  window: Accessor<ScrubWindow | null>;
  /** "live" (default) or "scrubbed" with a playhead time. */
  mode: Accessor<ScrubMode>;
  /** Historical snapshot fetched for the playhead. Null in live mode. */
  scrubbedSnapshot: Accessor<Snapshot | null>;
  /** True while a fetch is in flight (UI shows a subtle spinner). */
  loading: Accessor<boolean>;
  /** Last fetch error message, cleared on next success. */
  error: Accessor<string | null>;
  /** Convenience: are we in live mode? */
  isLive: Accessor<boolean>;
  /** Jump to a specific historical second. Pauses live updates. */
  scrubTo: (tSecs: number) => void;
  /** Snap back to live. Discards the scrubbed snapshot. */
  resumeLive: () => void;
  start: () => void;
  stop: () => void;
}

export const createScrubStore = (): ScrubStore => {
  const [windowState, setWindow] = createSignal<ScrubWindow | null>(null);
  const [mode, setMode] = createSignal<ScrubMode>({ kind: "live" });
  const [scrubbedSnapshot, setScrubbedSnapshot] = createSignal<Snapshot | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  let windowTimer: ReturnType<typeof setInterval> | null = null;
  let scrubTimer: ReturnType<typeof setInterval> | null = null;
  let inflight: AbortController | null = null;

  const fetchWindow = async () => {
    try {
      const res = await fetch("/scrub/window");
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as {
        now_secs: number;
        earliest_secs: number | null;
        max_window_secs: number;
      };
      setWindow({
        nowSecs: data.now_secs,
        earliestSecs: data.earliest_secs,
        maxWindowSecs: data.max_window_secs,
      });
    } catch {
      // Surface only via the scrubbed-fetch path; the window endpoint is
      // best-effort and a transient failure shouldn't disrupt the UI.
    }
  };

  const fetchScrubbed = async (tSecs: number) => {
    inflight?.abort();
    const ctrl = new AbortController();
    inflight = ctrl;
    setLoading(true);
    try {
      const res = await fetch(`/scrub?t=${tSecs.toFixed(3)}`, { signal: ctrl.signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const snap = (await res.json()) as Snapshot;
      setScrubbedSnapshot(snap);
      setError(null);
    } catch (e) {
      if ((e as Error).name === "AbortError") return;
      setError((e as Error).message);
    } finally {
      if (inflight === ctrl) setLoading(false);
    }
  };

  const start = () => {
    if (windowTimer) return;
    void fetchWindow();
    windowTimer = setInterval(fetchWindow, WINDOW_POLL_MS);
  };

  const stop = () => {
    if (windowTimer) clearInterval(windowTimer);
    windowTimer = null;
    if (scrubTimer) clearInterval(scrubTimer);
    scrubTimer = null;
    inflight?.abort();
  };

  const scrubTo = (tSecs: number) => {
    setMode({ kind: "scrubbed", tSecs });
    void fetchScrubbed(tSecs);
    // Refresh the snapshot periodically so the rate-window animation
    // stays current as flows age in/out of the playhead window.
    if (scrubTimer) clearInterval(scrubTimer);
    scrubTimer = setInterval(() => {
      const m = mode();
      if (m.kind === "scrubbed") void fetchScrubbed(m.tSecs);
    }, SCRUB_POLL_MS);
  };

  const resumeLive = () => {
    setMode({ kind: "live" });
    setScrubbedSnapshot(null);
    setError(null);
    if (scrubTimer) clearInterval(scrubTimer);
    scrubTimer = null;
    inflight?.abort();
  };

  onCleanup(stop);

  return {
    window: windowState,
    mode,
    scrubbedSnapshot,
    loading,
    error,
    isLive: () => mode().kind === "live",
    scrubTo,
    resumeLive,
    start,
    stop,
  };
};
