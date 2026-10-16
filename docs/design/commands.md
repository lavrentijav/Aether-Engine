# Commands — the full catalogue, core vs plugin (draft decisions)

Status: 📋 design, 2026-10-15. Goal: everything a server owner expects from vanilla + the usual plugin
stack (Paper/Spigot built-ins, EssentialsX, LuckPerms, WorldEdit, WorldGuard, CoreProtect,
GriefPrevention, chat/moderation, shops), split by the rule in `CONTRIBUTING.md` "Core or plugin?":
**core** = vanilla behaviour, server operation, hot-path or data-integrity mechanisms;
**plugin** = optional features that are not performance-critical. A plugin may own the command while the
core owns the mechanism it needs (marked "core API + plugin").

Already in Aether (commands/mod.rs, game/commands.rs, economy/commands.rs): `/gamemode` (`gm`), `/time`
(`day/night/noon/midnight`), `/give`, `/summon`, `/teleport` (`tp`), `/heal`, `/food`, `/kill`, `/killall`,
`/spawn`, `/inv` (`inventory`), `/stash`, `/audit`, `/inspect`, `/rollback` (`rb`), `/cache`, `/msg`
(`tell`, `w`), economy `/balance` (`bal`, `money`) `/pay /buy /sell /market /listings /unlist /econ`,
`/visibility`.

## 0.5 The command catalogue — one list (current state)

The single source of truth for **which commands exist** is
`crates/aether-server/src/commands/catalogue.rs` (`CATALOGUE`): one entry per command with
`name`, `aliases`, `node`, `op`, `task` (`CMD-nn` or `-`), `category`, `status`
(`Implemented` | `Pending`) and `gate`. The command tree's pending half is built from it
(`commands/tree.rs`), the Declare Commands packet and `/help` read it, and `docs/COMMANDS.md` +
`docs/COMMANDS.ru.md` are generated from it (`cargo-locked run -p aether-server --example
gen_commands_md`) and byte-compared by a test, so the docs cannot drift. This section holds the
*design decisions*; `COMMANDS.md` is the *current state* list — the two must not contradict each
other. There is no second list: `commands/registry.rs` holds only the plugin-command side.

