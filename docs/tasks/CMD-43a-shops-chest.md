# CMD-43a — Plugin: shops — chest shops & admin shops

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/shops`
Needs: `oc/P1`, `oc/CMD-42`
Parent/umbrella: `docs/tasks/CMD-43-shops.md`

## Commands
| command | syntax | node |
|---|---|---|
| shop | `/shop` (open the shop menu / create a chest shop) | `shops.shop` |
| chest shop | place a sign on a chest: `[<amount>] <item> B <price>:<qty>` / `S` | `shops.chest` |
| admin shop | `/adminshop add\|remove\|setprice <item> <price>` | `shops.admin` |

## Files
- `plugins/shops/`: manifest, `src/`, README.
- economy service (CMD-42), sign text input + container windows (CMD-32b), item KV.

## Edge cases to test explicitly
- buying from an unstocked chest → refused, no charge; buyer inventory full → refused before money
  moves.
- sign removed/chest broken with an active shop invalidates it; money/escrow handled.
- admin shop buy/sell at the fixed price; a price change applies to new trades only.
- a protected region/claim blocks shop creation without bypass.
- concurrent buyers of a limited-stock chest shop: no overselling (atomic with the economy trade).

## Done criteria
- Plugin + persistence + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
