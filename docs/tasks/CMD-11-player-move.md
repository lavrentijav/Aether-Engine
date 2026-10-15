# CMD-11 — V-player: teleport, spawn, ride, item (umbrella)

Status: 📋 umbrella brief (CMD0). Split into **CMD-11a** and **CMD-11b** (each about one session).
core: yes. Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`.

## Goal
Vanilla movement/positioning and inventory-item commands, plus bringing the existing `/tp` and
`/kill` up to vanilla selector semantics. Safe landing search is a core API (CMD-31); this group
uses it, it does not implement it.

## Split
| id | brief | contents | size |
|---|---|---|---|
| CMD-11a | `docs/tasks/CMD-11a-player-teleport.md` | `/teleport` `/tp` `/spreadplayers` `/spawnpoint` + upgrade existing `/tp` | M |
| CMD-11b | `docs/tasks/CMD-11b-player-misc.md` | `/spectate` `/ride` `/item` `/kill` `/rotate` | M |

## `/rotate` — vanilla since 1.21.2
`/rotate` **is** a vanilla command (added in 1.21.2 / protocol 768) and must match vanilla, not an
Aether extension:
```
/rotate <target> <rotation>                          # yaw pitch (or a single horizontal) applied to the entity
/rotate <target> facing <facingLocation>             # face a block position
/rotate <target> facing entity <facingEntity> [<facingAnchor>]   # face an entity (eyes|feet)
```
Permission `minecraft.command.rotate`, op level 2. Rotates the entity's yaw/pitch (works for any
entity; `<rotation>` accepts relative `~`/local `^` like vanilla's `rotation` argument), sends the
rotation to clients, and updates the entity's server-side facing. Record that it is 1.21.2+ only:
on older codecs there is no client packet for arbitrary entity rotation outside movement, so gate it
by release like the other post-1.21.2 features.

## Files (whole group)
- `crates/aether-server/src/game/commands.rs` (upgrade `/tp`, `/kill`).
- new `crates/aether-server/src/game/commands_move.rs`.
- `game/entities.rs` (ride/vehicle attachment, rotation), `game/interact.rs` (spectate), spawn-point
  storage.

## Done criteria
- Per the sub-briefs; fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
