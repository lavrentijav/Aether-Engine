# CMD-10b — V-player: experience, attribute, damage

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-10-player-effects.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/experience add <targets> <amount> [points\|levels]` | `minecraft.command.experience` | 2 | `Gave <amount> experience to <player>` |
| `/experience set <targets> <amount> [points\|levels]` | `minecraft.command.experience` | 2 | `Set <player>'s experience to <amount>` |
| `/experience query <targets> <points\|levels>` | `minecraft.command.experience` | 2 | `<player> has <amount> experience` |
| `/attribute <target> <attribute> get [<scale>]` | `minecraft.command.attribute` | 2 | `<attribute> of <target> is <value>` |
| `/attribute <target> <attribute> base set <value>` / `base get [scale]` | `minecraft.command.attribute` | 2 | `Base <attribute> of <target> set to <value>` |
| `/attribute <target> <attribute> modifier add <id> <value> add\|multiply\|multiply_base` | `minecraft.command.attribute` | 2 | `Added modifier <id> to …` |
| `/damage <target> <amount> [<type>] [at <loc>\|by <entity> [from <cause>]]` | `minecraft.command.damage` | 2 | `<target> was damaged` |

Alias `/experience` ↔ `/xp`.

## Files
- new `crates/aether-server/src/game/commands_player.rs` (or split from CMD-10a).
- attribute storage (grep "attribute"); if the player model has no attributes, add the minimum to
  satisfy `/attribute`. `game/combat.rs` for `/damage`.

## Edge cases to test explicitly
- `/experience add levels` at a very large amount clamps to the vanilla cap; `query` reports.
- `/attribute` unknown attribute/name errors; `modifier add` with a duplicate id replaces per
  vanilla rules; `base set` beyond range clamps.
- `/damage` with a damage type, with `by <entity>`, and with `at <location>`; a killing blow runs
  the normal death path **once** (no double death/drops).

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
