import { createResource, onCleanup, onMount, Show, type Component } from "solid-js";
import FlowGraph from "./FlowGraph";
import FlowTable from "./FlowTable";
import { createFlowStore } from "./flowStore";
import type { VersionInfo } from "./types";

const fetchVersion = async (): Promise<VersionInfo> => {
  const res = await fetch("/version");
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
};

const wsUrl = (): string => {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}/ws/flows`;
};

const App: Component = () => {
  const [version] = createResource(fetchVersion);
  const store = createFlowStore(wsUrl());

  onMount(store.connect);
  onCleanup(store.disconnect);

  return (
    <main class="min-h-screen bg-zinc-950 text-zinc-100 font-mono flex flex-col">
      <header class="px-4 py-3 border-b border-zinc-800 flex items-center justify-between">
        <div class="flex items-baseline gap-3">
          <h1 class="text-sm font-semibold tracking-tight">Lumen</h1>
          <Show when={version()}>
            {(v) => (
              <span class="text-[10px] text-zinc-600">
                v{v().version} · ABI {v().abi_version}
              </span>
            )}
          </Show>
        </div>
        <ConnectionPill state={store.connection()} count={store.flows().length} />
      </header>

      <div class="flex-1 grid grid-cols-1 lg:grid-cols-[3fr_2fr] gap-px bg-zinc-800 overflow-hidden">
        <section class="bg-zinc-950 p-4 overflow-hidden">
          <FlowGraph flows={store.flows()} />
        </section>
        <section class="bg-zinc-950 overflow-hidden">
          <FlowTable flows={store.flows()} />
        </section>
      </div>
    </main>
  );
};

const ConnectionPill: Component<{ state: string; count: number }> = (props) => {
  const dotClass = () => {
    switch (props.state) {
      case "open":
        return "bg-emerald-400";
      case "connecting":
        return "bg-amber-400 animate-pulse";
      default:
        return "bg-rose-400";
    }
  };

  return (
    <div class="flex items-center gap-2 text-[10px] text-zinc-500">
      <span class={`size-1.5 rounded-full ${dotClass()}`} />
      <span>{props.state}</span>
      <span class="text-zinc-700">·</span>
      <span>{props.count} flows</span>
    </div>
  );
};

export default App;
