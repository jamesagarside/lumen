import { createMemo, createSignal, onCleanup, onMount, Show, type Component } from "solid-js";
import type { ScrubStore } from "./scrubStore";

/**
 * Time scrubber bar (#26). Always visible at the bottom of the canvas,
 * parked at "now" by default.
 *
 * Interaction model:
 *   - Drag the playhead leftwards to rewind. The snapshot source switches
 *     to historical reconstruction, frozen until the user drags again.
 *   - Drag back to the right edge (or click "Live") to resume live
 *     updates. There's no explicit mode toggle: the right edge IS live.
 *   - When at "now", a subtle pulsing dot reminds you the data is fresh.
 *
 * The bar's domain is `[earliestSecs, nowSecs]`. As the buffer fills, the
 * left edge slides right; as time passes, the right edge slides right too.
 * The playhead's *wall-clock* time stays pinned even as the bar shifts —
 * so a moment you scrubbed to 8 minutes ago stays at "8 minutes ago" until
 * it ages off the left edge.
 */
interface Props {
  scrub: ScrubStore;
}

const TimeScrubber: Component<Props> = (props) => {
  let trackRef: HTMLDivElement | undefined;
  const [dragging, setDragging] = createSignal(false);

  const win = () => props.scrub.window();
  const earliest = (): number | null => {
    const w = win();
    if (!w) return null;
    // Don't offer to scrub to before the configured cap, even if the
    // buffer happens to hold one stragglering older flow.
    const capLeft = w.nowSecs - w.maxWindowSecs;
    return w.earliestSecs === null ? null : Math.max(w.earliestSecs, capLeft);
  };
  const right = (): number | null => win()?.nowSecs ?? null;

  /** Map a playhead time to a 0..1 fraction along the track. */
  const fractionFor = (tSecs: number): number => {
    const w = win();
    if (!w) return 1;
    const l = earliest() ?? w.nowSecs - w.maxWindowSecs;
    const r = w.nowSecs;
    if (r <= l) return 1;
    return Math.min(1, Math.max(0, (tSecs - l) / (r - l)));
  };

  /** Inverse of fractionFor: a 0..1 fraction → playhead second. */
  const timeAt = (fraction: number): number => {
    const w = win();
    if (!w) return Date.now() / 1000;
    const l = earliest() ?? w.nowSecs - w.maxWindowSecs;
    const r = w.nowSecs;
    return l + Math.min(1, Math.max(0, fraction)) * (r - l);
  };

  const playheadFraction = createMemo<number>(() => {
    const m = props.scrub.mode();
    if (m.kind === "live") return 1;
    return fractionFor(m.tSecs);
  });

  /** Distance from the right edge ("now"), formatted human-friendly. */
  const ageLabel = (): string => {
    const m = props.scrub.mode();
    const w = win();
    if (m.kind === "live" || !w) return "Live";
    const dt = Math.max(0, w.nowSecs - m.tSecs);
    if (dt < 1) return "Live";
    if (dt < 60) return `-${dt.toFixed(0)}s`;
    if (dt < 3600) {
      const mins = Math.floor(dt / 60);
      const secs = Math.round(dt - mins * 60);
      return secs === 0 ? `-${mins}m` : `-${mins}m ${secs}s`;
    }
    const hrs = Math.floor(dt / 3600);
    const mins = Math.round((dt - hrs * 3600) / 60);
    return `-${hrs}h ${mins}m`;
  };

  const handlePointer = (clientX: number) => {
    if (!trackRef) return;
    const rect = trackRef.getBoundingClientRect();
    const fraction = (clientX - rect.left) / rect.width;
    // Snap-to-live: dropping within ~2% of the right edge resumes live.
    if (fraction >= 0.98) {
      props.scrub.resumeLive();
      return;
    }
    const t = timeAt(fraction);
    props.scrub.scrubTo(t);
  };

  const onPointerDown = (e: PointerEvent) => {
    if (!trackRef) return;
    setDragging(true);
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    handlePointer(e.clientX);
  };
  const onPointerMove = (e: PointerEvent) => {
    if (!dragging()) return;
    handlePointer(e.clientX);
  };
  const onPointerUp = (e: PointerEvent) => {
    if (!dragging()) return;
    setDragging(false);
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
  };

  // Keyboard nudge: ← / → jump the playhead by 5s; Esc resumes live.
  const onKey = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement | null;
    if (target?.tagName === "INPUT" || target?.tagName === "TEXTAREA") return;
    if (e.key === "Escape" && !props.scrub.isLive()) {
      props.scrub.resumeLive();
      e.preventDefault();
      return;
    }
    const m = props.scrub.mode();
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      const w = win();
      if (!w) return;
      const step = e.shiftKey ? 30 : 5;
      const base = m.kind === "live" ? w.nowSecs : m.tSecs;
      const target = base + (e.key === "ArrowLeft" ? -step : step);
      if (target >= w.nowSecs - 0.5) {
        props.scrub.resumeLive();
      } else {
        props.scrub.scrubTo(target);
      }
      e.preventDefault();
    }
  };

  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));

  const haveData = () => earliest() !== null && right() !== null;

  return (
    <div class="border-t border-zinc-800/80 bg-zinc-950 px-3 py-2 flex items-center gap-3 select-none">
      <button
        type="button"
        class="text-[10px] uppercase tracking-wider px-2 py-1 rounded border transition-colors"
        classList={{
          "border-emerald-500/30 bg-emerald-500/10 text-emerald-300":
            props.scrub.isLive(),
          "border-zinc-800 text-zinc-500 hover:text-zinc-300 hover:border-zinc-700":
            !props.scrub.isLive(),
        }}
        onClick={() => props.scrub.resumeLive()}
        title={props.scrub.isLive() ? "live" : "resume live (Esc)"}
      >
        <span class="inline-flex items-center gap-1.5">
          <span
            class="size-1.5 rounded-full"
            classList={{
              "bg-emerald-400 animate-pulse": props.scrub.isLive(),
              "bg-zinc-600": !props.scrub.isLive(),
            }}
          />
          {props.scrub.isLive() ? "Live" : "Live ▸"}
        </span>
      </button>

      <div
        ref={trackRef}
        class="relative flex-1 h-6 cursor-pointer rounded"
        classList={{
          "bg-zinc-900/60": haveData(),
          "bg-zinc-900/30": !haveData(),
        }}
        onPointerDown={haveData() ? onPointerDown : undefined}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        title={haveData() ? "drag to scrub · click rightmost edge for live" : "no buffered data yet"}
      >
        <Show when={haveData()}>
          {/* Filled region from left edge up to the playhead. */}
          <div
            class="absolute inset-y-0 left-0 rounded-l"
            classList={{
              "bg-amber-500/15": !props.scrub.isLive(),
              "bg-emerald-500/10": props.scrub.isLive(),
            }}
            style={{ width: `${playheadFraction() * 100}%` }}
          />
          {/* Tick marks every ~minute. Visual rhythm only — not interactive. */}
          <TickMarks scrub={props.scrub} />
          {/* Playhead. */}
          <div
            class="absolute top-0 bottom-0 w-px"
            classList={{
              "bg-amber-400": !props.scrub.isLive(),
              "bg-emerald-400": props.scrub.isLive(),
            }}
            style={{ left: `${playheadFraction() * 100}%` }}
          >
            <div
              class="absolute -top-1 -translate-x-1/2 size-2.5 rounded-full"
              classList={{
                "bg-amber-400": !props.scrub.isLive(),
                "bg-emerald-400": props.scrub.isLive(),
              }}
            />
          </div>
        </Show>
        <Show when={!haveData()}>
          <div class="absolute inset-0 flex items-center justify-center text-[9px] text-zinc-700 uppercase tracking-wider">
            waiting for flow data…
          </div>
        </Show>
      </div>

      <div class="flex items-center gap-2 text-[10px] tabular-nums min-w-[5rem] justify-end">
        <span
          classList={{
            "text-emerald-300": props.scrub.isLive(),
            "text-amber-300": !props.scrub.isLive(),
          }}
        >
          {ageLabel()}
        </span>
        <Show when={props.scrub.loading() && !props.scrub.isLive()}>
          <span class="size-1.5 rounded-full bg-amber-400 animate-pulse" />
        </Show>
      </div>
    </div>
  );
};

