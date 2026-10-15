# CMD-14 — V-data: `/data` (NBT read/modify)

Status: 📋 task brief (CMD0) · size: L · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`

## Split (CMD0 review)
This L task is split into:
| id | brief | contents | size |
|---|---|---|---|
| CMD-14a | `docs/tasks/CMD-14a-data-storage-nbtpath.md` | NBT-path parser + `/data … storage` (get/merge/modify/remove) | M |
| CMD-14b | `docs/tasks/CMD-14b-data-block-entity.md` | `/data … block` and `/data … entity` + persistence | M |

## Goal
Vanilla `/data` over block entities, entities and command storage. This is the command surface for
NBT storage; if the engine lacks entity/block-entity NBT or command storage, add the minimum here
(command storage `minecraft:storage` in the world KV at least) and note the gaps.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/data get block <pos> [<path>] [<scale>]` | `minecraft.command.data` | 2 | `Block at <x>, <y>, <z> has the following block data: <nbt>` |
| `/data get entity <target> [<path>] [<scale>]` | `minecraft.command.data` | 2 | `<entity> has the following entity data: <nbt>` |
| `/data get storage <id> [<path>] [<scale>]` | `minecraft.command.data` | 2 | `Storage <id> has the following contents: <nbt>` |
| `/data merge block\|entity\|storage <target> <nbt>` | `minecraft.command.data` | 2 | `Merged block data ...` / `Modified entity data ...` |
| `/data modify <target> <path> set\|merge\|append\|prepend\|insert <...>` | `minecraft.command.data` | 2 | `Modified block data ...` |
| `/data remove <target> <path>` | `minecraft.command.data` | 2 | `Removed element ...` |

`<target>` forms: `block <pos>`, `entity <targets>` (exactly one required), `storage <resource id>`.
NBT path syntax (`foo.bar[0].baz`, `{key:value}` filters, `[]` all-elements) comes from CMD-06b.

## Files
- new `crates/aether-server/src/commands/data.rs`.
- `aether-world` block-entity model, entity NBT (if any), a new command-storage map keyed
  `minecraft:storage` persisted in the world KV (register the prefix, T5).

## Edge cases to test explicitly
- `get` with no path returns the whole tag; with a path the raw value and type.
- entity selector resolving to 0 or >1 entities errors (`No entity was found` /
  `Only one entity is allowed`).
- `modify set` creates intermediate tags per vanilla rules; `insert` at an out-of-range index.
- `merge` only overwrites top-level keys, not nested.
- `remove` of a missing path errors (`Nothing found to remove`).
- storage persists across restart and is namespaced.
- a block with no block entity errors (`The target block is not a block entity`).
- reading an unloaded position errors (`That position is not loaded`).

## Done criteria
- Implementations + unit tests for NBT-path operations and the selector constraints.
- fmt / clippy `-D warnings` / `cargo-locked test`.
