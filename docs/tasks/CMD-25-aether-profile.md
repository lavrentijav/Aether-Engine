# CMD-25 — `/aether profile` sub-tick profiler

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N)
Needs: `oc/CMD-00N`, `oc/CMD-05`, `oc/CMD-01`

## Goal
A per-system sub-tick profiler (BACKLOG O4). Turns "the tick is 40 ms" into "redstone is 22 ms,
light is 7 ms, entities 5 ms". Extends the `/aether` root from CMD-24.

**Tier (CMD0 review):** this is a **native plugin**, not core. The core exposes **named scope hooks**
(no policy) around the tick systems; the profiler command, sampling, histograms and report live in
the native plugin (`plugins/`, `needs: oc/CMD-00N`). It does **not** need P1 directly, but it does
need the native loader CMD-00N. Per-plugin timings are an optional addition once the host provides
them.

## Commands
| syntax | node | op | feedback |
|---|---|---|---|
| `/aether profile start` | `aether.command.profile` | 4 | `Profiling started` |
| `/aether profile stop` | `aether.command.profile` | 4 | `Profiling stopped; writing report` |
| `/aether profile dump` | `aether.command.profile` | 4 | top N systems by self time (and plugin time if the host is loaded) |
| `/aether profile status` | `aether.command.profile` | 4 | `Profiling <running\|stopped>; <n> samples` |

Implementation: a cheap scope guard around each system in the tick (`game`, `physics`, `light`,
`redstone`, `journal`, `net`, …) accumulating a histogram with a bounded ring. Sampling overhead
when stopped must be zero (a boolean flag checked once per system). Output to chat (paged) and
optionally a file under the world dir.

**Optional P1 hook.** The profiler exposes a named-scope API; when the plugin host is compiled in
and loaded, it can call it to report per-plugin `on-tick`/event time. If P1 is absent the plugin
section is simply empty. This is deliberately *not* a `needs: oc/P1` — CMD-25 ships without it.

## Files
- **core:** named scope hooks in the tick (`crates/aether-server/src/profiler.rs`) — a cheap
  `profile("redstone") { … }` guard whose only policy is on/off; the hook API is what a native
  plugin reads.
- **native plugin:** `plugins/` — the `/aether profile` command, histogram storage, report.

## Edge cases to test explicitly
- profiling off adds no measurable overhead (bench the tick with/without).
- start twice / stop twice; dump while stopped reports the last run or says none.
- a system that panics/errs still gets its scope closed (no leak).
- the report is deterministic for a fixed workload; percentages sum to ≤ 100.
- **core scope hooks work with the profiler plugin disabled** (no panic, no cost beyond the flag);
  per-plugin times appear only when the host reports them.

## Done criteria
- Core scope hooks + native profiler + tests; a bench proving the off-path is free.
- fmt / clippy `-D warnings` / `cargo-locked test`.
