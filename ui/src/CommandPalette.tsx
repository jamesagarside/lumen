import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Show,
  onCleanup,
  type Component,
} from "solid-js";
import { displayName, type Node, type Snapshot } from "./types";

interface Props {
  open: boolean;
  snapshot: Snapshot | null;
  onSelect: (id: string) => void;
  onClose: () => void;
}

const MAX_RESULTS = 12;

/**
 * Quick-open palette for the graph. Triggered by Cmd/Ctrl-K. Filters
 * nodes by label, brand, and IP; arrow keys move, enter selects, esc
 * closes. The point of existence: once the graph has more than a couple
 * of dozen nodes, hunting visually for "the AWS host we saw last week"
 * is unreasonable — you should be able to type "aws" and jump.
 */
const CommandPalette: Component<Props> = (props) => {
  const [query, setQuery] = createSignal("");
  const [active, setActive] = createSignal(0);
  let inputRef: HTMLInputElement | undefined;
  let listRef: HTMLUListElement | undefined;

  // Reset state and grab focus whenever the palette opens.
  createEffect(() => {
    if (props.open) {
      setQuery("");
      setActive(0);
      // Defer focus to next microtask so the input is mounted.
      queueMicrotask(() => inputRef?.focus());
    }
  });

  const nodes = createMemo<Node[]>(() => props.snapshot?.nodes ?? []);

  /**
   * Ranked match list. Scoring is intentionally simple — exact > prefix >
   * substring — because fuzzy matching for IPs has too many false
   * positives (every IP looks like every other IP under a typo).
   */
  const results = createMemo<Scored[]>(() => {
    const q = query().trim().toLowerCase();
    const candidates = nodes();
    if (!q) {
      // Empty query → show the busiest internal nodes first so the
      // palette is useful as a "what's on my network right now" launcher
      // even before you type. We don't have per-node rates in the
      // snapshot, so fall back to alphabetical-ish by name.
      return [...candidates]
        .sort((a, b) => {
          if (a.is_internal !== b.is_internal) return a.is_internal ? -1 : 1;
          return displayName(a).localeCompare(displayName(b));
        })
        .slice(0, MAX_RESULTS)
        .map((n) => ({ node: n, score: 0 }));
    }
    const scored: Scored[] = [];
    for (const n of candidates) {
      const name = displayName(n).toLowerCase();
      const ip = n.id.toLowerCase();
      const brand = n.brand?.toLowerCase() ?? "";
      const label = n.label?.toLowerCase() ?? "";
      const fields = [label, brand, name, ip].filter(Boolean);
      let best = 0;
      for (const f of fields) {
        if (f === q) {
          best = Math.max(best, 100);
        } else if (f.startsWith(q)) {
          best = Math.max(best, 60);
        } else if (f.includes(q)) {
          best = Math.max(best, 30);
        }
      }
      if (best > 0) scored.push({ node: n, score: best });
    }
    scored.sort((a, b) => {
      if (b.score !== a.score) return b.score - a.score;
      // Prefer internal hosts on ties — that's what an operator
      // typically wants to act on.
      if (a.node.is_internal !== b.node.is_internal) {
        return a.node.is_internal ? -1 : 1;
      }
      return displayName(a.node).localeCompare(displayName(b.node));
    });
    return scored.slice(0, MAX_RESULTS);
  });

  // Clamp active index when results shrink.
  createEffect(() => {
    const max = Math.max(0, results().length - 1);
    if (active() > max) setActive(max);
  });

  // Keep the active row scrolled into view (relevant after arrow nav).
  createEffect(() => {
    const idx = active();
    if (!listRef) return;
    const row = listRef.children[idx] as HTMLElement | undefined;
    row?.scrollIntoView({ block: "nearest" });
  });

  const choose = (idx: number) => {
    const r = results()[idx];
    if (!r) return;
    props.onSelect(r.node.id);
    props.onClose();
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      props.onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(results().length - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(active());
    }
  };

  // Close on click outside the panel (the backdrop).
  const onBackdropClick = (e: MouseEvent) => {
    if (e.target === e.currentTarget) props.onClose();
  };

  // Document-level focus trap (lightweight): swallow tab so it doesn't
  // jump to the underlying app behind the backdrop.
  const onDocKey = (e: KeyboardEvent) => {
    if (!props.open) return;
    if (e.key === "Tab") e.preventDefault();
  };
  if (typeof window !== "undefined") {
    window.addEventListener("keydown", onDocKey);
    onCleanup(() => window.removeEventListener("keydown", onDocKey));
  }

  return (
    <Show when={props.open}>
      <div
        class="fixed inset-0 z-50 bg-black/60 backdrop-blur-sm flex items-start justify-center pt-[15vh]"
        onClick={onBackdropClick}
        role="dialog"
        aria-modal="true"
        aria-label="Quick open"
      >
        <div class="w-full max-w-xl mx-4 bg-zinc-950 border border-zinc-800 rounded-md shadow-2xl overflow-hidden">
          <div class="border-b border-zinc-800/80 px-3 py-2 flex items-center gap-2">
            <span class="text-[9px] uppercase tracking-wider text-zinc-600">⌘K</span>
            <input
              ref={inputRef}
              type="text"
              placeholder="search nodes by label, brand or IP…"
              value={query()}
              onInput={(e) => {
                setQuery(e.currentTarget.value);
                setActive(0);
              }}
              onKeyDown={onKey}
              class="flex-1 bg-transparent outline-none text-sm text-zinc-100 placeholder:text-zinc-700"
            />
            <span class="text-[9px] text-zinc-700 tabular-nums">
              {results().length} / {nodes().length}
            </span>
          </div>
          <Show
            when={results().length > 0}
            fallback={
              <div class="px-3 py-8 text-center text-[11px] text-zinc-600">
                no matches
              </div>
            }
          >
            <ul ref={listRef} class="max-h-[50vh] overflow-auto">
              <For each={results()}>
                {(r, i) => (
                  <Row
                    node={r.node}
                    active={i() === active()}
                    onMouseEnter={() => setActive(i())}
                    onClick={() => choose(i())}
                  />
                )}
              </For>
            </ul>
          </Show>
          <div class="border-t border-zinc-800/80 px-3 py-1.5 text-[9px] text-zinc-700 flex gap-3">
            <span>↑↓ navigate</span>
            <span>⏎ select</span>
            <span>esc close</span>
          </div>
        </div>
      </div>
    </Show>
  );
};

interface Scored {
  node: Node;
  score: number;
}

const Row: Component<{
  node: Node;
  active: boolean;
  onMouseEnter: () => void;
  onClick: () => void;
}> = (props) => {
  const name = () => displayName(props.node);
  const showIp = () => name() !== props.node.id;
  return (
    <li>
      <button
        type="button"
        onMouseEnter={props.onMouseEnter}
        onClick={props.onClick}
        class="w-full text-left px-3 py-1.5 flex items-baseline gap-2 hover:bg-zinc-900/80"
        classList={{ "bg-zinc-900": props.active }}
      >
        <span
          class={`size-1.5 rounded-full flex-shrink-0 ${props.node.is_internal ? "bg-emerald-500/70" : "bg-zinc-600"}`}
          aria-hidden
        />
        <span class="text-xs text-zinc-100 truncate">{name()}</span>
        <Show when={showIp()}>
          <span class="text-[10px] text-zinc-600 font-mono">{props.node.id}</span>
        </Show>
        <span class="ml-auto text-[9px] uppercase tracking-wider text-zinc-700">
          {props.node.is_internal ? "internal" : "external"}
        </span>
      </button>
    </li>
  );
};

export default CommandPalette;
