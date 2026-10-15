# shops plugin — skeleton (CMD-43)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`) and the economy plugin/service
(`oc/CMD-42`). **No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-43** — `docs/tasks/CMD-43-shops.md`
- Reference: chest shops, an auction house and admin shops on the economy service.

## Commands
| command | syntax | permission |
|---|---|---|
| `/shop` | open the shop menu / create | `shops.shop` |
| sign chest shop | `[<amount>] <item> B <price>:<qty>` / `S` on a chest sign | `shops.chest` |
| `/auction start\|bid\|cancel …` (`/ah`) | | `shops.auction` |
| `/ah [<page>]`, `/ah search <query>` | browse | `shops.auction` |
| `/sell hand [<amount>]`, `/sell all [<material>]` | | `shops.sell` |
| `/adminshop add\|remove\|setprice <item> <price>` | | `shops.admin` |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `players`, `world.read`, `world.write`. Subscribes to
`on-command`, `before-use-item`, `before-container-click`.

## Notes
Auction listings hold the item in escrow until sold/cancelled, so one stack cannot be listed twice.
Shop signs use the host sign text input (PA1).
