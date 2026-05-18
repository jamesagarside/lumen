import { createMemo, Show, type Component } from "solid-js";
import { displayName, type Edge, type Node, type Snapshot } from "./types";

interface Props {
  snapshot: Snapshot | null;
  selectedEdge: { src: string; dst: string } | null;
  onClose: () => void;
  onSelectIp: (ip: string) => void;
}

/**
 * Right-pane inspector for an edge selection. Lumen aggregates flows
 * by (src,dst) pair, so each direction is a distinct edge in the graph
 * — but humans want to see them together. We look up both directions
 * here and present them as one panel with a reversible header.
 */
const EdgeInspector: Component<Props> = (props) => {
  const nodeById = createMemo<Map<string, Node>>(() => {
    const m = new Map<string, Node>();
    for (const n of props.snapshot?.nodes ?? []) m.set(n.id, n);
    return m;
  });

  const peers = createMemo<{ src: Node | null; dst: Node | null }>(() => {
    const sel = props.selectedEdge;
    if (!sel) return { src: null, dst: null };
    return {
      src: nodeById().get(sel.src) ?? null,
      dst: nodeById().get(sel.dst) ?? null,
    };
  });

  const forwardEdge = createMemo<Edge | null>(() => {
    const sel = props.selectedEdge;
    if (!sel || !props.snapshot) return null;
    return (
      props.snapshot.edges.find((e) => e.id.src === sel.src && e.id.dst === sel.dst) ?? null
    );
  });

  const reverseEdge = createMemo<Edge | null>(() => {
    const sel = props.selectedEdge;
    if (!sel || !props.snapshot) return null;
    return (
      props.snapshot.edges.find((e) => e.id.src === sel.dst && e.id.dst === sel.src) ?? null
    );
  });

  return (
    <Show when={props.selectedEdge}>
      {(sel) => (
        <div class="flex flex-col h-full">
          <div class="flex items-baseline justify-between px-3 py-2 border-b border-zinc-800/60">
            <div class="flex items-baseline gap-2">
              <span class="text-[10px] uppercase tracking-wider text-zinc-500">
                Edge
              </span>
              <span class="text-[10px] uppercase tracking-wider text-zinc-700">
                {reverseEdge() ? "bidirectional" : "one-way"}
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
            {/* Endpoints — both clickable to pivot the inspector. */}
            <div class="flex items-center gap-2 text-sm">
              <EndpointButton
                ip={sel().src}
                node={peers().src}
                onClick={() => props.onSelectIp(sel().src)}
              />
              <span class="text-zinc-700 text-base">→</span>
              <EndpointButton
                ip={sel().dst}
                node={peers().dst}
                onClick={() => props.onSelectIp(sel().dst)}
              />
            </div>

            <DirectionStats
              label={`${shortName(peers().src, sel().src)} → ${shortName(peers().dst, sel().dst)}`}
              edge={forwardEdge()}
            />
            <Show when={reverseEdge()}>
              <DirectionStats
                label={`${shortName(peers().dst, sel().dst)} → ${shortName(peers().src, sel().src)}`}
                edge={reverseEdge()}
              />
            </Show>

            <Show when={forwardEdge() || reverseEdge()}>
              {(_) => {
                const combined = () => {
                  const f = forwardEdge();
                  const r = reverseEdge();
                  const bytes = (f?.bytes_total ?? 0) + (r?.bytes_total ?? 0);
                  const packets = (f?.packets_total ?? 0) + (r?.packets_total ?? 0);
                  const flows = (f?.flows_seen ?? 0) + (r?.flows_seen ?? 0);
                  const bps = (f?.bytes_per_sec ?? 0) + (r?.bytes_per_sec ?? 0);
                  return { bytes, packets, flows, bps };
                };
                return (
                  <div>
                    <div class="text-[9px] uppercase tracking-wider text-zinc-600">
                      Combined
                    </div>
                    <div class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[11px] mt-1 tabular-nums">
                      <span class="text-zinc-600">rate</span>
                      <span class="text-zinc-200">{formatRate(combined().bps)}</span>
                      <span class="text-zinc-600">bytes</span>
                      <span class="text-zinc-200">{formatBytes(combined().bytes)}</span>
                      <span class="text-zinc-600">packets</span>
                      <span class="text-zinc-200">
                        {combined().packets.toLocaleString()}
                      </span>
                      <span class="text-zinc-600">flows</span>
                      <span class="text-zinc-200">
                        {combined().flows.toLocaleString()}
                      </span>
                    </div>
                  </div>
                );
              }}
            </Show>

            <Timestamps forward={forwardEdge()} reverse={reverseEdge()} />
          </div>
        </div>
      )}
    </Show>
  );
};

