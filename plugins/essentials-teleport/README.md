# essentials-teleport — homes, warps, requests, /back and vertical helpers (CMD-37a/b)

A **native** (tier N) Aether plugin: a Rust crate linked into the server
in-process. It owns the essentials *policy* — which homes/warps exist, the home
limit, request timeouts, cooldowns and warmups — and calls the **core teleport
service** (CMD-31) for every move. It keeps no second teleport or collision
implementation: the core's safe-spot search is what refuses lava, fire, cactus
and the void, and the same search backs `/top`, `/jump`, `/up` and `/rtp`.

- CMD-37a: [`docs/tasks/CMD-37a-teleport-homes-warps.md`](../../docs/tasks/CMD-37a-teleport-homes-warps.md)
- CMD-37b: [`docs/tasks/CMD-37b-teleport-requests.md`](../../docs/tasks/CMD-37b-teleport-requests.md)
- Umbrella: [`docs/tasks/CMD-37-essentials-teleport.md`](../../docs/tasks/CMD-37-essentials-teleport.md)

## Commands

| group | command | syntax | permission node |
|---|---|---|---|
| homes | | `/sethome [<name>]` | `essentials.sethome` |
| | | `/home [<name>]` | `essentials.home` |
| | | `/delhome <name>` | `essentials.delhome` |
| | | `/homes` | `essentials.homes` |
| warps | | `/setwarp <name>` | `essentials.setwarp` |
| | | `/warp <name>` | `essentials.warp` |
| | | `/delwarp <name>` | `essentials.delwarp` |
| | | `/warps` | `essentials.warps` |
| spawn | | `/setspawn` | `essentials.setspawn` |
| requests | | `/tpa <player>` | `essentials.tpa` |
| | | `/tpahere <player>` | `essentials.tpahere` |
| | | `/tpaccept [<player>]` | `essentials.tpaccept` |
| | | `/tpdeny [<player>]` | `essentials.tpdeny` |
| | | `/tpcancel` | `essentials.tpcancel` |
| ops | | `/tpall` | `essentials.tpall` |
| | | `/tphere <player>` | `essentials.tphere` |
| | | `/tppos <x> <y> <z>` | `essentials.tppos` |
| back | | `/back` | `essentials.back` |
| vertical | | `/top` | `essentials.top` |
| | | `/jump` | `essentials.jump` |
| | | `/up [<n>]` | `essentials.up` |
| random | | `/rtp [<radius>]` | `essentials.rtp` |

`/sethome` and `/home` without a name use `home`; `/delhome` always requires a
name (there is no "delete my default home" shortcut). A home needs **no bed or
anchor**: it records the player's current position. All command names are new;
the core names are never touched.

The admin commands (`/setwarp`, `/delwarp`, `/setspawn`, `/tpall`, `/tphere`,
`/tppos`) are routed by the core through a public command-registry node and
then gated by the plugin on their `essentials.*` node. That split matters in
this engine: a command registry op level is a hard floor that a non-op
permission grant cannot lower, while the plugin-side `essentials.*` nodes
default to operator-only and can be granted to a group explicitly. The
player-facing commands are public by default, as in EssentialsX; a deny still
wins.

### `/spawn` stays core

`/spawn` belongs to the core (CMD-11). This plugin does **not** register or
override it, and `plugin.toml`/the manifest do not list it. `/setspawn` only
stores the plugin's spawn point in KV for a spawn provider (or an operator
tool) to read; it does not answer `/spawn`.

## Teleport requests

`/tpa <player>` asks the target to let the **requester** teleport to them;
`/tpahere <player>` asks the target to teleport to the **requester**. Nothing
moves until the target accepts:

- `/tpaccept` accepts the newest incoming request; `/tpaccept <player>` accepts
  that requester's request.
- `/tpdeny` denies every incoming request; `/tpdeny <player>` denies one.
- `/tpcancel` drops all requests the caller sent and tells each target.
- A request to yourself is refused, a target that is not online is refused,
  and a target whose CMD-40 ignore list contains the requester is refused
  (read through the `player_ignores` host hook; the default is nobody).
- Several players may have a live request to the same target. A new request
  from the same requester replaces the old one.
