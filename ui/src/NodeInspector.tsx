import {
  createEffect,
  createMemo,
  createSignal,
  Show,
  type Component,
} from "solid-js";
import type { Edge, Node, Snapshot } from "./types";

interface Props {
  snapshot: Snapshot | null;
  selectedId: string | null;
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

  const incidentEdges = createMemo<{ out: Edge[]; in: Edge[] }>(() => {
    const id = props.selectedId;
    if (!id || !props.snapshot) return { out: [], in: [] };
    return {
      out: props.snapshot.edges.filter((e) => e.id.src === id),
      in: props.snapshot.edges.filter((e) => e.id.dst === id),
    };
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

  return (
    <Show when={node()} fallback={<EmptyHint />}>
      {(n) => (
        <div class="flex flex-col h-full">
          <div class="flex items-baseline justify-between px-3 py-2 border-b border-zinc-800/60">
            <div class="flex items-baseline gap-2">
              <span class="text-[10px] uppercase tracking-wider text-zinc-500">
                Inspector
              </span>
              <span class="text-[10px] text-zinc-700">
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
            <div>
              <div class="text-[9px] uppercase tracking-wider text-zinc-600">IP</div>
              <div class="text-xs text-zinc-200">{n().id}</div>
            </div>

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

            <EdgeStats title="Outgoing" edges={incidentEdges().out} />
            <EdgeStats title="Incoming" edges={incidentEdges().in} />
          </div>
        </div>
      )}
    </Show>
  );
};

const EdgeStats: Component<{ title: string; edges: Edge[] }> = (props) => {
  const total = createMemo(() => props.edges.reduce((s, e) => s + e.bytes_total, 0));
  const rate = createMemo(() => props.edges.reduce((s, e) => s + e.bytes_per_sec, 0));
  return (
    <Show when={props.edges.length > 0}>
      <div>
        <div class="text-[9px] uppercase tracking-wider text-zinc-600">
          {props.title} ({props.edges.length})
        </div>
        <div class="text-[11px] text-zinc-300">
          {formatBytes(total())} total · {formatRate(rate())}
        </div>
      </div>
    </Show>
  );
};

const EmptyHint: Component = () => (
  <div class="h-full flex items-center justify-center text-center text-[10px] text-zinc-700 px-6">
    click a node in the graph to inspect or rename it
  </div>
);

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
