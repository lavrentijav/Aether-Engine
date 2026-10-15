# CMD-25 — `/aether profile` sub-tick profiler

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/P1`

## Goal
A per-system, per-plugin sub-tick profiler (BACKLOG O4). Turns "the tick is 40 ms" into "redstone
is 22 ms, plugin `economy` is 9 ms". Extends the `/aether` root from CMD-24.

## Commands
| syntax | node | op | feedback |
|---|---|---|---|
| `/aether profile start` | `aether.command.profile` | 4 | `Profiling started` |
| `/aether profile stop` | `aether.command.profile` | 4 | `Profiling stopped; writing report` |
| `/aether profile dump` | `aether.command.profile` | 4 | top N systems by self time, plus plugin time |
| `/aether profile status` | `aether.command.profile` | 4 | `Profiling <running\|stopped>; <n> samples` |

Implementation: a cheap scope guard around each system in the tick (`game`, `physics`, `light`,
`redstone`, `journal`, `net`, each plugin's `on-tick`/event calls) accumulating a histogram with a
bounded ring. Sampling overhead when stopped must be zero (a boolean flag checked once per system).
Output to chat (paged) and optionally a file under the world dir. Aggregate plugin time from the
P1 host's per-call timing.

## Files
- new `crates/aether-server/src/commands/profile.rs` + `crates/aether-server/src/profiler.rs`.
- the tick loop: add named scopes; the P1 plugin bus: report per-plugin times.

## Edge cases to test explicitly
- profiling off adds no measurable overhead (bench the tick with/without).
- start twice / stop twice; dump while stopped reports the last run or says none.
- a system that panics/errs still gets its scope closed (no leak).
- the report is deterministic for a fixed workload; percentages sum to ≤ 100.
- with no plugins installed, plugin section is empty, not a panic.

## Done criteria
- Profiler + command + tests; a bench proving the off-path is free.
- fmt / clippy `-D warnings` / `cargo-locked test`.
