# Topology & Detection

The in-memory internet model and the rule-based anomaly heuristics that run over
it. Read this before changing detection logic, the AS graph, or the prefix
table.

## Topology structures (`chronos-topology`)

- **`PrefixTable`**: a radix/prefix trie over IPv4 and IPv6 (separate tries,
  each behind an `RwLock`). `observe(prefix, origin)` records the current origin
  and returns an `OriginObservation`; `remove(prefix)` forgets it (used on
  withdrawal); longest-prefix match resolves coverage. See
  [crates/chronos-topology/src/trie.rs](../../crates/chronos-topology/src/trie.rs).
- **`AsGraph`**: the peering graph. Adjacency is a `DashMap<u32, HashSet<u32>>`;
  edge last-seen times are a `DashMap<(u32,u32), f64>` for aging.
  `observe_path(path, now)` derives edges from consecutive AS_PATH hops and
  returns newly created ones; `sweep_expired(now, ttl)` returns aged-out edges;
  `snapshot_edges(max)` bounds the initial client snapshot. See
  [crates/chronos-topology/src/graph.rs](../../crates/chronos-topology/src/graph.rs).

Both are wrapped in `Arc` and shared across the async tasks: this is a
legitimate shared-ownership use, not a borrow-checker escape hatch. Prefer their
concurrent APIs over adding a coarse `Mutex`.

## Detection heuristics (`chronos-detect`)

All detectors return an `Anomaly` (`PrefixHijack` / `PathLeak` / `RouteChurn`)
carrying a `Severity` (`Low`/`Medium`/`High`, mapped to a normalized index via
`as_index()`). See [crates/chronos-detect/src/anomaly.rs](../../crates/chronos-detect/src/anomaly.rs).

- **Origin / prefix hijack**: `check_origin(prefix, observation)` flags an
  origin change or a more-specific announcement inconsistent with the recorded
  origin. See [crates/chronos-detect/src/origin.rs](../../crates/chronos-detect/src/origin.rs).
- **Path leak**: `PathLeakDetector<P>` applies the valley-free (Gao-Rexford)
  rule using a `RelationshipProvider`. See
  [crates/chronos-detect/src/pathleak.rs](../../crates/chronos-detect/src/pathleak.rs).
- **Route churn / surge**: `SurgeMonitor` tracks per-prefix announcement rates
  in a sliding ring buffer and flags MAD-based (median absolute deviation)
  outliers. `evict_stale(now)` prunes idle windows. See
  [crates/chronos-detect/src/surge.rs](../../crates/chronos-detect/src/surge.rs).

## Relationship provider (pluggable)

`RelationshipProvider` is a trait selected at startup as `Arc<dyn ...>`:

- **`CaidaRelationships`** when a CAIDA AS-relationship dataset is mounted
  (`parse_caida_as_rel`).
- **`DegreeHeuristic`** fallback otherwise (uses `CHRONOS_DEGREE_RATIO`).

This is real runtime polymorphism: keep the `dyn` boundary. Graceful fallback
when CAIDA is absent is mandatory; see [data-files.md](data-files.md). See
[crates/chronos-detect/src/relationships.rs](../../crates/chronos-detect/src/relationships.rs).

## Extending detection

- New anomaly types add a variant to `Anomaly` and a mapping in
  `Pipeline::handle_anomaly` (metric label + optional geo → `AreaDegraded`).
- Keep detectors allocation-light; they run in the per-message consumer path.
- Add unit tests alongside the detector and, where a full flow matters, extend
  the mock RIS integration test. See [testing.md](testing.md).
