# CMD-11b — V-player: spectate, ride, item, kill, rotate

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-11-player-move.md`

## Goal
Entity control and inventory-item commands, plus `/rotate` (vanilla since 1.21.2) and upgrading
the existing `/kill`.

## Commands (vanilla 1.21.11, except `/rotate` 1.21.2+)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/spectate [<target>] [<player>]` | `minecraft.command.spectate` | 2 | `Now spectating <entity>` / `Stopped spectating` |
| `/ride <target> <vehicle>` / `/ride <target>` | `minecraft.command.ride` | 2 | `Made <target> ride <vehicle>` / `Made <target> stop riding` |
| `/item replace <targets> <slot> with <item> [<count>]` | `minecraft.command.item` | 2 | `Replaced <slot> of <player> with <item> [x<count>]` |
| `/item modify <targets> <slot> <modifier>` | `minecraft.command.item` | 2 | `Modified <slot> of <player>` |
| `/kill [<targets>]` | `minecraft.command.kill` | 2 | `Killed <target>` |
| `/rotate <target> <rotation>` | `minecraft.command.rotate` | 2 | `Rotated <entity>` |
| `/rotate <target> facing <facingLocation>` | `minecraft.command.rotate` | 2 | `Rotated <entity>` |
| `/rotate <target> facing entity <facingEntity> [<facingAnchor>]` | `minecraft.command.rotate` | 2 | `Rotated <entity>` |

`/rotate` is a real vanilla command since **1.21.2** (used for display entities, armor stands,
any entity with a rotation). It applies yaw/pitch (relative `~` / local `^` allowed like the
vanilla `rotation` argument) or faces a location/entity (`eyes|feet`), updates the server-side
facing and sends the new rotation to clients. On codecs older than 1.21.2 there is no packet for
arbitrary entity rotation outside movement, so gate it by release.
Upgrade `/kill` (`game/commands.rs:135`): no target kills self (existing), with targets kills them.

## Files
- new `crates/aether-server/src/game/commands_move.rs`.
- `game/entities.rs` (ride/vehicle attachment, rotation), `game/interact.rs` (set game mode for
  spectate), combat/death path for `/kill`.

## Edge cases to test explicitly
- `/spectate` a player then `/spectate` again stops; spectating a mob that dies stops cleanly.
- `/ride`: riding a player is refused; a vehicle with a passenger is rejected; dismount frees the
  entity.
- `/item replace` into an armor/offhand slot; with a count > max stack; `with` an empty item clears.
- `/kill` with a selector kills several and journals/derives drops exactly as combat does.
- `/rotate <e> ~90 ~` adds to the current yaw; `facing 0 0 0` looks at the origin;
  `facing entity @e[limit=1] eyes`; a codec < 1.21.2 rejects/ignores per the release gate.
- no target found → `No entity was found` (not a silent success).

## Done criteria
- Implementation + tests above; rotation metadata sent to clients on 1.21.2+.
- fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
