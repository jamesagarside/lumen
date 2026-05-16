import { createMemo, For, type Component } from "solid-js";
import type { Snapshot } from "./types";

interface NodePos {
  id: string;
  x: number;
  y: number;
  internal: boolean;
}

interface EdgeLine {
  from: NodePos;
  to: NodePos;
  bytesPerSec: number;
}

const VIEWBOX = { w: 800, h: 480 };

const FlowGraph: Component<{ snapshot: Snapshot | null }> = (props) => {
  const layout = createMemo(() => {
    const snap = props.snapshot;
    if (!snap || snap.nodes.length === 0) {
      return { nodes: [] as NodePos[], edges: [] as EdgeLine[], maxRate: 1 };
    }

    const internal = snap.nodes.filter((n) => n.is_internal).map((n) => n.id);
    const external = snap.nodes.filter((n) => !n.is_internal).map((n) => n.id);

    const cx = VIEWBOX.w / 2;
    const cy = VIEWBOX.h / 2;
    const positions = new Map<string, NodePos>();

    internal.forEach((ip, i) => {
      const angle = (i / Math.max(internal.length, 1)) * Math.PI * 2;
      const r = internal.length === 1 ? 0 : 70;
      positions.set(ip, {
        id: ip,
        x: cx + Math.cos(angle) * r,
        y: cy + Math.sin(angle) * r,
        internal: true,
      });
    });

    external.forEach((ip, i) => {
      const angle = (i / Math.max(external.length, 1)) * Math.PI * 2 - Math.PI / 2;
      const r = Math.min(VIEWBOX.w, VIEWBOX.h) / 2 - 50;
      positions.set(ip, {
        id: ip,
        x: cx + Math.cos(angle) * r,
        y: cy + Math.sin(angle) * r,
        internal: false,
      });
    });

    const edges: EdgeLine[] = [];
    let maxRate = 1;
    for (const e of snap.edges) {
      const from = positions.get(e.id.src);
      const to = positions.get(e.id.dst);
      if (!from || !to) continue;
      edges.push({ from, to, bytesPerSec: e.bytes_per_sec });
      if (e.bytes_per_sec > maxRate) maxRate = e.bytes_per_sec;
    }

    return { nodes: [...positions.values()], edges, maxRate };
  });

  return (
    <svg
      viewBox={`0 0 ${VIEWBOX.w} ${VIEWBOX.h}`}
      class="w-full h-full"
      preserveAspectRatio="xMidYMid meet"
    >
      <For each={layout().edges}>
        {(e) => {
          const intensity = Math.max(0.15, e.bytesPerSec / layout().maxRate);
          return (
            <line
              x1={e.from.x}
              y1={e.from.y}
              x2={e.to.x}
              y2={e.to.y}
              stroke={`rgb(251 191 36 / ${intensity})`}
              stroke-width={Math.max(0.5, intensity * 3)}
            />
          );
        }}
      </For>
      <For each={layout().nodes}>
        {(n) => (
          <g>
            <circle
              cx={n.x}
              cy={n.y}
              r={n.internal ? 7 : 4}
              fill={n.internal ? "rgb(244 244 245)" : "rgb(113 113 122)"}
              stroke="rgb(24 24 27)"
              stroke-width="1.5"
            />
            <text
              x={n.x}
              y={n.y - 12}
              text-anchor="middle"
              class="fill-zinc-500 text-[9px] font-mono"
            >
              {n.id}
            </text>
          </g>
        )}
      </For>
    </svg>
  );
};

export default FlowGraph;
