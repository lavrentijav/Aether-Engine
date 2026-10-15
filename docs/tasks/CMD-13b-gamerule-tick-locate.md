# CMD-13b — V-world: gamerule, difficulty, tick, locate, waypoint

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-13-world-ops.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/gamerule <rule> [<value>]` / `/gamerule list` | `minecraft.command.gamerule` | 2 | `Gamerule <rule> is now set to: <value>` / `Game rules:` |
| `/difficulty [peaceful\|easy\|normal\|hard]` | `minecraft.command.difficulty` | 2 | `The difficulty has been set to <difficulty>` / `The difficulty is <difficulty>` |
| `/tick freeze\|unfreeze\|step [<time>]\|sprint [<ticks>]\|rate <rate>\|query` | `minecraft.command.tick` | 3 | `The game is frozen` / `Stepping the game <n> ticks` / `Sprint completed …` |
| `/locate structure\|biome\|poi <target> [<pos>]` | `minecraft.command.locate` | 2 | `The nearest <target> is at <x>, <y>, <z> (<distance> blocks away)` |
| `/waypoint list\|modify …` (1.21.6+ locator bar) | `minecraft.command.waypoint` | 2 | per subcommand |

`/gamerule` must implement the vanilla rule set the engine honours plus standard ones for
compatibility (`sendCommandFeedback`, `doDaylightCycle`, `keepInventory`, `mobGriefing`,
`randomTickSpeed`, `doWeatherCycle`, `doMobSpawning`, `spawnRadius`, `doFireTick`,
`announceAdvancements`, …). Rules the engine ignores are still stored/echoed; the module docs must
list honoured vs pass-through.

## Files
- new `crates/aether-server/src/commands/world_env.rs`.
- `game/mod.rs`, `config.rs`/world metadata, `game/tick.rs`/scheduler, `aether-worldgen` (locate).

## Edge cases to test explicitly
- `/gamerule sendCommandFeedback` interacts with CMD-05 exactly like vanilla; unknown rule →
  `Unknown game rule`; `list` format.
- `/tick freeze` stops block/entity ticks but not save; `step` advances exactly N; `rate` clamps.
- `/locate` deterministic for the world seed and errors when none is found (`Could not find …`).
- `/difficulty` query form; a value out of range errors.
- `/waypoint` on a codec < 1.21.6 degrades per the release gate.

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
