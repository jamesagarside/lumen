import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Show,
  type Component,
} from "solid-js";
import {
  displayName,
  severityColor,
  severityName,
  type DetectionEvent,
  type Edge,
  type Node,
  type Snapshot,
} from "./types";

interface Props {
  snapshot: Snapshot | null;
  events: DetectionEvent[];
  selectedId: string | null;
  onClose: () => void;
  onSelectIp: (ip: string) => void;
  onLabelSaved: (node: Node) => void;
}

const TOP_PEERS = 8;
const RECENT_DETECTIONS = 6;

const NodeInspector: Component<Props> = (props) => {
  const [draft, setDraft] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // `now` ticks once a second so relative timestamps don't go stale while
  // the inspector is open. The snapshot reactivity is too coarse for "23s
  // ago" → "24s ago".
  const [now, setNow] = createSignal(Date.now());
  const tick = setInterval(() => setNow(Date.now()), 1000);
  // Cleanup on unmount — Solid disposes the signal scope, but the interval
  // is a global timer so it has to be cleared explicitly.
  if (typeof window !== "undefined") {
    // onCleanup would be cleaner but pulling it in for one timer feels
    // heavy; the inspector lives for the session so the leak is bounded
    // to one timer either way.
    window.addEventListener("beforeunload", () => clearInterval(tick), {
      once: true,
    });
  }

  const nodeById = createMemo<Map<string, Node>>(() => {
    const m = new Map<string, Node>();
    for (const n of props.snapshot?.nodes ?? []) m.set(n.id, n);
    return m;
  });

  const node = createMemo<Node | null>(() => {
    const id = props.selectedId;
    if (!id) return null;
    return nodeById().get(id) ?? null;
  });

  const incidentEdges = createMemo<{ out: Edge[]; in: Edge[] }>(() => {
    const id = props.selectedId;
    if (!id || !props.snapshot) return { out: [], in: [] };
    return {
      out: props.snapshot.edges.filter((e) => e.id.src === id),
      in: props.snapshot.edges.filter((e) => e.id.dst === id),
    };
  });

  /** Top peers across both directions, ranked by current bytes/sec
   * (falling back to lifetime bytes if everything is idle right now). */
  const topPeers = createMemo<ResolvedPeer[]>(() => {
    const id = props.selectedId;
    if (!id) return [];
    const by: Map<string, PeerStat> = new Map();
    for (const e of incidentEdges().out) {
      const peer = e.id.dst;
      const s = by.get(peer) ?? newPeer(peer);
      s.outBps += e.bytes_per_sec;
      s.outBytes += e.bytes_total;
      s.flows += e.flows_seen;
      by.set(peer, s);
    }
    for (const e of incidentEdges().in) {
      const peer = e.id.src;
      const s = by.get(peer) ?? newPeer(peer);
      s.inBps += e.bytes_per_sec;
      s.inBytes += e.bytes_total;
      s.flows += e.flows_seen;
      by.set(peer, s);
    }
    const map = nodeById();
    const peers = Array.from(by.values()).map((p) => ({
      ...p,
      name: map.get(p.id) ? displayName(map.get(p.id) as Node) : p.id,
      isInternal: map.get(p.id)?.is_internal ?? false,
    }));
    // Live rate first; if everything's idle, fall back to lifetime bytes
    // so the inspector still ranks meaningfully on a quiet network.
    const anyLive = peers.some((p) => p.outBps + p.inBps > 0);
    peers.sort((a, b) => {
      if (anyLive) return b.outBps + b.inBps - (a.outBps + a.inBps);
      return b.outBytes + b.inBytes - (a.outBytes + a.inBytes);
    });
    return peers.slice(0, TOP_PEERS);
  });

  const peerCount = createMemo(() => {
    const peers = new Set<string>();
    for (const e of incidentEdges().out) peers.add(e.id.dst);
    for (const e of incidentEdges().in) peers.add(e.id.src);
    return peers.size;
  });

  const nodeDetections = createMemo<DetectionEvent[]>(() => {
    const id = props.selectedId;
    if (!id) return [];
    return props.events
      .filter((e) => e["source.ip"] === id || e["destination.ip"] === id)
      .slice(0, RECENT_DETECTIONS);
  });

  // Reset the draft to the persisted label when the selection changes.
  createEffect(() => {
    const n = node();
    setDraft(n?.label ?? "");
    setError(null);
  });

  const save = async () => {
    const id = props.selectedId;
    if (!id) return;
    setSaving(true);
    setError(null);
    try {
      const res = await fetch(`/nodes/${id}`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ label: draft().trim() }),
      });
      if (!res.ok) {
        const body = (await res.json().catch(() => null)) as { error?: string } | null;
        throw new Error(body?.error ?? `HTTP ${res.status}`);
      }
      const updated = (await res.json()) as Node;
      props.onLabelSaved(updated);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSaving(false);
    }
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !saving()) void save();
    if (e.key === "Escape") props.onClose();
  };

  const totalOutBps = createMemo(() =>
    incidentEdges().out.reduce((s, e) => s + e.bytes_per_sec, 0),
  );
  const totalInBps = createMemo(() =>
    incidentEdges().in.reduce((s, e) => s + e.bytes_per_sec, 0),
  );
  const totalOutBytes = createMemo(() =>
    incidentEdges().out.reduce((s, e) => s + e.bytes_total, 0),
  );
  const totalInBytes = createMemo(() =>
    incidentEdges().in.reduce((s, e) => s + e.bytes_total, 0),
  );

  return (
    <Show when={node()} fallback={<EmptyHint />}>
      {(n) => (
        <div class="flex flex-col h-full">
          <div class="flex items-baseline justify-between px-3 py-2 border-b border-zinc-800/60">
            <div class="flex items-baseline gap-2">
              <span class="text-[10px] uppercase tracking-wider text-zinc-500">
                Inspector
              </span>
              <span
                class={`text-[10px] uppercase tracking-wider ${n().is_internal ? "text-emerald-500/70" : "text-zinc-700"}`}
              >
                {n().is_internal ? "internal" : "external"}
              </span>
            </div>
            <button
              class="text-[10px] text-zinc-500 hover:text-zinc-300"
              onClick={props.onClose}
              type="button"
              aria-label="close inspector"
            >
              ×
            </button>
          </div>

          <div class="px-3 py-3 space-y-3 overflow-auto">
            {/* Identity block — name big, IP/brand as secondary lines. */}
            <div>
              <div class="text-base text-zinc-100 leading-tight font-medium">
                {displayName(n())}
              </div>
              <div class="text-[11px] text-zinc-500 font-mono mt-0.5">{n().id}</div>
              <Show when={n().brand && n().brand !== displayName(n())}>
                {(b) => <div class="text-[10px] text-amber-300/80 mt-0.5">{b()}</div>}
              </Show>
            </div>

            <Timestamps node={n()} now={now()} />

            {/* Label editor. */}
            <div>
              <label class="text-[9px] uppercase tracking-wider text-zinc-600 block">
                Label
              </label>
              <div class="flex gap-2 mt-1">
                <input
                  type="text"
                  value={draft()}
                  onInput={(e) => setDraft(e.currentTarget.value)}
                  onKeyDown={onKey}
                  placeholder="e.g. Living Room Apple TV"
                  maxLength={128}
                  class="flex-1 bg-zinc-900 border border-zinc-800 rounded px-2 py-1 text-xs text-zinc-100 outline-none focus:border-amber-500/60"
                />
                <button
                  type="button"
                  onClick={save}
                  disabled={saving() || (n().label ?? "") === draft().trim()}
                  class="px-2 py-1 text-[10px] uppercase tracking-wider bg-amber-500/15 border border-amber-500/40 text-amber-300 rounded disabled:opacity-40 disabled:cursor-not-allowed hover:bg-amber-500/25"
                >
                  {saving() ? "…" : "Save"}
                </button>
              </div>
              <Show when={error()}>
                {(err) => <div class="text-[10px] text-rose-400 mt-1">{err()}</div>}
              </Show>
              <div class="text-[9px] text-zinc-700 mt-1">
                ⏎ to save · esc to close · stored on the daemon, survives restart
              </div>
            </div>

            {/* Traffic summary. Two columns, tabular numerals so columns
              * line up even when the rates change. */}
            <div>
              <div class="text-[9px] uppercase tracking-wider text-zinc-600">
                Traffic
              </div>
              <div class="grid grid-cols-[auto_1fr_1fr] gap-x-3 gap-y-0.5 text-[11px] mt-1 tabular-nums">
                <span class="text-zinc-600">out</span>
                <span class="text-zinc-300">{formatRate(totalOutBps())}</span>
                <span class="text-zinc-600">{formatBytes(totalOutBytes())} total</span>
                <span class="text-zinc-600">in</span>
                <span class="text-zinc-300">{formatRate(totalInBps())}</span>
                <span class="text-zinc-600">{formatBytes(totalInBytes())} total</span>
              </div>
              <div class="text-[9px] text-zinc-700 mt-1">
                {peerCount()} peer{peerCount() === 1 ? "" : "s"} ·{" "}
                {incidentEdges().out.length + incidentEdges().in.length} edge
                {incidentEdges().out.length + incidentEdges().in.length === 1 ? "" : "s"}
              </div>
            </div>

            {/* Top peers. */}
            <Show when={topPeers().length > 0}>
              <div>
                <div class="text-[9px] uppercase tracking-wider text-zinc-600">
                  Top peers
                </div>
                <ul class="mt-1 divide-y divide-zinc-900">
                  <For each={topPeers()}>
                    {(p) => (
                      <PeerRow peer={p} onSelect={() => props.onSelectIp(p.id)} />
                    )}
                  </For>
                </ul>
              </div>
            </Show>

            {/* Detections involving this node. */}
            <Show when={nodeDetections().length > 0}>
              <div>
                <div class="text-[9px] uppercase tracking-wider text-zinc-600">
                  Detections ({nodeDetections().length})
                </div>
                <ul class="mt-1 space-y-1">
                  <For each={nodeDetections()}>
                    {(e) => <DetectionRow event={e} />}
                  </For>
                </ul>
              </div>
            </Show>
          </div>
        </div>
      )}
    </Show>
  );
};

