# CMD-12a — V-world: setblock, fill, clone

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-12-block-ops.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/setblock <pos> <block> [destroy\|keep\|replace]` | `minecraft.command.setblock` | 2 | `Successfully set block at <x>, <y>, <z> to <block>` |
| `/fill <from> <to> <block> [destroy\|hollow\|keep\|outline\|replace]` | `minecraft.command.fill` | 2 | `Successfully filled <n> blocks` |
| `/fill <from> <to> replace <filter> <block>` | `minecraft.command.fill` | 2 | `Successfully filled <n> blocks` |
| `/clone <begin> <end> <destination> [replace\|masked] [force\|normal] [filtered <filter>]` | `minecraft.command.clone` | 2 | `Successfully cloned <n> blocks` |
| `/clone … from <dim> … to <dim> …` (cross-dimension) | `minecraft.command.clone` | 2 | same |

Vanilla caps `/fill`/`/clone` at **32768** blocks (`Too many blocks in the specified area`); all
writes go through the normal placement path (journal, neighbour updates, light, chunk resend).
Use the bulk-edit core API (CMD-30) once merged; until then batch locally but never bypass the
journal.

## Files
- new `crates/aether-server/src/commands/world_edit.rs`.
- `placement.rs`, `game/provenance.rs`, `aether_world` block-state parsing from CMD-06b.

## Edge cases to test explicitly
- `/setblock keep` on an occupied cell → `That block is already <block>`, no change/journal.
- `/setblock destroy` on a non-air block drops its items.
- `/fill hollow` vs `outline` vs `replace` filter; 32769 blocks refused; from/to order irrelevant.
- `/clone masked` copies only non-air; `filtered` with a filter; `force` overwrites; normal into a
  non-air destination is refused.
- permission level 2 only.

## Done criteria
- Implementation + tests above; blocks visible to clients and journaled with actor `player:<uuid>`.
- fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
