# SP1 — admin spectate + vanish (native plugin `plugins/staff` on the SDK, core hooks where needed)
Read `docs/tasks/COMMON.md`, `docs/design/plugin-sdk.md`, AX1 notes (`docs/tasks/AX1-notes.md`, `[anti_xray]`).
## Vanish (`/vanish [player]`, perm `aether.staff.vanish`)
- Vanished staff are removed from other players' tab list, entity tracking, join/quit messages, server list
  player count/sample, `/list`, tab-completion of names, sleeping checks, mob targeting; still see each other
  (perm `aether.staff.vanish.see`). Silent container open (no animation/sound). Persist across relog (config).
- Indicator for the vanished player (actionbar). Every protocol.
## Spectate (`/spectate [player]`, `/spectate off`, perm `aether.staff.spectate`)
- Switch to spectator (auto-vanish), optionally attach to a player's camera; on `off` restore exact previous
  location, gamemode, inventory, health/food/effects, flight.
- **Anti-xray control for the spectating admin only**: `/spectate xray off|engine|culling|<level>` — turn anti-xray
  off for themselves, back on, or lower its level (engine-only = only buried ores, culling with a larger margin, etc.).
  Uses AX1's per-player bypass/level hook; on change resend affected chunks; reverts when leaving spectate.
- Journal: entering/leaving vanish/spectate and xray level changes are logged events (who, when) — staff accountability.
## Rules
Tests (tab/entity hiding per protocol, restore after spectate, xray level switch), docs EN+RU, fmt/clippy.
