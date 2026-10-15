# CMD-47 — Plugin: stats

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/stats`
Needs: `oc/P1`, `oc/PA2` (history read API)

## Goal
Player stats and leaderboards, read from the journal/history API (PA2) and the scoreboard/player
record — the catalogue's `/stats /leaderboard /playtime` (reads the journal via PA2).

## Commands
| command | syntax | node |
|---|---|---|
| stats | `/stats [<player>]` (blocks mined/placed, distance, deaths, kills, playtime) | `stats.stats` |
| leaderboard | `/leaderboard <stat> [<page>]` (`/lb`) | `stats.leaderboard` |
| playtime | `/playtime [<player>]`, `/playtime top [<page>]` | `stats.playtime` |

Data sources: PA2 journal queries (events in an area/time, per-actor) for mined/placed/deaths/kills;
a first-join/last-seen record for playtime. Aggregates are cached and refreshed on a schedule (P1),
never computed on the hot path. A stat the engine cannot derive is reported as `0` or hidden, with a
note in the README.

## Files
- `plugins/stats/`: manifest, `src/`, README.
- PA2 history API; player KV for playtime; the scoreboard model (CMD-15) if leaderboards are shown
  on a sidebar.

## Edge cases to test explicitly
- `/stats` for or about an offline player (from stored aggregates).
- `/lb` ties break deterministically; paging past the end is an error/empty page.
- playtime accrues only while online and survives a restart; a crash does not lose the session
  (checkpoint on join/leave).
- a stat with no data shows 0 rather than erroring.
- a large leaderboard does not scan the whole journal per request (cached aggregate).
- a player renamed/nicknamed (CMD-38) still maps to the same uuid.
- permission: staff can query others (`stats.stats.others`), players only themselves.

## Done criteria
- Plugin + aggregation + tests; README lists commands and links CMD-47.
- fmt / clippy `-D warnings` / `cargo-locked test`.
