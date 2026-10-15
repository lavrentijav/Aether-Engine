# CMD-06 — Selector and coordinate parser

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-03`

## Goal
One shared parser for entity selectors (`@a @p @r @s @e[...] @n`), coordinates (`~` relative, `^`
local, absolute), rotations, and the vanilla argument types that carry them (block position, vec3,
vec2, entity, entities, NBT path, item/block predicates). Every command brief from CMD-08 on uses
this; do not re-implement selectors per command.

## Selector syntax (must match vanilla 1.21.11)
`@a`, `@e`, `@p`, `@r`, `@s`, `@n` (nearest), plus `[key=value,...]`:
`type`, `name`, `distance` / `dx dy dz`, `x y z`, `scores`, `tag`, `team`, `limit`, `sort`
(`nearest|furthest|random|arbitrary`), `level`, `gamemode`, `advancements`, `predicate`, `nbt`,
`x_rotation`, `y_rotation`. Also `@e[type=!zombie]` negation. `@a` defaults to limit 0 (=all);
`@p/@r/@n` default limit 1; `@e` default 0. Selecting 0 entities is an error
("No entity was found"); more than the client/server cap is an error ("Too many entities …").
`@s` with no executing entity is an error ("No entity was found").

## Coordinates
- `~n` relative to the source position (parsed as double; `~` alone = `~0`).
- `^l ^r` local coordinates relative to yaw/pitch, only allowed where the whole set is local and
  the source has a rotation; mixing `~`/absolute with `^` is an error.
- absolute integer for block positions, double for exact positions.
- block position requires all three components; floor after resolving relatives like vanilla.

## Rotation / NBT-path / predicate args
- rotation: yaw/pitch (either `yaw pitch` or a single horizontal).
- block state string (`minecraft:stone[facing=north]`), item stack string
  (`minecraft:stone{...} 3`), NBT path, SNBT compound, message (greedy, quotes).
- Reuse `aether_world`'s block/item registries and component model; no string lookups by name on
  the hot path beyond parse time.

## Files
- new `crates/aether-server/src/commands/selector.rs` (+ `coordinates.rs`, `nbt_path.rs` if large).
- `crates/aether-server/src/commands/tree.rs`: register the argument types from CMD-03.
- read-only access to `players::SharedRegistry` and world/entity list via a `CommandSource` trait.

## Edge cases to test explicitly
- `~0 ~ ~` on a fractional source floors to the right block.
- `^ ^ ^5` from a rotated source resolves to the forward block; `^` mixed with `~` is rejected.
- `@a` on a full server caps/errs per vanilla; `@e[limit=1,sort=random]` is deterministic under a
  seeded RNG in tests.
- `@e[type=!player]` excludes players; `@e[distance=..5]` range syntax (`..`, `n..`, `..n`, `n..m`).
- `@p` with no players online is an error, not an empty result.
- an unknown selector key is an error naming the key; a bad value names the value.
- `@s` from console (no entity) is an error.
- selector parse must not hold the entity lock while parsing (lock order, see
  `docs/storage-v2/lock-order.md`).

## Done criteria
- Parser + unit tests for all selectors/coordinate forms above.
- A fuzz-ish property test: random valid selector strings round-trip; random garbage never panics.
- fmt / clippy `-D warnings` / `cargo-locked test`.
