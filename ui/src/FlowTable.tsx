import { For, Show, type Component } from "solid-js";
import type { Flow } from "./types";
import { protocolName } from "./types";

const formatBytes = (n: number): string => {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
};

const FlowTable: Component<{ flows: Flow[] }> = (props) => {
  return (
    <div class="w-full">
      <table class="w-full text-xs font-mono">
        <thead class="sticky top-0 bg-zinc-900/95 backdrop-blur text-zinc-400 text-left">
          <tr>
            <th class="px-3 py-2 font-medium">proto</th>
            <th class="px-3 py-2 font-medium">source</th>
            <th class="px-3 py-2 font-medium">destination</th>
            <th class="px-3 py-2 font-medium text-right">packets</th>
            <th class="px-3 py-2 font-medium text-right">bytes</th>
          </tr>
        </thead>
        <tbody>
          <Show when={props.flows.length > 0} fallback={<EmptyRow />}>
            <For each={props.flows}>
              {(f) => (
                <tr class="border-t border-zinc-800/60 hover:bg-zinc-800/40">
                  <td class="px-3 py-1.5 text-amber-300">{protocolName(f.protocol)}</td>
                  <td class="px-3 py-1.5 text-zinc-200">
                    {f.src.ip}
                    <span class="text-zinc-500">:{f.src.port}</span>
                  </td>
                  <td class="px-3 py-1.5 text-zinc-200">
                    {f.dst.ip}
                    <span class="text-zinc-500">:{f.dst.port}</span>
                  </td>
                  <td class="px-3 py-1.5 text-right text-zinc-300">{f.packets.toLocaleString()}</td>
                  <td class="px-3 py-1.5 text-right text-zinc-300">{formatBytes(f.bytes)}</td>
                </tr>
              )}
            </For>
          </Show>
        </tbody>
      </table>
    </div>
  );
};

const EmptyRow: Component = () => (
  <tr>
    <td colspan="5" class="px-3 py-12 text-center text-zinc-600">
      No flows received yet. Point a NetFlow v5 exporter at UDP&nbsp;2055.
    </td>
  </tr>
);

export default FlowTable;
