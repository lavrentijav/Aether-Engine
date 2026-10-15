# CMD-20b — V-data: recipe, advancement

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-20-loot-recipe.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/recipe give <targets> <recipe>` / `take <targets> <recipe>` / `<targets> *` | `minecraft.command.recipe` | 2 | `Unlocked <n> recipe(s) for <player>` / `Locked <n> recipe(s) …` |
| `/advancement grant <targets> only\|through\|from\|until <advancement> [<criterion>]` | `minecraft.command.advancement` | 2 | `Granted advancement <name> to <player>` |
| `/advancement revoke <targets> only\|through\|from\|until <advancement> [<criterion>]` | `minecraft.command.advancement` | 2 | `Revoked advancement <name> from <player>` |

## Files
- new `crates/aether-server/src/commands/recipe.rs`, `advancement.rs`.
- data pack recipe/advancement registries (CMD-21a); player recipe book + advancement state.

## Edge cases to test explicitly
- `/recipe give @a *` unlocks all; `take` locks; unknown recipe errors.
- `/advancement grant <target> only <adv>` with and without a criterion; `through` grants ancestors;
  `from`/`until` boundaries; revoking the root revokes descendants per vanilla.
- state persists across restart; an advancement already granted reports no change.

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test`.