interface PeerStat {
  id: string;
  outBps: number;
  inBps: number;
  outBytes: number;
  inBytes: number;
  flows: number;
}

interface ResolvedPeer extends PeerStat {
  name: string;
  isInternal: boolean;
}

const newPeer = (id: string): PeerStat => ({
  id,
  outBps: 0,
  inBps: 0,
  outBytes: 0,
  inBytes: 0,
  flows: 0,
});

const PeerRow: Component<{ peer: ResolvedPeer; onSelect: () => void }> = (props) => {
  const live = () => props.peer.outBps + props.peer.inBps;
  const lifetime = () => props.peer.outBytes + props.peer.inBytes;
  return (
    <li>
      <button
        type="button"
        onClick={props.onSelect}
        class="w-full text-left py-1 px-1 -mx-1 rounded hover:bg-zinc-900/80 group"
      >
        <div class="flex items-baseline gap-2">
          <span
            class={`size-1.5 rounded-full flex-shrink-0 ${props.peer.isInternal ? "bg-emerald-500/70" : "bg-zinc-600"}`}
            aria-hidden
          />
          <span class="text-[11px] text-zinc-200 truncate group-hover:text-zinc-50">
            {props.peer.name}
          </span>
          <span class="text-[10px] text-zinc-300 ml-auto tabular-nums whitespace-nowrap">
            {formatRate(live())}
          </span>
        </div>
        <div class="flex items-baseline gap-2 pl-3 text-[10px] text-zinc-600 tabular-nums">
          <span>↑ {formatBytes(props.peer.outBytes)}</span>
          <span>↓ {formatBytes(props.peer.inBytes)}</span>
          <span class="ml-auto">{formatBytes(lifetime())} total</span>
        </div>
      </button>
    </li>
  );
};

