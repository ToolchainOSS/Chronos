/**
 * WebSocket client for the Rust egress hub.
 *
 * Connects to the delta stream, unpacks JSON frames into `Delta` values, feeds
 * them into the Zustand store, and reconnects with capped exponential backoff.
 */

import { useChronosStore } from "./store";
import type { Delta } from "./types";

const MIN_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 15_000;

/** Resolve the egress WebSocket URL from the current page origin. */
function egressUrl(): string {
  const proto = window.location.protocol === "https:" ? "wss" : "ws";
  return `${proto}://${window.location.host}/ws`;
}

/**
 * Start the delta client. Returns a disposer that stops reconnection and closes
 * the socket.
 */
export function startDeltaClient(): () => void {
  let socket: WebSocket | null = null;
  let backoff = MIN_BACKOFF_MS;
  let reconnectTimer: number | undefined;
  let disposed = false;

  const { setStatus, applyDelta } = useChronosStore.getState();

  const connect = (): void => {
    if (disposed) {
      return;
    }
    setStatus("connecting");
    socket = new WebSocket(egressUrl());

    socket.onopen = () => {
      backoff = MIN_BACKOFF_MS;
      setStatus("open");
    };

    socket.onmessage = (event: MessageEvent<string>) => {
      try {
        const delta = JSON.parse(event.data) as Delta;
        applyDelta(delta);
      } catch {
        // A malformed frame is ignored rather than tearing down the stream.
      }
    };

    socket.onclose = () => {
      setStatus("closed");
      scheduleReconnect();
    };

    socket.onerror = () => {
      socket?.close();
    };
  };

  const scheduleReconnect = (): void => {
    if (disposed) {
      return;
    }
    const jitter = 0.8 + Math.random() * 0.4;
    const delay = Math.min(backoff, MAX_BACKOFF_MS) * jitter;
    reconnectTimer = window.setTimeout(connect, delay);
    backoff = Math.min(backoff * 2, MAX_BACKOFF_MS);
  };

  connect();

  return () => {
    disposed = true;
    if (reconnectTimer !== undefined) {
      window.clearTimeout(reconnectTimer);
    }
    socket?.close();
  };
}
