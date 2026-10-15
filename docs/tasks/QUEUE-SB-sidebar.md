# SB1 — sidebar scoreboard (the panel on the right): native plugin `plugins/sidebar`

Read `docs/tasks/COMMON.md`, `docs/design/plugins.md` (tiers), `docs/design/plugin-sdk.md` (SDK native
plugin + services), CMD-15 brief (vanilla `/scoreboard`), the scoreboard/teams packets per protocol.

## Why
Every server shows a per-player sidebar: name, donor rank, coins, ping, online, TPS, kills… Shops,
economy and ranks all want to put lines there. One flicker-free shared implementation.

## Tier
**Native plugin** `plugins/sidebar` (tier N, on the SDK), exporting a `sidebar` service so any plugin
(native or WASM) can register **placeholders** (`%economy_balance%`, `%rank_prefix%`) and lines. Core
keeps only the vanilla scoreboard model + packets (CMD-15) and a per-player packet API — the plugin must
not fight the vanilla `/scoreboard` sidebar (if an objective is set in the vanilla sidebar slot for a
player, the plugin yields or overrides per config).

## Features
- Per-player sidebar: title + up to 15 lines, from config (`plugins/sidebar/config.toml`) with conditions
  (permission / world / placeholder compare) to pick a layout; animated title/lines (frames, interval).
- Built-in placeholders: player name, display name, ping, online/max, world, x/y/z, TPS, health, level,
  time; others come from services (economy balance, rank/prefix from permissions, kills from stats).
- **Flicker-free updates**: lines are fixed entries whose text lives in team prefix/suffix (old protocols)
  or in the score display name / number format (1.20.3+: `number_format = blank` hides the red numbers);
  only changed lines are resent; update rate limited (default 4/s per player, placeholders cached).
- **Every protocol**: 1.8 limits (objective title 32, team prefix/suffix 16 chars, legacy colour codes),
  1.13+ JSON components and 64-char prefixes, 1.20.3+ NBT text + number formats; truncation rules tested.
- Also tab list header/footer with the same placeholders (separate toggle).
- API: `sidebar.set_line(player, idx, text)`, `sidebar.register_placeholder(name, fn)`, show/hide per player,
  `/sidebar toggle` command.

## Tests
Per-protocol encoding (lengths, colours, components), flicker-free diff (only changed lines resent),
placeholder resolution + caching, vanilla `/scoreboard` coexistence, live test via V1 harness (MCC reads
the sidebar lines).

Commit per step (SB1a core per-player scoreboard packet API, SB1b plugin + config + placeholders,
SB1c per-protocol encoding + flicker-free, SB1d tab header/footer, SB1e docs EN+RU). cargo-locked only.
