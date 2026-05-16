import { onCleanup, onMount, createEffect, createSignal, type Component } from "solid-js";
import Graph from "graphology";
import forceAtlas2 from "graphology-layout-forceatlas2";
import Sigma from "sigma";
import type { Snapshot } from "./types";

// Anchor positions for the semantic layout (CONTEXT.md §8):
// gateway pinned at the top, internal devices cluster centred,
// external services arc the perimeter.
const ANCHOR_RADIUS_INTERNAL = 0.18;
const ANCHOR_RADIUS_EXTERNAL = 0.85;

const COLOR_INTERNAL = "#f4f4f5"; // zinc-100
const COLOR_EXTERNAL = "#a1a1aa"; // zinc-400
const COLOR_GATEWAY = "#fbbf24";  // amber-400 — visually distinct
const COLOR_DIM = "#3f3f46";      // zinc-700 — for backgrounded nodes
const COLOR_HIGHLIGHT_EDGE = "#fbbf24"; // amber-400, full intensity

interface NodeMeta {
  id: string;
  internal: boolean;
  isGateway: boolean;
}

const isGatewayLikeIp = (ip: string): boolean => {
  // Heuristic: x.x.x.1 within an internal range. Plugins eventually
  // override this with authoritative topology (#18).
  const parts = ip.split(".");
  return parts.length === 4 && parts[3] === "1";
};

const initialPosition = (
  meta: NodeMeta,
  internalIndex: number,
  internalCount: number,
  externalIndex: number,
  externalCount: number,
): { x: number; y: number } => {
  if (meta.isGateway) return { x: 0, y: -0.9 };
  if (meta.internal) {
    const angle = (internalIndex / Math.max(internalCount, 1)) * Math.PI * 2;
    const r = internalCount === 1 ? 0 : ANCHOR_RADIUS_INTERNAL;
    return { x: Math.cos(angle) * r, y: Math.sin(angle) * r };
  }
  const angle = (externalIndex / Math.max(externalCount, 1)) * Math.PI * 2 - Math.PI / 2;
  return {
    x: Math.cos(angle) * ANCHOR_RADIUS_EXTERNAL,
    y: Math.sin(angle) * ANCHOR_RADIUS_EXTERNAL,
  };
};

const SigmaGraph: Component<{ snapshot: Snapshot | null }> = (props) => {
  let container: HTMLDivElement | undefined;
  let sigma: Sigma | null = null;
  let graph: Graph | null = null;

  // Hover and selection are mutated from Sigma event handlers.
  // Refs (not signals) are fine because Sigma's reducer reads them
  // synchronously via closure on every render and we trigger
  // re-render via sigma.refresh().
  let hoveredNode: string | null = null;
  let selectedNode: string | null = null;
  const [, setSelectionTick] = createSignal(0); // forces App-side reactivity if needed later

  const focused = () => selectedNode || hoveredNode;

  const ensureSigma = () => {
    if (sigma || !container) return;
    graph = new Graph({ multi: false, type: "directed" });

    sigma = new Sigma(graph, container, {
      defaultNodeColor: COLOR_INTERNAL,
      defaultEdgeColor: "rgba(251, 191, 36, 0.4)",
      labelColor: { color: "#a1a1aa" }, // zinc-400
      labelFont: "ui-monospace, SFMono-Regular, Menlo, monospace",
      labelSize: 11,
      labelWeight: "400",
      renderLabels: true,
      renderEdgeLabels: false,
      enableEdgeEvents: false,
      minCameraRatio: 0.1,
      maxCameraRatio: 5,
      nodeReducer: (id, attrs) => {
        const f = focused();
        if (!f || !graph) return attrs;
        if (id === f) {
          return { ...attrs, color: COLOR_HIGHLIGHT_EDGE, zIndex: 2, forceLabel: true };
        }
        if (graph.areNeighbors(f, id)) {
          return { ...attrs, zIndex: 1, forceLabel: true };
        }
        return { ...attrs, color: COLOR_DIM, label: "", zIndex: 0 };
      },
      edgeReducer: (id, attrs) => {
        const f = focused();
        if (!f || !graph) return attrs;
        const [src, dst] = graph.extremities(id);
        if (src === f || dst === f) {
          return { ...attrs, color: COLOR_HIGHLIGHT_EDGE, size: (attrs.size ?? 1) * 1.5, zIndex: 1 };
        }
        return { ...attrs, hidden: true };
      },
    });

    sigma.on("enterNode", ({ node }) => {
      hoveredNode = node;
      sigma?.refresh();
    });
    sigma.on("leaveNode", () => {
      hoveredNode = null;
      sigma?.refresh();
    });
    sigma.on("clickNode", ({ node }) => {
      selectedNode = selectedNode === node ? null : node;
      setSelectionTick((n) => n + 1);
      sigma?.refresh();
    });
    sigma.on("clickStage", () => {
      selectedNode = null;
      setSelectionTick((n) => n + 1);
      sigma?.refresh();
    });
  };

  onMount(() => ensureSigma());

  onCleanup(() => {
    sigma?.kill();
    sigma = null;
    graph = null;
  });

  // Apply each snapshot to the Sigma graph.
  createEffect(() => {
    const snap = props.snapshot;
    if (!snap || !graph || !sigma) return;
    const hadNewNodes = applySnapshot(graph, snap);
    if (hadNewNodes) runIncrementalLayout(graph);
    sigma.refresh();
  });

  return (
    <div class="relative w-full h-full">
      <div ref={container} class="absolute inset-0" style={{ background: "rgb(9 9 11)" }} />
      <Hint />
    </div>
  );
};