Categories are the fixed set `admin, server, player, world, data, ops, history, economy` (the
values the catalogue's `Category` uses, reused as the `/help` grouping). A pending command is still
declared in the tree (so it autocompletes and is permission-gated by its `node`/`op`) and answers
`/<name> is not implemented yet (CMD-nn)`; `/help` marks it as not implemented and only lists
commands the caller may run.

The `gate` field records what the tree enforces **today**: `node` (the declared node/op are
enforced — every pending command), `operator` (the legacy operator-only check; `node`/`op` are the
intended target, not yet enforced — e.g. `/gamemode`, `/give`, `/aether`), or `everyone` (no
requirement; `node`/`op` are the intended target — e.g. `/inspect`, `/kill`, `/spawn`, the economy
verbs). This is deliberate: the brief forbids changing access rights silently, so the catalogue is
brought in line with the real gate instead of claiming a node the tree does not check. A guard
(`an_implemented_entry_gate_matches_the_tree`) fails the build if `gate` and the tree disagree.

**To add a command:** one entry in `CATALOGUE`. If it is not written yet, set `status = Pending` and
its `CMD-nn` task; if it is, set `status = Implemented`, `task = "-"`, and wire the executor in the
owning module's `nodes()` (`game/commands.rs`, `economy/commands.rs` or `commands/core_nodes()`),
choosing `imp` (everyone), `imp_op` (operator) or a node gate. **To flip planned → working:** change
the entry's `status` (and `task`) and add the executor — no second list, no hardcoded
`"Implemented:"` string. The tests in `catalogue.rs` fail the build when the catalogue and the tree
disagree.

## 0. Command infrastructure (core, first)

| Piece | Decision |
|---|---|
| Brigadier command tree | **Core.** Declare Commands packet with argument types per protocol (all codecs), server-side parsing/suggestions, `execute`-style chaining later. Plugins register nodes through the plugin API (P3 registries). |
| Permissions | **Core API** (nodes, op levels 0–4, groups, inheritance, per-world context, default `true/false/op`); checked on the hot path. **LuckPerms-style management UI (`/lp …`, tracks, meta, web editor) = plugin.** |
| Selectors `@a @p @r @s @e[...]`, coordinates `~ ^`, NBT/component args | **Core**, shared parser. |
| Command feedback, `sendCommandFeedback`, `/help` generation from the tree, aliases | **Core.** |
| Console / RCON | **Core** (RCON protocol optional, off by default). |

## 1. Vanilla commands (all core)

`advancement` `attribute` `ban` `ban-ip` `banlist` `bossbar` `clear` `clone` `damage` `data` `datapack`
`debug` `defaultgamemode` `deop` `dialog` `difficulty` `effect` `enchant` `execute` `experience`/`xp`
`fill` `fillbiome` `forceload` `function` `gamemode` `gamerule` `give` `help` `item` `jfr` `kick` `kill`
`list` `locate` `loot` `me` `msg`/`tell`/`w` `op` `pardon` `pardon-ip` `particle` `perf` `place`
`playsound` `publish` `random` `recipe` `reload` `return` `ride` `rotate` `save-all` `save-off` `save-on`
`say` `schedule` `scoreboard` `seed` `setblock` `setidletimeout` `setworldspawn` `spawnpoint` `spectate`
`spreadplayers` `stop` `stopsound` `summon` `tag` `team` `teammsg`/`tm` `teleport`/`tp` `tellraw` `test`
`tick` `time` `title` `transfer` `trigger` `version` `waypoint` `weather` `whitelist` `worldborder`.

Groups for tasks: **V-admin** (op/deop/ban/kick/whitelist/stop/save/list/seed/version/setidletimeout/transfer),
**V-player** (gamemode/give/clear/effect/enchant/xp/kill/tp/spawnpoint/spectate/ride/attribute/damage/item),
**V-world** (setblock/fill/clone/fillbiome/place/forceload/locate/setworldspawn/worldborder/weather/time/gamerule/difficulty/tick),
**V-data** (data/scoreboard/tag/team/trigger/bossbar/title/tellraw/say/me/msg/teammsg/particle/playsound/stopsound/loot/recipe/advancement/random/schedule/function/datapack/reload/return/execute).
`execute`/`function`/`datapack` are large (datapack functions): last.

## 2. Server operation (core)

`/tps` `/mspt` `/plugins` (`/pl`) `/aether profile` (sub-tick profiler) `/aether backup` `/restart`
(supervised by systemd) `/gc`-style memory info, `/aether reload <part>`, `/visibility`, `/cache`,
`/aether world …` (list/load/unload worlds once multi-world exists).

## 3. History & protection (core mechanism)

| Commands | Decision |
|---|---|
| CoreProtect-like `/inspect` `/lookup` (`/co l u:<p> t:<time> r:<radius> a:<action>`) `/rollback` `/restore` `/purge` | **Core** — the journal is core; extend the existing `/inspect` `/audit` `/rollback` with lookup filters, restore, purge, preview. |
| WorldGuard regions `/rg define/redefine/remove/flag/addmember/addowner/info/list/priority` | **Core API + plugin**: the core provides a fast region index (per-chunk R-tree) and the protection *check* on block/entity/interaction events (hot path); the region model, flags and `/rg` commands are a plugin. |
| GriefPrevention claims `/claim` `/trust` `/untrust` `/abandonclaim` `/claimslist`, golden-shovel claiming | **Plugin** on the same region API. |
| WorldEdit `//wand //pos1 //pos2 //set //replace //walls //outline //copy //cut //paste //rotate //flip //stack //move //undo //redo //expand //contract //schem` brushes | **Core API + plugin**: the core provides a **bulk edit API** (batched multi-block writes, one journal operation, chunk-resend batching, undo via the journal) — millions of blocks are a perf path; selection/clipboard/commands/brushes/schematics are a plugin. |

## 4. EssentialsX-family (plugins)

| Plugin | Commands |
|---|---|
| **essentials-teleport** | `/home /sethome /delhome /homes` `/warp /setwarp /delwarp /warps` `/tpa /tpahere /tpaccept /tpdeny /tpcancel /tpall /tphere /tppos` `/back` `/top` `/jump` `/up` `/rtp` (random teleport, safe spot) `/spawn /setspawn` (plugin overrides core `/spawn` if installed) |
| **essentials-player** | `/fly /god /speed /feed /heal /repair /hat /nick /realname /whois /seen /afk /near /getpos /depth /compass /ptime /pweather /suicide /burn /thru /skull /more /condense /exp` |
| **essentials-inv** | `/invsee /enderchest (/ec) /workbench (/craft) /anvil /stonecutter /grindstone /trash (/disposal) /clearinventory (/ci) /kit /kits /createkit` |
| **essentials-chat** | `/msg /r /mail /ignore /socialspy /broadcast /motd /rules /me` formatting, prefixes from permissions meta |
| **moderation** | `/mute /tempmute /unmute /warn /warnings /tempban /jail /unjail /vanish (/v) /sudo /freeze /clearchat /slowmode /staffchat /report /reports /history <player>` (bans/kick stay core; temp/IP-range variants here) |
| **economy** (P2, existing logic) | `/balance (/bal) /pay /baltop /eco give|take|set /worth` + existing market `/buy /sell /market /listings /unlist` |
| **shops** | `/shop` chest shops, `/auction` (`/ah`), `/sell hand|all`, admin shops — on the economy service |
| **social** | `/party /friend /tpparty` |
| **integrations** | `/discord` (chat bridge), `/vote` (NuVotifier protocol), `/link` account linking, web stats |
| **fun/cosmetic** | `/firework /lightning (/smite) /tree /bigtree /book /powertool /particles /pet` |
| **stats** | `/stats /leaderboard /playtime` (reads the journal via PA2) |

## 5. Plugin-side needs (feed into P3 and PA1–PA3)

Command registration with permission nodes + suggestions; player meta storage (homes, nicknames) in
plugin KV; teleport with safe-spot search (core API); temporary effects/flight/speed through a player
API; inventory views for `/invsee` (PA1 windows over another player's inventory); scheduled tasks;
cross-plugin service lookup (economy, permissions, regions).

## 6. Order

1. Core infra: Brigadier tree + permissions core + selectors (prerequisite for everything).
2. Vanilla groups V-admin → V-player → V-world → V-data (execute/function/datapack last).
3. Server ops; history extensions (lookup/restore/purge).
4. Core APIs for plugins: bulk edit, region index + protection check, teleport/safe-spot, player API.
5. Plugins (after P1/P2/P3 merge): essentials-teleport, essentials-player, essentials-inv, chat,
   moderation, economy (P2), permissions UI, regions (WG), claims (GP), worldedit, shops, the rest.
