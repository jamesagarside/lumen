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
  protocolName,
  severityColor,
  severityName,
  type DetectionEvent,
  type Edge,
  type Flow,
  type Node,
  type Snapshot,
} from "./types";

interface Props {
  snapshot: Snapshot | null;
  /** Recent flow tail. Used to derive per-port / per-protocol breakdown. */
  flows?: Flow[];
  /** Recent detection events. Filtered down to ones involving this node. */
  events?: DetectionEvent[];
  selectedId: string | null;
  /** Pivot the selection to a peer when the user clicks one. */
  onSelectIp?: (id: string) => void;
  onClose: () => void;
  onLabelSaved: (node: Node) => void;
}

const NodeInspector: Component<Props> = (props) => {
  const [draft, setDraft] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const node = createMemo<Node | null>(() => {
    const id = props.selectedId;
    if (!id || !props.snapshot) return null;
    return props.snapshot.nodes.find((n) => n.id === id) ?? null;
  });

  /** Edges incident to the selected node, partitioned by direction. */
  const incidentEdges = createMemo<{ out: Edge[]; in: Edge[] }>(() => {
    const id = props.selectedId;
    if (!id || !props.snapshot) return { out: [], in: [] };
    return {
      out: props.snapshot.edges.filter((e) => e.id.src === id),
      in: props.snapshot.edges.filter((e) => e.id.dst === id),
    };
  });

  /** Flows from the recent tail that involve this node (either direction). */
  const incidentFlows = createMemo<Flow[]>(() => {
    const id = props.selectedId;
    const f = props.flows ?? [];
    if (!id) return [];
    return f.filter((x) => x.src.ip === id || x.dst.ip === id);
  });

  /** Detection events whose source or destination matches this node. */
  const incidentEvents = createMemo<DetectionEvent[]>(() => {
    const id = props.selectedId;
    const evs = props.events ?? [];
    if (!id) return [];
    return evs.filter((e) => e["source.ip"] === id || e["destination.ip"] === id);
  });

  // Reset the draft label when the selection changes.
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

  const onLabelKey = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !saving()) void save();
    if (e.key === "Escape") props.onClose();
  };

  return (
    <Show when={node()} fallback={<EmptyHint />}>
      {(n) => (
        <div class="flex flex-col h-full">
          <Header node={n()} onClose={props.onClose} />

          <div class="flex-1 min-h-0 overflow-auto">
            <IdentitySection
              node={n()}
              draft={draft()}
              onDraftChange={setDraft}
              onSave={() => void save()}
              onKeyDown={onLabelKey}
              saving={saving()}
              error={error()}
            />

            <ActivitySection node={n()} edges={incidentEdges()} />

            <TopPeersSection
              snapshot={props.snapshot}
              outgoing={incidentEdges().out}
              incoming={incidentEdges().in}
              onSelectIp={props.onSelectIp}
            />

            <TrafficMixSection node={n()} flows={incidentFlows()} />

            <DetectionsSection events={incidentEvents()} />
          </div>
        </div>
      )}
    </Show>
  );
};

// ── Header ─────────────────────────────────────────────────────────────────

const Header: Component<{ node: Node; onClose: () => void }> = (props) => (
  <div class="flex items-baseline justify-between px-3 py-2 border-b border-zinc-800/60 flex-shrink-0">
    <div class="flex items-baseline gap-2">
      <span class="text-[10px] uppercase tracking-wider text-zinc-500">Inspector</span>
      <span
        class="text-[9px] uppercase tracking-wider px-1.5 py-0.5 rounded border"
        classList={{
          "text-sky-300 border-sky-900 bg-sky-950/40": props.node.is_internal,
          "text-zinc-400 border-zinc-800 bg-zinc-900/40": !props.node.is_internal,
        }}
      >
        {props.node.is_internal ? "internal" : "external"}
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
);

// ── Identity section ───────────────────────────────────────────────────────