const EndpointButton: Component<{
  ip: string;
  node: Node | null;
  onClick: () => void;
}> = (props) => {
  const isInternal = () => props.node?.is_internal ?? false;
  const name = () => (props.node ? displayName(props.node) : props.ip);
  return (
    <button
      type="button"
      onClick={props.onClick}
      class="flex items-baseline gap-1.5 px-1 -mx-1 rounded hover:bg-zinc-900/80 text-left min-w-0"
      title={`open inspector for ${props.ip}`}
    >
      <span
        class={`size-1.5 rounded-full flex-shrink-0 ${isInternal() ? "bg-emerald-500/70" : "bg-zinc-600"}`}
        aria-hidden
      />
      <span class="text-zinc-100 truncate">{name()}</span>
      <Show when={name() !== props.ip}>
        <span class="text-[10px] text-zinc-600 font-mono truncate">{props.ip}</span>
      </Show>
    </button>
  );
};

const DirectionStats: Component<{ label: string; edge: Edge | null }> = (props) => {
  return (
    <div>
      <div class="text-[9px] uppercase tracking-wider text-zinc-600 truncate" title={props.label}>
        {props.label}
      </div>
      <Show
        when={props.edge}
        fallback={
          <div class="text-[10px] text-zinc-700 mt-0.5">no flows in this direction</div>
        }
      >
        {(e) => (
          <div class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[11px] mt-1 tabular-nums">
            <span class="text-zinc-600">rate</span>
            <span class="text-amber-300">{formatRate(e().bytes_per_sec)}</span>
            <span class="text-zinc-600">bytes</span>
            <span class="text-zinc-200">{formatBytes(e().bytes_total)}</span>
            <span class="text-zinc-600">packets</span>
            <span class="text-zinc-300">{e().packets_total.toLocaleString()}</span>
            <span class="text-zinc-600">flows</span>
            <span class="text-zinc-300">{e().flows_seen.toLocaleString()}</span>
          </div>
        )}
      </Show>
    </div>
  );
};

const Timestamps: Component<{
  forward: Edge | null;
  reverse: Edge | null;
}> = (props) => {
  const newestLast = createMemo(() => {
    const f = props.forward?.last_seen.secs_since_epoch ?? 0;
    const r = props.reverse?.last_seen.secs_since_epoch ?? 0;
    return Math.max(f, r);
  });
  const oldestFirst = createMemo(() => {
    const f = props.forward?.first_seen.secs_since_epoch ?? Number.MAX_SAFE_INTEGER;
    const r = props.reverse?.first_seen.secs_since_epoch ?? Number.MAX_SAFE_INTEGER;
    return Math.min(f, r);
  });
  return (
    <div class="grid grid-cols-2 gap-x-3 gap-y-0.5 text-[10px]">
      <div>
        <div class="text-[9px] uppercase tracking-wider text-zinc-600">Last seen</div>
        <div class="text-zinc-200 tabular-nums">
          {newestLast() === 0 ? "—" : relativeTime(newestLast())}
        </div>
      </div>
      <div>
        <div class="text-[9px] uppercase tracking-wider text-zinc-600">First seen</div>
        <div class="text-zinc-500 tabular-nums">
          {oldestFirst() === Number.MAX_SAFE_INTEGER ? "—" : relativeTime(oldestFirst())}
        </div>
      </div>
    </div>
  );
};

const shortName = (n: Node | null, ip: string): string => {
  if (!n) return ip;
  const name = displayName(n);
  return name.length > 18 ? name.slice(0, 17) + "…" : name;
};

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

/** Mirror of the inspector's relativeTime — kept here to avoid a cross-component import. */
const relativeTime = (epochSecs: number): string => {
  const delta = Math.max(0, Math.floor(Date.now() / 1000 - epochSecs));
  if (delta < 5) return "just now";
  if (delta < 60) return `${delta}s ago`;
  if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
  if (delta < 7 * 86400) return `${Math.floor(delta / 86400)}d ago`;
  return new Date(epochSecs * 1000).toISOString().slice(0, 10);
};

export default EdgeInspector;
