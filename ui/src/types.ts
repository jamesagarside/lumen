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

export interface Node {
  id: string; // serde-transparent NodeId(IpAddr) → IP string
  is_internal: boolean;
  first_seen: SystemTimeJson;
  last_seen: SystemTimeJson;
  label?: string;
}

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
