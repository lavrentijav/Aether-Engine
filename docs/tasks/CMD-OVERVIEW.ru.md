# CMD0 — каталог команд → задачи, заготовки, очередь

Статус: ✅ сделано (ветка `oc/CMD0`), обновлено по ревью владельца. Источник:
`docs/design/commands.md`, `docs/design/plugins.md`, `CONTRIBUTING.md` («Core or plugin?»).

## Что сделано в CMD0

Каталог команд разложен на **73 задачи** — по одной на сессию агента (крупные группы раздроблены,
см. ниже). Каждая задача — бриф `docs/tasks/CMD-<id>-<slug>.md`: цель, точный синтаксис и поведение
(ваниль 1.21.11 или референс-плагин: permission node, op level, текст ответа), файлы, **явные
краевые случаи для тестов**, зависимости (`needs:`), критерии готовности.

Три артефакта:

1. **Заготовки в ядре** — `crates/aether-server/src/commands/registry.rs`: запись на каждую ещё не
   реализованную core-команду с permission node, op level и id задачи. Команда из каталога отвечает
   `not implemented yet (CMD-<id>)` с проверкой прав; `/help` перечисляет их по группам.
2. **Заготовки плагинов** — `plugins/<name>/plugin.toml` + `README.md` для 15 плагинов. Хоста
   (`oc/P1`) в main нет, поэтому кода нет — только манифест и README.
3. **Манифест очереди** — `docs/tasks/CMD-queue.json`: `{id, title, brief, needs, size, core}` в
   топологически верном порядке.

Задачи **не запускались** — это планирование.

## Правила владения именами (важно)

- **Ванильное имя принадлежит ядру.** Плагин **не** перерегистрирует команду, которую уже
  отвечает ядро; он либо вешает хук на неё, либо добавляет **новое** имя.
  - `/msg /tell /w /say /me /tellraw /title /bossbar /dialog` — ядро (CMD-17); essentials-chat
    (CMD-40) добавляет только `/r /mail /ignore /socialspy /broadcast /motd /rules` и форматирует
    чат хуком на ядровый `/msg`.
  - `/tp /teleport` — ядро (CMD-11a); essentials-teleport (CMD-37) добавляет `/home /warp /tpa` и
    т.п., но `/spawn` не переопределяет (см. открытые вопросы).
- **Один диспетчер.** После CMD-03 не остаётся старого `match`-диспетчера: все существующие
  команды мигрированы на дерево в той же задаче, старый путь удалён.

## Core или плагин — почему так

Правило `CONTRIBUTING.md`: **ядро** — нужно каждому серверу и/или на горячем пути; **плагин** —
опциональное и не критичное по производительности, на API плагинов.

- **Ядро:** Brigadier-дерево, парсер селекторов/координат, проверка прав, фидбек и `/help`, консоль
  и RCON; все ванильные команды; серверные операции; журнал истории; **механизмы** для плагинов —
  индекс регионов и проверка защиты, bulk-edit, телепорт с безопасным местом, player/UI API.
- **Плагины:** всё опциональное — экономика, магазины, дома/варпы, инвентари, чат, модерация,
  регионы/клеймы, WorldEdit, права-UI, соцсети, интеграции, развлечения, статистика.
- Пограничные — «core API + plugin»: ядро владеет быстрым механизмом, плагин — моделью/флагами/
  командами (регионы WG CMD-29/34, клеймы GP CMD-29/35, WorldEdit CMD-30/36).

## Все 73 задачи

**Легенда:**

- **C / P** — **C** = core, **P** = plugin.
- **размер** — **S** ≈ одна сессия, **M** ≈ 2–4 коммита, **L** — большой. Остался один: **CMD-03** (миграция существующих команд должна быть в той же
  задаче).
- **зависит** — id задач этого манифеста, готовые раньше (граф строго назад, порядок строк —
  топологический).
- **внешние зависимости** (не CMD, а ветки/сервисы): **P1** — хост плагинов (в main ещё нет),
  **P2** — сервис экономики, **PA1** — UI-API, **PA2** — read-only API истории, **PA3** — сеть.
- **op level / permission node** — ванильный уровень 0–4 и узел прав; проверяет ядро прав
  (CMD-01/02).

