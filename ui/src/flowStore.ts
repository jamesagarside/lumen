import { createSignal, type Accessor } from "solid-js";
import type { Flow } from "./types";

const MAX_FLOWS = 200;

export type ConnectionState = "connecting" | "open" | "closed";

export interface FlowStore {
  flows: Accessor<Flow[]>;
  connection: Accessor<ConnectionState>;
  connect: () => void;
  disconnect: () => void;
}

export const createFlowStore = (url: string): FlowStore => {
  const [flows, setFlows] = createSignal<Flow[]>([]);
  const [connection, setConnection] = createSignal<ConnectionState>("connecting");

  let socket: WebSocket | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  const connect = () => {
    if (socket && socket.readyState !== WebSocket.CLOSED) return;
    setConnection("connecting");
    socket = new WebSocket(url);

    socket.addEventListener("open", () => setConnection("open"));
    socket.addEventListener("close", () => {
      setConnection("closed");
      reconnectTimer = setTimeout(connect, 2000);
    });
    socket.addEventListener("error", () => {
      socket?.close();
    });
    socket.addEventListener("message", (event) => {
      try {
        const flow = JSON.parse(event.data as string) as Flow;
        setFlows((prev) => {
          const next = [flow, ...prev];
          return next.length > MAX_FLOWS ? next.slice(0, MAX_FLOWS) : next;
        });
      } catch {
        // ignore malformed frames; the wire format hardens in #8
      }
    });
  };

  const disconnect = () => {
    if (reconnectTimer) clearTimeout(reconnectTimer);
    socket?.close();
  };

  return { flows, connection, connect, disconnect };
};
