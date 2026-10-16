<!-- GENERATED FILE — do not edit by hand.
     Source:     crates/aether-server/src/commands/catalogue.rs
     Regenerate: cargo-locked run -p aether-server --example gen_commands_md -->

# Aether Engine — Command Catalogue

> **Русская версия:** [COMMANDS.ru.md](COMMANDS.ru.md)

Every command Aether knows, grouped by category: the ones that run today and the ones that are only specified. It is generated from one list in the server (`crates/aether-server/src/commands/catalogue.rs`), so nothing is listed twice and a planned command cannot quietly disappear.

*This file is generated — do not edit it by hand. Regenerate with `cargo-locked run -p aether-server --example gen_commands_md`.*

**Legend:** ✅ working · 🚧 pending (`CMD-nn` is the task that will implement it). **Node** is the permission node checked before the command runs; **Op** is the vanilla operator level required (0 everyone … 4 owner).

**Totals:** 114 commands — 31 working, 83 pending.

## Contents

- [admin](#admin) — Player and access management. (10)
- [server](#server) — Server control and information. (10)
- [player](#player) — Player verbs: inventory, movement, effects and items. (23)
- [world](#world) — Blocks, environment and world rules. (16)
- [data](#data) — Data, scoreboard, teams and chat. (27)
- [ops](#ops) — Server diagnostics and lifecycle. (11)
- [history](#history) — World history and protection. (9)
- [economy](#economy) — The economy. (8)

## admin

Player and access management.

| Command | Aliases | Status | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- |
| `/help` | `?` | ✅ working | `minecraft.command.help` | 0 | `/help [command]` |
| `/op` | – | 🚧 pending CMD-08 | `minecraft.command.op` | 3 | `/op <targets>` |
| `/deop` | – | 🚧 pending CMD-08 | `minecraft.command.deop` | 3 | `/deop <targets>` |
| `/ban` | – | 🚧 pending CMD-08 | `minecraft.command.ban` | 3 | `/ban <targets> [reason]` |
| `/ban-ip` | – | 🚧 pending CMD-08 | `minecraft.command.ban-ip` | 3 | `/ban-ip <targets> [reason]` |
| `/banlist` | – | 🚧 pending CMD-08 | `minecraft.command.banlist` | 3 | `/banlist [ips]` |
| `/pardon` | – | 🚧 pending CMD-08 | `minecraft.command.pardon` | 3 | `/pardon <targets>` |
| `/pardon-ip` | – | 🚧 pending CMD-08 | `minecraft.command.pardon-ip` | 3 | `/pardon-ip <targets>` |
| `/kick` | – | 🚧 pending CMD-08 | `minecraft.command.kick` | 3 | `/kick <targets> [reason]` |
| `/whitelist` | – | 🚧 pending CMD-08 | `minecraft.command.whitelist` | 3 | `/whitelist <add,remove,list,on,off,reload> [name]` |

## server

Server control and information.

| Command | Aliases | Status | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- |
| `/stop` | – | 🚧 pending CMD-09 | `minecraft.command.stop` | 4 | `/stop` |
| `/save-all` | – | 🚧 pending CMD-09 | `minecraft.command.save-all` | 4 | `/save-all [flush]` |
| `/save-on` | – | 🚧 pending CMD-09 | `minecraft.command.save-on` | 4 | `/save-on` |
| `/save-off` | – | 🚧 pending CMD-09 | `minecraft.command.save-off` | 4 | `/save-off` |
| `/list` | – | 🚧 pending CMD-09 | `minecraft.command.list` | 0 | `/list [uuids]` |
| `/seed` | – | 🚧 pending CMD-09 | `minecraft.command.seed` | 2 | `/seed` |
| `/version` | – | 🚧 pending CMD-09 | `minecraft.command.version` | 2 | `/version` |
| `/setidletimeout` | – | 🚧 pending CMD-09 | `minecraft.command.setidletimeout` | 3 | `/setidletimeout <minutes>` |
| `/transfer` | – | 🚧 pending CMD-09 | `minecraft.command.transfer` | 3 | `/transfer <hostname> [port] [players]` |
| `/publish` | – | 🚧 pending CMD-09 | `minecraft.command.publish` | 4 | `/publish [allow-commands] [gamemode] [port]` |

## player

Player verbs: inventory, movement, effects and items.

| Command | Aliases | Status | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- |
| `/gamemode` | `gm` | ✅ working | `minecraft.command.gamemode` | 2 | `/gamemode <mode> [player]` |
| `/give` | – | ✅ working | `minecraft.command.give` | 2 | `/give <item> [count]` |
| `/summon` | – | ✅ working | `minecraft.command.summon` | 2 | `/summon <entity> [pos]` |
| `/killall` | – | ✅ working | `aether.command.killall` | 2 | `/killall` |
| `/kill` | – | ✅ working | `minecraft.command.kill` | 2 | `/kill [targets]` |
| `/spawn` | – | ✅ working | `aether.command.spawn` | 0 | `/spawn` |
| `/tp` | – | ✅ working | `minecraft.command.tp` | 2 | `/tp <target>` |
| `/block` | – | ✅ working | `aether.command.block` | 0 | `/block <x> <y> <z>` |
| `/food` | – | ✅ working | `aether.command.food` | 2 | `/food [level]` |
| `/heal` | – | ✅ working | `aether.command.heal` | 2 | `/heal` |
| `/inv` | `inventory` | ✅ working | `aether.command.inventory` | 0 | `/inv` |
| `/clear` | – | 🚧 pending CMD-10 | `minecraft.command.clear` | 2 | `/clear [targets] [item] [max-count]` |
| `/effect` | – | 🚧 pending CMD-10 | `minecraft.command.effect` | 2 | `/effect <targets> <effect> [seconds] [amplifier]` |
| `/enchant` | – | 🚧 pending CMD-10 | `minecraft.command.enchant` | 2 | `/enchant <targets> <enchantment> [level]` |
| `/experience` | `xp` | 🚧 pending CMD-10 | `minecraft.command.experience` | 2 | `/experience <targets> <amount> [points,levels]` |
| `/attribute` | – | 🚧 pending CMD-10 | `minecraft.command.attribute` | 2 | `/attribute <target> <attribute> <get,base> [value]` |
| `/damage` | – | 🚧 pending CMD-10 | `minecraft.command.damage` | 2 | `/damage <target> <amount> [type]` |
| `/teleport` | – | 🚧 pending CMD-11 | `minecraft.command.teleport` | 2 | `/teleport <target>` |
| `/spreadplayers` | – | 🚧 pending CMD-11 | `minecraft.command.spreadplayers` | 2 | `/spreadplayers <center> <spread> <max-range> [teams]` |
| `/spawnpoint` | – | 🚧 pending CMD-11 | `minecraft.command.spawnpoint` | 2 | `/spawnpoint [target] [pos]` |
| `/ride` | – | 🚧 pending CMD-11 | `minecraft.command.ride` | 2 | `/ride <target> <vehicle>` |
| `/item` | – | 🚧 pending CMD-11 | `minecraft.command.item` | 2 | `/item <target> <slot> <replace,modify> [...]` |
| `/rotate` | – | 🚧 pending CMD-11 | `aether.command.rotate` | 2 | `/rotate <target> <yaw> <pitch>` |

## world

Blocks, environment and world rules.

| Command | Aliases | Status | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- |
| `/time` | – | ✅ working | `minecraft.command.time` | 2 | `/time [set] <day,noon,night,midnight,ticks>` |
| `/setblock` | – | 🚧 pending CMD-12 | `minecraft.command.setblock` | 2 | `/setblock <pos> <block> [mode]` |
| `/fill` | – | 🚧 pending CMD-12 | `minecraft.command.fill` | 2 | `/fill <from> <to> <block> [mode]` |
| `/clone` | – | 🚧 pending CMD-12 | `minecraft.command.clone` | 2 | `/clone <from> <to> <dest> [mask] [mode]` |
| `/fillbiome` | – | 🚧 pending CMD-12 | `minecraft.command.fillbiome` | 2 | `/fillbiome <from> <to> <biome>` |
| `/place` | – | 🚧 pending CMD-12 | `minecraft.command.place` | 2 | `/place <feature,template> <pos> [...]` |
| `/forceload` | – | 🚧 pending CMD-12 | `minecraft.command.forceload` | 2 | `/forceload <add,remove,query> <from> [to]` |
| `/locate` | – | 🚧 pending CMD-13 | `minecraft.command.locate` | 2 | `/locate <structure,biome,poi> [...]` |
| `/setworldspawn` | – | 🚧 pending CMD-13 | `minecraft.command.setworldspawn` | 2 | `/setworldspawn [pos] [angle]` |
| `/worldborder` | – | 🚧 pending CMD-13 | `minecraft.command.worldborder` | 2 | `/worldborder <set,center,damage,get,warning> [...]` |
| `/weather` | – | 🚧 pending CMD-13 | `minecraft.command.weather` | 2 | `/weather <clear,rain,thunder> [duration]` |
| `/gamerule` | – | 🚧 pending CMD-13 | `minecraft.command.gamerule` | 2 | `/gamerule <rule> [value]` |
| `/difficulty` | – | 🚧 pending CMD-13 | `minecraft.command.difficulty` | 2 | `/difficulty <peaceful,easy,normal,hard>` |
| `/tick` | – | 🚧 pending CMD-13 | `minecraft.command.tick` | 3 | `/tick <rate,freeze,step,sprint,unfreeze,query> [...]` |
| `/defaultgamemode` | – | 🚧 pending CMD-13 | `minecraft.command.defaultgamemode` | 2 | `/defaultgamemode <mode>` |
| `/waypoint` | – | 🚧 pending CMD-13 | `minecraft.command.waypoint` | 2 | `/waypoint <list,modify>` |

## data

Data, scoreboard, teams and chat.

| Command | Aliases | Status | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- |
| `/msg` | `tell`, `w` | ✅ working | `minecraft.command.msg` | 0 | `/msg <player> <message>` |
| `/data` | – | 🚧 pending CMD-14 | `minecraft.command.data` | 2 | `/data <get,merge,modify,remove> <target> <path> [...]` |
| `/scoreboard` | – | 🚧 pending CMD-15 | `minecraft.command.scoreboard` | 2 | `/scoreboard <objectives,players> [...]` |
| `/tag` | – | 🚧 pending CMD-16 | `minecraft.command.tag` | 2 | `/tag <targets> <add,remove,list> <name>` |
| `/team` | – | 🚧 pending CMD-16 | `minecraft.command.team` | 2 | `/team <add,list,modify,join,leave,remove> [...]` |
| `/teammsg` | `tm` | 🚧 pending CMD-16 | `minecraft.command.teammsg` | 0 | `/teammsg <message>` |
| `/say` | – | 🚧 pending CMD-17 | `minecraft.command.say` | 2 | `/say <message>` |
| `/me` | – | 🚧 pending CMD-17 | `minecraft.command.me` | 0 | `/me <action>` |
| `/tellraw` | – | 🚧 pending CMD-17 | `minecraft.command.tellraw` | 2 | `/tellraw <targets> <message>` |
| `/title` | – | 🚧 pending CMD-17 | `minecraft.command.title` | 2 | `/title <targets> <title,subtitle,actionbar,times,reset> [...]` |
| `/bossbar` | – | 🚧 pending CMD-17 | `minecraft.command.bossbar` | 2 | `/bossbar <add,get,list,remove,set> [...]` |
| `/dialog` | – | 🚧 pending CMD-17 | `minecraft.command.dialog` | 2 | `/dialog show <targets> <dialog>` |
| `/trigger` | – | 🚧 pending CMD-18 | `minecraft.command.trigger` | 0 | `/trigger <objective> [add,set] [value]` |
| `/particle` | – | 🚧 pending CMD-19 | `minecraft.command.particle` | 2 | `/particle <name> <pos> [delta] [speed] [count]` |
| `/playsound` | – | 🚧 pending CMD-19 | `minecraft.command.playsound` | 2 | `/playsound <sound> <source> <targets> [pos]` |
| `/stopsound` | – | 🚧 pending CMD-19 | `minecraft.command.stopsound` | 2 | `/stopsound <targets> [source] [sound]` |
| `/loot` | – | 🚧 pending CMD-20 | `minecraft.command.loot` | 2 | `/loot <give,insert,spawn,replace> [...]` |
| `/recipe` | – | 🚧 pending CMD-20 | `minecraft.command.recipe` | 2 | `/recipe <give,take> <targets> <recipe>` |
| `/advancement` | – | 🚧 pending CMD-20 | `minecraft.command.advancement` | 2 | `/advancement <grant,revoke> <targets> <advancement>` |
| `/random` | – | 🚧 pending CMD-20 | `minecraft.command.random` | 2 | `/random <value,roll> [...]` |
| `/schedule` | – | 🚧 pending CMD-20 | `minecraft.command.schedule` | 2 | `/schedule <function> <clear,replace> <time>` |
| `/function` | – | 🚧 pending CMD-21 | `minecraft.command.function` | 2 | `/function <name> [arguments]` |
| `/datapack` | – | 🚧 pending CMD-21 | `minecraft.command.datapack` | 2 | `/datapack <enable,disable,list> [name]` |
| `/reload` | – | 🚧 pending CMD-21 | `minecraft.command.reload` | 4 | `/reload` |
| `/return` | – | 🚧 pending CMD-21 | `minecraft.command.return` | 2 | `/return <value>` |
| `/test` | – | 🚧 pending CMD-21 | `minecraft.command.test` | 2 | `/test <run,create,clear,list> [...]` |
| `/execute` | – | 🚧 pending CMD-22 | `minecraft.command.execute` | 2 | `/execute <...> run <command>` |

## ops

Server diagnostics and lifecycle.

| Command | Aliases | Status | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- |
| `/cache` | – | ✅ working | `aether.command.cache` | 3 | `/cache` |
| `/aether` | – | ✅ working | `aether.command.aether` | 4 | `/aether <subcommand>` |
| `/visibility` | – | ✅ working | `aether.command.visibility` | 3 | `/visibility [on,off,status]` |
| `/tps` | – | 🚧 pending CMD-23 | `aether.command.tps` | 3 | `/tps` |
| `/mspt` | – | 🚧 pending CMD-23 | `aether.command.mspt` | 3 | `/mspt` |
| `/gc` | – | 🚧 pending CMD-23 | `aether.command.gc` | 3 | `/gc` |
| `/plugins` | `pl` | 🚧 pending CMD-23 | `aether.command.plugins` | 3 | `/plugins` |
| `/restart` | – | 🚧 pending CMD-23 | `aether.command.restart` | 4 | `/restart` |
| `/debug` | – | 🚧 pending CMD-23 | `minecraft.command.debug` | 3 | `/debug <start,stop,function> [...]` |
| `/perf` | – | 🚧 pending CMD-23 | `minecraft.command.perf` | 4 | `/perf <start,stop>` |
| `/jfr` | – | 🚧 pending CMD-23 | `minecraft.command.jfr` | 4 | `/jfr <start,stop,dump> [...]` |

## history

World history and protection.

| Command | Aliases | Status | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- |
| `/inspect` | `i` | ✅ working | `aether.command.inspect` | 2 | `/inspect` |
| `/lookup` | `l` | ✅ working | `aether.command.lookup` | 2 | `/lookup <filters>` |
| `/rollback` | `rb` | ✅ working | `aether.command.rollback` | 3 | `/rollback <filters>` |
| `/page` | `pg` | ✅ working | `aether.command.page` | 2 | `/page next,prev,number` |
| `/stash` | – | ✅ working | `aether.command.stash` | 2 | `/stash` |
| `/audit` | – | ✅ working | `aether.command.audit` | 2 | `/audit` |
| `/co` | – | 🚧 pending CMD-27 | `aether.command.lookup` | 2 | `/co <l,u:<player> t:<time> r:<radius> a:<action>>` |
| `/restore` | – | 🚧 pending CMD-28 | `aether.command.restore` | 3 | `/restore <filters>` |
| `/purge` | – | 🚧 pending CMD-28 | `aether.command.purge` | 4 | `/purge <filters>` |

## economy

The economy.

| Command | Aliases | Status | Node | Op | Usage |
| --- | --- | --- | --- | --- | --- |
| `/balance` | `bal`, `money` | ✅ working | `aether.command.balance` | 0 | `/balance [player]` |
| `/pay` | – | ✅ working | `aether.command.pay` | 0 | `/pay <player> <amount>` |
| `/sell` | – | ✅ working | `aether.command.sell` | 0 | `/sell <count> <price> [duration]` |
| `/market` | – | ✅ working | `aether.command.market` | 0 | `/market [filters]` |
| `/buy` | – | ✅ working | `aether.command.buy` | 0 | `/buy <id> [count]` |
| `/listings` | – | ✅ working | `aether.command.listings` | 0 | `/listings` |
| `/unlist` | – | ✅ working | `aether.command.unlist` | 0 | `/unlist <id>` |
| `/econ` | – | ✅ working | `aether.command.econ` | 3 | `/econ give <player> <amount> [reason]` |

