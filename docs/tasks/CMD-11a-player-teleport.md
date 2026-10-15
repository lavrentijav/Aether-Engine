# CMD-11a — V-player: teleport, spreadplayers, spawnpoint

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-11-player-move.md`

## Goal
Vanilla movement commands and upgrading the existing `/tp` handler to the full vanilla grammar.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/teleport <targets> <location>` | `minecraft.command.teleport` | 2 | `Teleported <target> to <x>, <y>, <z>` |
| `/teleport <targets> <destination>` | `minecraft.command.teleport` | 2 | `Teleported <target> to <entity>` |
| `/teleport <location>` (self) | `minecraft.command.teleport` | 2 | `Teleported <self> to <x>, <y>, <z>` |
| `/spreadplayers <center> <spreadDistance> <maxRange> [under <maxHeight>] <respectTeams> <targets>` | `minecraft.command.spreadplayers` | 2 | `Spread <n> entities around <x>, <y>, <z>` |
| `/spawnpoint [<targets>] [<pos>] [<angle>]` | `minecraft.command.spawnpoint` | 2 | `Set the spawn point for <player> to <x>, <y>, <z>` |

Alias `/tp` → `/teleport` (both vanilla words). Upgrade the existing
`/tp <x> <y> <z>`/`/tp <player>` handler in `game/commands.rs:157` to the full form without changing
the plain-self-tp replies players rely on. `/teleport` accepts a rotation and carries it.

## Files
- `crates/aether-server/src/game/commands.rs` (extend `/tp`).
- new `crates/aether-server/src/game/commands_move.rs`.
- spawn-point storage (grep `spawn` in the player model / `game/commands.rs:149`).

## Edge cases to test explicitly
- `/teleport @a ~ ~5 ~` moves every player; a target with a rotation carries it; cross-world later.
- `/teleport` into unloaded chunks loads them like vanilla (or errors identically if not).
- `/spreadplayers` with respectTeams keeps teams together; with a maxHeight picks a safe Y.
- `/spawnpoint` on a non-respawn-anchor block; query form `/spawnpoint <target>` prints current.
- no target found → `No entity was found` (not a silent success).
- existing `/tp x y z` and `/tp <player>` replies are byte-identical after the upgrade.

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
