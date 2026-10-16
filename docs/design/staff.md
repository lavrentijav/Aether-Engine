# Staff — vanish, admin spectate and per-admin anti-xray control (SP1)

> **Read this in other languages:** [Русский 🇷🇺](staff.ru.md)

Status: ✅ implemented (2026-10-16), task SP1. The policy is a **native plugin**
(`plugins/staff`, tier N per [`plugins.md`](plugins.md)); the mechanism lives in
the core (`crates/aether-server/src/staff.rs`, `src/visibility.rs`,
`src/session.rs`) because another player's client and the render path need the
answer without a plugin call. Known gaps are in
[KNOWN_ISSUES §21](../KNOWN_ISSUES.md).

## 1. Why a plugin, and what the core keeps

An optional admin feature belongs in a plugin, not the core. But vanish and
spectate are not self-contained: whether a *different* client spawns a vanished
player, and whether the render path masks an ore, are decisions the core makes
on threads that cannot wait for a plugin call. So the split is:

| layer | owns |
|---|---|
| `plugins/staff` (native) | command parsing, gating, who is vanished, which anti-xray level, persistence across a relog, the tick indicator, the journal entries |
| core `staff.rs` | the per-player flags, the visibility predicate, the spectate snapshot/restore, the anti-xray override |
| host bridge (`HostBackend::staff_*`) | the one API the plugin drives the mechanism through |

Nothing in the core references the plugin; the plugin is disableable like any
other (`[plugins.staff] enabled = false`), and with it off nothing vanishes.

## 2. Commands

The plugin self-registers its commands through the core command registry
(`HostBackend::register_command`), so they are dispatched and permission-checked
by the same gate as a core command.

```text
/vanish [player]                              # aether.staff.vanish
/spectate [player]                            # aether.staff.spectate
/spectate off
/spectate xray off|engine|culling|<level>
```

`/spectate` is deliberately not a core stub any more: it is the admin feature
this task adds, with the `aether.staff.spectate` node, not vanilla's
entity-following `/spectate`. Until the permission store (CMD-01) lands the gate
is the operator flag, the same stand-in the core's own commands use; the nodes
are declared now so the resolver can take over unchanged.

## 3. Vanish

A vanished player is removed from other players' **tab list** and **entity
tracking** (despawned, and never spawned for a viewer who may not see them),
from **join/quit messages** and from **mob targeting**. Those are the paths this
engine has, and each consults [`staff::visible_to`], the one predicate:

```rust
!target.vanished || viewer.sees_vanished || (viewer.vanished && target.vanished)
```

So vanished staff see each other (the stand-in for
`aether.staff.vanish.see`, set for operators on join), and a vanished viewer
still sees everyone. A vanished player is never spawned for a viewer who may
not see them, so movement broadcasts to an untracked entity are ignored by the
client.

The remaining items in the brief's list are **not wired because the core feature
does not exist yet**, and so are absent by construction until it lands: `/list`
is still a CMD-09 stub, the command tree is static (no dynamic name completion),
there is no server-list player count/sample, and beds only set the spawn point
(no sleep count). Each of those must call `staff::visible_to` when it is built —
it is the one place the rule lives.

Container opens are already silent in this engine (no open animation or chest
sound is broadcast), so a vanished player's containers are silent by
construction.

**Indicator.** Every `tick_period` (1 s) the plugin sends the vanished admin an
action-bar line. `ServerEvent::ActionBar` renders as 1.19+ System Chat with the
overlay flag; older codecs rewrite it to a chat line via `ServerEvent::legacy`,
so it shows in every protocol.

**Persistence.** The plugin keeps the vanished *names* in its KV
(`vanished`, newline-separated) and re-applies the vanish in
`after-player-join`. `[plugins.staff] persist = false` drops a vanish on logout.
There is a one-tick window on relog (the join burst runs before the plugin's
join callback); the re-apply despawns the player immediately after, so the leak
is at most one tick of a player's own spawn burst.

## 4. Spectate

`/spectate` saves the exact previous state — position/look, game mode,
inventory, health/hunger/saturation, experience, effects, absorption and whether
the admin was already vanished — then switches to spectator (`GameMode::Spectator`,
added across every codec) and auto-vanishes. `/spectate off` restores the
snapshot and pushes it back through the normal `after_spawn` path (position,
mode, effects, health, experience, whole inventory).

The **camera is not attached**: this protocol layer has no Set Camera packet, so
`/spectate <player>` moves the spectator onto the target's position instead of
riding their eyes. This is a known gap, not a silent one.

## 5. Per-admin anti-xray control

AX1 left anti-xray a global mode plus a per-player *bypass*. SP1 adds a
per-player **level** override on the player and consults it first in
`anti_xray_bypass`, `mask_for_render` and `mask_block`, and the game tick now
splits its passes by each player's effective mode:

| level | effect |
|---|---|
| `off` | bypass — see everything |
| `engine` | force engine mode (only fully buried ores) |
| `culling` | force culling mode |
| `<n>` | culling with margin `n` blocks (a larger margin hides less) |
| `default` | follow `[anti_xray]` and the bypass rules |

An override also *turns anti-xray on* for an admin who holds an explicit bypass
grant (operators are not bypassed automatically; see
[Known Issues A.16](../KNOWN_ISSUES.md)), which is the point: a spectating admin
can lower the level and see exactly what a normal player would see. On change the columns the admin holds are
re-sent (the reachable set is invalidated first, so a margin change is
recomputed) and the override is dropped with the spectate snapshot.

## 6. Journal

Entering and leaving vanish and spectate, the auto-vanish, and every x-ray level
change are appended to the world journal as `EventBody::Custom` under a staff
tag (`TAG_CUSTOM_MIN`), payload `action\x1fdetail`, actor = the staff member's
UUID — so a review can answer "who changed what, when".

## 7. Testing

- `plugins/staff/tests/e2e.rs` drives the plugin through the real `HostBackend`
  with a recording stub: gating, persistence across a fresh plugin (a restart),
  the auto-vanish, the x-ray gate, the journal and the indicator.
- `aether-server` unit tests cover the pure visibility rule and the level
  parser; the visibility override and the restore are exercised through the same
  code the tick and render paths use.
