import { createMemo, For, type Component } from "solid-js";
import type { Flow } from "./types";

interface NodePos {
  id: string;
  x: number;
  y: number;
  internal: boolean;
}

interface Edge {
  from: NodePos;
  to: NodePos;
  weight: number;
}

const isPrivate = (ip: string): boolean => {
  if (ip.startsWith("10.") || ip.startsWith("192.168.")) return true;
  if (ip.startsWith("172.")) {
    const second = parseInt(ip.split(".")[1] ?? "0", 10);
    return second >= 16 && second <= 31;
  }
  if (ip.startsWith("127.") || ip === "::1") return true;
  if (ip.startsWith("fe80:")) return true;
  return false;
};

const VIEWBOX = { w: 800, h: 480 };

const FlowGraph: Component<{ flows: Flow[] }> = (props) => {
  const layout = createMemo(() => {
    // Aggregate flows into edge weights per (src, dst) IP pair.
    const edgeWeights = new Map<string, number>();
    const ips = new Set<string>();
    for (const f of props.flows) {
      ips.add(f.src.ip);
      ips.add(f.dst.ip);
      const key = `${f.src.ip}|${f.dst.ip}`;
      edgeWeights.set(key, (edgeWeights.get(key) ?? 0) + f.bytes);
    }

    // Internal nodes laid out in a centre cluster, external around the
    // perimeter. This is a placeholder layout — the real semantic-anchor
    // force-directed layout lands in #23.
    const internal: string[] = [];
    const external: string[] = [];
    for (const ip of ips) {
      (isPrivate(ip) ? internal : external).push(ip);
    }
    internal.sort();
    external.sort();

    const positions = new Map<string, NodePos>();
    const cx = VIEWBOX.w / 2;
    const cy = VIEWBOX.h / 2;

    internal.forEach((ip, i) => {
      const angle = (i / Math.max(internal.length, 1)) * Math.PI * 2;
      const r = internal.length === 1 ? 0 : 60;
      positions.set(ip, {
        id: ip,
        x: cx + Math.cos(angle) * r,
        y: cy + Math.sin(angle) * r,
        internal: true,
      });
    });

    external.forEach((ip, i) => {
      const angle = (i / Math.max(external.length, 1)) * Math.PI * 2;
      const r = Math.min(VIEWBOX.w, VIEWBOX.h) / 2 - 40;
      positions.set(ip, {
        id: ip,
        x: cx + Math.cos(angle) * r,
        y: cy + Math.sin(angle) * r,
        internal: false,
      });
    });

    const edges: Edge[] = [];
    for (const [key, weight] of edgeWeights) {
      const [src, dst] = key.split("|");
      const from = positions.get(src);
      const to = positions.get(dst);
      if (from && to) edges.push({ from, to, weight });
    }

    const maxWeight = Math.max(1, ...edges.map((e) => e.weight));

    return {
      nodes: [...positions.values()],
      edges,
      maxWeight,
    };
  });

  return (
    <svg
      viewBox={`0 0 ${VIEWBOX.w} ${VIEWBOX.h}`}
      class="w-full h-full"
      preserveAspectRatio="xMidYMid meet"
    >
      <For each={layout().edges}>
        {(e) => (
          <line
            x1={e.from.x}
            y1={e.from.y}
            x2={e.to.x}
            y2={e.to.y}
            stroke="rgb(251 191 36 / 0.4)"
            stroke-width={Math.max(0.5, (e.weight / layout().maxWeight) * 3)}
          />
        )}
      </For>
      <For each={layout().nodes}>
        {(n) => (
          <g>
            <circle
              cx={n.x}
              cy={n.y}
              r={n.internal ? 6 : 4}
              fill={n.internal ? "rgb(244 244 245)" : "rgb(113 113 122)"}
              stroke="rgb(24 24 27)"
              stroke-width="1.5"
            />
            <text
              x={n.x}
              y={n.y - 10}
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
