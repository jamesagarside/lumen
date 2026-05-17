export interface FlowEndpoint {
  ip: string;
  port: number;
}

export type FlowSource =
  | "netflow_v5"
  | "netflow_v9"
  | "ipfix"
  | "syslog"
  | "json_http"
  | "otlp";

export interface Flow {
  source: FlowSource;
  src: FlowEndpoint;
  dst: FlowEndpoint;
  protocol: number;
  bytes: number;
  packets: number;
  // SystemTime serialised as { secs_since_epoch, nanos_since_epoch }
  start: { secs_since_epoch: number; nanos_since_epoch: number };
  end: { secs_since_epoch: number; nanos_since_epoch: number };
}

export interface VersionInfo {
  name: string;
  version: string;
  abi_version: string;
}

// Mirror of lumen_core::Snapshot. SystemTime serialises as
// { secs_since_epoch, nanos_since_epoch }.
type SystemTimeJson = { secs_since_epoch: number; nanos_since_epoch: number };

export interface Position {
  x: number;
  y: number;
}

export interface Node {
  id: string; // serde-transparent NodeId(IpAddr) → IP string
  is_internal: boolean;
  first_seen: SystemTimeJson;
  last_seen: SystemTimeJson;
  label?: string;
  position?: Position;
  brand?: string;
}

/** Display name precedence: user label > recognised brand > raw IP. */
export const displayName = (n: Node): string => n.label || n.brand || n.id;

// ── Detection events ────────────────────────────────────────────────────────

export interface DetectionEvent {
  "@timestamp": SystemTimeJson;
  "event.kind": "alert" | "event" | "signal" | "state";
  "event.category"?: string[];
  "event.severity": number;
  "event.action"?: string;
  message: string;
  rule?: {
    id?: string;
    name?: string;
    description?: string;
    category?: string;
  };
  agent: { type: string; vendor?: string; version?: string };
  "source.ip"?: string;
  "destination.ip"?: string;
  "url.original"?: string;
  extra?: unknown;
}

export const severityName = (s: number): string => {
  if (s <= 2) return "info";
  if (s <= 4) return "low";
  if (s === 5) return "medium";
  if (s === 6) return "high";
  return "critical";
};

/**
 * Tailwind class chunk for a severity badge. Kept here so the same
 * palette is used in the sidebar pill, the graph halo, and any future
 * scrubber mark.
 */
export const severityColor = (
  s: number,
): { dot: string; text: string; ring: string } => {
  if (s <= 2) return { dot: "bg-zinc-500", text: "text-zinc-400", ring: "rgba(161,161,170,0.7)" };
  if (s <= 4) return { dot: "bg-sky-500", text: "text-sky-400", ring: "rgba(56,189,248,0.7)" };
  if (s === 5) return { dot: "bg-amber-500", text: "text-amber-400", ring: "rgba(245,158,11,0.7)" };
  if (s === 6) return { dot: "bg-orange-500", text: "text-orange-400", ring: "rgba(249,115,22,0.85)" };
  return { dot: "bg-rose-500", text: "text-rose-400", ring: "rgba(244,63,94,0.95)" };
};

export interface EdgeId {
  src: string;
  dst: string;
}

export interface Edge {
  id: EdgeId;
  first_seen: SystemTimeJson;
  last_seen: SystemTimeJson;
  bytes_total: number;
  packets_total: number;
  flows_seen: number;
  bytes_per_sec: number;
}

export interface Snapshot {
  generated_at: SystemTimeJson;
  nodes: Node[];
  edges: Edge[];
}

export const protocolName = (n: number): string => {
  switch (n) {
    case 1:
      return "icmp";
    case 6:
      return "tcp";
    case 17:
      return "udp";
    case 58:
      return "icmpv6";
    default:
      return String(n);
  }
};