- `request-timeout` (or `request-timeout-ticks`) expires requests; both sides
  are notified. A requester who leaves or disconnects has their requests
  dropped.
- Requests are session state: they do not survive a restart, and accepting
  after the requester went offline is an error.

An accepted `tpa`/`tpahere` goes through the same warmup and safe-spot path as
a home or warp.

## `/back`

`/back` returns to the player's last death or pre-teleport position:

- a death stores the death location;
- every successful teleport (home, warp, request, ops move, `/top`, `/jump`,
  `/up`, `/tppos`, `/rtp`) stores where the player was before it;
- `/back` itself is a teleport, so using it again ping-pongs between the last
  two locations, as EssentialsX does.

With no history the command errors and does not move anyone. Back locations
are persisted per player (UUID when the host exposes one, lower-cased name
otherwise) in plugin KV and replayed on `init`.

## Vertical helpers and `/rtp`

All four use CMD-31 rather than re-deriving collision (`/up` uses its exact
teleport path after the plugin has validated the destination cells, as noted
below):

- `/top` asks the core for the highest safe spot in the player's column,
  starting from the world's maximum Y. It refuses a solid column (for example
  bedrock all the way to the build limit), never puts a body in lava or fire,
  and stands on top of leaves rather than inside them.
- `/jump` casts the player's look ray (yaw/pitch) up to `jump-distance`
  blocks, and moves to the safe spot above the first solid block it hits. With
  no block ahead it reports that and does not move.
- `/up [<n>]` climbs at most `n` blocks (default 1), stopping before the first
  cell whose feet or head is solid or hazardous. `n` is bounded by `max-up`;
  it never teleports into a solid block. The plugin validates the final two
  body cells against the same collision/hazard rules as CMD-31 and then asks
  the host for an **exact** move (centred on the column). The safe search
  cannot be used for the target: its feet cell is air by definition, so it
  would snap back onto the floor below, and `/up 1` would not move at all on
  ordinary full-block ground.
- `/rtp [<radius>]` asks CMD-31's random search for a safe spot between 1 and
  the radius (default and maximum `rtp-radius`). If the core's bounded attempt
  budget finds nothing, the command reports it and does not teleport.

## Warmup and cooldowns

`warmup-ticks` delays a move; `warmup` (or `warmup-seconds`) accepts seconds
and is converted at 20 ticks per second. The default is `0` (immediate). While
a move is pending it is cancelled if the player:

- moves more than `0.05` blocks (checked every tick),
- takes damage (`after-damage`),
- dies (`after-entity-death`),
- leaves the server.

`cooldown` (or `cooldown-ticks`) adds a per-player, per-command cooldown; the
default is `0`. A command's cooldown and warmup are both bypassed by
`essentials.<command>.bypass` (for example `essentials.tpa.bypass`,
`essentials.back.bypass`), on top of the operator bypass.

The player is told the delay, the remaining cooldown and the cancellation
reason. The teleport itself carries `TeleportCause::Plugin` and goes through
CMD-31's safe-spot search.

## Home limit

The default is one home (`max-homes`). A permission raises the cap:

| node | meaning |
|---|---|
| `essentials.sethome.multiple.<n>` | allow up to `n` homes (probes 1..=64) |
| `essentials.sethome.multiple` | unlimited homes |
| `essentials.sethome.unlimited` | alias for unlimited |

The limit applies to a **new** name only; replacing a home the player already
owns always works. `/homes` shows `(used/limit)`. Operators bypass the
plugin-side gates, like the other native plugins.

## Names

Warp (and home) names are sanitised deterministically: ASCII letters/digits are
lower-cased, runs of other characters become `_`, the result is capped at 32
characters and leading/trailing separators are trimmed **after** the cap, so
the stored key is a fixed point and can always be typed back in. For example
`/setwarp My_Warp!!` stores `my_warp`, and `/warp MY_WARP` finds it. An empty
result is an error. The sanitised name is what `/warps` lists.

A name is a **single command token**: the server splits plugin-command
arguments on whitespace, so a space-containing name cannot be entered in game
(`/setwarp "My Warp!"` arrives as two tokens and is rejected). Use letters,
digits, `_` and `-`; anything else is folded to `_`.

