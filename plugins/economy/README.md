# economy plugin — skeleton (CMD-42)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`) and the economy service WIT + ledger
(`oc/P2`; BACKLOG PM1). **No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-42** — `docs/tasks/CMD-42-economy.md`
- Reference: the current built-in economy (`crates/aether-server/src/economy/`), ported to
  `aether:plugin/economy@0.1` behind `storage.ledger`. Provides the `economy` service.

## Commands
| command | syntax | permission |
|---|---|---|
| `/balance [<player>]` (`/bal`, `/money`) | | `economy.balance` |
| `/pay <player> <amount>` | | `economy.pay` |
| `/baltop [<page>]` | | `economy.baltop` |
| `/eco give\|take\|set <player> <amount>` | | `economy.admin` |
| `/worth [<item>]` | | `economy.worth` |
| `/sell <count> <price> [<time>]`, `/buy <id> [<count>]`, `/market`, `/listings`, `/unlist <id>` | | `economy.market.*` |

## Capabilities / events
`storage.ledger`, `storage.kv`, `log`, `config`, `supports`. Subscribes to `on-command`.

## Notes
Integrity is unchanged: one transaction per trade, balance never negative, listing + both balances
move together. The built-in Postgres economy stays as fallback until this plugin passes the
conformance suite, selected by `[economy] provider`.