const Hint: Component = () => (
  <div class="absolute bottom-3 left-3 text-[9px] text-zinc-700 font-mono pointer-events-none select-none">
    drag to pan · scroll to zoom · hover or click a node
  </div>
);

/**
 * Reconcile the Sigma graph against the latest snapshot. Returns
 * true if any new nodes were added — caller uses this to decide
 * whether a layout run is needed (no-op when topology is stable).
 */
function applySnapshot(graph: Graph, snap: Snapshot): boolean {
  const incomingNodes = new Set(snap.nodes.map((n) => n.id));
  const incomingEdges = new Set(snap.edges.map((e) => `${e.id.src}->${e.id.dst}`));

  // Drop entities that disappeared from the topology.
  for (const id of [...graph.nodes()]) {
    if (!incomingNodes.has(id)) graph.dropNode(id);
  }
  for (const key of [...graph.edges()]) {
    if (!incomingEdges.has(key)) graph.dropEdge(key);
  }

  // Pre-compute per-ring indices for new nodes so anchors fan out evenly.
  const newInternal = snap.nodes.filter((n) => n.is_internal && !graph.hasNode(n.id));
  const newExternal = snap.nodes.filter((n) => !n.is_internal && !graph.hasNode(n.id));
  const internalCount = snap.nodes.filter((n) => n.is_internal).length;
  const externalCount = snap.nodes.filter((n) => !n.is_internal).length;

  let internalIdx = countWith(graph, (a) => a.internal === true);
  let externalIdx = countWith(graph, (a) => a.internal === false);
  const hadNewNodes = newInternal.length > 0 || newExternal.length > 0;

  for (const node of [...newInternal, ...newExternal]) {
    const meta: NodeMeta = {
      id: node.id,
      internal: node.is_internal,
      isGateway: node.is_internal && isGatewayLikeIp(node.id),
    };
    const pos = initialPosition(
      meta,
      meta.internal ? internalIdx++ : 0,
      internalCount,
      meta.internal ? 0 : externalIdx++,
      externalCount,
    );
    const baseColor = meta.isGateway
      ? COLOR_GATEWAY
      : meta.internal
        ? COLOR_INTERNAL
        : COLOR_EXTERNAL;
    graph.addNode(node.id, {
      x: pos.x,
      y: pos.y,
      size: meta.isGateway ? 9 : meta.internal ? 6 : 4,
      color: baseColor,
      label: node.id,
      internal: meta.internal,
      isGateway: meta.isGateway,
    });
  }

  // Edges: add new ones, update sizes/colors for existing. Intensity
  // scales by sqrt so a 10× bandwidth difference shows as ~3× stroke
  // — keeps the high-traffic edges legible without making the
  // low-traffic ones invisible.
  const maxRate = Math.max(1, ...snap.edges.map((e) => e.bytes_per_sec));
  for (const e of snap.edges) {
    const key = `${e.id.src}->${e.id.dst}`;
    const rateRatio = e.bytes_per_sec / maxRate;
    const intensity = Math.max(0.08, Math.sqrt(rateRatio));
    const size = 0.4 + intensity * 2.5;
    const color = `rgba(251, 191, 36, ${intensity * 0.85})`;
    if (graph.hasEdge(key)) {
      graph.setEdgeAttribute(key, "size", size);
      graph.setEdgeAttribute(key, "color", color);
      graph.setEdgeAttribute(key, "weight", e.bytes_per_sec);
    } else if (graph.hasNode(e.id.src) && graph.hasNode(e.id.dst)) {
      graph.addEdgeWithKey(key, e.id.src, e.id.dst, {
        size,
        color,
        weight: e.bytes_per_sec,
      });
    }
  }
  return hadNewNodes;
}

function countWith(graph: Graph, pred: (a: { internal?: boolean }) => boolean): number {
  let n = 0;
  graph.forEachNode((_id, a) => {
    if (pred(a as { internal?: boolean })) n++;
  });
  return n;
}

/**
 * Settle the layout. Tuning notes:
 *  - lower `gravity` + higher `scalingRatio` spread nodes apart so
 *    the graph doesn't collapse into a hairball at scale;
 *  - `edgeWeightInfluence: 0` keeps high-bandwidth edges from yanking
 *    their endpoints together (rate is shown via stroke, not position);
 *  - `linLogMode` separates clusters more cleanly than linear mode
 *    when there are many edges per node.
 * Cheap (~1ms for a few hundred nodes); we re-run on every snapshot
 * that introduces new nodes so the topology breathes naturally.
 */
function runIncrementalLayout(graph: Graph) {
  if (graph.order < 2) return;
  forceAtlas2.assign(graph, {
    iterations: 80,
    settings: {
      gravity: 0.3,
      scalingRatio: 30,
      strongGravityMode: false,
      slowDown: 2,
      barnesHutOptimize: graph.order > 100,
      adjustSizes: true,
      edgeWeightInfluence: 0,
      linLogMode: true,
      outboundAttractionDistribution: false,
    },
  });
}

export default SigmaGraph;
