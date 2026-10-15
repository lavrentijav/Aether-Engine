# CMD-20c — V-data: random, schedule

Status: 📋 task brief (CMD0) · size: S · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-20-loot-recipe.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/random value <range> [<sequence>]` | `minecraft.command.random` | 2 | `<sequence> returned <n>` / `Random value <n>` |
| `/random roll <range> [<sequence>]` | `minecraft.command.random` | 2 | `<sequence> rolled <n>` |
| `/schedule function <function> <time> [append\|replace]` | `minecraft.command.schedule` | 2 | `Scheduled function <name> in <time>` |
| `/schedule clear <function>` | `minecraft.command.schedule` | 2 | `Cleared function <name>` |

`<range>` is `min..max` (inclusive) with integer bounds; sequences come from the data pack
(`worldgen/random_sequence`). `<time>` is a vanilla duration (`1d`, `2t`, …). Functions come from
CMD-21b.

## Files
- new `crates/aether-server/src/commands/random.rs`, `schedule.rs`.
- the game scheduler (`game/tickstore.rs`) — schedule entries must survive a restart.

## Edge cases to test explicitly
- `/random value 1..10` stays in range; `roll` respects a sequence's bias/table.
- a named sequence is deterministic (seeded test); an unknown sequence errors.
- `/schedule` with a past/negative time fires next tick; `append` keeps duplicates, `replace`
  replaces; a scheduled function survives restart; `clear` with no entry errors.

## Done criteria
- Implementation + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
