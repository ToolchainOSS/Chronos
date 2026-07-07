# Engineering Standards & Operating Contract

These standards are **enforced on every coding-agent session** working in this
repository. The root [AGENTS.md](../../AGENTS.md) carries the condensed,
always-on summary; this file is the authoritative, full version. Load it before
any non-trivial Rust change or refactor.

> Scope note: The upstream system prompt also covered C# and Haskell. Chronos is
> a **Rust + TypeScript/React** codebase, so only the Rust and
> language-agnostic directives are retained here. C#/Haskell-specific rules are
> intentionally omitted.

## 1. Core Operating Rules

1. **Autonomy with documented assumptions.** Operate without asking for
   permission to proceed on routine steps. When faced with ambiguity, make the
   most reasonable technical assumption, document it in your output, and
   continue. Reserve questions for genuinely destructive or irreversible
   actions (see [AGENTS.md](../../AGENTS.md) → Boundaries).
2. **State management.** Maintain and actively update a TODO list (up to ~100
   items) for any multi-step task so you never lose your place in complex
   workflows.
3. **Context optimization.** When a subtask risks overwhelming the context
   window, delegate it to a read-only exploration subagent rather than loading
   everything into the main thread.
4. **Termination protocol.** Do not stop silently. When the objective is
   verifiably complete (build, lint, and tests green), emit an explicit final
   status message and halt.

## 2. Universal Engineering Directives

- **Make invalid states unrepresentable.** Use the type system to prevent
  invalid states at compile time rather than validating at runtime. Chronos
  already leans on this: the `Asn(u32)` newtype, the `IpPrefix` V4/V6 enum, and
  the `#[serde(tag = "kind")]` `Delta` enum make malformed wire data a parse
  error, not a runtime branch. Extend that pattern; do not add stringly-typed
  fields that need re-validation downstream.
- **Ruthless refactoring: but two contracts are sacred.** You may break,
  rename, or delete *internal* code interfaces freely (no backward-compat duty
  for internal APIs) and prune dead code aggressively. Chronos has **no
  database**, so the usual "migrations are sacred" carve-out maps onto two
  boundaries instead:
  1. **The `Delta` wire contract** between the Rust backend and the React
     frontend. Any change ships on both sides in the same commit with a
     round-trip test. See [egress-frontend.md](egress-frontend.md).
  2. **Mounted-data-file behavior.** GeoLite2 / CAIDA files are external,
     read-only inputs; startup must degrade gracefully (never panic) when they
     are absent, and the engine must never require or bundle them. See
     [data-files.md](data-files.md).
- **Aggressive modularization (500-line soft limit).** No source file should
  exceed ~500 lines. Split files approaching the limit into cohesive submodules
  (the crate split into types, ingest, topology, detect, geo, and server is the
  precedent to follow).
- **Idiomatic error handling.** Never swallow errors. Use `Result`/`Option`
  with `?`, `thiserror` for typed library errors (e.g. `ParseError`,
  `PrefixParseError`), and `anyhow` at binary boundaries.

## 3. Rust-Specific Execution

- **Design for the borrow checker.** Pre-calculate ownership and lifetime
  hierarchies; design data flow so the compiler is satisfied by the
  architecture, not by escape hatches.
- **Avoid reflexive `.clone()` / `Rc` / `Arc` / `Copy`.** Do not reach for these
  merely to appease the borrow checker. If lifetimes clash, redesign the data
  flow. Legitimate shared-ownership needs remain acceptable when the ownership
  requirement is real: Chronos genuinely shares `Arc<AsGraph>`,
  `Arc<GeoResolver>`, and `Arc<dyn RelationshipProvider>` across async tasks,
  and uses tokio broadcast/mpsc channels for fan-out and backpressure. Those are
  correct; a `.clone()` inside the per-message hot loop is not.
- **Zero-cost abstractions.** Prefer traits, generics, and monomorphization over
  dynamic dispatch (`Box<dyn _>`) unless runtime polymorphism is genuinely
  required (e.g. `Arc<dyn RelationshipProvider>` selects CAIDA vs. degree
  heuristic at startup: real polymorphism, keep it).
- **Concurrency.** Favor message passing and the existing async primitives over
  ad-hoc shared mutable state. The ingest reader must stay non-blocking: it
  `try_send`s into a bounded channel and drops on full. Do detection work on the
  consumer side. Concurrent maps (`DashMap`) and `RwLock`-guarded tries are the
  approved shared-state primitives; do not wrap the whole topology in a coarse
  `Mutex`. See [pipeline.md](pipeline.md).

## 4. Output Contract (per step)

For each step of a non-trivial task, structure your progress as:

- **Current State**: what was just completed.
- **Assumptions Made**: independent technical decisions.
- **TODO Update**: items added or checked off.
- **Architectural Plan**: when writing/modifying code, briefly state:
  1. *Resource/Effect strategy*: ownership, lifetimes, borrowing, channels.
  2. *Type-state / PLT plan*: how the type system blocks invalid states here.
  3. *Pruning targets*: legacy code being deleted or large files being split.
  (Write `N/A` when not coding.)
- **Next Action**: the exact command, edit, or subagent being executed now.

## 5. Definition of Done

A task is complete only when **all** of the following pass locally
(see [testing.md](testing.md) for commands):

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`, including the mock RIS Live integration test
- Frontend `npm run typecheck`, `npm run lint`, and `npm run build` (when the
  frontend changed)
- Any `Delta` protocol change reflected on both Rust and TypeScript sides with a
  round-trip test
- The engine still starts and degrades gracefully with GeoLite2 / CAIDA files
  absent
