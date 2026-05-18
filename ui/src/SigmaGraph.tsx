import { onCleanup, onMount, createEffect, Show, type Component } from "solid-js";
import Graph from "graphology";
import forceAtlas2 from "graphology-layout-forceatlas2";
import Sigma from "sigma";
import GraphEmptyState from "./GraphEmptyState";
import {
  graphAnchor,
  isGatewayLikeIp,
  layoutArchitecture,
  layoutVlan,
  type ViewKind,
} from "./layouts";
import { displayName, severityColor } from "./types";
import type { DetectionEvent, Snapshot } from "./types";

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

interface Props {
  snapshot: Snapshot | null;
  /** Recent detection events — drives the halo overlay. */
  events?: DetectionEvent[];
  onSelectionChange?: (id: string | null) => void;
  selectedNodeId?: string | null;
  /** Edge selection. `{src,dst}` per the wire shape; null = no edge selected. */
  onEdgeSelectionChange?: (id: { src: string; dst: string } | null) => void;
  selectedEdgeId?: { src: string; dst: string } | null;
  /** Trigger a one-shot full re-layout (called when the user clicks Re-layout). */
  relayoutSignal?: number;
  /** Which layout to apply. Changing this re-runs the positioner. */
  view?: ViewKind;
}



const SigmaGraph: Component<Props> = (props) => {
  let container: HTMLDivElement | undefined;
  let sigma: Sigma | null = null;
  let graph: Graph | null = null;

  let hoveredNode: string | null = null;
  let selectedNode: string | null = null;
  let selectedEdge: string | null = null;
  let draggedNode: string | null = null;
  let dragSuppressClick = false;

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
      enableEdgeEvents: true,
      // Sigma's default labelDensity (1) hides most labels at default
      // zoom to avoid clutter. With recognisable brands as labels for
      // externals + user labels for internals, we want them visible —
      // bumping density shows roughly 3× as many before clutter wins.
      labelDensity: 3,
      labelGridCellSize: 60,
      minCameraRatio: 0.1,
      maxCameraRatio: 5,
      nodeReducer: (id, attrs) => {
        // If the node has a recent detection event, paint it in
        // the severity colour and force its label visible regardless
        // of hover/selection — alerts must always be findable.
        const halo = attrs.haloColor as string | undefined;
        const sevDecoration = halo
          ? { color: halo, size: ((attrs.size as number | undefined) ?? 4) + 2, forceLabel: true, zIndex: 3 }
          : null;

        // Selected edge: highlight both endpoints, dim everything else.
        if (selectedEdge !== null && graph) {
          const [es, ed] = graph.extremities(selectedEdge);
          if (id === es || id === ed) {
            return {
              ...attrs,
              ...sevDecoration,
              color: COLOR_HIGHLIGHT_EDGE,
              zIndex: 4,
              forceLabel: true,
            };
          }
          return sevDecoration
            ? { ...attrs, ...sevDecoration, label: "" }
            : { ...attrs, color: COLOR_DIM, label: "", zIndex: 0 };
        }

        const f = focused();
        if (!f || !graph) return sevDecoration ? { ...attrs, ...sevDecoration } : attrs;
        if (id === f) {
          return { ...attrs, ...sevDecoration, color: COLOR_HIGHLIGHT_EDGE, zIndex: 4, forceLabel: true };
        }
        if (graph.areNeighbors(f, id)) {
          return { ...attrs, ...sevDecoration, zIndex: 1, forceLabel: true };
        }
        return sevDecoration
          ? { ...attrs, ...sevDecoration, label: "" }
          : { ...attrs, color: COLOR_DIM, label: "", zIndex: 0 };
      },
      edgeReducer: (id, attrs) => {
        // Selected edge wins over node focus: highlight it brightly and
        // dim everything else so the picked flow stands out.
        if (selectedEdge !== null) {
          if (id === selectedEdge) {
            return {
              ...attrs,
              color: COLOR_HIGHLIGHT_EDGE,
              size: (attrs.size ?? 1) * 2,
              zIndex: 2,
            };
          }
          return { ...attrs, hidden: true };
        }
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
      if (dragSuppressClick) {
        // A drag just ended on this node — don't toggle selection.
        dragSuppressClick = false;
        return;
      }
      selectedNode = selectedNode === node ? null : node;
      // Node and edge selection are mutually exclusive — clicking a
      // node clears any edge selection so the right pane shows the
      // node inspector.
      if (selectedEdge !== null) {
        selectedEdge = null;
        props.onEdgeSelectionChange?.(null);
      }
      props.onSelectionChange?.(selectedNode);
      sigma?.refresh();
    });
    sigma.on("clickEdge", ({ edge }) => {
      // Sigma identifies edges by the key we passed at insertion time.
      // We use `${src}->${dst}` — split it back out for the callback.
      const arrow = edge.indexOf("->");
      if (arrow < 0) return;
      const src = edge.slice(0, arrow);
      const dst = edge.slice(arrow + 2);
      selectedEdge = selectedEdge === edge ? null : edge;
      if (selectedNode !== null) {
        selectedNode = null;
        props.onSelectionChange?.(null);
      }
      props.onEdgeSelectionChange?.(selectedEdge ? { src, dst } : null);
      sigma?.refresh();
    });
    sigma.on("clickStage", () => {
      let changed = false;
      if (selectedNode !== null) {
        selectedNode = null;
        props.onSelectionChange?.(null);
        changed = true;
      }
      if (selectedEdge !== null) {
        selectedEdge = null;
        props.onEdgeSelectionChange?.(null);
        changed = true;
      }
      if (changed) sigma?.refresh();
    });

    // ── drag-to-pin wiring ────────────────────────────────────────────────
    // Sigma exposes mouse captor events. Standard pattern from their docs:
    // downNode → start drag, mousemovebody → update position,
    // mouseup → commit + PATCH.
    sigma.on("downNode", ({ node }) => {
      draggedNode = node;
      if (sigma && graph) {
        graph.setNodeAttribute(node, "highlighted", true);
        sigma.getCamera().disable();
      }
    });

    sigma.getMouseCaptor().on("mousemovebody", (e) => {
      if (!draggedNode || !sigma || !graph) return;
      const coords = sigma.viewportToGraph(e);
      graph.setNodeAttribute(draggedNode, "x", coords.x);
      graph.setNodeAttribute(draggedNode, "y", coords.y);
      e.preventSigmaDefault();
      e.original.preventDefault();
      e.original.stopPropagation();
    });

    const finishDrag = () => {
      if (!draggedNode || !graph || !sigma) return;
      const x = graph.getNodeAttribute(draggedNode, "x") as number;
      const y = graph.getNodeAttribute(draggedNode, "y") as number;
      const id = draggedNode;
      graph.removeNodeAttribute(id, "highlighted");
      sigma.getCamera().enable();
      draggedNode = null;
      dragSuppressClick = true;
      // Fire-and-forget; snapshot poll will pick up the persisted value.
      void fetch(`/nodes/${id}`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ position: { x, y } }),
      }).catch(() => {
        // Surfaced via the snapshot store's error indicator if the
        // server is also unreachable; otherwise it's a transient
        // failure and the next drag will retry.
      });
    };
    sigma.getMouseCaptor().on("mouseup", finishDrag);
    sigma.getMouseCaptor().on("mouseleave", finishDrag);
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
    const newIds = applySnapshot(graph, snap);
    if (newIds.size > 0) runIncrementalLayout(graph, newIds);
    sigma.refresh();
  });

  // View change → re-layout everything to the new view's positions.
  // Tracks `props.view` reactively. We always wipe drag-pinned
  // positions here — switching views is an explicit "show me this
  // differently" gesture, the previous arrangement should not bleed
  // through.
  let lastView: ViewKind | undefined;
  createEffect(() => {
    const view = props.view;
    const snap = props.snapshot;
    if (view === undefined || view === lastView) {
      lastView = view;
      return;
    }
    lastView = view;
    if (!snap || !graph || !sigma) return;
    applyViewLayout(graph, snap, view);
    sigma.refresh();
  });

  // External "deselect" (inspector close button).
  createEffect(() => {
    const ext = props.selectedNodeId;
    if (ext === undefined) return;
    if (selectedNode !== ext) {
      selectedNode = ext;
      sigma?.refresh();
    }
  });

  // External edge deselect (inspector close, or the parent clearing
  // when a node was clicked elsewhere).
  createEffect(() => {
    const ext = props.selectedEdgeId;
    if (ext === undefined) return;
    const want = ext ? `${ext.src}->${ext.dst}` : null;
    if (selectedEdge !== want) {
      selectedEdge = want;
      sigma?.refresh();
    }
  });

  // Detection events → per-node halo. For each affected IP we keep
  // the *highest severity* seen recently (one halo per node, the
  // worst thing). Hovering / selecting a node continues to dim
  // unrelated nodes — the halo just renders underneath that as a
  // ring drawn during paint.
  createEffect(() => {
    const evts = props.events ?? [];
    if (!graph || !sigma) return;
    // Reset halos.
    graph.forEachNode((id) => {
      if (graph!.getNodeAttribute(id, "haloSev")) {
        graph!.removeNodeAttribute(id, "haloSev");
        graph!.removeNodeAttribute(id, "haloColor");
      }
    });
    for (const e of evts) {
      const sev = e["event.severity"] ?? 1;
      const ring = severityColor(sev).ring;
      for (const ip of [e["source.ip"], e["destination.ip"]]) {
        if (!ip || !graph.hasNode(ip)) continue;
        const existing = graph.getNodeAttribute(ip, "haloSev") as number | undefined;
        if (existing === undefined || sev > existing) {
          graph.setNodeAttribute(ip, "haloSev", sev);
          graph.setNodeAttribute(ip, "haloColor", ring);
        }
      }
    }
    sigma.refresh();
  });

  // Manual full re-layout (escape hatch when the user wants a fresh
  // organic arrangement). Triggered by the "Re-layout" button which
  // increments a counter prop.
  createEffect(() => {
    const tick = props.relayoutSignal;
    if (tick === undefined || tick === 0 || !graph || !sigma) return;
    runFullLayout(graph);
    sigma.refresh();
    // Persist the new positions for every node so the re-layout sticks.
    persistAllPositions(graph);
  });

  const isEmpty = () => {
    const s = props.snapshot;
    return !s || s.nodes.length === 0;
  };

  return (
    <div class="relative w-full h-full">
      <div ref={container} class="absolute inset-0" style={{ background: "rgb(9 9 11)" }} />
      <Show when={isEmpty()}>
        <GraphEmptyState />
      </Show>
      <Show when={!isEmpty()}>
        <Hint />
      </Show>
    </div>
  );
};

