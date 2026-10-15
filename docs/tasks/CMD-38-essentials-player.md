# CMD-38 — Plugin: essentials-player

Status: 📋 task brief (CMD0) · size: L · core: no · plugin: `plugins/essentials-player`
Needs: `oc/P1`, `oc/CMD-32`

## Goal
Personal player utilities from EssentialsX (`/fly`, `/god`, `/speed`, …), on the player/UI API
(CMD-32).

## Commands (EssentialsX reference)
| command | syntax | node |
|---|---|---|
| flight | `/fly [<player>]` | `essentials.fly` (self) / `essentials.fly.others` |
| god | `/god [<player>]` | `essentials.god` |
| speed | `/speed [walk\|fly] <0-1\|1-10> [<player>]` | `essentials.speed` |
| food | `/feed [<player>]` | `essentials.feed` |
| heal | `/heal [<player>]` | `essentials.heal` |
| repair | `/repair [all\|hand\|armor]` | `essentials.repair` |
| hat | `/hat` (equip held item) | `essentials.hat` |
| nick | `/nick <nickname>`, `/nick off`, `/realname <nick>` | `essentials.nick` |
| identity | `/whois [<player>]`, `/seen <player>` | `essentials.whois`, `essentials.seen` |
| afk | `/afk [<message>]` | `essentials.afk` |
| proximity | `/near [<radius>]` | `essentials.near` |
| position | `/getpos [<player>]`, `/depth [<player>]`, `/compass`, `/ptime <time>`, `/pweather <weather>` | `essentials.getpos`, … |
| self | `/suicide`, `/burn [<player>] [<seconds>]` | `essentials.suicide`, `essentials.burn` |
| misc | `/thru`, `/skull [<player>]`, `/more`, `/condense [<item>]`, `/exp [<amount>]` | `essentials.thru`, … |

`/god` makes the player invulnerable (EssentialsX semantics), `/speed` maps to the fly/walk speed
attribute; `/nick`/`/afk` render in chat/tab (depends on CMD-40 formatting); `ptime`/`pweather` set
a per-player time/weather (client-side packet). Toggle state persists for the session and is
restored on rejoin where EssX does.

## Files
- `plugins/essentials-player/`: manifest, `src/`, README.
- player API (CMD-32), effects/flight/speed host imports; chat/formatting from CMD-40 for nick/afk.

## Edge cases to test explicitly
- `/fly` in a claim/region where flight is denied (region flag) — decide and test.
- `/god` + the void: an invulnerable player still takes void damage like EssX (or not); document.
- `/speed` bounds: walk 0..1, fly 1..10; out of range clamps/errors like EssX.
- `/hat` with an empty hand; `/repair` cost (XPs) if configured; `/more` fills the held stack to max.
- `/nick` colour/JSON sanitised; `/realname` resolves the nick; nickname persists.
- `/afk` auto-toggles on idle, cleared on movement/chat; `/seen` after a restart.
- `/ptime`/`/pweather` send the right per-player packets and reset on `/ptime reset`.
- `/suicide` runs the normal death path once (no double drop); `/burn` respects fire resistance.
- permission self vs others for every command.

## Done criteria
- Plugin + tests; README lists commands and links CMD-38.
- fmt / clippy `-D warnings` / `cargo-locked test`.
