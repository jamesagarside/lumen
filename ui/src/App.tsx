import {
  createResource,
  createSignal,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
  type Component,
} from "solid-js";
import DaemonBanner from "./DaemonBanner";
import EventsSidebar from "./EventsSidebar";
import FlowTable from "./FlowTable";
import Login from "./Login";
import NodeInspector from "./NodeInspector";
import SigmaGraph from "./SigmaGraph";
import { createAuthStore } from "./authStore";
import { createEventsStore } from "./eventsStore";
import { createFlowStore } from "./flowStore";
import { createSnapshotStore } from "./snapshotStore";
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

const formatBytesPerSec = (n: number): string => {
  if (n < 1024) return `${n.toFixed(0)} B/s`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB/s`;
  return `${(n / 1024 / 1024).toFixed(2)} MB/s`;
};

const App: Component = () => {
  const authStore = createAuthStore();

  onMount(() => {
    void authStore.refresh();
  });

  return (
    <Switch>
      <Match when={authStore.state().status === "loading"}>
        <main class="h-screen bg-zinc-950 text-zinc-100 font-mono flex items-center justify-center">
          <span class="text-[10px] text-zinc-600">loading…</span>
        </main>
      </Match>
      <Match when={authStore.state().status === "anonymous"}>
        <Login onLogin={authStore.login} />
      </Match>
      <Match when={authStore.state().status === "authed"}>
        <AuthedApp
          onLogout={() => void authStore.logout()}
          can={authStore.can}
          userEmail={
            authStore.state().status === "authed"
              ? (authStore.state() as { status: "authed"; me: { user: { email: string } } }).me
                  .user.email
              : ""
          }
        />
      </Match>
    </Switch>
  );
};

interface AuthedAppProps {
  onLogout: () => void;
  can: (capability: string) => boolean;
  userEmail: string;
}

type RightPane = "flows" | "events";

const AuthedApp: Component<AuthedAppProps> = (props) => {
  const [version] = createResource(fetchVersion);
  const flowStore = createFlowStore(wsUrl());
  const snapshotStore = createSnapshotStore();
  const eventsStore = createEventsStore();
  const [selectedNode, setSelectedNode] = createSignal<string | null>(null);
  const [relayoutTick, setRelayoutTick] = createSignal(0);
  const [pane, setPane] = createSignal<RightPane>("flows");

  onMount(() => {
    flowStore.connect();
    snapshotStore.start();
    eventsStore.start();
  });
  onCleanup(() => {
    flowStore.disconnect();
    snapshotStore.stop();
    eventsStore.stop();
  });

  // Unread-badge state on the Detections tab.
  const detectionsCount = () => eventsStore.events().length;
  let seenAtCount = 0;

  const totalBytesPerSec = () => {
    const s = snapshotStore.snapshot();
    if (!s) return 0;
    return s.edges.reduce((sum, e) => sum + e.bytes_per_sec, 0);
  };

  return (
    <main class="h-screen bg-zinc-950 text-zinc-100 font-mono flex flex-col overflow-hidden">
      <DaemonBanner
        unreachable={snapshotStore.unreachable()}
        wsState={flowStore.connection()}
      />
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
        <div class="flex items-center gap-4">
          <Show when={!snapshotStore.unreachable() ? snapshotStore.error() : null}>
            {(err) => (
              <span class="text-[10px] text-rose-400" title={err()}>
                snapshot: {err()}
              </span>
            )}
          </Show>
          <TopologyStat snapshot={snapshotStore.snapshot()} totalBps={totalBytesPerSec()} />
          <button
            type="button"
            class="text-[10px] uppercase tracking-wider text-zinc-500 hover:text-zinc-300 px-2 py-1 border border-zinc-800 rounded hover:border-zinc-700"
            title="re-run layout on every node from scratch"
            onClick={() => setRelayoutTick((n) => n + 1)}
          >
            Re-layout
          </button>
          <ConnectionPill state={flowStore.connection()} />
          <div class="flex items-center gap-2 text-[10px] text-zinc-500 border-l border-zinc-800 pl-3 ml-1">
            <span title={props.userEmail}>{props.userEmail.split("@")[0]}</span>
            <button
              type="button"
              class="text-zinc-500 hover:text-zinc-300"
              onClick={props.onLogout}
              title="sign out"
            >
              ↩
            </button>
          </div>
        </div>
      </header>

      <div class="flex-1 min-h-0 grid grid-cols-1 lg:grid-cols-[3fr_2fr] gap-px bg-zinc-800">
        <section class="bg-zinc-950 min-h-0 overflow-hidden">
          <SigmaGraph
            snapshot={snapshotStore.snapshot()}
            events={eventsStore.events()}
            selectedNodeId={selectedNode()}
            onSelectionChange={setSelectedNode}
            relayoutSignal={relayoutTick()}
          />
        </section>
        <section class="bg-zinc-950 min-h-0 overflow-hidden flex flex-col">
          <Show
            when={selectedNode()}
            fallback={
              <div class="flex-1 min-h-0 flex flex-col">
                <div class="flex border-b border-zinc-800/60 flex-shrink-0">
                  <PaneTab
                    label="Recent flows"
                    active={pane() === "flows"}
                    onClick={() => setPane("flows")}
                  />
                  <PaneTab
                    label="Detections"
                    active={pane() === "events"}
                    count={detectionsCount()}
                    unread={pane() !== "events" ? Math.max(0, detectionsCount() - seenAtCount) : 0}
                    onClick={() => {
                      seenAtCount = detectionsCount();
                      setPane("events");
                    }}
                  />
                  <span class="ml-auto px-3 py-1.5 text-[9px] text-zinc-700 self-center">
                    {pane() === "flows" ? "live tail · resets on refresh" : "polled · 2Hz"}
                  </span>
                </div>
                <div class="flex-1 min-h-0 overflow-hidden">
                  <Show when={pane() === "flows"}>
                    <div class="h-full overflow-auto">
                      <FlowTable flows={flowStore.flows()} />
                    </div>
                  </Show>
                  <Show when={pane() === "events"}>
                    <EventsSidebar
                      events={eventsStore.events()}
                      onSelectIp={(ip) => setSelectedNode(ip)}
                    />
                  </Show>
                </div>
              </div>
            }
          >
            <NodeInspector
              snapshot={snapshotStore.snapshot()}
              selectedId={selectedNode()}
              onClose={() => setSelectedNode(null)}
              onLabelSaved={() => {
                // Snapshot poll picks up the new label on the next tick.
              }}
            />
          </Show>
        </section>
      </div>
    </main>
  );
};

const TopologyStat: Component<{
  snapshot: ReturnType<ReturnType<typeof createSnapshotStore>["snapshot"]>;
  totalBps: number;
}> = (props) => {
  const counts = () => {
    const s = props.snapshot;
    if (!s) return { nodes: 0, edges: 0 };
    return { nodes: s.nodes.length, edges: s.edges.length };
  };

  return (
    <div class="text-[10px] text-zinc-500 flex items-center gap-3">
      <span>
        <span class="text-zinc-300">{counts().nodes}</span> nodes
      </span>
      <span class="text-zinc-700">·</span>
      <span>
        <span class="text-zinc-300">{counts().edges}</span> edges
      </span>
      <span class="text-zinc-700">·</span>
      <span class="text-amber-300">{formatBytesPerSec(props.totalBps)}</span>
    </div>
  );
};

const PaneTab: Component<{
  label: string;
  active: boolean;
  count?: number;
  unread?: number;
  onClick: () => void;
}> = (props) => (
  <button
    type="button"
    onClick={props.onClick}
    class={`px-3 py-1.5 text-[10px] uppercase tracking-wider border-b -mb-px transition-colors ${
      props.active
        ? "text-zinc-200 border-amber-500/60"
        : "text-zinc-500 border-transparent hover:text-zinc-300"
    }`}
  >
    {props.label}
    <Show when={props.count !== undefined && props.count > 0}>
      <span class="ml-1.5 text-zinc-600">{props.count}</span>
    </Show>
    <Show when={props.unread !== undefined && props.unread > 0}>
      <span class="ml-1 inline-flex items-center justify-center min-w-[14px] px-1 py-px text-[9px] bg-rose-500/30 text-rose-300 rounded-full">
        {props.unread}
      </span>
    </Show>
  </button>
);

const ConnectionPill: Component<{ state: string }> = (props) => {
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
    </div>
  );
};

export default App;