/**
 * Minute-spaced tick marks across the visible domain. Number of ticks
 * scales with the window so we don't end up with 60 ticks at long windows
 * or 1 tick at short ones — aim for ~6 evenly spaced marks.
 */
const TickMarks: Component<{ scrub: ScrubStore }> = (props) => {
  const ticks = createMemo<number[]>(() => {
    const w = props.scrub.window();
    if (!w) return [];
    const left = w.earliestSecs === null
      ? w.nowSecs - w.maxWindowSecs
      : Math.max(w.earliestSecs, w.nowSecs - w.maxWindowSecs);
    const span = Math.max(1, w.nowSecs - left);
    const stepCandidates = [10, 30, 60, 120, 300, 600];
    const step = stepCandidates.find((s) => span / s <= 8) ?? 600;
    const out: number[] = [];
    // Anchor ticks to "now" so the rightmost tick is always at the right
    // edge — gives a stable visual rhythm as the bar shifts.
    for (let t = w.nowSecs; t > left; t -= step) {
      out.push((t - left) / span);
    }
    return out;
  });
  return (
    <>
      {ticks().map((f) => (
        <div
          class="absolute inset-y-1 w-px bg-zinc-800/70"
          style={{ left: `${f * 100}%` }}
        />
      ))}
    </>
  );
};

export default TimeScrubber;
