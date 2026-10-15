# CMD-13a — V-world: time, weather, worldborder, spawn, default gamemode

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-13-world-ops.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/time add\|set\|query <day\|noon\|night\|midnight\|<time>>` | `minecraft.command.time` | 2 | `Set the time to <n>` / `The time is <n>` |
| `/weather clear\|rain\|thunder [<duration>]` | `minecraft.command.weather` | 2 | `Set the weather to clear/rain/thunder` |
| `/worldborder add\|set\|center\|damage amount\|damage buffer\|get\|warning distance\|warning time …` | `minecraft.command.worldborder` | 2 | `Set the world border to <n> blocks wide` / `… center …` |
| `/setworldspawn [<pos>] [<angle>]` | `minecraft.command.setworldspawn` | 2 | `Set the world spawn point to <x>, <y>, <z>` |
| `/defaultgamemode <gamemode>` | `minecraft.command.defaultgamemode` | 2 | `The default game mode is now <mode>` |

Port the existing `/time` (`game/commands.rs:53`) onto the tree with the full `add/set/query`
grammar, keeping the old replies for the forms players use.

## Files
- `crates/aether-server/src/game/commands.rs` (port `/time`).
- new `crates/aether-server/src/commands/world_env.rs`.
- world metadata (spawn, border), `game/mod.rs` (time/weather state).

## Edge cases to test explicitly
- `/time set` then `/time query daytime`; overflow wraps like vanilla; `/time add` negative.
- `/time set day` = 1000, noon 6000, night 13000, midnight 18000.
- weather duration default when omitted; rain→thunder; `/weather clear` stops rain.
- world border grows/shrinks over time; outside players take configured damage; buffer/warning.
- `/defaultgamemode` affects only new joins.

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
