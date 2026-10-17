<!-- GENERATED FILE — do not edit by hand.
     Source:     crates/aether-server/src/commands/catalogue.rs
     Regenerate: cargo-locked run -p aether-server --example gen_commands_md -->

# Aether Engine — Command Catalogue

> **Русская версия:** [COMMANDS.ru.md](COMMANDS.ru.md)

Every command Aether knows, grouped by category: the ones that run today and the ones that are only specified. It is generated from one list in the server (`crates/aether-server/src/commands/catalogue.rs`), so nothing is listed twice and a planned command cannot quietly disappear.

*This file is generated — do not edit it by hand. Regenerate with `cargo-locked run -p aether-server --example gen_commands_md`.*

**Legend:** ✅ working · 🚧 pending (`CMD-nn` is the task that will implement it). **Node** is the permission node and **Op** the vanilla operator level (0 everyone … 4 owner); for a pending command the stub enforces exactly them. **Gate** is what the server checks *today*: `node` (enforced node/op), `operator` (the legacy operator-only check — Node/Op are the intended target, not yet enforced), or `everyone` (no requirement; Node/Op are the intended target).

**Totals:** 113 commands — 45 working, 68 pending.

## Contents

- [admin](#admin) — Player and access management. (10)
- [server](#server) — Server control and information. (10)
- [player](#player) — Player verbs: inventory, movement, effects and items. (22)
- [world](#world) — Blocks, environment and world rules. (16)
- [data](#data) — Data, scoreboard, teams and chat. (27)
- [ops](#ops) — Server diagnostics and lifecycle. (11)
- [history](#history) — World history and protection. (9)
- [economy](#economy) — The economy. (8)

## admin

Player and access management.

| Command | Aliases | Status | Gate | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- | --- |
| `/help` | `?` | ✅ working | `everyone` | `minecraft.command.help` | 0 | `/help [command]` |
| `/op` | – | ✅ working | `node` | `minecraft.command.op` | 3 | `/op <targets>` |
| `/deop` | – | ✅ working | `node` | `minecraft.command.deop` | 3 | `/deop <targets>` |
| `/ban` | – | ✅ working | `node` | `minecraft.command.ban` | 3 | `/ban <targets> [reason]` |
| `/ban-ip` | – | ✅ working | `node` | `minecraft.command.ban-ip` | 3 | `/ban-ip <targets> [reason]` |
| `/banlist` | – | ✅ working | `node` | `minecraft.command.banlist` | 3 | `/banlist [ips\|players]` |
| `/pardon` | – | ✅ working | `node` | `minecraft.command.pardon` | 3 | `/pardon <targets>` |
| `/pardon-ip` | – | ✅ working | `node` | `minecraft.command.pardon-ip` | 3 | `/pardon-ip <targets>` |
| `/kick` | – | ✅ working | `node` | `minecraft.command.kick` | 3 | `/kick <targets> [reason]` |
| `/whitelist` | – | ✅ working | `node` | `minecraft.command.whitelist` | 3 | `/whitelist <on\|off\|list\|add\|remove\|reload>` |

## server

Server control and information.

| Command | Aliases | Status | Gate | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- | --- |
| `/stop` | – | ✅ working | `node` | `minecraft.command.stop` | 4 | `/stop` |
| `/save-all` | – | ✅ working | `node` | `minecraft.command.save-all` | 4 | `/save-all` |
| `/save-on` | – | 🚧 pending CMD-09 | `node` | `minecraft.command.save-on` | 4 | `/save-on` |
| `/save-off` | – | 🚧 pending CMD-09 | `node` | `minecraft.command.save-off` | 4 | `/save-off` |
| `/list` | – | 🚧 pending CMD-09 | `node` | `minecraft.command.list` | 0 | `/list [uuids]` |
| `/seed` | – | 🚧 pending CMD-09 | `node` | `minecraft.command.seed` | 2 | `/seed` |
| `/version` | – | 🚧 pending CMD-09 | `node` | `minecraft.command.version` | 2 | `/version` |
| `/setidletimeout` | – | 🚧 pending CMD-09 | `node` | `minecraft.command.setidletimeout` | 3 | `/setidletimeout <minutes>` |
| `/transfer` | – | 🚧 pending CMD-09 | `node` | `minecraft.command.transfer` | 3 | `/transfer <hostname> [port] [players]` |
| `/publish` | – | 🚧 pending CMD-09 | `node` | `minecraft.command.publish` | 4 | `/publish [allow-commands] [gamemode] [port]` |

## player

Player verbs: inventory, movement, effects and items.

| Command | Aliases | Status | Gate | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- | --- |
| `/gamemode` | `gm` | ✅ working | `operator` | `minecraft.command.gamemode` | 2 | `/gamemode <mode> [player]` |
| `/give` | – | ✅ working | `operator` | `minecraft.command.give` | 2 | `/give <item> [count]` |
| `/summon` | – | ✅ working | `operator` | `minecraft.command.summon` | 2 | `/summon <entity> [pos]` |
| `/killall` | – | ✅ working | `operator` | `aether.command.killall` | 2 | `/killall` |
| `/kill` | – | ✅ working | `everyone` | `minecraft.command.kill` | 2 | `/kill [targets]` |
| `/spawn` | – | ✅ working | `everyone` | `aether.command.spawn` | 0 | `/spawn` |
| `/teleport` | `tp` | ✅ working | `operator` | `minecraft.command.teleport` | 2 | `/teleport <target>` |
| `/block` | – | ✅ working | `everyone` | `aether.command.block` | 0 | `/block <x> <y> <z>` |
| `/food` | – | ✅ working | `operator` | `aether.command.food` | 2 | `/food [level]` |
| `/heal` | – | ✅ working | `operator` | `aether.command.heal` | 2 | `/heal` |
| `/inv` | `inventory` | ✅ working | `everyone` | `aether.command.inventory` | 0 | `/inv` |
| `/clear` | – | 🚧 pending CMD-10 | `node` | `minecraft.command.clear` | 2 | `/clear [targets] [item] [max-count]` |
| `/effect` | – | 🚧 pending CMD-10 | `node` | `minecraft.command.effect` | 2 | `/effect <targets> <effect> [seconds] [amplifier]` |
| `/enchant` | – | 🚧 pending CMD-10 | `node` | `minecraft.command.enchant` | 2 | `/enchant <targets> <enchantment> [level]` |
| `/experience` | `xp` | 🚧 pending CMD-10 | `node` | `minecraft.command.experience` | 2 | `/experience <targets> <amount> [points,levels]` |
| `/attribute` | – | 🚧 pending CMD-10 | `node` | `minecraft.command.attribute` | 2 | `/attribute <target> <attribute> <get,base> [value]` |
| `/damage` | – | 🚧 pending CMD-10 | `node` | `minecraft.command.damage` | 2 | `/damage <target> <amount> [type]` |
| `/spreadplayers` | – | 🚧 pending CMD-11 | `node` | `minecraft.command.spreadplayers` | 2 | `/spreadplayers <center> <spread> <max-range> [teams]` |
| `/spawnpoint` | – | 🚧 pending CMD-11 | `node` | `minecraft.command.spawnpoint` | 2 | `/spawnpoint [target] [pos]` |
| `/ride` | – | 🚧 pending CMD-11 | `node` | `minecraft.command.ride` | 2 | `/ride <target> <vehicle>` |
| `/item` | – | 🚧 pending CMD-11 | `node` | `minecraft.command.item` | 2 | `/item <target> <slot> <replace,modify> [...]` |
| `/rotate` | – | 🚧 pending CMD-11 | `node` | `minecraft.command.rotate` | 2 | `/rotate <target> <yaw> <pitch>` |

## world

Blocks, environment and world rules.

| Command | Aliases | Status | Gate | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- | --- |
| `/time` | – | ✅ working | `operator` | `minecraft.command.time` | 2 | `/time [set] <day,noon,night,midnight,ticks>` |
| `/setblock` | – | ✅ working | `node` | `minecraft.command.setblock` | 2 | `/setblock <pos> <block> [destroy\|keep\|replace\|strict]` |
| `/fill` | – | ✅ working | `node` | `minecraft.command.fill` | 2 | `/fill <from> <to> <block> [destroy\|hollow\|keep\|outline\|replace\|strict] \| <from> <to> <block> replace <filter> [destroy\|hollow\|keep\|outline\|strict]` |
| `/clone` | – | ✅ working | `node` | `minecraft.command.clone` | 2 | `/clone [from <dimension>] <begin> <end> [to <dimension>] <destination> [strict] [replace\|masked\|filtered <filter>] [force\|move\|normal]` |
| `/fillbiome` | – | 🚧 pending CMD-12 | `node` | `minecraft.command.fillbiome` | 2 | `/fillbiome <from> <to> <biome>` |
| `/place` | – | 🚧 pending CMD-12 | `node` | `minecraft.command.place` | 2 | `/place <feature,template> <pos> [...]` |
| `/forceload` | – | 🚧 pending CMD-12 | `node` | `minecraft.command.forceload` | 2 | `/forceload <add,remove,query> <from> [to]` |
| `/locate` | – | 🚧 pending CMD-13 | `node` | `minecraft.command.locate` | 2 | `/locate <structure,biome,poi> [...]` |
| `/setworldspawn` | – | ✅ working | `node` | `minecraft.command.setworldspawn` | 2 | `/setworldspawn [<pos>] [<angle>]` |
| `/worldborder` | – | ✅ working | `node` | `minecraft.command.worldborder` | 2 | `/worldborder <add,set,center,damage,get,warning> [...]` |
| `/weather` | – | ✅ working | `node` | `minecraft.command.weather` | 2 | `/weather <clear,rain,thunder> [<duration>]` |
| `/gamerule` | – | 🚧 pending CMD-13 | `node` | `minecraft.command.gamerule` | 2 | `/gamerule <rule> [value]` |
| `/difficulty` | – | 🚧 pending CMD-13 | `node` | `minecraft.command.difficulty` | 2 | `/difficulty <peaceful,easy,normal,hard>` |
| `/tick` | – | 🚧 pending CMD-13 | `node` | `minecraft.command.tick` | 3 | `/tick <rate,freeze,step,sprint,unfreeze,query> [...]` |
| `/defaultgamemode` | – | ✅ working | `node` | `minecraft.command.defaultgamemode` | 2 | `/defaultgamemode <gamemode>` |
| `/waypoint` | – | 🚧 pending CMD-13 | `node` | `minecraft.command.waypoint` | 2 | `/waypoint <list,modify>` |

## data

Data, scoreboard, teams and chat.

| Command | Aliases | Status | Gate | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- | --- |
| `/msg` | `tell`, `w` | ✅ working | `everyone` | `minecraft.command.msg` | 0 | `/msg <player> <message>` |
| `/data` | – | 🚧 pending CMD-14 | `node` | `minecraft.command.data` | 2 | `/data <get,merge,modify,remove> <target> <path> [...]` |
| `/scoreboard` | – | 🚧 pending CMD-15 | `node` | `minecraft.command.scoreboard` | 2 | `/scoreboard <objectives,players> [...]` |
| `/tag` | – | 🚧 pending CMD-16 | `node` | `minecraft.command.tag` | 2 | `/tag <targets> <add,remove,list> <name>` |
| `/team` | – | 🚧 pending CMD-16 | `node` | `minecraft.command.team` | 2 | `/team <add,list,modify,join,leave,remove> [...]` |
| `/teammsg` | `tm` | 🚧 pending CMD-16 | `node` | `minecraft.command.teammsg` | 0 | `/teammsg <message>` |
| `/say` | – | 🚧 pending CMD-17 | `node` | `minecraft.command.say` | 2 | `/say <message>` |
| `/me` | – | 🚧 pending CMD-17 | `node` | `minecraft.command.me` | 0 | `/me <action>` |
| `/tellraw` | – | 🚧 pending CMD-17 | `node` | `minecraft.command.tellraw` | 2 | `/tellraw <targets> <message>` |
| `/title` | – | 🚧 pending CMD-17 | `node` | `minecraft.command.title` | 2 | `/title <targets> <title,subtitle,actionbar,times,reset> [...]` |
| `/bossbar` | – | 🚧 pending CMD-17 | `node` | `minecraft.command.bossbar` | 2 | `/bossbar <add,get,list,remove,set> [...]` |
| `/dialog` | – | 🚧 pending CMD-17 | `node` | `minecraft.command.dialog` | 2 | `/dialog show <targets> <dialog>` |
| `/trigger` | – | 🚧 pending CMD-18 | `node` | `minecraft.command.trigger` | 0 | `/trigger <objective> [add,set] [value]` |
| `/particle` | – | 🚧 pending CMD-19 | `node` | `minecraft.command.particle` | 2 | `/particle <name> <pos> [delta] [speed] [count]` |
| `/playsound` | – | 🚧 pending CMD-19 | `node` | `minecraft.command.playsound` | 2 | `/playsound <sound> <source> <targets> [pos]` |
| `/stopsound` | – | 🚧 pending CMD-19 | `node` | `minecraft.command.stopsound` | 2 | `/stopsound <targets> [source] [sound]` |
| `/loot` | – | 🚧 pending CMD-20 | `node` | `minecraft.command.loot` | 2 | `/loot <give,insert,spawn,replace> [...]` |
| `/recipe` | – | 🚧 pending CMD-20 | `node` | `minecraft.command.recipe` | 2 | `/recipe <give,take> <targets> <recipe>` |
| `/advancement` | – | 🚧 pending CMD-20 | `node` | `minecraft.command.advancement` | 2 | `/advancement <grant,revoke> <targets> <advancement>` |
| `/random` | – | 🚧 pending CMD-20 | `node` | `minecraft.command.random` | 2 | `/random <value,roll> [...]` |
| `/schedule` | – | 🚧 pending CMD-20 | `node` | `minecraft.command.schedule` | 2 | `/schedule <function> <clear,replace> <time>` |
| `/function` | – | 🚧 pending CMD-21 | `node` | `minecraft.command.function` | 2 | `/function <name> [arguments]` |
| `/datapack` | – | 🚧 pending CMD-21 | `node` | `minecraft.command.datapack` | 2 | `/datapack <enable,disable,list> [name]` |
| `/reload` | – | 🚧 pending CMD-21 | `node` | `minecraft.command.reload` | 4 | `/reload` |
| `/return` | – | 🚧 pending CMD-21 | `node` | `minecraft.command.return` | 2 | `/return <value>` |
| `/test` | – | 🚧 pending CMD-21 | `node` | `minecraft.command.test` | 2 | `/test <run,create,clear,list> [...]` |
| `/execute` | – | 🚧 pending CMD-22 | `node` | `minecraft.command.execute` | 2 | `/execute <...> run <command>` |

## ops

Server diagnostics and lifecycle.

| Command | Aliases | Status | Gate | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- | --- |
| `/cache` | – | ✅ working | `everyone` | `aether.command.cache` | 3 | `/cache` |
| `/aether` | – | ✅ working | `operator` | `aether.command.aether` | 4 | `/aether <subcommand>` |
| `/visibility` | – | ✅ working | `operator` | `aether.command.visibility` | 3 | `/visibility [on,off,status]` |
| `/tps` | – | 🚧 pending CMD-23 | `node` | `aether.command.tps` | 3 | `/tps` |
| `/mspt` | – | 🚧 pending CMD-23 | `node` | `aether.command.mspt` | 3 | `/mspt` |
| `/gc` | – | 🚧 pending CMD-23 | `node` | `aether.command.gc` | 3 | `/gc` |
| `/plugins` | `pl` | 🚧 pending CMD-23 | `node` | `aether.command.plugins` | 3 | `/plugins` |
| `/restart` | – | 🚧 pending CMD-23 | `node` | `aether.command.restart` | 4 | `/restart` |
| `/debug` | – | 🚧 pending CMD-23 | `node` | `minecraft.command.debug` | 3 | `/debug <start,stop,function> [...]` |
| `/perf` | – | 🚧 pending CMD-23 | `node` | `minecraft.command.perf` | 4 | `/perf <start,stop>` |
| `/jfr` | – | 🚧 pending CMD-23 | `node` | `minecraft.command.jfr` | 4 | `/jfr <start,stop,dump> [...]` |

## history

World history and protection.

| Command | Aliases | Status | Gate | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- | --- |
| `/inspect` | `i` | ✅ working | `everyone` | `aether.command.inspect` | 2 | `/inspect` |
| `/lookup` | `l` | ✅ working | `everyone` | `aether.command.lookup` | 2 | `/lookup <filters>` |
| `/rollback` | `rb` | ✅ working | `everyone` | `aether.command.rollback` | 3 | `/rollback <filters>` |
| `/page` | `pg` | ✅ working | `everyone` | `aether.command.page` | 2 | `/page next,prev,number` |
| `/stash` | – | ✅ working | `everyone` | `aether.command.stash` | 2 | `/stash` |
| `/audit` | – | ✅ working | `everyone` | `aether.command.audit` | 2 | `/audit` |
| `/co` | – | 🚧 pending CMD-27 | `node` | `aether.command.lookup` | 2 | `/co <l,u:<player> t:<time> r:<radius> a:<action>>` |
| `/restore` | – | 🚧 pending CMD-28 | `node` | `aether.command.restore` | 3 | `/restore <filters>` |
| `/purge` | – | 🚧 pending CMD-28 | `node` | `aether.command.purge` | 4 | `/purge <filters>` |

## economy

The economy.

| Command | Aliases | Status | Gate | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- | --- |
| `/balance` | `bal`, `money` | ✅ working | `everyone` | `aether.command.balance` | 0 | `/balance [player]` |
| `/pay` | – | ✅ working | `everyone` | `aether.command.pay` | 0 | `/pay <player> <amount>` |
| `/sell` | – | ✅ working | `everyone` | `aether.command.sell` | 0 | `/sell <count> <price> [duration]` |
| `/market` | – | ✅ working | `everyone` | `aether.command.market` | 0 | `/market [filters]` |
| `/buy` | – | ✅ working | `everyone` | `aether.command.buy` | 0 | `/buy <id> [count]` |
| `/listings` | – | ✅ working | `everyone` | `aether.command.listings` | 0 | `/listings` |
| `/unlist` | – | ✅ working | `everyone` | `aether.command.unlist` | 0 | `/unlist <id>` |
| `/econ` | – | ✅ working | `operator` | `aether.command.econ` | 3 | `/econ give <player> <amount> [reason]` |

