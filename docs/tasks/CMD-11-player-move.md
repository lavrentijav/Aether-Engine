# CMD-11 — V-player: teleport, spawn, ride, item

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`

## Goal
Vanilla movement/positioning and inventory-item commands, plus bringing the existing `/tp` and
`/kill` up to vanilla selector semantics. Safe landing search is a core API (CMD-31); this task
uses it, it does not implement it.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/teleport <targets> <location>` | `minecraft.command.teleport` | 2 | `Teleported <target> to <x>, <y>, <z>` |
| `/teleport <targets> <destination>` | `minecraft.command.teleport` | 2 | `Teleported <target> to <entity>` |
| `/teleport <location>` (self) | `minecraft.command.teleport` | 2 | `Teleported <self> to <x>, <y>, <z>` |
| `/spreadplayers <center> <spreadDistance> <maxRange> [under <maxHeight>] <respectTeams> <targets>` | `minecraft.command.spreadplayers` | 2 | `Spread <n> entities around <x>, <y>, <z>` |
| `/spawnpoint [<targets>] [<pos>] [<angle>]` | `minecraft.command.spawnpoint` | 2 | `Set the spawn point for <player> to <x>, <y>, <z>` |
| `/spectate [<target>] [<player>]` | `minecraft.command.spectate` | 2 | `Now spectating <entity>` / `Stopped spectating` |
| `/ride <target> <vehicle>` / `/ride <target>` | `minecraft.command.ride` | 2 | `Made <target> ride <vehicle>` / `Made <target> stop riding` |
| `/item replace <targets> <slot> with <item> [<count>]` | `minecraft.command.item` | 2 | `Replaced <slot> of <player> with <item> [x<count>]` |
| `/item modify <targets> <slot> <modifier>` | `minecraft.command.item` | 2 | `Modified <slot> of <player>` |
| `/kill [<targets>]` | `minecraft.command.kill` | 2 | `Killed <target>` |

Alias: `/tp` → teleport, `/teleport` canonical. Vanilla `/tp` (1.13+ alias) and `/teleport` are the
same command; keep both words. Upgrade the existing `/tp <x> <y> <z>`/`/tp <player>` handler in
`game/commands.rs:157` to the full form without changing the plain-self-tp replies players rely on.
Upgrade `/kill` (`game/commands.rs:135`) so with no target it kills self (existing behaviour) and
with targets it kills them; the feedback for self-kill may stay empty.

⚠ **`/rotate` is in the catalogue but is not a vanilla 1.21.11 command.** There is no vanilla
`/rotate`. Implement it only if it is decided to be a core extension; otherwise drop it. This brief
flags it rather than silently adding a non-vanilla command (see the review report).

## Files
- `crates/aether-server/src/game/commands.rs` (extend `/tp`, `/kill`).
- new `crates/aether-server/src/game/commands_move.rs` for the rest.
- `game/entities.rs` (ride/vehicle attachment), `game/interact.rs` (set game mode for spectate),
  spawn-point storage (grep `spawn` in the player model / `game/commands.rs:149`).

## Edge cases to test explicitly
- `/teleport @a ~ ~5 ~` moves every player; a target with a rotation carries it; cross-world later.
- `/teleport` into unloaded chunks loads them like vanilla (or errors identically if not).
- `/spreadplayers` with respectTeams keeps teams together; with a maxHeight picks a safe Y.
- `/spawnpoint` on a non-respawn-anchor block; query form `/spawnpoint <target>` prints current.
- `/spectate` a player then `/spectate` again stops; spectating a mob that dies stops cleanly.
- `/ride`: riding a player is refused; a vehicle with a passenger is rejected; dismount frees the
  entity.
- `/item replace` into an armor/offhand slot; with a count > max stack; `with` an empty item clears.
- `/kill` with a selector kills several and journals/derives drops exactly as combat does.
- no target found → `No entity was found` (not a silent success).

## Done criteria
- Implementations + tests for the edge cases; riding metadata sent to clients on 1.21.11.
- fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
