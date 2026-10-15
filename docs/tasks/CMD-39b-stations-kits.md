# CMD-39b — Plugin: essentials-inv — stations & kits

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/essentials-inv`
Needs: `oc/P1`, `oc/CMD-32b`, `oc/CMD-39a`
Parent/umbrella: `docs/tasks/CMD-39-essentials-inv.md`

## Commands (EssentialsX reference)
| command | syntax | node |
|---|---|---|
| workbench | `/workbench [<player>]` (`/craft`) | `essentials.workbench` |
| anvil | `/anvil` | `essentials.anvil` |
| stonecutter | `/stonecutter` | `essentials.stonecutter` |
| grindstone | `/grindstone` | `essentials.grindstone` |
| trash | `/trash` (`/disposal`) | `essentials.trash` |
| kits | `/kit [<kit>] [<player>]`, `/kits` | `essentials.kit`, `essentials.kits` |
| create kit | `/createkit <name> [<cooldown>]` | `essentials.createkit` |

Kits are config (items, enchants, cooldown, `essentials.kits.<name>`); `/createkit` captures the
player's inventory.

## Files
- `plugins/essentials-inv/`: manifest, `src/`, README, `kits.yml` example.
- window API (CMD-32b); item serialization for kit files.

## Edge cases to test explicitly
- `/kit` before cooldown expires is refused with the remaining time; a permission-locked kit.
- `/createkit` with an empty inventory; a kit with an item the version lacks (compat stand-in).
- a target with a full inventory on `/kit` (EssX drops or refuses); `/trash` destroys + journals.
- kit cooldown persists across restart (per player + kit).

## Done criteria
- Plugin + kit config + persistence + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
