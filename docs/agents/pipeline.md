# Ingestion & Pipeline

The path from the live BGP feed to broadcast delta frames. Read this before
touching ingestion, the bounded channel, reconnect logic, or the consumer.

## Shape

```
RIS Live WebSocket ──(chronos-ingest)──▶ bounded mpsc ──(chronos-server pipeline)──▶ broadcast ──▶ /ws clients
```

- **Producer** (`chronos-ingest`): connects to RIPE RIS Live, subscribes,
  parses each envelope into `RisData`, and pushes it into a bounded `mpsc`
  channel. See [crates/chronos-ingest/src/client.rs](../../crates/chronos-ingest/src/client.rs).
- **Consumer** (`chronos-server` pipeline): a single task drains the channel,
  updates topology, runs detection, and broadcasts deltas. A companion interval
  ages out stale edges. See [crates/chronos-server/src/pipeline.rs](../../crates/chronos-server/src/pipeline.rs).

## Non-negotiable: never block or unbound the reader

The reader uses `try_send` into a **bounded** channel
(`CHRONOS_INGEST_CHANNEL_BOUND`, default 16384) and **drops on full**, counting
drops in `IngestStats`. This bounds memory and keeps the socket draining under
load.

- Do NOT switch to an unbounded channel or `send().await` in the read loop.
- Do NOT run detection, geo lookups, or serialization on the producer side.
- All heuristic and topology work belongs in `Pipeline::process`.

Drops are surfaced monotonically into the `INGEST_DROPPED` counter during the
sweep tick, not per-message.

## Reconnect & resilience

The client reconnects with capped exponential backoff plus jitter, mirroring the
frontend's [ws.ts](../../frontend/src/ws.ts) approach. A parse failure on one
message must never tear down the connection: count it and continue.

## Consumer responsibilities (`Pipeline::process`)

For each `RisData`:
1. `graph.observe_path(&path, now)` → any newly observed edges emit `LinkUp`.
2. `leak.inspect(&path)` → a path-leak anomaly if valley-free is violated.
3. Per announced prefix: `prefixes.observe(...)` + `check_origin(...)` (hijack),
   and `surge.record(...)` (route churn).
4. Withdrawals call `prefixes.remove(...)` so a re-announcement is treated as
   fresh, not a false origin change.

The sweep tick (`CHRONOS_SWEEP_INTERVAL_SECS`) ages edges past
`CHRONOS_EDGE_TTL_SECS` (emitting `LinkDown`), evicts stale surge windows, and
refreshes the `GRAPH_NODES` / `GRAPH_EDGES` gauges.

## Timestamps

Use the collector timestamp when present and plausible; otherwise fall back to
wall-clock (`event_time`). Detection and edge aging are driven by this value.

## Related

- Detection heuristics & topology structures → [detection.md](detection.md).
- Delta egress and the wire contract → [egress-frontend.md](egress-frontend.md).
- Metrics/observability constants live in
  [crates/chronos-server/src/metrics.rs](../../crates/chronos-server/src/metrics.rs).