const IdentitySection: Component<{
  node: Node;
  draft: string;
  onDraftChange: (v: string) => void;
  onSave: () => void;
  onKeyDown: (e: KeyboardEvent) => void;
  saving: boolean;
  error: string | null;
}> = (props) => (
  <Section title="Identity">
    <div class="space-y-3">
      <div class="space-y-0.5">
        <div class="text-base text-zinc-100 leading-tight">{displayName(props.node)}</div>
        <div class="text-[10px] text-zinc-500 font-mono">{props.node.id}</div>
      </div>

      <div class="grid grid-cols-2 gap-x-3 gap-y-2 text-[10px]">
        <MetaCell label="Network">{networkLabel(props.node)}</MetaCell>
        <MetaCell label="Subnet">{subnetLabel(props.node.id)}</MetaCell>
        <Show when={props.node.brand}>
          {(b) => <MetaCell label="Brand">{b()}</MetaCell>}
        </Show>
      </div>

      <div>
        <label class="text-[9px] uppercase tracking-wider text-zinc-600 block">Label</label>
        <div class="flex gap-2 mt-1">
          <input
            type="text"
            value={props.draft}
            onInput={(e) => props.onDraftChange(e.currentTarget.value)}
            onKeyDown={props.onKeyDown}
            placeholder="e.g. Living Room Apple TV"
            maxLength={128}
            class="flex-1 bg-zinc-900 border border-zinc-800 rounded px-2 py-1 text-xs text-zinc-100 outline-none focus:border-amber-500/60"
          />
          <button
            type="button"
            onClick={props.onSave}
            disabled={
              props.saving || (props.node.label ?? "") === props.draft.trim()
            }
            class="px-2 py-1 text-[10px] uppercase tracking-wider bg-amber-500/15 border border-amber-500/40 text-amber-300 rounded disabled:opacity-40 disabled:cursor-not-allowed hover:bg-amber-500/25"
          >
            {props.saving ? "…" : "Save"}
          </button>
        </div>
        <Show when={props.error}>
          {(err) => <div class="text-[10px] text-rose-400 mt-1">{err()}</div>}
        </Show>
        <div class="text-[9px] text-zinc-700 mt-1">
          ⏎ to save · esc to close · persisted on the daemon
        </div>
      </div>
    </div>
  </Section>
);

// ── Activity ───────────────────────────────────────────────────────────────

const ActivitySection: Component<{
  node: Node;
  edges: { out: Edge[]; in: Edge[] };
}> = (props) => {
  const totals = createMemo(() => {
    const all = [...props.edges.out, ...props.edges.in];
    return {
      flows: all.reduce((s, e) => s + e.flows_seen, 0),
      bytes: all.reduce((s, e) => s + e.bytes_total, 0),
      bps: all.reduce((s, e) => s + e.bytes_per_sec, 0),
      out: props.edges.out.length,
      in: props.edges.in.length,
    };
  });

  return (
    <Section title="Activity">
      <div class="grid grid-cols-2 gap-x-3 gap-y-2 text-[10px]">
        <MetaCell label="First seen">{relativeTime(props.node.first_seen)}</MetaCell>
        <MetaCell label="Last seen">{relativeTime(props.node.last_seen)}</MetaCell>
        <MetaCell label="Current rate">
          <span class="text-amber-300">{formatRate(totals().bps)}</span>
        </MetaCell>
        <MetaCell label="Total bytes">{formatBytes(totals().bytes)}</MetaCell>
        <MetaCell label="Flows seen">{totals().flows.toLocaleString()}</MetaCell>
        <MetaCell label="Peers">
          {totals().out + totals().in} ({totals().out}↗ {totals().in}↙)
        </MetaCell>
      </div>
    </Section>
  );
};

// ── Top peers ──────────────────────────────────────────────────────────────

interface PeerRow {
  peerId: string;
  peerLabel: string;
  bps: number;
  bytes: number;
  flows: number;
}

