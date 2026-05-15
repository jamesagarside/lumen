import { createResource, Show, type Component } from "solid-js";

interface VersionInfo {
  name: string;
  version: string;
  abi_version: string;
}

const fetchVersion = async (): Promise<VersionInfo> => {
  const res = await fetch("/version");
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
};

const App: Component = () => {
  const [version] = createResource(fetchVersion);

  return (
    <main class="min-h-screen bg-zinc-950 text-zinc-100 flex items-center justify-center font-mono">
      <div class="text-center space-y-3">
        <h1 class="text-3xl font-semibold tracking-tight">Lumen</h1>
        <p class="text-sm text-zinc-400">
          Real-time network visualization. Waiting for flows.
        </p>
        <Show when={version()} fallback={<p class="text-xs text-zinc-600">…</p>}>
          {(v) => (
            <p class="text-xs text-zinc-600">
              v{v().version} · ABI {v().abi_version}
            </p>
          )}
        </Show>
      </div>
    </main>
  );
};

export default App;
