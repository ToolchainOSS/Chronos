# Resource baseline

Measured bandwidth, CPU, and memory for a typical running `chronos-server`
instance, so capacity planning and regressions have a concrete reference. The
figures come from [scripts/resource-baseline.sh](../../scripts/resource-baseline.sh),
which runs the release binary against the real data sources and samples the
kernel's per-process accounting.

## Headline numbers

Full unfiltered RIS Live firehose, geo resolution enabled, steady state after
warmup:

| Resource   | Baseline (typical instance) |
|------------|-----------------------------|
| CPU        | ~20% of one core (~5% of a 4-vCPU host) |
| Memory RSS | ~125 MiB average, ~135 MiB peak |
| Ingress    | ~1.9 MiB/s (~2000 KiB/s), ~163 GiB/day, ~4.8 TiB/month |
| Throughput | ~4400 RIS messages/s (~460 bytes/message on the wire) |
| Topology   | ~14k ASNs, ~33k edges after ~4 min (grows toward the full table) |
| Dropped    | 0 frames (bounded ingest channel kept up) |

The single largest cost is **network ingress**: the RIS Live firehose is a
high-volume stream, so egress bandwidth from RIPE dominates the resource
profile. CPU and memory are modest and comfortably fit a small container.

## How it was measured

- **Binary:** `cargo build --release --bin chronos-server` (optimized profile).
- **Data sources:** the real RIPE RIS Live feed
  (`ws://ris-live.ripe.net/v1/ws/?client=chronos`), the CAIDA AS-relationship
  dataset (auto-downloaded), and both GeoLite2 databases (City + ASN, downloaded
  so geo resolution is active, matching a typical deployment).
- **Window:** 45s warmup (CAIDA download, RIS connect, initial topology burst)
  then a 180s measurement window.
- **CPU:** `utime + stime` from `/proc/<pid>/stat` divided by the window and by
  `CLK_TCK` (100), reported both per core and normalized to the host vCPU count.
- **Memory:** `VmRSS` from `/proc/<pid>/status`, sampled every 5s for average
  and peak.
- **Ingress:** the RIS TCP socket's `bytes_received` counter from `ss -tinp`,
  summed across the server's established connections. This reads the socket
  counter directly because `/proc/<pid>/io` `rchar` does **not** count socket
  `recv()` traffic and reports zero for network reads.
- **Throughput / topology:** deltas of the Prometheus counters
  (`chronos_messages_processed_total`, `chronos_graph_nodes`,
  `chronos_graph_edges`, `chronos_ingest_dropped_total`) from `/metrics`.
- **Host:** 4 vCPU Linux container.

Reproduce with:

```bash
cargo build --release --bin chronos-server
scripts/resource-baseline.sh            # 45s warmup, 180s window (defaults)
scripts/resource-baseline.sh 60 300     # custom warmup/window in seconds
```

## Caveats and how to read these numbers

- **Ingress is the full firehose.** The baseline uses no collector filter, so it
  captures every RIS update globally. Setting `CHRONOS_RIS_HOST` to a single
  collector cuts ingress (and CPU) by a large factor; size bandwidth for the
  filter you actually deploy.
- **Bandwidth varies with BGP activity.** RIS volume rises during routing
  churn (leaks, large withdrawals), so treat the ~1.9 MiB/s figure as a typical
  steady-state rate, not a hard ceiling. Provision headroom.
- **Memory grows with the AS graph, then plateaus.** Early in a run the topology
  is still filling in; RSS climbs as ASNs and edges accumulate, then levels off
  once the in-memory graph approximates the full table. The ~135 MiB peak is a
  short-window figure; expect it to settle somewhat higher over a long run as
  the graph converges, but still on the order of a few hundred MiB, not GiB.
- **CPU is single-window.** ~20% of a core reflects parse + graph-update work at
  ~4400 msg/s. It scales roughly with message rate, so a filtered feed uses
  proportionally less.
- **Egress to browsers is not included.** These numbers cover ingest and the
  engine. WebSocket egress to connected frontends adds bandwidth proportional to
  client count and delta rate; see
  [egress-frontend.md](egress-frontend.md).

## Sizing guidance

- A single small instance (1 vCPU, 256 to 512 MiB RAM) comfortably runs the full
  firehose with headroom.
- The binding constraint is usually **inbound bandwidth**, not CPU or memory.
  Budget for roughly 160 GiB/day of ingress on an unfiltered feed, or apply
  `CHRONOS_RIS_HOST` to reduce it.
- No disk or database load: datasets are cached under `CHRONOS_DATA_DIR` and the
  topology is entirely in memory.