## Persistence and ownership

State lives in plugin KV (P1) as four JSON values:

- `homes` — `owner -> name -> {world, x, y, z}`;
- `warps` — `name -> {world, x, y, z}`;
- `spawn` — one `{world, x, y, z}`;
- `back` — `owner -> {world, x, y, z}`.

The owner key is the player's UUID when the host exposes one, otherwise the
lower-cased login name. Homes and back locations are therefore **per player**:
two players may both own a home called `home`, and one can never replace, list
or delete the other's. A UUID also keeps the data after a rename.

Every mutation serialises the complete new value and writes it with
`kv_try_put` **before** adopting it in memory. A failed write leaves the old,
still-persisted state in place and the command reports the error. `init`
replays the keys, which is what makes homes, warps, the spawn and `/back`
survive a restart. An unreadable value refuses the plugin load rather than
silently starting empty and overwriting recoverable bytes. Requests and
cooldowns are in-memory session state by design.

The core teleport API is intra-world today; a home, warp, back location or
`/tppos` destination in another world is refused with a clear message instead
of moving the player to the same coordinates in the wrong dimension. A
destination in an unloaded chunk of the current world is fine: CMD-31
loads/settles it through the normal teleport path.

## Configuration

```toml
[plugins]
dir = "plugins"                # the subsystem master switch

[plugins.essentials-teleport]
enabled = true
# Native plugins ask the host for capabilities; the operator grants them here.
capabilities = [
  "log", "config", "supports", "storage.kv",
  "players", "players.teleport", "world.read", "scheduler",
]
config = {
  max-homes = "1",              # default cap; a permission can raise it
  warmup = "0",                 # seconds, or warmup-ticks = "0"
  cooldown = "0",               # seconds, or cooldown-ticks = "0"
  request-timeout = "60",       # seconds, or request-timeout-ticks
  rtp-radius = "1000",          # default and maximum /rtp radius
  max-up = "64",                # /up limit
  jump-distance = "20",         # /jump reach in blocks
}
```

`players.teleport` is needed because every move runs through the core service;
`world.read` is needed by `/top`, `/jump`, `/up` and `/rtp`. Grants are
deny-by-default, exactly like every other plugin.

## Tests

```sh
cargo-locked test -p aether-essentials-teleport-plugin
```

The suite covers: no bed/anchor requirement, multiple homes up to the
permission limit (and refusal above it), replacing an existing home while at
the limit, a nameless `/delhome` showing usage, unknown `/delhome`/`/warp`/
`/delwarp`, `/setwarp`/`/delwarp`/`/setspawn` permission gating, sanitised and
ordered warp listings (including a name capped on a separator), warmup
cancellation on movement, damage, death and leaving, interleaved players with
separate pending moves, the safe teleport request to CMD-31 (and a reported
safe-spot failure), another world being refused, one player not overwriting
another's home, rename-by-UUID, and homes/warps/spawn/back persistence across
a restart. CMD-37b adds: `/tpa` to self/offline/ignored, accept without a
request, accept after the requester went offline, request expiry, multiple
requests and `/tpaccept <player>`, `/tpahere`, `/tpcancel`, `/tpdeny` of all,
cooldowns and the bypass node, `/back` after death and after a teleport (and
no-history), `/top` on lava/leaves/a solid ceiling, `/jump` with no block
ahead, `/up` stopping at the first blocked cell and its limit, `/tppos`
absolute/relative, `/rtp` success/failure and radius bound, `/tpall` and
`/tphere`, overlapping requests, and every new command's permission node.

## Wiring

The server compiles the plugin behind its `plugins` feature, adds it to
`install_native` in `crates/aether-server/src/plugins.rs`, and registers the
twenty-two commands through the core command registry (the same ownership and
permission gate as a core command). The `world.read`, safe-spot and random-
safe-spot host hooks front CMD-31's `find_safe_spot` / `random_safe_spot`; the
plugin never copies the search. `/up` uses the host's exact-teleport path only
after checking its two body cells; every other move takes the safe path. It is disabled with
`[plugins.essentials-teleport] enabled = false`, and still requires
`[plugins] dir` to be set.
