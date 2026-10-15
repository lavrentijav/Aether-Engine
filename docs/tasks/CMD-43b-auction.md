# CMD-43b — Plugin: shops — auction house & /sell

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/shops`
Needs: `oc/P1`, `oc/CMD-42`, `oc/CMD-43a`
Parent/umbrella: `docs/tasks/CMD-43-shops.md`

## Commands
| command | syntax | node |
|---|---|---|
| auction | `/auction start <price> [<time>]`, `/auction bid <id> <amount>`, `/auction cancel <id>` (`/ah`) | `shops.auction` |
| browse | `/ah [<page>]`, `/ah search <query>` | `shops.auction` |
| sell | `/sell hand [<amount>]`, `/sell all [<material>]` | `shops.sell` |

Auction listings hold the item **in escrow** until sold/cancelled, so one stack cannot be listed
twice.

## Files
- `plugins/shops/`: manifest, `src/`, README.
- economy service (CMD-42), item serialization (KV).

## Edge cases to test explicitly
- auction bid by the seller is refused; outbidding refunds the previous bidder atomically.
- auction expiry returns the escrow to the seller (offline-safe).
- `/sell hand` on a non-sellable item; `/sell all` with a full balance.
- concurrent buyers of one listing: exactly one succeeds.

## Done criteria
- Plugin + escrow persistence + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
