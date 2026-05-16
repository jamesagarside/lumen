import { Show, type Component } from "solid-js";

interface Props {
  unreachable: boolean;
  wsState: string;
}

const DaemonBanner: Component<Props> = (props) => {
  // WS being closed for a moment is normal during reconnect; only
  // promote to "down" if /snapshot has also failed repeatedly.
  const isDown = () => props.unreachable;

  return (
    <Show when={isDown()}>
      <div class="bg-rose-950/80 border-b border-rose-900 text-rose-200 px-4 py-2.5 text-xs flex items-center justify-between">
        <div class="flex items-center gap-3">
          <span class="size-2 rounded-full bg-rose-400 animate-pulse" />
          <span>
            <strong>Daemon not reachable.</strong> The Lumen daemon at{" "}
            <code class="bg-rose-900/60 px-1 py-0.5 rounded">localhost:3000</code> isn't
            responding. Check the daemon terminal — it may have crashed or never started.
          </span>
        </div>
        <code class="text-[10px] text-rose-300 hidden sm:block">
          make dev-daemon
        </code>
      </div>
    </Show>
  );
};

export default DaemonBanner;
