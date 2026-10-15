# CMD-39 — Plugin: essentials-inv

Status: 📋 task brief (CMD0) · size: L · core: no · plugin: `plugins/essentials-inv`
Needs: `oc/P1`, `oc/CMD-32`

## Goal
Inventory, ender-chest and crafting-station access, kits and a trash can — the EssentialsX
inventory family, on the window/player API (CMD-32, PA1).

## Commands (EssentialsX reference)
| command | syntax | node |
|---|---|---|
| invsee | `/invsee <player>` | `essentials.invsee` |
| enderchest | `/enderchest [<player>]` (`/ec`) | `essentials.enderchest` |
| workbench | `/workbench [<player>]` (`/craft`) | `essentials.workbench` |
| anvil | `/anvil` | `essentials.anvil` |
| stonecutter | `/stonecutter` | `essentials.stonecutter` |
| grindstone | `/grindstone` | `essentials.grindstone` |
| trash | `/trash` (`/disposal`) | `essentials.trash` |
| clear inv | `/clearinventory [<player>] [<item>]` (`/ci`) | `essentials.clearinventory` |
| kits | `/kit [<kit>] [<player>]`, `/kits` | `essentials.kit`, `essentials.kits` |
| create kit | `/createkit <name> [<cooldown>]` | `essentials.createkit` |

`/invsee` opens a live window over the target's inventory (CMD-32) — edits affect the owner. Craft
station commands may be client-side commands (`/craft` opens the client's crafting screen) where a
codec supports it, else a server window. Kits are YAML/JSON config with items, enchants, cooldown,
permission (`essentials.kits.<name>`); `/createkit` captures the player's inventory.

## Files
- `plugins/essentials-inv/`: manifest, `src/`, README, `kits.yml` example.
- window API (CMD-32), container/inventory model, item serialization for kit files.

## Edge cases to test explicitly
- `/invsee` another player: shift-click/drag validated; edits persist; closing is safe.
- `/ec` of an offline player is refused (no live ender chest to open).
- `/kit` before the cooldown expires is refused with the remaining time; a permission-locked kit.
- `/createkit` with an empty inventory; a kit with an item the version lacks (compat stand-in).
- `/ci` with an item filter; a target with a full inventory on `/kit` (EssX drops or refuses).
- `/trash` items are destroyed (journaled as destruction, no duplication).
- opening a station on an old codec without server windows: clear error (see CMD-32).
- kit cooldown persists across restart (per player + kit).

## Done criteria
- Plugin + kit config + persistence + tests; README lists commands and links CMD-39.
- fmt / clippy `-D warnings` / `cargo-locked test`.