### Инфраструктура команд (core)

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-01 | Ядро прав: узлы, op-level 0–4, default true/false/op; `operators` — seed группы `op` | C | M | — |
| CMD-02 | Группы, наследование, per-world контекст, meta; встроенная группа `op` | C | M | CMD-01 |
| CMD-03 | Brigadier-дерево + миграция ВСЕХ существующих команд, один диспетчер | C | L | CMD-01 |
| CMD-04 | Пакет Declare Commands для каждого кодека (из дерева) | C | M | CMD-03 |
| CMD-05 | Фидбек команд, `/help`, алиасы, `sendCommandFeedback` | C | M | CMD-03 |
| CMD-06a | Парсер аргументов и координат `~ ^`, повороты | C | M | CMD-03 |
| CMD-06b | Парсер селекторов `@a/@p/@r/@s/@e` + семантика выбора | C | M | CMD-03, CMD-06a |
| CMD-07 | Консоль и RCON как источники команд | C | M | CMD-03, CMD-01 |

### V-admin (core)

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-08 | `/op /deop /ban /ban-ip /banlist /pardon /pardon-ip /kick /whitelist` | C | M | CMD-05, CMD-01, CMD-06b |
| CMD-09 | `/stop /save-all /save-on /save-off /list /seed /version /setidletimeout /transfer /publish` | C | M | CMD-05 |

### V-player (core)

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-10a | `/clear /effect /enchant` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-10b | `/experience /xp /attribute /damage` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-11a | `/teleport /tp /spreadplayers /spawnpoint` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-11b | `/spectate /ride /item /kill /rotate` | C | M | CMD-06b, CMD-05, CMD-01 |

### V-world (core)

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-12a | `/setblock /fill /clone` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-12b | `/fillbiome /place /forceload` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-13a | `/time /weather /worldborder /setworldspawn /defaultgamemode` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-13b | `/gamerule /difficulty /tick /locate /waypoint` | C | M | CMD-06b, CMD-05, CMD-01 |

### V-data (core)

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-14a | NBT-path парсер + `/data … storage` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-14b | `/data … block` и `/data … entity` | C | M | CMD-06b, CMD-05, CMD-01, CMD-14a |
| CMD-15a | модель scoreboard + `/scoreboard objectives …` | C | M | CMD-05, CMD-01, CMD-06b |
| CMD-15b | `/scoreboard players …` | C | M | CMD-05, CMD-01, CMD-06b, CMD-15a |
| CMD-16 | `/tag /team /teammsg` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-17a | `/say /me /msg /tell /w /tellraw` (ядро владеет именами) | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-17b | `/title /bossbar /dialog` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-18 | `/trigger` | C | S | CMD-15a, CMD-05 |
| CMD-19 | `/particle /playsound /stopsound` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-20a | `/loot` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-20b | `/recipe /advancement` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-20c | `/random /schedule` | C | S | CMD-06b, CMD-05, CMD-01 |
| CMD-21a | загрузчик датапаков (`pack.mcmeta`, реестры, теги) | C | M | CMD-05, CMD-01, CMD-20a/b/c |
| CMD-21b | `/function /return /test` + исполнение функций/макросы | C | M | CMD-05, CMD-01, CMD-21a |
| CMD-21c | `/datapack /reload` | C | S | CMD-05, CMD-01, CMD-21a |
| CMD-22a | `/execute` модификаторы | C | M | CMD-06b, CMD-05, CMD-21b |
| CMD-22b | `/execute` условия, `store`, `run` | C | M | CMD-06b, CMD-05, CMD-15a, CMD-22a |

### Серверные операции и история (core)

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-23 | `/tps /mspt /gc /plugins /restart /debug /perf /jfr` | C | M | CMD-05, CMD-01 |
| CMD-24 | `/aether reload` и `/aether backup` | C | M | CMD-05, CMD-01, CMD-21a |
| CMD-25 | `/aether profile` — подтиковый профайлер (**без P1**; плагины — опц.) | C | M | CMD-05, CMD-01 |
| CMD-26 | `/aether world` — список/загрузка/выгрузка миров | C | M | CMD-05, CMD-01 |
| CMD-27 | История: `/co`, фильтры lookup, превью | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-28 | История: `/restore` и `/purge` | C | M | CMD-27 |

