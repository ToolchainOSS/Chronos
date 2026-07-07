# Egress & Frontend

How deltas leave the backend and drive the React UI. Read this before changing
the `Delta` protocol, the Axum hub, the WebSocket client, or the store.

## The Delta wire contract (the one sacred boundary)

The backend never ships the full graph. It emits minimal, internally tagged
JSON frames keyed by `kind`:

```json
{ "kind": "LinkUp", "a": 1234, "b": 5678 }
{ "kind": "LinkDown", "a": 1234, "b": 5678 }
{ "kind": "AreaDegraded", "region": "US-CA", "severity": 0.87 }
```

This is **one protocol with two implementations**:

- Rust: `chronos_types::Delta`: `#[serde(tag = "kind")]` enum. See
  [crates/chronos-types/src/delta.rs](../../crates/chronos-types/src/delta.rs).
- TypeScript: the `Delta` union in
  [frontend/src/types.ts](../../frontend/src/types.ts).

**Rules:**
- Any change (new variant, renamed/added field, changed type) ships on **both
  sides in the same commit**.
- Keep the Rust round-trip test in `delta.rs` green, and mirror the shape in
  `types.ts`. `severity` is clamped to `0.0..=1.0` on the server; the client may
  assume that range.
- Field names are the wire format: renaming `a`/`b`/`region`/`severity` is a
  breaking protocol change, not a local refactor.

## Egress hub (Axum)

[crates/chronos-server/src/hub.rs](../../crates/chronos-server/src/hub.rs)
exposes:

- `GET /ws`: upgrades to WebSocket. On connect, sends a **bounded snapshot**
  (`snapshot_edges(snapshot_max)` as `LinkUp` frames), then streams live deltas
  from a `broadcast::Receiver`.
- `GET /healthz`: liveness. `GET /readyz`: readiness (gated on `state.ready`).
- `GET /metrics`: Prometheus text exposition.

Backpressure is the broadcast ring buffer: a slow client gets `RecvError::Lagged`
(oldest frames dropped) and is logged, never stalling the producer. Do not add
per-client queues that can grow unbounded.

## Frontend data flow

```
/ws ──▶ ws.ts (reconnect) ──▶ applyDelta ──▶ Zustand store ──▶ LogicalPanel / GeoPanel
```

- **[frontend/src/ws.ts](../../frontend/src/ws.ts)** is the *only* place a
  `WebSocket` is opened. It parses frames, calls `applyDelta`, and reconnects
  with capped exponential backoff + jitter. A malformed frame is ignored, not
  fatal. Components must not open their own sockets.
- **[frontend/src/store.ts](../../frontend/src/store.ts)** (Zustand,
  `useChronosStore`) holds nodes/links/status and applies deltas: `LinkUp` adds,
  `LinkDown` prunes (`pruneLink`), `AreaDegraded` updates region severity.
- **`LogicalPanel`** renders the AS graph via `react-force-graph-2d`
  (LinkDown drives particle dissipation). **`GeoPanel`** renders the map via
  `react-map-gl` / `maplibre` (`AreaDegraded` drives `setFeatureState`).

Components subscribe to store slices; keep rendering logic out of `ws.ts` and
network logic out of components.

## Frontend quality gate

```bash
cd frontend && npm ci && npm run typecheck && npm run lint && npm run build
```

Dev proxy: `vite.config.ts` proxies `/ws` to `:8080`, so `npm run dev` works
against a locally running backend. Per project policy, do not run
Playwright/Chromium E2E in this environment: rely on CI for browser tests.
