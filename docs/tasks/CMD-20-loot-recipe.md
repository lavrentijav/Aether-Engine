# CMD-20 — V-data: loot, recipes, advancements, random, schedule

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`

## Split (CMD0 review)
This L group is split into:
| id | brief | contents | size |
|---|---|---|---|
| CMD-20a | `docs/tasks/CMD-20a-loot.md` | `/loot` | M |
| CMD-20b | `docs/tasks/CMD-20b-recipe-advancement.md` | `/recipe` `/advancement` | M |
| CMD-20c | `docs/tasks/CMD-20c-random-schedule.md` | `/random` `/schedule` | S |

## Goal
Data-pack-driven commands: loot tables, recipe unlocking, advancements, random sequences and
scheduled functions.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/loot give <players> <loot>` | `minecraft.command.loot` | 2 | `Gave <n> items to <player>` |
| `/loot insert\|spawn\|replace <...>` | `minecraft.command.loot` | 2 | `Spawned <n> items` / `Replaced slot ...` |
| `/recipe give <targets> <recipe>` / `take` / `*` | `minecraft.command.recipe` | 2 | `Unlocked <n> recipe(s) for <player>` |
| `/advancement grant\|revoke <targets> only\|through\|from\|until <advancement> [<criterion>]` | `minecraft.command.advancement` | 2 | `Granted advancement <name> to <player>` |
| `/random value <range> [<sequence>]` | `minecraft.command.random` | 2 | `<sequence> returned <n>` / `Random value <n>` |
| `/random roll <range> [<sequence>]` | `minecraft.command.random` | 2 | `<sequence> rolled <n>` |
| `/schedule function <function> <time> [append\|replace]` | `minecraft.command.schedule` | 2 | `Scheduled function <name> in <time>` |
| `/schedule clear <function>` | `minecraft.command.schedule` | 2 | `Cleared function <name>` |

`random` `<range>` is `min..max` (inclusive) with integer bounds; sequences come from the data pack
(`worldgen/random_sequence`). `schedule <time>` is a vanilla duration (`1d`, `2t`, …). Functions
come from CMD-21.

## Files
- new `crates/aether-server/src/commands/loot.rs`, `recipe.rs`, `advancement.rs`, `random.rs`,
  `schedule.rs` (or one module).
- data pack loader (`datapack`) for loot tables/functions/advancements; the game scheduler
  (`game/tickstore.rs`) for `/schedule`.
- existing inventory/provenance so loot spawns are journaled as derived/minted like other grants.

## Edge cases to test explicitly
- `/loot give @a loot ...` applies per player; `/loot replace entity <target> <slot>` writes a slot.
- a loot table name as `loot <path>`, `kill <entity>`, `mine <pos> [<tool>]`, `fish`, `block <pos>`
  and the `... with <item>`/`... with <loot>` context; an empty result reports 0.
- `/recipe give @a *` unlocks all; `take` removes; unknown recipe errors.
- `/advancement grant <target> only <adv>` with and without a criterion; `through` grants ancestors.
- `/random value 1..10` stays in range; `roll` respects the sequence's bias/table.
- `/schedule` with a negative/past time fires next tick; `append` keeps duplicates, `replace`
  replaces; the scheduled function survives a restart (durable scheduler).
- `/schedule clear` with no such entry errors.

## Done criteria
- Implementations + tests; a deterministic-seed test for `/random` and `/loot`.
- fmt / clippy `-D warnings` / `cargo-locked test`.