### Core API для плагинов

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-29a | индекс регионов (per-chunk, снапшоты) | C | M | CMD-01, P1 |
| CMD-29b | проверка защиты + хук-точки + bypass | C | M | CMD-01, P1, CMD-29a |
| CMD-30a | BulkEdit API + группировка журнала + undo | C | M | CMD-01, P1 |
| CMD-30b | публикация bulk-edit: resend/light/бюджет тика | C | M | CMD-01, P1, CMD-30a |
| CMD-31 | телепорт + поиск безопасного места | C | M | CMD-01, P1 |
| CMD-32a | player API: состояние, эффекты/полёт/скорость, scheduler | C | M | CMD-01, P1 |
| CMD-32b | оконный/контейнерный API (PA1) | C | M | CMD-01, P1, CMD-32a |
| CMD-32c | UI-примитивы + поиск сервисов | C | M | CMD-01, P1, CMD-32a |

### Плагины

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-33a | permissions: узлы и родители | P | M | P1, CMD-02 |
| CMD-33b | permissions: meta, треки, веб-редактор | P | M | P1, CMD-02, CMD-33a |
| CMD-34a | regions: модель + `/rg define/redefine/remove/priority/info/list` | P | M | P1, CMD-29b |
| CMD-34b | regions: флаги, участники/владельцы, bypass | P | M | P1, CMD-29b, CMD-34a |
| CMD-35a | claims: модель + `/claim /abandonclaim /claimslist` + лопата | P | M | P1, CMD-29b |
| CMD-35b | claims: доверие (access/container/build/manage) + лимиты | P | S | P1, CMD-29b, CMD-35a |
| CMD-36a | WorldEdit: выделение, `//set/replace/walls/outline`, `//undo/redo/expand/contract` | P | M | P1, CMD-30b, CMD-06b |
| CMD-36b | WorldEdit: clipboard, `//rotate/flip/stack/move`, `//schem`, кисти | P | M | P1, CMD-30b, CMD-36a |
| CMD-37a | essentials-teleport: дома, варпы, `/setspawn` | P | M | P1, CMD-31 |
| CMD-37b | essentials-teleport: `/tpa`, `/back`, вертикаль, `/rtp` | P | M | P1, CMD-31, CMD-37a |
| CMD-38a | essentials-player: `/fly /god /speed /feed /heal /repair /hat` | P | M | P1, CMD-32a |
| CMD-38b | essentials-player: ник/инфо/afk/позиция/прочее | P | M | P1, CMD-32a, CMD-32c, CMD-38a |
| CMD-39a | essentials-inv: `/invsee /enderchest /clearinventory` | P | M | P1, CMD-32b |
| CMD-39b | essentials-inv: станции, `/trash`, киты | P | M | P1, CMD-32b, CMD-39a |
| CMD-40 | essentials-chat: только новые имена, хук на ядровый `/msg` | P | M | P1, CMD-32c, CMD-33b |
| CMD-41a | moderation: mute/warn/tempban, контроль чата | P | M | P1, CMD-32a, CMD-40 |
| CMD-41b | moderation: jail/vanish/freeze/sudo/report/history | P | M | P1, CMD-32a, CMD-40, CMD-41a |
| CMD-42 | economy (порт P2 на WASM + ledger) | P | M | P1, P2 |
| CMD-43a | shops: сундук-шоп и admin-shop | P | M | P1, CMD-42 |
| CMD-43b | shops: аукцион и `/sell` | P | M | P1, CMD-42, CMD-43a |
| CMD-44 | social (`/party /friend /tpparty`) | P | S | P1, CMD-32a, CMD-37b |
| CMD-45 | integrations (Discord, NuVotifier, `/link`, web-stats) | P | M | P1, PA3, CMD-32c |
| CMD-46 | fun/cosmetic (firework/smite/tree/book/powertool/pet) | P | M | P1, CMD-32a, CMD-32c |
| CMD-47 | stats (`/stats /leaderboard /playtime` через PA2) | P | M | P1, PA2 |

## Порядок и волны (что можно параллельно)

Граф зависимостей строго назад (топология проверена). Внешние: `oc/P1` (**в main нет**),
`oc/P2`, `oc/PA2`, `oc/PA3`.

