import { For, Show, type Component } from "solid-js";
import type { DetectionEvent } from "./types";
import { severityColor, severityName } from "./types";

interface Props {
  events: DetectionEvent[];
  onSelectIp: (ip: string) => void;
}

const EventsSidebar: Component<Props> = (props) => {
  return (
    <div class="w-full h-full overflow-auto">
      <Show
        when={props.events.length > 0}
        fallback={
          <div class="h-full flex items-center justify-center text-[10px] text-zinc-700 px-6 text-center">
            no detection events yet. Push to <code>POST /ingest/events</code> or let an integration provider push.
          </div>
        }
      >
        <ul class="divide-y divide-zinc-800/60">
          <For each={props.events}>
            {(e) => <EventRow event={e} onSelectIp={props.onSelectIp} />}
          </For>
        </ul>
      </Show>
    </div>
  );
};

const EventRow: Component<{ event: DetectionEvent; onSelectIp: (ip: string) => void }> = (
  props,
) => {
  const sev = () => props.event["event.severity"] ?? 1;
  const colors = () => severityColor(sev());
  const timestamp = () => {
    const t = props.event["@timestamp"];
    if (!t) return "";
    const d = new Date(t.secs_since_epoch * 1000);
    return d.toLocaleTimeString();
  };
  const onIpClick = (ip: string | undefined) => {
    if (ip) props.onSelectIp(ip);
  };

  return (
    <li class="px-3 py-2 hover:bg-zinc-900/60">
      <div class="flex items-center gap-2 mb-0.5">
        <span class={`size-1.5 rounded-full ${colors().dot}`} />
        <span class={`text-[10px] uppercase tracking-wider ${colors().text}`}>
          {severityName(sev())}
        </span>
        <span class="text-[10px] text-zinc-600">{props.event.agent.type}</span>
        <span class="text-[10px] text-zinc-700 ml-auto">{timestamp()}</span>
      </div>
      <div class="text-xs text-zinc-200 mb-1 truncate" title={props.event.message}>
        {props.event.message}
      </div>
      <div class="flex items-baseline gap-2 text-[10px] text-zinc-500">
        <Show when={props.event["source.ip"]}>
          {(ip) => (
            <button
              type="button"
              class="hover:text-zinc-200"
              onClick={() => onIpClick(ip())}
              title={`select ${ip()} in the graph`}
            >
              {ip()}
            </button>
          )}
        </Show>
        <Show when={props.event["destination.ip"]}>
          {(ip) => (
            <>
              <span class="text-zinc-700">→</span>
              <button
                type="button"
                class="hover:text-zinc-200"
                onClick={() => onIpClick(ip())}
                title={`select ${ip()} in the graph`}
              >
                {ip()}
              </button>
            </>
          )}
        </Show>
        <Show when={props.event["url.original"]}>
          {(url) => (
            <a
              href={url()}
              target="_blank"
              rel="noopener noreferrer"
              class="ml-auto text-zinc-600 hover:text-zinc-300"
              title="open in source system"
            >
              ↗
            </a>
          )}
        </Show>
      </div>
    </li>
  );
};

export default EventsSidebar;
