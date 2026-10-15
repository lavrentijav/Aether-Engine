# CMD-25 — `/aether profile` sub-tick profiler

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`

## Goal
A per-system sub-tick profiler (BACKLOG O4). Turns "the tick is 40 ms" into "redstone is 22 ms,
light is 7 ms, entities 5 ms". Extends the `/aether` root from CMD-24. **Core, no P1 dependency**:
the per-system scopes are measured in the tick itself. Per-plugin timings are an optional addition
that activates only when the plugin host (`oc/P1`) is present (see below); the plugin must not be a
build or runtime requirement of this task.

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
- new `crates/aether-server/src/commands/profile.rs` + `crates/aether-server/src/profiler.rs`.
- the tick loop: add named scopes.

## Edge cases to test explicitly
- profiling off adds no measurable overhead (bench the tick with/without).
- start twice / stop twice; dump while stopped reports the last run or says none.
- a system that panics/errs still gets its scope closed (no leak).
- the report is deterministic for a fixed workload; percentages sum to ≤ 100.
- **without P1** the profiler builds and runs, and the plugin section is empty (not a panic, not a
  compile error); **with P1** per-plugin times appear when the host calls the scope API.

## Done criteria
- Profiler + command + tests; a bench proving the off-path is free.
- fmt / clippy `-D warnings` / `cargo-locked test`.
