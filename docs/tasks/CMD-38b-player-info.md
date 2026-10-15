# CMD-38b — Plugin: essentials-player — identity & info

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/essentials-player`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-32a`, `oc/CMD-32c`, `oc/CMD-38a`
Parent/umbrella: `docs/tasks/CMD-38-essentials-player.md`

## Commands (EssentialsX reference)
| command | syntax | node |
|---|---|---|
| nick | `/nick <nickname>`, `/nick off`, `/realname <nick>` | `essentials.nick` |
| identity | `/whois [<player>]`, `/seen <player>` | `essentials.whois`, `essentials.seen` |
| afk | `/afk [<message>]` | `essentials.afk` |
| proximity | `/near [<radius>]` | `essentials.near` |
| position | `/getpos [<player>]`, `/depth [<player>]`, `/compass`, `/ptime <time>`, `/pweather <weather>` | `essentials.getpos`, … |
| self | `/suicide`, `/burn [<player>] [<seconds>]` | `essentials.suicide`, `essentials.burn` |
| misc | `/thru`, `/skull [<player>]`, `/more`, `/condense [<item>]`, `/exp [<amount>]` | `essentials.thru`, … |

`/nick`/`/afk` render through chat formatting (CMD-40); `/ptime`/`/pweather` set per-player
client-side time/weather (CMD-32c).

## Files
- `plugins/essentials-player/`: manifest, `src/`, README.
- player API (CMD-32a/32c); chat formatting from CMD-40 (a `needs` note — if CMD-40 is absent, the
  nickname still works, only the chat display degrades).

## Edge cases to test explicitly
- `/nick` colour/JSON sanitised; `/realname` resolves the nick; nickname persists.
- `/afk` auto-toggles on idle, cleared on movement/chat; `/seen` after a restart.
- `/ptime`/`/pweather` reset form; `/suicide` runs the normal death path once; `/burn` respects fire
  resistance; `/more` fills the held stack to max; `/condense` merges.

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