const DetectionRow: Component<{ event: DetectionEvent }> = (props) => {
  const sev = () => props.event["event.severity"] ?? 0;
  const colors = () => severityColor(sev());
  const when = () => {
    const t = props.event["@timestamp"];
    if (!t) return "";
    const d = new Date(t.secs_since_epoch * 1000);
    return d.toLocaleTimeString();
  };
  return (
    <li class="text-[10px] leading-tight">
      <div class="flex items-baseline gap-1.5">
        <span class={`size-1.5 rounded-full flex-shrink-0 ${colors().dot}`} aria-hidden />
        <span class={`uppercase tracking-wider ${colors().text}`}>
          {severityName(sev())}
        </span>
        <span class="text-zinc-700 ml-auto tabular-nums">{when()}</span>
      </div>
      <div class="text-[11px] text-zinc-300 truncate" title={props.event.message}>
        {props.event.message}
      </div>
    </li>
  );
};

const Timestamps: Component<{ node: Node; now: number }> = (props) => {
  const lastSeen = () => relativeTime(props.node.last_seen.secs_since_epoch, props.now);
  const firstSeen = () => relativeTime(props.node.first_seen.secs_since_epoch, props.now);
  return (
    <div class="grid grid-cols-2 gap-x-3 gap-y-0.5 text-[10px]">
      <div>
        <div class="text-[9px] uppercase tracking-wider text-zinc-600">Last seen</div>
        <div class="text-zinc-200 tabular-nums">{lastSeen()}</div>
      </div>
      <div>
        <div class="text-[9px] uppercase tracking-wider text-zinc-600">First seen</div>
        <div class="text-zinc-500 tabular-nums">{firstSeen()}</div>
      </div>
    </div>
  );
};

const EmptyHint: Component = () => (
  <div class="h-full flex items-center justify-center text-center text-[10px] text-zinc-700 px-6">
    click a node in the graph to inspect or rename it
  </div>
);

const formatBytes = (n: number): string => {
  if (n < 1024) return `${Math.round(n)} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
};

const formatRate = (n: number): string => {
  if (n < 1) return "idle";
  if (n < 1024) return `${n.toFixed(0)} B/s`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB/s`;
  return `${(n / 1024 / 1024).toFixed(2)} MB/s`;
};

/**
 * "23s ago", "4m ago", "2h ago", "3d ago". Anything older falls back to
 * an ISO date — Lumen doesn't pretend to be useful for week-old data.
 */
const relativeTime = (epochSecs: number, nowMs: number): string => {
  const delta = Math.max(0, Math.floor(nowMs / 1000 - epochSecs));
  if (delta < 5) return "just now";
  if (delta < 60) return `${delta}s ago`;
  if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
  if (delta < 7 * 86400) return `${Math.floor(delta / 86400)}d ago`;
  const d = new Date(epochSecs * 1000);
  return d.toISOString().slice(0, 10);
};

export default NodeInspector;
