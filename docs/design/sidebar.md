# Sidebar scoreboard — native plugin `plugins/sidebar`

Status: ✅ implemented (2026-10-15), queue SB1. Tier **N** (native plugin) on the plugin API, with a
small core mechanism underneath. This document is the as-built record; the task is in
[`QUEUE-SB-sidebar.md`](../tasks/QUEUE-SB-sidebar.md).

> **Read this in other languages:** [Русский 🇷🇺](sidebar.ru.md)

## Why

Every server wants a per-player panel on the right: name, rank, coins, ping, online count, TPS,
kills. Shops, economy and ranks all want to put a line there. This is one shared, flicker-free
implementation, so plugins do not each reinvent the scoreboard packets (and get the per-version
details wrong).

## Tier: core mechanism, plugin policy

The split follows [`plugins.md`](plugins.md)'s rule — the core keeps mechanisms, a plugin keeps
policy.

| | where | what |
|---|---|---|
| **core** (`crates/aether-server`) | `protocol/scoreboard.rs` + `PlayerHandle` | the neutral sidebar model, the explicit scoreboard operations, the per-version `Wire` table, and the per-player packet API (`update_sidebar`, `set_tab_header_footer`) with the flicker-free diff |
| **plugin** (`plugins/sidebar`) | the layouts, conditions and placeholder values | which lines a player sees, resolved from `config.toml` and `%placeholders%`, at a rate limit |

A native plugin compiles into the server, so it can be disabled like any other
(`[plugins.sidebar] enabled = false`), and the core runs correctly without it. The plugin never
builds packets; it hands the core a `SidebarBoard` and the core renders it in each client's own
dialect.

## Core: the per-player packet API

`SidebarBoard { title, lines }` (up to fifteen lines, plain text with `§` codes). A host pushes it
through a `HostBackend` method:

```rust
backend.set_sidebar(plugin, player, &board);        // show / update
backend.clear_sidebar(plugin, player);              // hide
backend.set_tab_header_footer(plugin, player, &header, &footer);
```

`ServerBackend` maps these onto `PlayerHandle::update_sidebar` / `set_tab_header_footer`, which hold
the player's last board and header/footer and diff against them. **Only changed lines produce a
packet.** An unchanged tick writes nothing to the socket, which is what keeps a periodically
re-evaluated sidebar from flickering.

The diff yields explicit `ScoreboardOp`s (`ObjectiveAdd`, `Display`, `TeamAdd`, `TeamUpdate`,
`ScoreAdd`, `ResetScore`, …). A `Wire` (packet ids + field shapes) turns each op into bytes; the
wire is chosen from the codec's `protocol_id()` by `scoreboard::wire_for`, so **adding a release is
one row in one table**, not an edit to every codec module. `PlayerHandle::render` handles the
scoreboard and tab-list events itself, so the twenty-four codec `encode`s are untouched.

### Two rendering strategies

- **Up to 1.20.2 (`SidebarStyle::Teams`)**: the visible text rides in a team's prefix/suffix; the
  score entry is an invisible colour code. A line change rewrites that team's prefix — one packet
  — so the objective, the display slot and the other lines are never torn down.
- **1.20.3+ (`SidebarStyle::DisplayNames`)**: the text rides in the score's own display name and
  `number_format = blank` hides the red number. No teams at all. A line change is one score packet.

The strategy is derived from whether the version has a number format (`Wire::number_format`).

### Wire coverage

| protocols | versions | text | teams | display slot | reset |
|---|---|---|---|---|---|
| 47 | 1.8.9 | raw `§` string | `Legacy`: `displayName` + 16-char halves | byte | score action `1` |
| 755–762 | 1.17–1.19.4 | JSON component | `Classic` (JSON name/prefix/suffix, coll. string) | byte | score action `1` |
| 763–764 | 1.20/1.20.1, 1.20.2 | JSON component | `Classic` | byte / VarInt | score action `1` |
| 765–769 | 1.20.3–1.21.4 | NBT component | `Nbt` (NBT name/prefix/suffix, coll. string) | VarInt | `reset_score` |
| 770 | 1.21.5 | NBT component | `NbtEnums` (collision/visibility became VarInt enums) | VarInt | `reset_score` |
| 771–774 | 1.21.6–1.21.11 | NBT component | `NbtFlags` (friendly-fire is a flag byte) | VarInt | `reset_score` |
| 775–777 | 26.1–26.3 | NBT component | `NbtFlags` | VarInt | `reset_score` |