const Hint: Component = () => (
  <div class="absolute bottom-3 left-3 text-[9px] text-zinc-700 font-mono pointer-events-none select-none">
    drag a node to pin · scroll to zoom · click to inspect
  </div>
);

/**
 * Reconcile the Sigma graph against the latest snapshot. Returns the
 * set of node IDs that were *added* in this call — caller uses this
 * to constrain the layout pass (only new nodes get force-layout
 * applied; existing positions are preserved to avoid jitter).
 */
function applySnapshot(graph: Graph, snap: Snapshot): Set<string> {
  const incomingNodes = new Set(snap.nodes.map((n) => n.id));
  const incomingEdges = new Set(snap.edges.map((e) => `${e.id.src}->${e.id.dst}`));

  for (const id of [...graph.nodes()]) {
    if (!incomingNodes.has(id)) graph.dropNode(id);
  }
  for (const key of [...graph.edges()]) {
    if (!incomingEdges.has(key)) graph.dropEdge(key);
  }

  const newInternal = snap.nodes.filter((n) => n.is_internal && !graph.hasNode(n.id));
  const newExternal = snap.nodes.filter((n) => !n.is_internal && !graph.hasNode(n.id));
  const internalCount = snap.nodes.filter((n) => n.is_internal).length;
  const externalCount = snap.nodes.filter((n) => !n.is_internal).length;

  let internalIdx = countWith(graph, (a) => a.internal === true);
  let externalIdx = countWith(graph, (a) => a.internal === false);
  const newIds = new Set<string>();

  for (const node of [...newInternal, ...newExternal]) {
    const meta: NodeMeta = {
      id: node.id,
      internal: node.is_internal,
      isGateway: node.is_internal && isGatewayLikeIp(node.id),
    };
    // Persisted position wins. Otherwise fall back to the semantic
    // anchor based on internal/external ring index.
    const indexedInternal = meta.internal ? internalIdx++ : 0;
    const indexedExternal = meta.internal ? 0 : externalIdx++;
    const pos =
      node.position ??
      graphAnchor(node, indexedInternal, internalCount, indexedExternal, externalCount);
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
      label: displayName(node),
      internal: meta.internal,
      isGateway: meta.isGateway,
    });
    newIds.add(node.id);
  }

  // Existing nodes: refresh label in case it was renamed via PATCH
  // or its brand classification was updated.
  for (const n of snap.nodes) {
    if (graph.hasNode(n.id) && !newIds.has(n.id)) {
      const want = displayName(n);
      if (graph.getNodeAttribute(n.id, "label") !== want) {
        graph.setNodeAttribute(n.id, "label", want);
      }
    }
  }

  // Edges: add new ones, update sizes/colors for existing. Intensity
  // scales by sqrt so a 10× bandwidth difference shows as ~3× stroke.
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
  return newIds;
}

