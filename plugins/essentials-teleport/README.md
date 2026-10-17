# essentials-teleport — homes, warps and plugin spawn (CMD-37a)

A **native** (tier N) Aether plugin: a Rust crate linked into the server
in-process. It owns the essentials *policy* — which homes/warps exist, the home
limit and the warmup — and calls the **core teleport service** (CMD-31) for the
move. It keeps no second teleport or collision implementation: the core's safe
spot search is what refuses lava, fire, cactus and the void.

- Task: [`docs/tasks/CMD-37a-teleport-homes-warps.md`](../../docs/tasks/CMD-37a-teleport-homes-warps.md)
- Umbrella: [`docs/tasks/CMD-37-essentials-teleport.md`](../../docs/tasks/CMD-37-essentials-teleport.md)
- CMD-37b (`/tpa`, `/back`, `/rtp`, vertical helpers) is a separate slice.

## Commands

| command | syntax | permission node |
|---|---|---|
| homes | `/sethome [<name>]` | `essentials.sethome` |
| | `/home [<name>]` | `essentials.home` |
| | `/delhome <name>` | `essentials.delhome` |
| | `/homes` | `essentials.homes` |
| warps | `/setwarp <name>` | `essentials.setwarp` |
| | `/warp <name>` | `essentials.warp` |
| | `/delwarp <name>` | `essentials.delwarp` |
| | `/warps` | `essentials.warps` |
| spawn | `/setspawn` | `essentials.setspawn` |

`/sethome` and `/home` without a name use `home`. A home needs **no bed or
anchor**: it records the player's current position. All command names are new;
the core names are never touched.

The admin commands (`/setwarp`, `/delwarp`, `/setspawn`) are routed by the core
through a public command-registry node and then gated by the plugin on their
`essentials.*` node. That split matters in this engine: a command registry op
level is a hard floor that a non-op permission grant cannot lower, while the
plugin-side `essentials.setwarp` / `essentials.delwarp` / `essentials.setspawn`
nodes default to operator-only and can be granted to a group explicitly.
`/warp`, the home commands and the listings are public by default, as in
EssentialsX; a deny still wins.

### `/spawn` stays core

`/spawn` belongs to the core (CMD-11). This plugin does **not** register or
override it, and `plugin.toml`/the manifest do not list it. `/setspawn` only
stores the plugin's spawn point in KV for a spawn provider (or an operator
tool) to read; it does not answer `/spawn`.

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

## Warmup

`warmup-ticks` delays `/home` and `/warp`; `warmup` (or `warmup-seconds`)
accepts seconds and is converted at 20 ticks per second. The default is `0`
(immediate). While a move is pending, it is cancelled if the player:

- moves more than `0.05` blocks (checked every tick),
- takes damage (`after-damage`),
- dies (`after-entity-death`),
- leaves the server.

The player is told the delay and the cancellation reason. The teleport itself
carries `TeleportCause::Plugin` and goes through CMD-31's safe-spot search.

## Names

Warp (and home) names are sanitised deterministically: ASCII letters/digits are
lower-cased, runs of other characters become `_`, leading/trailing separators
are trimmed and the result is capped at 32 characters. `"  My Warp!! "` is
stored as `my_warp`, and `/warp MY_WARP` finds it. An empty result is an error.
The sanitised name is what `/warps` lists.

## Persistence and ownership

State lives in plugin KV (P1) as three JSON values:

- `homes` — `owner -> name -> {world, x, y, z}`;
- `warps` — `name -> {world, x, y, z}`;
- `spawn` — one `{world, x, y, z}`.

The owner key is the player's UUID when the host exposes one, otherwise the
lower-cased login name. Homes are therefore **per player**: two players may
both own a home called `home`, and one can never replace, list or delete the
other's. A UUID also keeps the homes after a rename.

Every mutation serialises the complete new value and writes it with
`kv_try_put` **before** adopting it in memory. A failed write leaves the old,
still-persisted state in place and the command reports the error. `init`
replays the three keys, which is what makes homes, warps and the spawn survive
a restart. An unreadable homes/warps/spawn value refuses the plugin load rather
than silently starting empty and overwriting recoverable bytes.

The core teleport API is intra-world today; a home recorded in another world
is refused with a clear message instead of moving the player to the same
coordinates in the wrong dimension. A home in an unloaded chunk of the current
world is fine: CMD-31 loads/settles the destination through the normal teleport
path.

## Configuration

```toml
[plugins]
dir = "plugins"                # the subsystem master switch

[plugins.essentials-teleport]
enabled = true
# Native plugins ask the host for capabilities; the operator grants them here.
capabilities = [
  "log", "config", "supports", "storage.kv",
  "players", "players.teleport", "scheduler",
]
config = {
  max-homes = "1",             # default cap; a permission can raise it
  warmup-ticks = "0",          # or warmup = "3" (seconds)
}
```

`players.teleport` is needed because the move runs through the core service.
Grants are deny-by-default, exactly like every other plugin.

## Tests

```sh
cargo-locked test -p aether-essentials-teleport-plugin
```

The suite covers: no bed/anchor requirement, multiple homes up to the
permission limit (and refusal above it), unknown `/delhome`, unknown `/warp`,
`/setwarp` permission gating, sanitised and ordered warp listings, warmup
movement and damage cancellation, the safe teleport request to CMD-31 (and a
reported safe-spot failure), another world being refused, one player not
overwriting another's home, rename-by-UUID, and homes/warps/spawn persistence
across a restart.

## Wiring

The server compiles the plugin behind its `plugins` feature, adds it to
`install_native` in `crates/aether-server/src/plugins.rs`, and registers the
nine commands through the core command registry (the same ownership and
permission gate as a core command). It is disabled with
`[plugins.essentials-teleport] enabled = false`, and still requires
`[plugins] dir` to be set.
