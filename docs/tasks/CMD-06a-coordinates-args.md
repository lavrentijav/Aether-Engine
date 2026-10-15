# CMD-06a — Argument types + coordinate/rotation parser

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-03`
Parent/umbrella: `docs/tasks/CMD-06-selectors-coordinates.md`

## Goal
The non-entity argument parsers and the coordinate/rotation resolver used by nearly every command.

## Coordinate rules (must match vanilla 1.21.11)
- `~n` relative to the source position (double; `~` alone = `~0`).
- `^l ^r` local coordinates relative to yaw/pitch; only when the whole set is local and the source
  has a rotation; mixing `~`/absolute with `^` is an error.
- absolute integer for block positions, double for exact positions.
- a block position needs all three components; floor after resolving relatives like vanilla.

## Argument types
`block_pos`, `vec3`, `vec2`, `rotation`, `swizzle`, `angle`, `block_state`
(`minecraft:stone[facing=north]`), `item_stack` (`minecraft:stone{…} 3`), `nbt_compound`, `component`,
`resource_location`, `resource_or_tag`, `game_profile`, `message` (greedy, quotes), `time`,
`gamemode`, `difficulty`, `particle`, `score_holder`, `operation`.
(`nbt_path` comes from CMD-14a; the base scalar/bool/int/float/long/string parsers are built in
CMD-03.)

## Files
- new `crates/aether-server/src/commands/coordinates.rs` (+ `args.rs` if large).
- `crates/aether-server/src/commands/tree.rs`: register the argument types.
- reuse `aether_world` block/item registries and the component model.

## Edge cases to test explicitly
- `~0 ~ ~` on a fractional source floors to the right block.
- `^ ^ ^5` from a rotated source resolves to the forward block; `^` mixed with `~` is rejected.
- a block-state string with properties round-trips; an unknown block/property errors.
- item-stack string with components and a count; an invalid component errors.
- message parsing with quotes/escapes and a trailing slash.
- a relative coordinate with no executing entity (console) errors like vanilla.

## Done criteria
- Parser + unit tests; a fuzz-ish property test (random valid strings parse, garbage never panics).
- fmt / clippy `-D warnings` / `cargo-locked test`.
