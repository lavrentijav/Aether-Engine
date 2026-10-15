# CMD-06b — Entity selector parser + selection semantics

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-03`, `oc/CMD-06a`
Parent/umbrella: `docs/tasks/CMD-06-selectors-coordinates.md`

## Goal
The `@…` selector parser and the selection rules every command uses.

## Selector syntax (must match vanilla 1.21.11)
`@a`, `@e`, `@p`, `@r`, `@s`, `@n`, plus `[key=value,…]`: `type` (with `!` negation), `name`,
`distance` / `dx dy dz`, `x y z`, `scores`, `tag`, `team`, `limit`, `sort`
(`nearest|furthest|random|arbitrary`), `level`, `gamemode`, `advancements`, `predicate`, `nbt`,
`x_rotation`, `y_rotation`.
Defaults: `@a` limit 0 (=all); `@p/@r/@n` limit 1; `@e` limit 0. Selecting 0 entities is an error
("No entity was found"); over the cap is an error ("Too many entities …"). `@s` with no executing
entity is an error. `distance`/`level`/`x_rotation`/`y_rotation` support range syntax
(`..`, `n..`, `..n`, `n..m`).

## Files
- new `crates/aether-server/src/commands/selector.rs`.
- read-only access to `players::SharedRegistry` and the world entity list via a `CommandSource` trait.

## Edge cases to test explicitly
- `@a` on a full server caps/errs per vanilla; `@e[limit=1,sort=random]` deterministic under a
  seeded RNG in tests.
- `@e[type=!player]` excludes players; `@e[distance=..5]` ranges.
- `@p` with no players online is an error, not an empty result.
- an unknown selector key errors naming the key; a bad value names the value.
- `@s` from console is an error.
- selection must not hold the entity lock while parsing (lock order,
  `docs/storage-v2/lock-order.md`).

## Done criteria
- Parser + selection + unit tests for the forms above; fuzz-ish property test.
- fmt / clippy `-D warnings` / `cargo-locked test`.
