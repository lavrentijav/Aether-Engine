<!-- СГЕНЕРИРОВАННЫЙ ФАЙЛ — не редактируйте вручную.
     Источник:    crates/aether-server/src/commands/catalogue.rs
     Пересобрать: cargo-locked run -p aether-server --example gen_commands_md -->

# Aether Engine — Каталог команд

> **English version:** [COMMANDS.md](COMMANDS.md)

Каждая известная Aether команда, сгруппированная по категориям: и те, что работают сейчас, и те, что пока лишь запланированы. Файл сгенерирован из одного списка в сервере (`crates/aether-server/src/commands/catalogue.rs`), поэтому ни одна команда не указана дважды и запланированная не может незаметно исчезнуть.

*Файл сгенерирован — не редактируйте его вручную. Пересобрать: `cargo-locked run -p aether-server --example gen_commands_md`.*

**Легенда:** ✅ работает · 🚧 ожидает (`CMD-nn` — задача, которая её реализует). **Узел** — правомочие, а **Op** — ванильный уровень оператора (0 — всем … 4 — владельцу); для ожидающей команды заглушка требует ровно их. **Gate** — что сервер проверяет *сегодня*: `node` (узел/op применяются), `operator` (прежняя проверка «только оператор» — узел/op это цель, пока не применяются) или `everyone` (требований нет; узел/op — цель).

**Итого:** 113 команд — 42 работают, 71 ожидают.

## Содержание