- **Волна 0:** CMD-01.
- **Волна 1:** CMD-02, CMD-03.
- **Волна 2:** CMD-04, CMD-05, CMD-06a, CMD-06b, CMD-07.
- **Волна 3 (ядро, параллельно):** CMD-08, 09, 10a, 10b, 11a, 11b, 12a, 12b, 13a, 13b, 14a, 15a,
  16, 17a, 17b, 19, 20a, 20b, 20c, 23, 25, 26, 27.
- **Волна 4:** CMD-14b (после 14a), CMD-15b (после 15a), CMD-18 (после 15a), CMD-21a (после 20a/b/c),
  CMD-28 (после 27).
- **Волна 5:** CMD-21b, CMD-21c (после 21a), CMD-22a (после 21b), CMD-24 (после 21a).
- **Волна 6:** CMD-22b (после 15a и 22a).
- **После `oc/P1` (ядро-API):** CMD-29a, 30a, 31, 32a параллельно; затем CMD-29b, 30b, 32b, 32c.
- **Плагины после P1:** CMD-33a/34a/35a/37a/38a/39a/46 параллельно (каждый за своим core-API),
  затем вторые половины (33b/34b/35b/36a/37b/38b/39b), далее 36b, 40, 41a→41b, 42 (после P2),
  43a→43b, 44, 45 (после PA3), 47 (после PA2).

Ключевые цепочки: `CMD-01 → 03 → 06a → 06b → 08…22`; `CMD-20a/b/c → 21a → 21b → 22a → 22b`;
`CMD-29a → 29b → 34a/35a`; `CMD-30a → 30b → 36a → 36b`; `CMD-31 → 37a → 37b`;
`CMD-32a → 32b/32c → 38/39/40/45/46`; `CMD-40 → 41a → 41b`; `CMD-42 → 43a → 43b`.

## Открытые вопросы

1. **`/rotate` — ванильная с 1.21.2** (`/rotate <target> <rotation>` / `facing …`, узел
   `minecraft.command.rotate`, op 2). Специфицирована в CMD-11b; на кодеках < 1.21.2 гейтится,
   т.к. отдельного пакета произвольного поворота энтити до неё нет. Вопрос снят.
2. **`/publish` — команда встроенного сервера одиночной игры**; на дедике ваниль её не отдаёт.
   CMD-09 требует решить по фактическому поведению 1.21.11, а не выдумывать.
3. **`/visibility` существует в main** (`game/commands.rs:212`, `protocol/commands.rs:75`, влит с
   S1). В этой ветке его ещё нет (main новее), но в планировании он считается существующим: в
   заготовках не дублируется, CMD-04/CMD-03 мигрируют его на дерево как рабочую команду. Вопрос
   снят.
4. **`needs` расширен** сверх `oc/CMD-*`/`oc/P1`: экономика/интеграции/статистика реально зависят
   от `oc/P2`, `oc/PA3`, `oc/PA2`.
5. **`oc/P1` ещё не в main** — скелеты плагинов только манифест+README; полный код после слияния.
6. **Экономика** в каталоге — плагин, но её сервис — P2 (WIT + ledger). CMD-42 — порт; встроенная
   остаётся fallback до conformance-набора.
7. **`/spawn` и essentials-teleport.** По правилу владения ядро оставляет `/spawn` себе; чтобы
   плагин мог задавать свою точку, ядровый `/spawn` должен читать **сервис-провайдер точки
   спавна**, а не плагин отвечать тем же именем. Нужно решить, вводим ли такой сервис (и в какой
   задаче), либо отказываемся от кастомного `/spawn`.
8. **`/heal` / `/feed` у essentials-player** пересекаются с существующей ядровой `/heal` (Aether).
   По правилу ядро владеет именем; плагин либо вешает хук, либо берёт другое имя. Нужно решение.
9. **`operators` как seed:** конфиг применяется как членство в группе `op` (уровень 4) при старте,
   дальше источник истины — хранилище прав. Осталось решить и записать правило при удалении имени
   из конфига (не деопать задним числом) и запрет удаления группы `op`.
10. **`permission node` vs `op level`:** сейчас в ядре только `is_operator`. CMD-01 вводит узлы и
    уровни; решить, остаётся ли `operators` лишь начальным наполнением групп.
