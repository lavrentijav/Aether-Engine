# CMD-14b — V-data: /data block and entity targets

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-14a`
Parent/umbrella: `docs/tasks/CMD-14-data.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/data get block <pos> [<path>] [<scale>]` | `minecraft.command.data` | 2 | `Block at <x>, <y>, <z> has the following block data: <nbt>` |
| `/data get entity <target> [<path>] [<scale>]` | `minecraft.command.data` | 2 | `<entity> has the following entity data: <nbt>` |
| `/data merge block\|entity <target> <nbt>` | `minecraft.command.data` | 2 | `Merged block data …` / `Modified entity data …` |
| `/data modify block\|entity <target> <path> …` | `minecraft.command.data` | 2 | `Modified block/entity data …` |
| `/data remove block\|entity <target> <path>` | `minecraft.command.data` | 2 | `Removed element …` |

Uses the NBT-path parser and the `merge/modify/remove` semantics from CMD-14a.

## Files
- `crates/aether-server/src/commands/data.rs`.
- `aether-world` block-entity model, entity NBT storage (if any).

## Edge cases to test explicitly
- entity selector resolving to 0 (→ `No entity was found`) or >1 (→ `Only one entity is allowed`).
- a block with no block entity errors (`The target block is not a block entity`).
- reading an unloaded position errors (`That position is not loaded`).
- `merge` into a block entity that cannot be saved (e.g. a container mid-tick) is deferred, not torn.

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test`.