- [admin](#admin) — Управление игроками и доступом. (10)
- [server](#server) — Управление сервером и информация о нём. (10)
- [player](#player) — Игрок: инвентарь, перемещение, эффекты и предметы. (22)
- [world](#world) — Блоки, окружение и правила мира. (16)
- [data](#data) — Данные, счётная доска, команды и чат. (27)
- [ops](#ops) — Диагностика и жизненный цикл сервера. (11)
- [history](#history) — История мира и защита. (9)
- [economy](#economy) — Экономика. (8)

## admin

Управление игроками и доступом.

| Команда | Синонимы | Статус | Gate | Узел | Op | Использование |
| --- | --- | --- | --- | --- | --- | --- |
| `/help` | `?` | ✅ работает | `everyone` | `minecraft.command.help` | 0 | `/help [command]` |
| `/op` | – | ✅ работает | `node` | `minecraft.command.op` | 3 | `/op <targets>` |
| `/deop` | – | ✅ работает | `node` | `minecraft.command.deop` | 3 | `/deop <targets>` |
| `/ban` | – | ✅ работает | `node` | `minecraft.command.ban` | 3 | `/ban <targets> [reason]` |
| `/ban-ip` | – | ✅ работает | `node` | `minecraft.command.ban-ip` | 3 | `/ban-ip <targets> [reason]` |
| `/banlist` | – | ✅ работает | `node` | `minecraft.command.banlist` | 3 | `/banlist [ips]` |
| `/pardon` | – | ✅ работает | `node` | `minecraft.command.pardon` | 3 | `/pardon <targets>` |
| `/pardon-ip` | – | ✅ работает | `node` | `minecraft.command.pardon-ip` | 3 | `/pardon-ip <targets>` |
| `/kick` | – | ✅ работает | `node` | `minecraft.command.kick` | 3 | `/kick <targets> [reason]` |
| `/whitelist` | – | ✅ работает | `node` | `minecraft.command.whitelist` | 3 | `/whitelist <add,remove,list,on,off,reload> [name]` |

## server

Управление сервером и информация о нём.

| Команда | Синонимы | Статус | Gate | Узел | Op | Использование |
| --- | --- | --- | --- | --- | --- | --- |
| `/stop` | – | ✅ работает | `node` | `minecraft.command.stop` | 4 | `/stop` |
| `/save-all` | – | ✅ работает | `node` | `minecraft.command.save-all` | 4 | `/save-all` |
| `/save-on` | – | 🚧 ожидает CMD-09 | `node` | `minecraft.command.save-on` | 4 | `/save-on` |
| `/save-off` | – | 🚧 ожидает CMD-09 | `node` | `minecraft.command.save-off` | 4 | `/save-off` |
| `/list` | – | 🚧 ожидает CMD-09 | `node` | `minecraft.command.list` | 0 | `/list [uuids]` |
| `/seed` | – | 🚧 ожидает CMD-09 | `node` | `minecraft.command.seed` | 2 | `/seed` |
| `/version` | – | 🚧 ожидает CMD-09 | `node` | `minecraft.command.version` | 2 | `/version` |
| `/setidletimeout` | – | 🚧 ожидает CMD-09 | `node` | `minecraft.command.setidletimeout` | 3 | `/setidletimeout <minutes>` |
| `/transfer` | – | 🚧 ожидает CMD-09 | `node` | `minecraft.command.transfer` | 3 | `/transfer <hostname> [port] [players]` |
| `/publish` | – | 🚧 ожидает CMD-09 | `node` | `minecraft.command.publish` | 4 | `/publish [allow-commands] [gamemode] [port]` |

## player

Игрок: инвентарь, перемещение, эффекты и предметы.

| Команда | Синонимы | Статус | Gate | Узел | Op | Использование |
| --- | --- | --- | --- | --- | --- | --- |
| `/gamemode` | `gm` | ✅ работает | `operator` | `minecraft.command.gamemode` | 2 | `/gamemode <mode> [player]` |
| `/give` | – | ✅ работает | `operator` | `minecraft.command.give` | 2 | `/give <item> [count]` |
| `/summon` | – | ✅ работает | `operator` | `minecraft.command.summon` | 2 | `/summon <entity> [pos]` |
| `/killall` | – | ✅ работает | `operator` | `aether.command.killall` | 2 | `/killall` |
| `/kill` | – | ✅ работает | `everyone` | `minecraft.command.kill` | 2 | `/kill [targets]` |
| `/spawn` | – | ✅ работает | `everyone` | `aether.command.spawn` | 0 | `/spawn` |
| `/teleport` | `tp` | ✅ работает | `operator` | `minecraft.command.teleport` | 2 | `/teleport <target>` |
| `/block` | – | ✅ работает | `everyone` | `aether.command.block` | 0 | `/block <x> <y> <z>` |
| `/food` | – | ✅ работает | `operator` | `aether.command.food` | 2 | `/food [level]` |
| `/heal` | – | ✅ работает | `operator` | `aether.command.heal` | 2 | `/heal` |
| `/inv` | `inventory` | ✅ работает | `everyone` | `aether.command.inventory` | 0 | `/inv` |
| `/clear` | – | 🚧 ожидает CMD-10 | `node` | `minecraft.command.clear` | 2 | `/clear [targets] [item] [max-count]` |
| `/effect` | – | 🚧 ожидает CMD-10 | `node` | `minecraft.command.effect` | 2 | `/effect <targets> <effect> [seconds] [amplifier]` |
| `/enchant` | – | 🚧 ожидает CMD-10 | `node` | `minecraft.command.enchant` | 2 | `/enchant <targets> <enchantment> [level]` |
| `/experience` | `xp` | 🚧 ожидает CMD-10 | `node` | `minecraft.command.experience` | 2 | `/experience <targets> <amount> [points,levels]` |
| `/attribute` | – | 🚧 ожидает CMD-10 | `node` | `minecraft.command.attribute` | 2 | `/attribute <target> <attribute> <get,base> [value]` |
| `/damage` | – | 🚧 ожидает CMD-10 | `node` | `minecraft.command.damage` | 2 | `/damage <target> <amount> [type]` |
| `/spreadplayers` | – | 🚧 ожидает CMD-11 | `node` | `minecraft.command.spreadplayers` | 2 | `/spreadplayers <center> <spread> <max-range> [teams]` |
| `/spawnpoint` | – | 🚧 ожидает CMD-11 | `node` | `minecraft.command.spawnpoint` | 2 | `/spawnpoint [target] [pos]` |
| `/ride` | – | 🚧 ожидает CMD-11 | `node` | `minecraft.command.ride` | 2 | `/ride <target> <vehicle>` |
| `/item` | – | 🚧 ожидает CMD-11 | `node` | `minecraft.command.item` | 2 | `/item <target> <slot> <replace,modify> [...]` |
| `/rotate` | – | 🚧 ожидает CMD-11 | `node` | `minecraft.command.rotate` | 2 | `/rotate <target> <yaw> <pitch>` |

## world

Блоки, окружение и правила мира.

| Команда | Синонимы | Статус | Gate | Узел | Op | Использование |
| --- | --- | --- | --- | --- | --- | --- |
| `/time` | – | ✅ работает | `operator` | `minecraft.command.time` | 2 | `/time [set] <day,noon,night,midnight,ticks>` |
| `/setblock` | – | 🚧 ожидает CMD-12 | `node` | `minecraft.command.setblock` | 2 | `/setblock <pos> <block> [mode]` |
| `/fill` | – | 🚧 ожидает CMD-12 | `node` | `minecraft.command.fill` | 2 | `/fill <from> <to> <block> [mode]` |
| `/clone` | – | 🚧 ожидает CMD-12 | `node` | `minecraft.command.clone` | 2 | `/clone <from> <to> <dest> [mask] [mode]` |
| `/fillbiome` | – | 🚧 ожидает CMD-12 | `node` | `minecraft.command.fillbiome` | 2 | `/fillbiome <from> <to> <biome>` |
| `/place` | – | 🚧 ожидает CMD-12 | `node` | `minecraft.command.place` | 2 | `/place <feature,template> <pos> [...]` |
| `/forceload` | – | 🚧 ожидает CMD-12 | `node` | `minecraft.command.forceload` | 2 | `/forceload <add,remove,query> <from> [to]` |
| `/locate` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.locate` | 2 | `/locate <structure,biome,poi> [...]` |
| `/setworldspawn` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.setworldspawn` | 2 | `/setworldspawn [pos] [angle]` |
| `/worldborder` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.worldborder` | 2 | `/worldborder <set,center,damage,get,warning> [...]` |
| `/weather` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.weather` | 2 | `/weather <clear,rain,thunder> [duration]` |
| `/gamerule` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.gamerule` | 2 | `/gamerule <rule> [value]` |
| `/difficulty` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.difficulty` | 2 | `/difficulty <peaceful,easy,normal,hard>` |
| `/tick` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.tick` | 3 | `/tick <rate,freeze,step,sprint,unfreeze,query> [...]` |
| `/defaultgamemode` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.defaultgamemode` | 2 | `/defaultgamemode <mode>` |
| `/waypoint` | – | 🚧 ожидает CMD-13 | `node` | `minecraft.command.waypoint` | 2 | `/waypoint <list,modify>` |

## data

Данные, счётная доска, команды и чат.

| Команда | Синонимы | Статус | Gate | Узел | Op | Использование |
| --- | --- | --- | --- | --- | --- | --- |
| `/msg` | `tell`, `w` | ✅ работает | `everyone` | `minecraft.command.msg` | 0 | `/msg <player> <message>` |
| `/data` | – | 🚧 ожидает CMD-14 | `node` | `minecraft.command.data` | 2 | `/data <get,merge,modify,remove> <target> <path> [...]` |
| `/scoreboard` | – | 🚧 ожидает CMD-15 | `node` | `minecraft.command.scoreboard` | 2 | `/scoreboard <objectives,players> [...]` |
| `/tag` | – | 🚧 ожидает CMD-16 | `node` | `minecraft.command.tag` | 2 | `/tag <targets> <add,remove,list> <name>` |
| `/team` | – | 🚧 ожидает CMD-16 | `node` | `minecraft.command.team` | 2 | `/team <add,list,modify,join,leave,remove> [...]` |
| `/teammsg` | `tm` | 🚧 ожидает CMD-16 | `node` | `minecraft.command.teammsg` | 0 | `/teammsg <message>` |
| `/say` | – | 🚧 ожидает CMD-17 | `node` | `minecraft.command.say` | 2 | `/say <message>` |
| `/me` | – | 🚧 ожидает CMD-17 | `node` | `minecraft.command.me` | 0 | `/me <action>` |
| `/tellraw` | – | 🚧 ожидает CMD-17 | `node` | `minecraft.command.tellraw` | 2 | `/tellraw <targets> <message>` |
| `/title` | – | 🚧 ожидает CMD-17 | `node` | `minecraft.command.title` | 2 | `/title <targets> <title,subtitle,actionbar,times,reset> [...]` |
| `/bossbar` | – | 🚧 ожидает CMD-17 | `node` | `minecraft.command.bossbar` | 2 | `/bossbar <add,get,list,remove,set> [...]` |
| `/dialog` | – | 🚧 ожидает CMD-17 | `node` | `minecraft.command.dialog` | 2 | `/dialog show <targets> <dialog>` |
| `/trigger` | – | 🚧 ожидает CMD-18 | `node` | `minecraft.command.trigger` | 0 | `/trigger <objective> [add,set] [value]` |
| `/particle` | – | 🚧 ожидает CMD-19 | `node` | `minecraft.command.particle` | 2 | `/particle <name> <pos> [delta] [speed] [count]` |
| `/playsound` | – | 🚧 ожидает CMD-19 | `node` | `minecraft.command.playsound` | 2 | `/playsound <sound> <source> <targets> [pos]` |
| `/stopsound` | – | 🚧 ожидает CMD-19 | `node` | `minecraft.command.stopsound` | 2 | `/stopsound <targets> [source] [sound]` |
| `/loot` | – | 🚧 ожидает CMD-20 | `node` | `minecraft.command.loot` | 2 | `/loot <give,insert,spawn,replace> [...]` |
| `/recipe` | – | 🚧 ожидает CMD-20 | `node` | `minecraft.command.recipe` | 2 | `/recipe <give,take> <targets> <recipe>` |
| `/advancement` | – | 🚧 ожидает CMD-20 | `node` | `minecraft.command.advancement` | 2 | `/advancement <grant,revoke> <targets> <advancement>` |
| `/random` | – | 🚧 ожидает CMD-20 | `node` | `minecraft.command.random` | 2 | `/random <value,roll> [...]` |
| `/schedule` | – | 🚧 ожидает CMD-20 | `node` | `minecraft.command.schedule` | 2 | `/schedule <function> <clear,replace> <time>` |
| `/function` | – | 🚧 ожидает CMD-21 | `node` | `minecraft.command.function` | 2 | `/function <name> [arguments]` |
| `/datapack` | – | 🚧 ожидает CMD-21 | `node` | `minecraft.command.datapack` | 2 | `/datapack <enable,disable,list> [name]` |
| `/reload` | – | 🚧 ожидает CMD-21 | `node` | `minecraft.command.reload` | 4 | `/reload` |
| `/return` | – | 🚧 ожидает CMD-21 | `node` | `minecraft.command.return` | 2 | `/return <value>` |
| `/test` | – | 🚧 ожидает CMD-21 | `node` | `minecraft.command.test` | 2 | `/test <run,create,clear,list> [...]` |
| `/execute` | – | 🚧 ожидает CMD-22 | `node` | `minecraft.command.execute` | 2 | `/execute <...> run <command>` |

## ops

Диагностика и жизненный цикл сервера.

| Команда | Синонимы | Статус | Gate | Узел | Op | Использование |
| --- | --- | --- | --- | --- | --- | --- |
| `/cache` | – | ✅ работает | `everyone` | `aether.command.cache` | 3 | `/cache` |
| `/aether` | – | ✅ работает | `operator` | `aether.command.aether` | 4 | `/aether <subcommand>` |
| `/visibility` | – | ✅ работает | `operator` | `aether.command.visibility` | 3 | `/visibility [on,off,status]` |
| `/tps` | – | 🚧 ожидает CMD-23 | `node` | `aether.command.tps` | 3 | `/tps` |
| `/mspt` | – | 🚧 ожидает CMD-23 | `node` | `aether.command.mspt` | 3 | `/mspt` |
| `/gc` | – | 🚧 ожидает CMD-23 | `node` | `aether.command.gc` | 3 | `/gc` |
| `/plugins` | `pl` | 🚧 ожидает CMD-23 | `node` | `aether.command.plugins` | 3 | `/plugins` |
| `/restart` | – | 🚧 ожидает CMD-23 | `node` | `aether.command.restart` | 4 | `/restart` |
| `/debug` | – | 🚧 ожидает CMD-23 | `node` | `minecraft.command.debug` | 3 | `/debug <start,stop,function> [...]` |
| `/perf` | – | 🚧 ожидает CMD-23 | `node` | `minecraft.command.perf` | 4 | `/perf <start,stop>` |
| `/jfr` | – | 🚧 ожидает CMD-23 | `node` | `minecraft.command.jfr` | 4 | `/jfr <start,stop,dump> [...]` |

## history

История мира и защита.

| Команда | Синонимы | Статус | Gate | Узел | Op | Использование |
| --- | --- | --- | --- | --- | --- | --- |
| `/inspect` | `i` | ✅ работает | `everyone` | `aether.command.inspect` | 2 | `/inspect` |
| `/lookup` | `l` | ✅ работает | `everyone` | `aether.command.lookup` | 2 | `/lookup <filters>` |
| `/rollback` | `rb` | ✅ работает | `everyone` | `aether.command.rollback` | 3 | `/rollback <filters>` |
| `/page` | `pg` | ✅ работает | `everyone` | `aether.command.page` | 2 | `/page next,prev,number` |
| `/stash` | – | ✅ работает | `everyone` | `aether.command.stash` | 2 | `/stash` |
| `/audit` | – | ✅ работает | `everyone` | `aether.command.audit` | 2 | `/audit` |
| `/co` | – | 🚧 ожидает CMD-27 | `node` | `aether.command.lookup` | 2 | `/co <l,u:<player> t:<time> r:<radius> a:<action>>` |
| `/restore` | – | 🚧 ожидает CMD-28 | `node` | `aether.command.restore` | 3 | `/restore <filters>` |
| `/purge` | – | 🚧 ожидает CMD-28 | `node` | `aether.command.purge` | 4 | `/purge <filters>` |

## economy

Экономика.

| Команда | Синонимы | Статус | Gate | Узел | Op | Использование |
| --- | --- | --- | --- | --- | --- | --- |
| `/balance` | `bal`, `money` | ✅ работает | `everyone` | `aether.command.balance` | 0 | `/balance [player]` |
| `/pay` | – | ✅ работает | `everyone` | `aether.command.pay` | 0 | `/pay <player> <amount>` |
| `/sell` | – | ✅ работает | `everyone` | `aether.command.sell` | 0 | `/sell <count> <price> [duration]` |
| `/market` | – | ✅ работает | `everyone` | `aether.command.market` | 0 | `/market [filters]` |
| `/buy` | – | ✅ работает | `everyone` | `aether.command.buy` | 0 | `/buy <id> [count]` |
| `/listings` | – | ✅ работает | `everyone` | `aether.command.listings` | 0 | `/listings` |
| `/unlist` | – | ✅ работает | `everyone` | `aether.command.unlist` | 0 | `/unlist <id>` |
| `/econ` | – | ✅ работает | `operator` | `aether.command.econ` | 3 | `/econ give <player> <amount> [reason]` |