function countWith(graph: Graph, pred: (a: { internal?: boolean }) => boolean): number {
  let n = 0;
  graph.forEachNode((_id, a) => {
    if (pred(a as { internal?: boolean })) n++;
  });
  return n;
}

/**
 * Snap every node to the position dictated by the chosen view. For
 * VLAN and Architecture the positions are deterministic; for Graph
 * we drop back to the semantic-anchor + forceatlas2 settle pass.
 */
function applyViewLayout(graph: Graph, snap: Snapshot, view: ViewKind) {
  if (view === "vlan") {
    const { positions } = layoutVlan(snap.nodes);
    positions.forEach((pos, id) => {
      if (graph.hasNode(id)) {
        graph.setNodeAttribute(id, "x", pos.x);
        graph.setNodeAttribute(id, "y", pos.y);
      }
    });
    return;
  }
  if (view === "architecture") {
    const positions = layoutArchitecture(snap.nodes);
    positions.forEach((pos, id) => {
      if (graph.hasNode(id)) {
        graph.setNodeAttribute(id, "x", pos.x);
        graph.setNodeAttribute(id, "y", pos.y);
      }
    });
    return;
  }
  // graph view: re-anchor + settle
  const internalCount = snap.nodes.filter((n) => n.is_internal).length;
  const externalCount = snap.nodes.filter((n) => !n.is_internal).length;
  let internalIdx = 0;
  let externalIdx = 0;
  for (const node of snap.nodes) {
    if (!graph.hasNode(node.id)) continue;
    const pos = graphAnchor(
      node,
      node.is_internal ? internalIdx++ : 0,
      internalCount,
      node.is_internal ? 0 : externalIdx++,
      externalCount,
    );
    graph.setNodeAttribute(node.id, "x", pos.x);
    graph.setNodeAttribute(node.id, "y", pos.y);
  }
  // One quick settle pass for graph view only.
  if (graph.order >= 2) {
    forceAtlas2.assign(graph, {
      iterations: 60,
      settings: {
        gravity: 0.3,
        scalingRatio: 30,
        slowDown: 2,
        edgeWeightInfluence: 0,
        linLogMode: true,
        barnesHutOptimize: graph.order > 100,
      },
    });
  }
}

