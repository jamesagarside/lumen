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
import AdminSettings from "./AdminSettings";
import DaemonBanner from "./DaemonBanner";
import EventsSidebar from "./EventsSidebar";
import FlowTable from "./FlowTable";
import Login from "./Login";
import NodeInspector from "./NodeInspector";
import SigmaGraph from "./SigmaGraph";
import TimeScrubber from "./TimeScrubber";
import { createAuthStore } from "./authStore";
import { createEventsStore } from "./eventsStore";
import { createFlowStore } from "./flowStore";
import { createScrubStore } from "./scrubStore";
import { createSnapshotStore } from "./snapshotStore";
import type { ViewKind } from "./layouts";
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
          userRole={
            authStore.state().status === "authed"
              ? (authStore.state() as { status: "authed"; me: { user: { role: string } } }).me
                  .user.role
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
  userRole: string;
}

type RightPane = "flows" | "events";

/** True when ?kiosk=1 is in the URL. Stable across renders. */
const kioskFromUrl = (): boolean => {
  if (typeof window === "undefined") return false;
  const params = new URLSearchParams(window.location.search);
  return params.get("kiosk") === "1";
};

const AuthedApp: Component<AuthedAppProps> = (props) => {
  const [version] = createResource(fetchVersion);
  const flowStore = createFlowStore(wsUrl());
  const snapshotStore = createSnapshotStore();
  const eventsStore = createEventsStore();
  const scrubStore = createScrubStore();
  const [selectedNode, setSelectedNode] = createSignal<string | null>(null);
  const [relayoutTick, setRelayoutTick] = createSignal(0);
  const [pane, setPane] = createSignal<RightPane>("flows");
  const [view, setView] = createSignal<ViewKind>("graph");
  const [settingsOpen, setSettingsOpen] = createSignal(false);

  // While scrubbed back in time, the inspector + graph read from the
  // historical reconstruction instead of the live snapshot poll. Live
  // mode flips back to the snapshot store seamlessly.
  const effectiveSnapshot = () =>
    scrubStore.isLive() ? snapshotStore.snapshot() : scrubStore.scrubbedSnapshot();

  // Cmd/Ctrl-1/2/3 shortcuts for view switching. Mounted on
  // window so they fire regardless of focus, except when the user
  // is typing in an input (label edit, search box).
  const onKey = (e: KeyboardEvent) => {
    if (!(e.metaKey || e.ctrlKey)) return;
    const target = e.target as HTMLElement | null;
    if (target?.tagName === "INPUT" || target?.tagName === "TEXTAREA") return;
    const view = ({ "1": "graph", "2": "vlan", "3": "architecture" } as const)[e.key];
    if (view) {
      e.preventDefault();
      setView(view);
    }
  };

  onMount(() => {
    flowStore.connect();
    snapshotStore.start();
    eventsStore.start();
    scrubStore.start();
    window.addEventListener("keydown", onKey);
  });
  onCleanup(() => {
    flowStore.disconnect();
    snapshotStore.stop();
    eventsStore.stop();
    scrubStore.stop();
    window.removeEventListener("keydown", onKey);
  });

  // Unread-badge state on the Detections tab.
  const detectionsCount = () => eventsStore.events().length;
  let seenAtCount = 0;

  // Kiosk mode: stripped chrome for wall-display use. Activated by
  // ?kiosk=1 in the URL OR by the user having the NocDisplay role.
  const isKiosk = () => kioskFromUrl() || props.userRole === "noc_display";

  const totalBytesPerSec = () => {
    const s = effectiveSnapshot();
    if (!s) return 0;
    return s.edges.reduce((sum, e) => sum + e.bytes_per_sec, 0);
  };

  return (
    <main class="h-screen bg-zinc-950 text-zinc-100 font-mono flex flex-col overflow-hidden">
      <DaemonBanner
        unreachable={snapshotStore.unreachable()}
        wsState={flowStore.connection()}
      />
      <Show when={!isKiosk()}>
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
          <TopologyStat snapshot={effectiveSnapshot()} totalBps={totalBytesPerSec()} />
          <ViewSwitcher value={view()} onChange={setView} />
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
              <Show when={props.can("manage_settings")}>
                <button
                  type="button"
                  class="text-zinc-500 hover:text-amber-300"
                  onClick={() => setSettingsOpen(true)}
                  title="settings"
                >
                  ⚙
                </button>
              </Show>
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
      </Show>

      <div
        class="flex-1 min-h-0 grid gap-px bg-zinc-800"
        classList={{
          "grid-cols-1 lg:grid-cols-[3fr_2fr]": !isKiosk(),
          "grid-cols-1": isKiosk(),
        }}
      >
        <section class="bg-zinc-950 min-h-0 overflow-hidden flex flex-col">
          <div class="flex-1 min-h-0 relative">
            <SigmaGraph
              snapshot={effectiveSnapshot()}
              events={eventsStore.events()}
              selectedNodeId={selectedNode()}
              onSelectionChange={setSelectedNode}
              relayoutSignal={relayoutTick()}
              view={view()}
            />
          </div>
          <Show when={!isKiosk()}>
            <TimeScrubber scrub={scrubStore} />
          </Show>
        </section>
        <Show when={!isKiosk()}>
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
              snapshot={effectiveSnapshot()}
              selectedId={selectedNode()}
              onClose={() => setSelectedNode(null)}
              onLabelSaved={() => {
                // Snapshot poll picks up the new label on the next tick.
              }}
            />
          </Show>
        </section>
        </Show>
      </div>
      <Show when={settingsOpen()}>
        <AdminSettings onClose={() => setSettingsOpen(false)} />
      </Show>
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

const ViewSwitcher: Component<{
  value: ViewKind;
  onChange: (v: ViewKind) => void;
}> = (props) => {
  const cmd = typeof navigator !== "undefined" && /Mac/.test(navigator.platform) ? "⌘" : "Ctrl";
  const opts: Array<{ key: ViewKind; label: string; hotkey: string }> = [
    { key: "graph", label: "Graph", hotkey: "1" },
    { key: "vlan", label: "VLAN", hotkey: "2" },
    { key: "architecture", label: "Arch", hotkey: "3" },
  ];
  return (
    <div class="flex border border-zinc-800 rounded overflow-hidden">
      {opts.map((o) => (
        <button
          type="button"
          title={`${o.label} view  (${cmd}+${o.hotkey})`}
          onClick={() => props.onChange(o.key)}
          class={`px-2 py-1 text-[10px] uppercase tracking-wider transition-colors ${
            props.value === o.key
              ? "bg-amber-500/15 text-amber-300"
              : "text-zinc-500 hover:text-zinc-300 hover:bg-zinc-900"
          }`}
        >
          {o.label}
        </button>
      ))}
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
