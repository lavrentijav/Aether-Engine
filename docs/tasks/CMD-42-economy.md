# CMD-42 — Plugin: economy (P2 port)

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/economy`
Needs: `oc/P1`, `oc/P2` (economy service WIT + ledger; BACKLOG PM1)

## Goal
Move the existing built-in economy to a WASM plugin behind the stable `aether:plugin/economy@0.1`
interface and pass the conformance suite (`docs/design/plugins.md` "Services: economy as the first
one"). The server keeps only the interface; `/money` etc. stay command names but are served by the
plugin. This is BACKLOG PM1/P2.

## Commands (existing logic)
| command | syntax | node |
|---|---|---|
| balance | `/balance [<player>]` (`/bal`, `/money`) | `economy.balance` |
| pay | `/pay <player> <amount>` | `economy.pay` |
| baltop | `/baltop [<page>]` | `economy.baltop` |
| admin | `/eco give\|take\|set <player> <amount>` | `economy.admin` |
| worth | `/worth [<item>]` | `economy.worth` |
| market | `/sell <count> <price> [<time>]`, `/market`, `/buy <id> [<count>]`, `/listings`, `/unlist <id>` | `economy.market.*` |

Integrity rules from `economy/pg.rs` must survive: one transaction per trade, balance never
negative, listing count and both balances move together or not at all; implemented on
`storage.ledger` (P2). The built-in Postgres economy stays as fallback until the plugin passes; then
it is removed or feature-gated, selected by `[economy] provider = "plugin:economy" | "builtin"`.

## Files
- `plugins/economy/`: manifest, `src/` (port of `crates/aether-server/src/economy/pg.rs` and
  `commands.rs` logic), `Cargo.toml`, README.
- server `WasmEconomy` adapter (P2); conformance suite run against both implementations.

## Edge cases to test explicitly
- conformance suite: balance/pay/mint/list/buy/cancel/market/listings-of with the same `TradeError`.
- a trade that fails halfway leaves both balances and the listing unchanged (atomic).
- pay to self / to an offline player (stored balance) ; pay more than the balance.
- two concurrent buyers of one listing: exactly one succeeds.
- `/baltop` order and ties; huge balances format like the built-in.
- negative/zero amounts rejected; an amount that would overflow is rejected.
- provider switch: `builtin` and `plugin:economy` give the same results on the suite.

## Done criteria
- Conformance suite green against the plugin; README lists commands and links CMD-42.
- fmt / clippy `-D warnings` / `cargo-locked test`.
