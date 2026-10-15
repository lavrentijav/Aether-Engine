# CMD-20a — V-data: /loot

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-20-loot-recipe.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/loot give <players> <loot>` | `minecraft.command.loot` | 2 | `Gave <n> items to <player>` |
| `/loot insert <targetPos> <loot>` | `minecraft.command.loot` | 2 | `Inserted <n> items` |
| `/loot spawn <targetPos> <loot>` | `minecraft.command.loot` | 2 | `Spawned <n> items` |
| `/loot replace entity\|block <target> <slot> <loot>` | `minecraft.command.loot` | 2 | `Replaced <n> items …` |

`<loot>` forms: `loot <path>`, `kill <entity>`, `mine <pos> [<tool>]`, `fish <pos> [<tool>]`,
`block <pos> [<tool>]`, each optionally `… with <item>` / `… with <loot>` context.

## Files
- new `crates/aether-server/src/commands/loot.rs`.
- data pack loot tables (CMD-21a), world block-entity inventory, provenance for minted items.

## Edge cases to test explicitly
- `/loot give @a loot …` applies per player; `/loot replace entity <target> <slot>` writes a slot.
- an empty result reports 0; an unknown loot table errors; `mine` with/without a tool.
- `with` context and a nested loot table resolve; a `kill` loot from a mob with no drops is 0.

## Done criteria
- Implementation + deterministic-seed test; fmt / clippy `-D warnings` / `cargo-locked test`.
