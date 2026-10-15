# CMD-13 — V-world: environment & rules

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`

## Goal
Vanilla world/environment commands: time, weather, world border, world spawn, gamerules,
difficulty, tick control, locate, default game mode.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/time add\|set\|query <day\|noon\|night\|midnight\|<time>>` | `minecraft.command.time` | 2 | `Set the time to <n>` / `The time is <n>` |
| `/weather clear\|rain\|thunder [<duration>]` | `minecraft.command.weather` | 2 | `Set the weather to clear` / `... to rain` / `... to thunder` |
| `/worldborder add\|set\|center\|damage amount\|damage buffer\|get\|warning distance\|warning time ...` | `minecraft.command.worldborder` | 2 | `Set the world border to <n> blocks wide` / `Set the world border center to ...` |
| `/setworldspawn [<pos>] [<angle>]` | `minecraft.command.setworldspawn` | 2 | `Set the world spawn point to <x>, <y>, <z>` |
| `/gamerule <rule> [<value>]` / `/gamerule list` | `minecraft.command.gamerule` | 2 | `Gamerule <rule> is now set to: <value>` / `Game rules:` |
| `/difficulty [peaceful\|easy\|normal\|hard]` | `minecraft.command.difficulty` | 2 | `The difficulty has been set to <difficulty>` / `The difficulty is <difficulty>` |
| `/tick freeze\|unfreeze\|step [<time>]\|sprint [<ticks>]\|rate <rate>\|query` | `minecraft.command.tick` | 3 | `The game is frozen` / `Stepping the game <n> ticks` / `Sprint completed with ...` |
| `/locate structure\|biome\|poi <target> [<pos>]` | `minecraft.command.locate` | 2 | `The nearest <target> is at <x>, <y>, <z> (<distance> blocks away)` |
| `/defaultgamemode <gamemode>` | `minecraft.command.defaultgamemode` | 2 | `The default game mode is now <mode>` |
| `/waypoint list\|modify ...` (1.21.6+ locator bar) | `minecraft.command.waypoint` | 2 | per subcommand |

Existing `/time` (`game/commands.rs:53`) already supports `set <day|noon|night|midnight|ticks>`;
port it onto the tree with the full `add/set/query` grammar and keep the old replies for the forms
players use. `time:9999d`-style magic is **not** vanilla; drop only if it is unused (grep).

`/gamerule` must implement the vanilla rule set that the engine actually honours plus the
standard ones for compatibility (`sendCommandFeedback`, `doDaylightCycle`, `keepInventory`,
`mobGriefing`, `randomTickSpeed`, `doWeatherCycle`, `doMobSpawning`, `spawnRadius`, `doFireTick`,
`announceAdvancements`, …). A rule the engine ignores is still stored/echoed so a data pack that
reads it sees something; the brief must list which are honoured vs pass-through.

## Files
- `crates/aether-server/src/game/commands.rs` (port `/time`).
- new `crates/aether-server/src/commands/world_env.rs`.
- `game/mod.rs` (time/weather state), `aether-worldgen` (locate structure/biome),
  `config.rs`/world metadata (world spawn, world border), `game/tick.rs`/scheduler (tick control).

## Edge cases to test explicitly
- `/time set` then `/time query daytime` returns the added value; overflow wraps like vanilla.
- `/time add` with a negative amount; `/time set day` = 1000, noon 6000, night 13000, midnight 18000.
- weather duration default when omitted; rain then thunder transition; `/weather clear` stops rain.
- world border grows/shrinks over time, damages players outside with the buffer/damage settings.
- `/gamerule sendCommandFeedback` interacts with CMD-05 exactly like vanilla.
- `/gamerule` unknown rule → `Unknown game rule`; list format.
- `/tick freeze` stops block/entity ticks but not the save; `step` advances exactly N; `rate`
  clamps 1..10000.
- `/locate` deterministic for the world seed and errors when none is found
  (`Could not find <target>`).
- `/defaultgamemode` affects only new joins.

## Done criteria
- Implementations + tests for each edge case; the honoured/pass-through gamerule table is written
  down in the module docs.
- fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
