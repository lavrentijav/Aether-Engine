# CMD-32a — Core API: player state, effects, scheduler

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-01`, `oc/P1`
Parent/umbrella: `docs/tasks/CMD-32-player-api.md`

## Goal
The base player host API plugin features need: reading player state and applying temporary
state changes, plus the scheduler callback the other sub-tasks build on.

## API sketch (WIT imports; host adapters in `aether-server`)
- `set-temporary-effect(player, effect, ticks)`, `set-flight(player, bool)`,
  `set-speed(player, multiplier)` / `set-attribute`, `heal`, `feed`, `set-gamemode`.
- `scheduler::run-after(ticks, callback)` — runs on the tick thread at a safe point (verify P1's
  scheduler; drop a callback whose plugin was disabled).
- Temporary effects are **removed on plugin unload** and must not clobber a permanent longer/
  stronger effect (EssX semantics).

## Files
- `crates/aether-plugin` WIT + host imports (P1), `aether-server` adapters.
- `game/effects.rs`.

## Edge cases to test explicitly
- temporary-effect removal on unload; a permanent longer effect is not clobbered.
- `run-after` after the plugin is disabled is dropped, not executed.
- `set-flight`/`set-speed` survive a rejoin per EssX rules (document); `heal`/`feed` clamp to max.
- no lock-order inversion with the entity lock.

## Done criteria
- WIT + adapters + a reference test plugin using effect/flight/scheduler.
- fmt / clippy `-D warnings` / `cargo-locked test`.
