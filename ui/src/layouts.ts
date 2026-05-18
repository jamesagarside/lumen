/**
 * Layout strategies for the SigmaGraph.
 *
 * Each layout returns a `(nodeId → {x, y})` map that the caller can
 * apply to graphology. Layouts are pure — they look at a snapshot of
 * nodes + edges and produce coordinates. Whether/how to animate is
 * decided by the caller.
 *
 * Per CONTEXT.md §8 we ship three:
 *   - `graph`:        force-directed with semantic anchors (default).
 *   - `vlan`:         nodes grouped by /24 subnet, each subnet a
 *                     positioned cluster around a ring.
 *   - `architecture`: tiered top→bottom — gateway, internal, external.
 */
import type { Node } from "./types";

export type ViewKind = "graph" | "vlan" | "architecture";

export interface LayoutPosition {
  x: number;
  y: number;
}

/** Heuristic: x.x.x.1 inside an internal range is the gateway. */
export const isGatewayLikeIp = (ip: string): boolean => {
  const parts = ip.split(".");
  return parts.length === 4 && parts[3] === "1";
};

/**
 * /24-subnet group for an internal node. For 192.168.1.42 returns
 * "192.168.1.0/24". External IPs are bucketed under "external".
 */
const subnetOf = (node: Node): string => {
  if (!node.is_internal) return "external";
  const parts = node.id.split(".");
  if (parts.length === 4) {
    return `${parts[0]}.${parts[1]}.${parts[2]}.0/24`;
  }
  // IPv6 / other — group everything under one bucket for now.
  return "internal";
};

const ANCHOR_RADIUS_INTERNAL = 0.18;
const ANCHOR_RADIUS_EXTERNAL = 0.85;

// ── Graph view ─────────────────────────────────────────────────────────────

/** Initial anchor for a single node in the original Graph view. */
export const graphAnchor = (
  node: Node,
  internalIndex: number,
  internalCount: number,
  externalIndex: number,
  externalCount: number,
): LayoutPosition => {
  const isGateway = node.is_internal && isGatewayLikeIp(node.id);
  if (isGateway) return { x: 0, y: -0.9 };
  if (node.is_internal) {
    const angle = (internalIndex / Math.max(internalCount, 1)) * Math.PI * 2;
    const r = internalCount === 1 ? 0 : ANCHOR_RADIUS_INTERNAL;
    return { x: Math.cos(angle) * r, y: Math.sin(angle) * r };
  }
  const angle =
    (externalIndex / Math.max(externalCount, 1)) * Math.PI * 2 - Math.PI / 2;
  return {
    x: Math.cos(angle) * ANCHOR_RADIUS_EXTERNAL,
    y: Math.sin(angle) * ANCHOR_RADIUS_EXTERNAL,
  };
};

// ── VLAN view ──────────────────────────────────────────────────────────────

export interface VlanLayout {
  /** Position per node. */
  positions: Map<string, LayoutPosition>;
  /** Group centroids — used by callers that draw labels per subnet. */
  groups: Array<{ subnet: string; center: LayoutPosition; nodeCount: number }>;
}

export const layoutVlan = (nodes: Node[]): VlanLayout => {
  // Bucket.
  const buckets = new Map<string, Node[]>();
  for (const n of nodes) {
    const key = subnetOf(n);
    let arr = buckets.get(key);
    if (!arr) {
      arr = [];
      buckets.set(key, arr);
    }
    arr.push(n);
  }
  // External as a single perimeter group; internal subnets get their
  // own clusters arranged around a circle inside the perimeter.
  const internalGroups = [...buckets.entries()]
    .filter(([k]) => k !== "external" && k !== "internal")
    .sort((a, b) => a[0].localeCompare(b[0]));
  const externalNodes = buckets.get("external") ?? [];

  const positions = new Map<string, LayoutPosition>();
  const groups: VlanLayout["groups"] = [];

  // Internal subnets around a ring of radius 0.45; each subnet is a
  // mini cluster of its own.
  const ringRadius = 0.45;
  const clusterRadius = 0.13;
  internalGroups.forEach(([subnet, members], i) => {
    const groupAngle =
      (i / Math.max(internalGroups.length, 1)) * Math.PI * 2 - Math.PI / 2;
    const cx = Math.cos(groupAngle) * ringRadius;
    const cy = Math.sin(groupAngle) * ringRadius;
    groups.push({ subnet, center: { x: cx, y: cy }, nodeCount: members.length });
    members.forEach((node, j) => {
      const innerAngle =
        (j / Math.max(members.length, 1)) * Math.PI * 2;
      const r = members.length === 1 ? 0 : clusterRadius;
      positions.set(node.id, {
        x: cx + Math.cos(innerAngle) * r,
        y: cy + Math.sin(innerAngle) * r,
      });
    });
  });

  // External services arc around the perimeter.
  externalNodes.forEach((node, i) => {
    const angle =
      (i / Math.max(externalNodes.length, 1)) * Math.PI * 2 - Math.PI / 2;
    positions.set(node.id, {
      x: Math.cos(angle) * ANCHOR_RADIUS_EXTERNAL,
      y: Math.sin(angle) * ANCHOR_RADIUS_EXTERNAL,
    });
  });
  if (externalNodes.length > 0) {
    groups.push({
      subnet: "external",
      center: { x: 0, y: 0 },
      nodeCount: externalNodes.length,
    });
  }

  return { positions, groups };
};

// ── Architecture view ──────────────────────────────────────────────────────

/**
 * Top-to-bottom tiered layout (Sigma's y is +up):
 *   gateway nodes (heuristic: x.x.x.1)    →  y =  0.85  (top)
 *   other internal devices                →  y =  0.00  (middle)
 *   external services                     →  y = -0.75  (bottom)
 *
 * Within each tier nodes spread evenly across x ∈ [-0.9, 0.9].
 * Without authoritative topology from an integration this is a
 * heuristic — internal that aren't gateways all collapse into one
 * middle tier rather than the AP/switch/host hierarchy the design
 * spec eventually wants.
 */
export const layoutArchitecture = (
  nodes: Node[],
): Map<string, LayoutPosition> => {
  const positions = new Map<string, LayoutPosition>();
  const gateways = nodes.filter((n) => n.is_internal && isGatewayLikeIp(n.id));
  const internalOther = nodes.filter(
    (n) => n.is_internal && !isGatewayLikeIp(n.id),
  );
  const external = nodes.filter((n) => !n.is_internal);

  const spread = (arr: Node[], y: number) => {
    if (arr.length === 0) return;
    if (arr.length === 1) {
      positions.set(arr[0].id, { x: 0, y });
      return;
    }
    const left = -0.9;
    const right = 0.9;
    arr.forEach((n, i) => {
      const x = left + ((right - left) * i) / (arr.length - 1);
      positions.set(n.id, { x, y });
    });
  };

  spread(gateways, 0.85);
  spread(internalOther, 0.0);
  spread(external, -0.75);

  return positions;
};
