# CMD-14a — V-data: NBT-path parser + /data storage

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-14-data.md`

## Goal
The NBT-path parser (shared with `/data`, `/execute if data`, `store … data`) and the
`minecraft:storage` command storage, plus `/data` for the `storage` target.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/data get storage <id> [<path>] [<scale>]` | `minecraft.command.data` | 2 | `Storage <id> has the following contents: <nbt>` |
| `/data merge storage <id> <nbt>` | `minecraft.command.data` | 2 | `Merged storage data …` |
| `/data modify storage <id> <path> set\|merge\|append\|prepend\|insert <…>` | `minecraft.command.data` | 2 | `Modified storage data …` |
| `/data remove storage <id> <path>` | `minecraft.command.data` | 2 | `Removed element …` |

NBT path syntax (`foo.bar[0].baz`, `{key:value}` filters, `[]` all-elements) is provided here for
CMD-14b and CMD-22.

## Files
- new `crates/aether-server/src/commands/data.rs` + `commands/nbt_path.rs`.
- command storage persisted in the world KV under `minecraft:storage` (register the prefix, T5).

## Edge cases to test explicitly
- `get` with no path returns the whole tag; a path returns the raw value and type.
- `modify set` creates intermediate tags per vanilla; `insert` at an out-of-range index errors.
- `merge` only overwrites top-level keys, not nested.
- `remove` of a missing path errors (`Nothing found to remove`).
- storage persists across restart and is namespaced; unknown path syntax errors naming the path.

## Done criteria
- Parser + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
