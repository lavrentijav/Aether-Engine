# CMD-18 — V-data: `/trigger`

Status: 📋 task brief (CMD0) · size: S · tier: core (C)
Needs: `oc/CMD-15`, `oc/CMD-05`

## Goal
Vanilla `/trigger`: let an ordinary player set a scoreboard objective whose criterion is `trigger`,
which a data pack/function then reacts to. This is the only command a non-op writes scores with.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/trigger [<objective>] [add\|set <value>]` | `minecraft.command.trigger` | 0 | `Triggered <objective> (set to <value>)` / `Triggered <objective> (added <value>)` |

Rules: only the executing player's own score; the objective must exist and have criterion `trigger`;
the objective must be **enabled** for the player (vanilla locks a trigger objective on join/reset;
a `/scoreboard players enable` unlocks it). With no value, `/trigger <obj>` sets it to 1. After a
successful trigger the objective becomes locked again (`enable` is reset).

## Files
- `crates/aether-server/src/commands/trigger.rs`.
- scoreboard model (CMD-15): objective criterion + per-player enable flag.

## Edge cases to test explicitly
- triggering an objective with a non-`trigger` criterion errors.
- triggering before `enable` errors (`<objective> is not enabled`); `enable` unlocks exactly once.
- a trigger from console (no player) errors.
- `add` and `set` forms; no value defaults to 1.
- after triggering, the objective is locked again.
- unknown objective errors.
- permission level 0: a vanilla player can run it; no operator needed.

## Done criteria
- Implementation + tests above; scoreboard integration test.
- fmt / clippy `-D warnings` / `cargo-locked test`.
