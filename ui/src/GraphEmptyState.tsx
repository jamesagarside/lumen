import type { Component } from "solid-js";

/**
 * First-run guidance when no flows have arrived yet. Overlaid on top
 * of the Sigma canvas via absolute positioning so it disappears the
 * moment the first node lands without us having to re-mount Sigma.
 */
const GraphEmptyState: Component = () => (
  <div class="absolute inset-0 flex items-center justify-center pointer-events-none">
    <div class="max-w-sm text-center space-y-3 px-6">
      <h2 class="text-sm font-semibold tracking-tight text-zinc-300">
        Waiting for traffic
      </h2>
      <p class="text-[11px] text-zinc-500 leading-relaxed">
        Lumen builds the graph from network flow data. Try one of:
      </p>
      <ul class="text-[11px] text-zinc-400 space-y-2 text-left">
        <li>
          <span class="text-zinc-600">→</span>{" "}
          Point a NetFlow v5/v9/IPFIX exporter at{" "}
          <code class="text-amber-300 bg-amber-500/10 px-1 rounded">UDP 2055</code>
        </li>
        <li>
          <span class="text-zinc-600">→</span>{" "}
          Forward iptables-LOG syslog to{" "}
          <code class="text-amber-300 bg-amber-500/10 px-1 rounded">UDP 5514</code>
        </li>
        <li>
          <span class="text-zinc-600">→</span>{" "}
          POST records to{" "}
          <code class="text-amber-300 bg-amber-500/10 px-1 rounded">/ingest/flows</code>
        </li>
        <li>
          <span class="text-zinc-600">→</span>{" "}
          Or just run{" "}
          <code class="text-amber-300 bg-amber-500/10 px-1 rounded">make demo-stream</code>{" "}
          for a synthetic feed
        </li>
      </ul>
    </div>
  </div>
);

export default GraphEmptyState;
