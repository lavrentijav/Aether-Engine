# CMD-43 — Plugin: shops

Status: 📋 task brief (CMD0) · size: L · core: no · plugin: `plugins/shops`
Needs: `oc/P1`, `oc/CMD-42`

## Goal
Chest shops, an auction house and admin shops on the economy service (CMD-42).

## Commands
| command | syntax | node |
|---|---|---|
| shop | `/shop` (open a shop menu / create a chest shop) | `shops.shop` |
| chest shop | place a sign on a chest with `[<amount>] <item> B <price>:<qty>` / `S` — sign-based | `shops.chest` |
| auction | `/auction start <price> [<time>]`, `/auction bid <id> <amount>`, `/auction cancel <id>` (`/ah`) | `shops.auction` |
| auction browse | `/ah [<page>]`, `/ah search <query>` | `shops.auction` |
| sell | `/sell hand [<amount>]`, `/sell all [<material>]` | `shops.sell` |
| admin shop | `/adminshop add\|remove\|setprice <item> <price>` | `shops.admin` |

Chest shops: a sign (front/back edit via sign text input, PA1) with the shop definition; an owner
stocking the chest; a buyer right-clicking executes one economy transaction (CMD-42) and moves the
items atomically with the listing/count. Admin shops are infinite in/out at a fixed price. Auction
listings carry an escrow (the item is held by the plugin until sold/cancelled) so the same item
cannot be listed twice.

## Files
- `plugins/shops/`: manifest, `src/`, README.
- economy service (CMD-42), sign text input + container windows (CMD-32), item serialization (KV).

## Edge cases to test explicitly
- buying from a chest shop with an unstocked chest → refused, no charge.
- the buyer's inventory full → trade refused before money moves.
- sign removed/chest broken with an active shop: the shop is invalidated and money/escrow handled.
- auction bid by the seller is refused; outbidding refunds the previous bidder atomically.
- auction expiry returns the escrow to the seller (offline-safe).
- `/sell hand` on a non-sellable item; `/sell all` with a full balance.
- admin shop buy/sell at the fixed price; a price change applies to new trades only.
- a protected region/claim blocks shop creation without bypass.
- concurrent buyers of a limited-stock chest shop: no overselling.

## Done criteria
- Plugin + escrow persistence + tests; README lists commands and links CMD-43.
- fmt / clippy `-D warnings` / `cargo-locked test`.
