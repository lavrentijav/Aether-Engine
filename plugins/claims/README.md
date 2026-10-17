# Claims plugin

GriefPrevention-style player-owned land for Aether, as a **native plugin**
(tier N, linked in-process). The fast region index and the protection verdict
are the core's (CMD-29a/29b); this plugin owns the claim model, the commands,
the limits and the golden shovel.

- Task: [CMD-35](../../docs/tasks/CMD-35-claims-plugin.md); slices
  [CMD-35a](../../docs/tasks/CMD-35a-claims-model.md) (model, shovel, limits)
  and [CMD-35b](../../docs/tasks/CMD-35b-claims-trust.md) (trust levels).
- Default limits, storage and trust are documented below.

## Commands

| command | syntax | permission |
|---|---|---|
| `/claim` | create a claim in the chunk you stand in, or inspect the claim there | `griefprevention.claim` |
| `/claim <radius>` | create a square claim (full blocks) around you | `griefprevention.claim` |
| `/abandonclaim` | give up the claim you stand in | `griefprevention.abandonclaim` |
| `/abandonallclaims` | give up every claim; the first run asks for a second within 30 s | `griefprevention.abandonallclaims` |
| `/claimslist [player]` | list your claims; a moderator may name another player | `griefprevention.claimslist` (+ `.others` or `griefprevention.admin`) |
| `/trust <player>` | give **build** trust in the claim you stand in | `griefprevention.trust` |
| `/containertrust <player>` | give **container** trust (chests, doors, buttons; no breaking) | `griefprevention.trust` |
| `/accesstrust <player>` | give **access** trust (enter only) | `griefprevention.trust` |
| `/managetrust <player>` | give **manage** trust (build plus changing this claim's trust) | `griefprevention.trust` |
| `/untrust <player>` | remove every trust level from one player | `griefprevention.trust` |
| `/untrust all` | remove every trustee from the claim you stand in | `griefprevention.trust` |
| golden shovel | right-click corner A, then corner B to claim; right-click an existing claim to inspect | `griefprevention.claim` |

`/claim` with no argument claims the whole 16×16 chunk containing the player's
feet. `/claim <radius>` is a square of side `2 * radius + 1` blocks. A claim is
a full-height column (`y = -64..=383`), so it covers the whole vertical stack of
the columns it names.

### Golden shovel

The first right-click stores a corner in memory and sends the coordinates. The
second click on a different block creates the rectangle spanning both corners.
Clicking the same corner again, an existing claim, or running
`/abandonallclaims` clears the pending selection. A mob/entity right-click never
claims: the plugin only subscribes to the block right-click event.
`minecraft:golden_shovel` is the only item that triggers claiming; any other
item falls through to vanilla behaviour.

### Trust levels

Trust is per claim and stored on the claim record. The levels are hierarchical;
each command replaces the player's previous level and `/untrust` removes every
level at once:

| level | may enter | open containers / use | break and place | change trust |
|---|---|---|---|---|
| access | yes | no | no | no |
| container | yes | yes | no | no |
| build | yes | yes | yes | no |
| manage | yes | yes | yes | yes |

Only the claim's owner or a player with **manage** trust may change the list.
`/untrust all` clears every trustee of the claim the caller stands in. The
core region check receives one focused grant per player (the `use` flag for
container trust, `use` + `build` for build/manage), so a container trustee can
open a chest but still cannot break; the claim's overall flags/defaults are
unchanged. An explicit region-wide deny still wins over a grant.

## Protection and precedence

Every claim is written to the core region index as a region with the owner as an
owner and the plugin's configured priority (default `10`). The core's normal
protection check then denies breaking, placing, using and pvp for non-members,
and the region survives a restart because it is replayed from the plugin KV at
startup.

- Overlapping **another claim** is refused, including your own.
- Overlapping a WorldGuard-style region (`plugins/regions`) is allowed; the core
  resolves the overlap by priority, ties by id. A claim at the default priority
  10 beats a region at the default priority 0, and loses to a region with a
  higher priority. Set `priority` if your region policy differs.
- `/abandonclaim` removes the region from the index at the core's next safe
  snapshot, exactly like any other region edit.

## Configuration

Native plugins read their values from `[plugins.claims] config`; values are
strings in TOML.

```toml
[plugins]
dir = "plugins"

[plugins.claims]
enabled = true
config = { max-claims = "10", max-claim-size = "10000", max-claim-blocks = "100000", max-radius = "64", priority = "10" }
```

| key | default | meaning |
|---|---|---|
| `max-claims` | `10` | claims a single player may own |
| `max-claim-size` | `10000` | maximum area of one claim, in blocks |
| `max-claim-blocks` | `100000` | maximum total area one player may claim, in blocks (alias `max-accrued-blocks`) |
| `max-radius` | `64` | largest accepted `/claim <radius>` |
| `priority` | `10` | region priority written to the core index |

A claim over either area limit is refused with the number of blocks over the
limit (the shortfall), and a smaller claim still succeeds. An invalid or
too-large radius is refused before anything is written.

## Persistence and files

Claim state lives in the plugin KV (`storage.kv`) under `claim:<id>` as one
compact JSON record, plus `next-id`. Ids live in a claims-namespaced range
(`0x434C4149_00000000 | serial`); the serial comes from the host's process-wide
atomic allocator, so two plugin instances cannot pick the same id, and the
persisted `next-id` raises its floor so a restart never reuses one. A fresh
claim insert is atomic in the core store and refuses **any** occupied id (even
the plugin's own), so two players racing for the same land or id produce exactly
one claim; a refused id is retried rather than silently persisted. On startup
the plugin loads every record and replays it with the idempotent same-id upsert,
reassigning any record whose id another plugin now owns. Abandoning a claim
deletes both the KV record and its region; `/abandonallclaims` is intentionally
two-step so a typo cannot wipe a player's land. Trust is stored in the same
record, so it survives a restart; a trust edit publishes the new region first
and rolls it back if the KV write fails, and a process-wide lock serialises the
read-modify-write so two concurrent trust commands cannot lose a trustee.
Pending shovel corners and armed confirmations are cleared when the player
leaves.

Files:

```text
plugins/claims/
  Cargo.toml
  plugin.toml
  src/lib.rs          claim model, commands, trust, shovel, persistence
  tests/e2e.rs        policy tests through the host trait and the core check
```

The plugin implementation is the workspace crate `aether-claims-plugin`; the
server links it in `install_native` behind its `plugins` feature.