const TopPeersSection: Component<{
  snapshot: Snapshot | null;
  outgoing: Edge[];
  incoming: Edge[];
  onSelectIp?: (id: string) => void;
}> = (props) => {
  const peerLabel = (id: string): string => {
    const n = props.snapshot?.nodes.find((x) => x.id === id);
    return n ? displayName(n) : id;
  };
  const top = (edges: Edge[], direction: "out" | "in"): PeerRow[] =>
    edges
      .map((e) => ({
        peerId: direction === "out" ? e.id.dst : e.id.src,
        peerLabel: peerLabel(direction === "out" ? e.id.dst : e.id.src),
        bps: e.bytes_per_sec,
        bytes: e.bytes_total,
        flows: e.flows_seen,
      }))
      .sort((a, b) => b.bps - a.bps || b.bytes - a.bytes)
      .slice(0, 5);

  const out = createMemo(() => top(props.outgoing, "out"));
  const inc = createMemo(() => top(props.incoming, "in"));

  return (
    <Show when={out().length > 0 || inc().length > 0}>
      <Section title="Top peers">
        <div class="space-y-3">
          <Show when={out().length > 0}>
            <PeerList
              direction="out"
              rows={out()}
              total={props.outgoing.length}
              onSelectIp={props.onSelectIp}
            />
          </Show>
          <Show when={inc().length > 0}>
            <PeerList
              direction="in"
              rows={inc()}
              total={props.incoming.length}
              onSelectIp={props.onSelectIp}
            />
          </Show>
        </div>
      </Section>
    </Show>
  );
};

const PeerList: Component<{
  direction: "out" | "in";
  rows: PeerRow[];
  total: number;
  onSelectIp?: (id: string) => void;
}> = (props) => (
  <div>
    <div class="flex items-baseline justify-between mb-1">
      <div class="text-[9px] uppercase tracking-wider text-zinc-600">
        {props.direction === "out" ? "Outgoing" : "Incoming"} · top{" "}
        {Math.min(props.rows.length, props.total)} of {props.total}
      </div>
    </div>
    <ul class="space-y-px">
      <For each={props.rows}>
        {(row) => (
          <li>
            <button
              type="button"
              onClick={() => props.onSelectIp?.(row.peerId)}
              class="w-full text-left flex items-baseline gap-2 px-2 py-1 rounded text-[11px] hover:bg-zinc-900/60 group"
            >
              <span class="text-zinc-600 text-[10px]">
                {props.direction === "out" ? "→" : "←"}
              </span>
              <span class="flex-1 truncate text-zinc-200 group-hover:text-amber-300">
                {row.peerLabel}
                <Show when={row.peerLabel !== row.peerId}>
                  <span class="text-zinc-600 text-[10px] font-mono ml-1">
                    {row.peerId}
                  </span>
                </Show>
              </span>
              <span class="text-amber-300 tabular-nums text-[10px]">
                {formatRate(row.bps)}
              </span>
              <span class="text-zinc-500 tabular-nums text-[10px] min-w-[3.5rem] text-right">
                {formatBytes(row.bytes)}
              </span>
            </button>
          </li>
        )}
      </For>
    </ul>
  </div>
);

// ── Traffic mix (from recent flow tail) ────────────────────────────────────

interface PortBucket {
  port: number;
  protocol: number;
  flows: number;
  bytes: number;
}

