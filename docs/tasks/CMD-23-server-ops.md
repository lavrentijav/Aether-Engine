# CMD-23 — Server diagnostics & lifecycle

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-05`, `oc/CMD-01`

## Goal
Operator commands for server health and lifecycle. Vanilla (`/debug`, `/perf`, `/jfr`) plus the
server-owner staples every host expects (`/tps`, `/mspt`, `/gc`, `/plugins`, `/restart`). Reference
behaviour for the non-vanilla ones is Paper/Spigot; match their intent, and state the exact strings
in the module.

## Commands
| syntax | node | op | feedback |
|---|---|---|---|
| `/tps` | `aether.command.tps` | 3 | `TPS from last 1m, 5m, 15m: <a>, <b>, <c>` (Paper) |
| `/mspt` | `aether.command.mspt` | 3 | `Server tick times (ms): ...` (Paper) |
| `/gc` | `aether.command.gc` | 3 | `Memory: <used>/<max> MB, <free>% free` (Spigot) |
| `/plugins` (alias `/pl`) | `aether.command.plugins` | 3 | `<n> plugin(s): <ids>` |
| `/restart` | `aether.command.restart` | 4 | `Restarting the server...` (Aether; supervised by systemd) |
| `/debug start\|stop\|function <name>` | `minecraft.command.debug` | 3 | `Started debug profiling` / `Stopped debug profiling` |
| `/perf start\|stop` | `minecraft.command.perf` | 4 | `Started/Stopped performance profiling` |
| `/jfr start\|stop` | `minecraft.command.jfr` | 4 | `Started/Stopped JFR` |

Tick timing: reuse the telemetry crate (`aether-telemetry`) — if it already samples tick duration,
read it; otherwise add a rolling 1m/5m/15m average in the tick loop (cheap, lock-free).
`/plugins` reads the plugin host (P1) once merged; until then it lists the built-in services and
reports that the plugin host is unavailable.
`/restart` must save (`/save-all` path) then exit with a code systemd restarts; never `exec` itself.

## Files
- new `crates/aether-server/src/commands/diagnostics.rs`.
- tick loop / telemetry for TPS/MSPT; `plugins` from the host; `restart` from `main.rs` shutdown.
- `/visibility` and `/cache` already exist (catalogue) — do not re-implement; list them here only
  if they need a permission node added.

## Edge cases to test explicitly
- `/tps` before enough samples: report `0`/`*` not a panic; after a stall the averages move.
- `/gc` parses a metrics source that is unavailable without crashing.
- `/plugins` with the host disabled and enabled.
- `/restart` with unsaved chunks: saves first; a save failure aborts the restart with an error.
- op levels: level 3 cannot `/restart`; console is 4.
- `/debug`/`/perf`/`/jfr` start/stop twice (second start errors or is a no-op, matching vanilla).

## Done criteria
- Implementations + tests for the timing maths and permission gating.
- fmt / clippy `-D warnings` / `cargo-locked test`.
