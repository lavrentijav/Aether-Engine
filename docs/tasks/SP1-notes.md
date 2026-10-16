# SP1 notes — admin spectate + vanish

Branch `oc/SP1`, from `main`. Commits start with `SP1`.

## What shipped

A native plugin `plugins/staff` (tier N) on the host API, with the mechanism in
the core. See [`docs/design/staff.md`](../design/staff.md) (EN) and
[`staff.ru.md`](../design/staff.ru.md) (RU).

- **`/vanish [player]`** (`aether.staff.vanish`): the vanished player leaves the
  tab list, entity tracking, join/quit messages and mob targeting; vanished
  staff see each other (`aether.staff.vanish.see`, set for operators until
  CMD-01). Action-bar indicator every second on every protocol. Persisted by
  name in the plugin KV and re-applied on join (`[plugins.staff] persist`).
- **`/spectate [player]` / `/spectate off`** (`aether.staff.spectate`): saves the
  exact previous state (position, mode, inventory, health/hunger/effects, XP,
  absorption, prior vanish), switches to spectator (auto-vanish) and restores on
  `off` through the normal `after_spawn` path.
- **`/spectate xray off|engine|culling|<level>`**: a per-player anti-xray level
  override (AX1's bypass extended to a level), re-sending the admin's columns on
  change and reverting with the spectate snapshot.
- **Journal**: enter/leave vanish and spectate, the auto-vanish and every x-ray
  change are appended as `EventBody::Custom` under a staff tag, actor = UUID.

## Core changes

- `GameMode::Spectator` across every codec (wire 3, abilities
  `invulnerable | allow flying`); `ClientEvent` decode accepts 3.
- `ServerEvent::ActionBar`; modern codecs render it as 1.19 System Chat with the
  overlay flag, older codecs rewrite it to chat via `ServerEvent::legacy`.
- `crates/aether-server/src/staff.rs`: per-player flags, the visibility
  predicate, the spectate snapshot/restore, the level type.
- `visibility.rs`: `effective_mode`/`effective_enabled`/`effective_margin`, the
  reachable search with an explicit margin, per-player pass splitting in `tick`,
  `resend_held_columns`, `State::invalidate`.
- `HostBackend::staff_*` (all defaulted to neutral) + the `ServerBackend`
  implementation; native command self-registration through
  `HostBackend::register_command`.
- `session.rs` join/leave/spawn paths consult `staff::visible_to`; `after_spawn`
  is `pub(crate)` for the restore.

## Decisions / deviations

- **`/spectate` is not a vanilla command here.** It was a CMD-11 stub in the core
  registry; SP1 removes it from the stub list and implements it in the plugin
  with `aether.staff.spectate` and the per-admin anti-xray control, which
  vanilla's entity-following `/spectate` has no equivalent of.
- **The gate is the operator flag** until CMD-01 lands; the permission nodes
  are declared now.
- **Vanish is a plugin, the mechanism is core**: another client's spawn decision
  and the render path cannot wait for a plugin call.
- **No camera attach**: the protocol layer has no Set Camera packet;
  `/spectate <player>` teleports onto the target instead.
- **Not wired because the core feature is absent**: `/list` (CMD-09 stub), the
  static command tree, the server-list sample and the sleep count.

## Follow-up (after review)

- **Operators are no longer bypassed automatically.** `anti_xray_bypass` no
  longer consults `is_operator`; bypass is explicit only — `/spectate xray off`,
  `/visibility off`, a name in `[anti_xray] bypass_players`, or the
  `aether.antixray.bypass` node granted through the new `[permissions] grants`
  list (`name:node`, `name@address` supported). This lets an operator watch the
  protection work and turn it off deliberately.
- **`/visibility status`** reports the effective anti-xray state for the
  player (`visibility::player_status`); `/visibility off` stays as the explicit
  toggle alias.
- The startup INFO line (`anti-xray  : <mode> (...)`) is unchanged.

## Checks

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -j4 -- -D warnings` — clean.
- `cargo-locked test -p aether-staff-plugin` — 9/9 pass.
- `cargo-locked test -p aether-server --lib` — 1089/1089 pass.
- `cargo-locked test -p aether-server --test anti_xray_e2e` — 2/2 pass.
- `cargo-locked test -p aether-plugin` — all suites pass (lib 10, plugin_host
  22, sdk_bridge 2, sidebar_bridge 2, placeholders_bridge 2, wit_world 3).
