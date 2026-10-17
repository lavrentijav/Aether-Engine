# stats plugin — player stats and leaderboards (CMD-47)

A **WASM** plugin (sandboxed, `wasm32-wasip2`) that answers `/stats`,
`/leaderboard` (`/lb`) and `/playtime` from the journal history API (PA2). It
folds the journal into a small per-player cache on a schedule (P1) and answers
every command from that cache, so a request never walks the journal and an
offline player is answered from the stored aggregate just the same.

- CMD task: **CMD-47** — [`docs/tasks/CMD-47-stats.md`](../../docs/tasks/CMD-47-stats.md)
- History API (PA2): [`docs/design/history-api.md`](../../docs/design/history-api.md)
- Plugin tiers and the SDK: [`docs/design/plugins.md`](../../docs/design/plugins.md)

## Commands

| command | syntax | node | op |
|---|---|---|---|
| stats | `/stats [<player>]` | `stats.stats` | 0 |
| leaderboard | `/leaderboard <stat> [<page>]` | `stats.leaderboard` | 0 |
| leaderboard (alias) | `/lb <stat> [<page>]` | `stats.leaderboard` | 0 |
| playtime | `/playtime [<player>]`, `/playtime top [<page>]` | `stats.playtime` | 0 |

`<stat>` is one of `mined` (`blocks`), `placed`, `distance`, `deaths`, `kills`
or `playtime`. A page holds 10 rows; page 1 is the default. Asking about a
player other than yourself additionally needs the `stats.stats.others` node —
players may always ask about themselves, staff may ask about anyone.

## Where the numbers come from

- **Blocks mined / placed** are read from the journal (PA2): a `block_set`
  whose new block is air is a mine, one whose old block was air is a place.
  Every tracked player is read through their own actor index
  (`player-events`), and only events newer than the last watermark are folded
  in, so a refresh is bounded by what happened since the previous one — never
  a scan of the whole journal. There is no per-request scan at all.
- **Playtime** is tracked by the plugin itself, in world ticks, from
  `after-player-join` / `after-player-leave`. It accrues only while the player
  is online and is **checkpointed to the plugin's KV** on join, on leave and on
  the scheduled refresh, so it survives a restart. A crash loses at most the
  ticks since the last checkpoint: recovery folds an open session up to the
  last heartbeat, and because the world tick does not advance while the server
  is down, the downtime is never charged.

## What the engine cannot derive

The journal records **block edits and item moves**, not movement, damage or
death, so these stats are always reported as `0` (never hidden, never an
error), and the README says so as the brief requires:

| stat | status |
|---|---|
| distance | always `0` — movement is not journalled |
| deaths | always `0` — deaths are not journalled |
| kills | always `0` — kills are not journalled |

They are kept in the model so a later task that produces them can fill them in
without changing the command surface. If the history API ever grows a
movement/death event, only `model::classify_block_change` and the `Stats`
struct need to learn it.

A player the plugin has never seen since it was installed (or a name it has no
record for) answers `0` / "no data", not an error. A player renamed (CMD-38)
maps to the same UUID: records are keyed by UUID with a name index, so only the
displayed name changes.

## Caching and the schedule

Aggregates live in the plugin's KV (`storage.kv`) and are refreshed by an SDK
scheduler timer (P1), polled from `on_tick`. The refresh period is
operator-tunable:

```toml
[plugins.stats]
refresh_seconds = 30   # default 30, clamped to 1..=3600
```

Commands are read-only against the cache.

## Capabilities / services

Capabilities: `log`, `config`, `supports`, `storage.kv`, `history.read`.
Required SDK services: `sdk.commands@0.1`, `sdk.history@0.1`,
`sdk.permissions@0.1`, `sdk.players@0.1`, `sdk.scheduler@0.1`. It subscribes
to `on-command`, `after-player-join`, `after-player-leave` and `on-tick`.

## Build and test

The crate is a workspace member so the pure model runs under `cargo test`; the
component glue is compiled only for the component target.

```sh
# host tests (model + engine)
cargo-locked test -p aether-stats-plugin

# the deployable component (needs `rustup target add wasm32-wasip2`)
make            # -> stats.wasm next to plugin.toml
make check      # type-check the component only
```

## Layout

- `src/model.rs` — identities, journal classification, aggregates, leaderboard
  ordering and the playtime ledger; no host, no clock.
- `src/engine.rs` — the model wired to the `History`, `Store` and
  `PermissionCheck` traits; the commands are answered here.
- `src/component.rs` — the `wasm32-wasip2` guest (WASM only).
- `src/tests.rs` — the edge-case suite (offline target, deterministic ties,
  paging past the end, zero-not-error, restart + crash, rename, permissions,
  no journal scan per request).