Truncation: objective names stay short; 1.8 splits a long line across a 16-char prefix and a
16-char suffix (a split never leaves a dangling `§`); from 1.13 the halves are 64 characters. The
title is `aether.side`, clear of the vanilla `/scoreboard` names. Every id and layout above is
pinned against `minecraft-data` (and, for 26.x, each release's own data-generator `packets.json`).

Tab list: the header/footer packet — `playerlist_header` (renamed `message_header` in 1.19.2,
where the tab list itself is `0x63`, and `tab_list` from 26.x) — with a different id in every
generation, JSON before 1.20.3 and NBT from 1.20.3.

## Plugin: config, layouts, placeholders

`plugins/sidebar/config.toml` is data-only:

```toml
title = "&6&lAether"
update_hz = 4          # times per second; default 4, capped at 20
tab = false            # mirror the board into the tab list header/footer
mode = "override"      # "override" | "yield" (see limitations)

[[layout]]
priority = 0
lines = ["&7Online: &f%online%&7/&f%max%", "&7Ping: &f%ping%ms", "&e%player%"]

[[layout]]
priority = 10
permission = "sidebar.vip"
world = "minecraft:the_end"
lines = ["&dVIP", "&7Online: &f%online%"]
```

The highest-priority layout whose conditions pass is drawn. Conditions: `permission` (needs the SDK
permission service; fails closed while CMD-01/02 are absent), `world`, and `placeholder`/`equals`.

**Placeholders are not the sidebar's own.** They are shared with menus and chat, so the registry is
the SDK's `placeholders` service (`sdk.placeholders@0.1`) — see
[plugin-sdk.md §7.9](plugin-sdk.md#79-placeholders--one-shared-name-registry). The sidebar
**requires** it and resolves every title and line through `resolve(caller, player, text)`. Built-in
tokens: `%player%`, `%displayname%`, `%ping%`, `%online%`, `%max%`, `%world%`, `%x%`/`%y%`/`%z%`,
`%tps%`, `%health%`, `%level%`, `%time%`/`%world_time%` (world clock) and `%server_time%` (server
wall clock). `%ping%` and `%tps%` are measured, not invented: the core times the keep-alive round
trip and smooths the actual tick rate. The sidebar applies `&`→`§` colour translation to its own
strings; an unknown token is left literal, so a typo shows up instead of vanishing.

**Other plugins** feed placeholders (`%economy_balance%`, `%rank_prefix%`) through that shared
service — a native closure, or (for WASM) a declared name + a pushed per-player value:

```rust
// Native: hand over a closure once.
placeholders().register("economy_balance".into(), Arc::new(|q| economy.balance(q.player)))?;
```

```rust
// WASM: declare once, push the per-player value.
placeholders::register("economy_balance");
placeholders::set("economy_balance", &p, balance.to_string());
```

To put a value on **this** panel specifically, the same plugins use the `sidebar@0.1` service:
`sidebar.set_line(player, 3, "&6%economy_balance%")` / `clear_line` / `show` / `hide` / `toggle`.
A WASM plugin imports `aether:sidebar/sidebar@0.1` (its own package). The host bridge
(`crates/aether-plugin/src/sdk.rs`) forwards each import to the registered provider; with the
sidebar plugin disabled the call is refused and answers the neutral value, so a dependent still
loads.

The **plugin** also registers a `/sidebar toggle|show|hide|reload` command (through the core command
registry, so it is permission-checked like any other). `toggle`/`show`/`hide` are open to any
player; `reload` is refused unless the caller is an operator (`p.operator`) and runs on a background
thread — the file read never blocks the tick thread, the parsed config is applied on the next tick.

## Tests

- `crates/aether-server/src/protocol/scoreboard.rs`: the diff (empty on unchanged, one packet per
  changed line, add/remove lines, title-only rename, hide), the fifteen-line cap, 1.8 splitting, and
  wire-level encoding per family — 1.8 string criterion and score action, JSON components, modern
  NBT + blank number format + `reset_score`, the 1.8 vs modern team field order, and the tab list.
- `plugins/sidebar`: config parsing, colour translation, and an end-to-end test that drives the
  real `NativePlugin` through a recording `HostBackend` and a stub placeholders service (board
  pushed and resolved, line override, toggle, tab mirror).
- `plugins/sdk`: the placeholder runtime (built-ins, native/pushed registration, the resolved-token
  cache, per-tick query refresh).
- `crates/aether-plugin/tests/sidebar_bridge.rs` and `placeholders_bridge.rs`: a real WASM component
  (`tests/fixtures/sidebar/sidebar.wasm`, built from `tests/guests/sidebar`) calls every
  `aether:sidebar` and `aether:sdk/placeholders` import and the host forwards each to a mock
  provider with the caller's id; further tests pin the neutral answer when no provider is
  registered. `tests/wit_world.rs` pins the `aether:sidebar@0.1.0` package identity.

Checks per [`COMMON.md`](../tasks/COMMON.md): `cargo fmt`, `cargo clippy -- -D warnings`,
`cargo-locked test`.

## Limitations

- **CMD-15 is not landed.** The core keeps only what a sidebar needs (the sidebar model + these
  packets); the full vanilla `/scoreboard` model, commands and storage are a separate task
  (CMD-15a/b). `mode = "yield"` is **wired**: the plugin asks
  `HostBackend::vanilla_sidebar_claimed` and hides its own panel while the vanilla model owns the
  slot, but until CMD-15 lands the core always answers `false`, so `yield` has no visible effect
  yet. `override` is the default.
- **`%level%` is 0, `%world%` is the overworld and `%displayname%` is the login.** The experience
  level is tracked on `PlayerState` but not routed into `PlayerExtra`, no per-player dimension is
  plumbed through, and nicks are not stored; `%ping%`, `%tps%`, and the clocks are real.
- **Placeholders live in the SDK, not here.** `%time%` is the world clock and `%server_time%` the
  server wall clock; there is no sidebar-local clock switch (the former `clock` config was dropped
  when the registry became shared). WASM providers push values rather than being called back; a
  custom name is owned by its first registrant, and the built-ins are reserved — both documented at
  [`plugin-sdk.md §7.9`](plugin-sdk.md#79-placeholders--one-shared-name-registry).
- **The 26.1–26.3 packet ids were taken from each release's own data generator**, not assumed: the
  table values for 775/776 (a shift of two from 1.21.11) and 777 (a shift of four) come from
  `packets.json` produced by that release's `net.minecraft.data.Main --reports`
  (`set_display_objective`, `set_objective`, `set_player_team`, `set_score`, `reset_score`; the
  header/footer packet renamed `tab_list`). The field *shapes* are unchanged from 1.21.9+, which the
  release notes corroborate.