/**
 * Force-atlas settle pass that respects existing positions: snapshot
 * the (x, y) of every non-new node, run the layout, restore. Only
 * the new nodes' positions move. Side effect: layout has to ripple
 * across the whole graph to settle the new nodes against their
 * neighbours, but that work is wasted for existing positions — we
 * throw it away. Cheap enough at A-tier scale (<1ms for ~50 nodes).
 */
function runIncrementalLayout(graph: Graph, newIds: Set<string>) {
  if (graph.order < 2) return;
  const saved = new Map<string, { x: number; y: number }>();
  graph.forEachNode((id, attrs) => {
    if (!newIds.has(id)) {
      saved.set(id, { x: attrs.x as number, y: attrs.y as number });
    }
  });

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

  for (const [id, pos] of saved) {
    graph.setNodeAttribute(id, "x", pos.x);
    graph.setNodeAttribute(id, "y", pos.y);
  }
}

/**
 * Full unconstrained layout — called by the "Re-layout" button.
 * Moves every node, ignores anchors. The user explicitly asked
 * for a fresh organic arrangement.
 */
function runFullLayout(graph: Graph) {
  if (graph.order < 2) return;
  forceAtlas2.assign(graph, {
    iterations: 200,
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

/**
 * After a full re-layout, persist every position so the new
 * arrangement survives reload + restart. Fire-and-forget per node;
 * each failure is logged (server side) and ignored client side.
 */
function persistAllPositions(graph: Graph) {
  graph.forEachNode((id, attrs) => {
    const x = attrs.x as number;
    const y = attrs.y as number;
    void fetch(`/nodes/${id}`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ position: { x, y } }),
    }).catch(() => {});
  });
}

export default SigmaGraph;