const TrafficMixSection: Component<{ node: Node; flows: Flow[] }> = (props) => {
  /// Aggregate this node's recent flows by (dst_port, protocol) when we're
  /// the source, and by (src_port, protocol) when we're the destination —
  /// in both cases that's the "well-known port" side of the conversation.
  const ports = createMemo<PortBucket[]>(() => {
    const id = props.node.id;
    const buckets = new Map<string, PortBucket>();
    for (const f of props.flows) {
      const port = f.src.ip === id ? f.dst.port : f.src.port;
      const key = `${port}/${f.protocol}`;
      const b = buckets.get(key);
      if (b) {
        b.flows += 1;
        b.bytes += f.bytes;
      } else {
        buckets.set(key, { port, protocol: f.protocol, flows: 1, bytes: f.bytes });
      }
    }
    return [...buckets.values()].sort((a, b) => b.bytes - a.bytes || b.flows - a.flows).slice(0, 6);
  });

  return (
    <Section title="Traffic mix">
      <Show
        when={ports().length > 0}
        fallback={
          <p class="text-[10px] text-zinc-600">
            No recent flows in the live tail. Mix populates as new flows arrive.
          </p>
        }
      >
        <ul class="space-y-px">
          <For each={ports()}>
            {(p) => (
              <li class="flex items-baseline gap-2 px-2 py-1 text-[11px]">
                <span class="font-mono text-zinc-200 min-w-[3.5rem]">
                  {p.port}
                </span>
                <span class="text-[10px] text-zinc-500 uppercase">
                  {protocolName(p.protocol)}
                </span>
                <span class="flex-1 text-[10px] text-zinc-700">
                  {portHint(p.port, p.protocol)}
                </span>
                <span class="text-zinc-500 tabular-nums text-[10px]">
                  {p.flows} flow{p.flows === 1 ? "" : "s"}
                </span>
                <span class="text-amber-300 tabular-nums text-[10px] min-w-[3.5rem] text-right">
                  {formatBytes(p.bytes)}
                </span>
              </li>
            )}
          </For>
        </ul>
        <div class="text-[9px] text-zinc-700 px-2 mt-1">
          from the last {props.flows.length} flow{props.flows.length === 1 ? "" : "s"}
        </div>
      </Show>
    </Section>
  );
};

// ── Recent detections ──────────────────────────────────────────────────────

const DetectionsSection: Component<{ events: DetectionEvent[] }> = (props) => (
  <Show when={props.events.length > 0}>
    <Section title={`Detections (${props.events.length})`}>
      <ul class="space-y-1.5">
        <For each={props.events.slice(0, 5)}>
          {(e) => {
            const sev = e["event.severity"] ?? 1;
            const tone = severityColor(sev);
            return (
              <li class="px-2 py-1.5 rounded bg-zinc-900/40 border border-zinc-800/50">
                <div class="flex items-baseline gap-2">
                  <span class={`size-1.5 rounded-full ${tone.dot} flex-shrink-0`} />
                  <span class={`text-[9px] uppercase tracking-wider ${tone.text}`}>
                    {severityName(sev)}
                  </span>
                  <span class="text-[9px] text-zinc-600 ml-auto tabular-nums">
                    {relativeTime(e["@timestamp"])}
                  </span>
                </div>
                <div class="text-[11px] text-zinc-200 mt-0.5 break-words">{e.message}</div>
                <Show when={e.rule?.name}>
                  <div class="text-[9px] text-zinc-500 mt-0.5">
                    rule: <span class="text-zinc-400">{e.rule!.name}</span>
                  </div>
                </Show>
              </li>
            );
          }}
        </For>
      </ul>
      <Show when={props.events.length > 5}>
        <div class="text-[9px] text-zinc-600 mt-1.5 px-2">
          + {props.events.length - 5} older
        </div>
      </Show>
    </Section>
  </Show>
);

// ── Layout primitives ──────────────────────────────────────────────────────

const Section: Component<{ title: string; children: import("solid-js").JSX.Element }> = (
  props,
) => (
  <section class="px-3 py-3 border-b border-zinc-800/40 last:border-b-0">
    <h3 class="text-[9px] uppercase tracking-wider text-zinc-500 mb-2">{props.title}</h3>
    {props.children}
  </section>
);

const MetaCell: Component<{ label: string; children: import("solid-js").JSX.Element }> = (
  props,
) => (
  <div>
    <div class="text-[9px] uppercase tracking-wider text-zinc-600">{props.label}</div>
    <div class="text-[11px] text-zinc-300 truncate">{props.children}</div>
  </div>
);

const EmptyHint: Component = () => (
  <div class="h-full flex items-center justify-center text-center text-[10px] text-zinc-700 px-6">
    click a node in the graph to inspect or rename it
  </div>
);

// ── Helpers ────────────────────────────────────────────────────────────────

/** Sigma's wire shape for SystemTime — either form is accepted upstream. */
type SystemTimeIsh = { secs_since_epoch: number; nanos_since_epoch?: number };

/** ms-since-epoch from the daemon's serialised SystemTime. */
const epochMs = (t: SystemTimeIsh): number =>
  t.secs_since_epoch * 1000 + Math.floor((t.nanos_since_epoch ?? 0) / 1_000_000);

const relativeTime = (t: SystemTimeIsh): string => {
  const ms = epochMs(t);
  const diff = Date.now() - ms;
  if (diff < 0) return "future"; // clock skew
  const sec = Math.round(diff / 1000);
  if (sec < 5) return "just now";
  if (sec < 60) return `${sec}s ago`;
  const min = Math.round(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `${hr}h ago`;
  const days = Math.round(hr / 24);
  if (days < 30) return `${days}d ago`;
  const months = Math.round(days / 30);
  if (months < 12) return `${months}mo ago`;
  return `${Math.round(months / 12)}y ago`;
};

/** Friendly classification of the node's network position. */
const networkLabel = (n: Node): string => {
  if (!n.is_internal) return "Public Internet";
  const range = rfc1918Range(n.id);
  return range ? `RFC1918 · ${range}` : "Private";
};

/**
 * Heuristic /24 (or /16) bucket for the IP. Purely client-side and only
 * useful as an at-a-glance grouping — the real subnet topology lives in
 * the router config, which the daemon doesn't have access to yet.
 */
const subnetLabel = (ip: string): string => {
  // IPv4 only; IPv6 fall-through prints the host bits trimmed.
  const parts = ip.split(".").map((p) => Number.parseInt(p, 10));
  if (parts.length === 4 && parts.every((p) => Number.isFinite(p))) {
    return `${parts[0]}.${parts[1]}.${parts[2]}.0/24`;
  }
  // Hide the trailing /128 host bits for v6 — show the /64.
  if (ip.includes(":")) {
    const head = ip.split(":").slice(0, 4).join(":");
    return `${head}::/64`;
  }
  return ip;
};

const rfc1918Range = (ip: string): string | null => {
  const parts = ip.split(".").map((p) => Number.parseInt(p, 10));
  if (parts.length !== 4 || parts.some((p) => !Number.isFinite(p))) return null;
  const [a, b] = parts;
  if (a === 10) return "10.0.0.0/8";
  if (a === 172 && b >= 16 && b <= 31) return "172.16.0.0/12";
  if (a === 192 && b === 168) return "192.168.0.0/16";
  if (a === 127) return "127.0.0.0/8 (loopback)";
  if (a === 169 && b === 254) return "169.254.0.0/16 (link-local)";
  return null;
};

/// One-line hint for the well-known-port side of a conversation. Covers the
/// handful of ports that explain ~all home-network traffic at a glance;
/// anything unknown shows nothing (better than guessing wrong).
const portHint = (port: number, _proto: number): string => {
  const hints: Record<number, string> = {
    22: "ssh",
    53: "dns",
    67: "dhcp",
    68: "dhcp",
    80: "http",
    123: "ntp",
    137: "netbios",
    143: "imap",
    443: "https",
    445: "smb",
    500: "ipsec",
    548: "afp",
    554: "rtsp",
    587: "smtp submission",
    631: "ipp / printers",
    993: "imaps",
    1900: "ssdp / upnp",
    3389: "rdp",
    5353: "mdns",
    5355: "llmnr",
    8080: "http (alt)",
    8443: "https (alt)",
  };
  return hints[port] ?? "";
};

const formatBytes = (n: number): string => {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
};

const formatRate = (n: number): string => {
  if (n < 1024) return `${n.toFixed(0)} B/s`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB/s`;
  return `${(n / 1024 / 1024).toFixed(2)} MB/s`;
};

export default NodeInspector;
