# CMD0 — каталог команд → задачи, заготовки, очередь

Статус: ✅ сделано (ветка `oc/CMD0`). Источник: `docs/design/commands.md`, `docs/design/plugins.md`,
`CONTRIBUTING.md` («Core or plugin?»).

## Что сделано в CMD0

Каталог команд (`docs/design/commands.md`) разложен на **47 задач** — по одной на сессию агента.
Каждая задача — отдельный бриф `docs/tasks/CMD-<id>-<slug>.md`: цель, точный синтаксис и поведение
(ваниль 1.21.11 или референс-плагин: permission node, op level, текст ответа), файлы, **явные
краевые случаи для тестов**, зависимости (`needs:`), критерии готовности.

Три дополнительных артефакта:

1. **Заготовки в ядре** — `crates/aether-server/src/commands/registry.rs`: запись на каждую ещё не
   реализованную core-команду с permission node, op level и id задачи. Неизвестная команда из
   каталога отвечает `not implemented yet (CMD-<id>)` с проверкой прав; `/help` их перечисляет по
   группам. Уже существующие команды (`/gamemode`, `/give`, `/lookup`, экономика, …) не тронуты —
   имя команды отвечает ровно одно место.
2. **Заготовки плагинов** — `plugins/<name>/plugin.toml` + `README.md` для всех 15 плагинов
   (`permissions`, `regions`, `claims`, `worldedit`, `essentials-teleport/-player/-inv/-chat`,
   `moderation`, `economy`, `shops`, `social`, `integrations`, `fun`, `stats`). Хоста плагинов
   (`oc/P1`) в main ещё нет, поэтому **код не писался** — только манифест и README со списком
   команд и id задачи.
3. **Манифест очереди** — `docs/tasks/CMD-queue.json`: массив `{id, title, brief, needs, size, core}`
   в топологически верном порядке.

Задачи **не запускались** — это только планирование.

## Core или плагин — почему так

Правило из `CONTRIBUTING.md`: **ядро** — то, что нужно каждому серверу и/или лежит на горячем пути;
**плагин** — опциональное и не критичное по производительности, на API плагинов.

- **Ядро:** Brigadier-дерево, парсер селекторов/координат, проверка прав, фидбек и `/help`, консоль
  и RCON; все ванильные команды; серверные операции; журнал истории; **механизмы**, нужные плагинам —
  индекс регионов и проверка защиты (вызывается на каждое событие с блоком), bulk-edit (миллионы
  блоков), телепорт с поиском безопасного места, player/UI API.
- **Плагины:** всё опциональное — экономика, магазины, дома/варпы, инвентари-семейство, чат,
  модерация, регионы/клеймы (модель и команды), WorldEdit, права-UI, соцсети, интеграции, развлечения,
  статистика.
- Пограничные случаи отмечены «core API + plugin»: ядро владеет быстрым механизмом, плагин —
  моделью, флагами и командами (регионы WG, клеймы GP, WorldEdit).

## Все 47 задач

**Легенда:**

- **C / P** — где живёт задача: **C** = core (ядро), **P** = plugin (плагин). Выбор по правилу из
  `CONTRIBUTING.md` («Core or plugin?»), см. раздел выше.
- **размер** — оценка объёма: **S** ≈ одна сессия агента, **M** ≈ 2–4 коммита, **L** — большой,
  дробить перед выдачей.
- **L→split** — задача помечена L и уже описана с разбиением на под-задачи (`CMD-36a/b`,
  `CMD-41a/b`) внутри брифа; в манифесте это одна строка, чтобы id оставались стабильными.
- **зависит** — id задач **этого же** манифеста, которые должны быть готовы раньше (граф строго
  назад, порядок строк — топологический).
- **внешние зависимости** (не CMD-задачи, а ветки/сервисы проекта):
  **P1** — хост плагинов (WASM, события, реестры; **в main ещё нет**),
  **P2** — сервис экономики (WIT + ledger),
  **PA1** — UI-API плагинов (окна, title/bossbar),
  **PA2** — read-only API истории журнала,
  **PA3** — сетевой доступ плагинов (HTTP).
- **op level** в брифах — ванильный уровень доступа 0–4 (0 всем … 4 владелец);
  **permission node** — узел прав (например `minecraft.command.ban`), проверяется ядром прав
  (CMD-01/02). Если узел не задан явно — действует уровень.

### Инфраструктура команд (core)

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-01 | Ядро прав: узлы, op-level 0–4, default true/false/op | C | M | — |
| CMD-02 | Группы, наследование, per-world контекст, meta (prefix/suffix) | C | M | CMD-01 |
| CMD-03 | Brigadier-дерево: парсинг, подсказки, диспетчер | C | L | CMD-01 |
| CMD-04 | Пакет Declare Commands для каждого кодека | C | M | CMD-03 |
| CMD-05 | Фидбек команд, `/help`, алиасы, `sendCommandFeedback` | C | M | CMD-03 |
| CMD-06 | Парсер селекторов `@a/@p/@r/@s/@e` и координат `~ ^` | C | L | CMD-03 |
| CMD-07 | Консоль и RCON как источники команд | C | M | CMD-03, CMD-01 |

### Ванильные команды — ядро

#### V-admin

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-08 | `/op /deop /ban /ban-ip /banlist /pardon /pardon-ip /kick /whitelist` | C | M | CMD-05, CMD-01, CMD-06 |
| CMD-09 | `/stop /save-all /save-on /save-off /list /seed /version /setidletimeout /transfer /publish` | C | M | CMD-05 |

#### V-player

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-10 | `/clear /effect /enchant /experience /attribute /damage` | C | L | CMD-06, CMD-05, CMD-01 |
| CMD-11 | `/teleport /tp /spreadplayers /spawnpoint /spectate /ride /item /kill` | C | L | CMD-06, CMD-05, CMD-01 |

#### V-world

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-12 | `/setblock /fill /clone /fillbiome /place /forceload` | C | L | CMD-06, CMD-05, CMD-01 |
| CMD-13 | `/time /weather /worldborder /setworldspawn /gamerule /difficulty /tick /locate /defaultgamemode /waypoint` | C | L | CMD-06, CMD-05, CMD-01 |

#### V-data

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-14 | `/data` (NBT чтение/правка блоков, сущностей, storage) | C | L | CMD-06, CMD-05, CMD-01 |
| CMD-15 | `/scoreboard` (objective, criteria, players, display) | C | L | CMD-05, CMD-01, CMD-06 |
| CMD-16 | `/tag`, `/team`, `/teammsg` | C | M | CMD-06, CMD-05, CMD-01 |
| CMD-17 | `/say /me /msg /tellraw /title /bossbar /dialog` | C | L | CMD-06, CMD-05, CMD-01 |
| CMD-18 | `/trigger` | C | S | CMD-15, CMD-05 |
| CMD-19 | `/particle /playsound /stopsound` | C | M | CMD-06, CMD-05, CMD-01 |
| CMD-20 | `/loot /recipe /advancement /random /schedule` | C | L | CMD-06, CMD-05, CMD-01 |
| CMD-21 | `/function /datapack /reload /return /test` + загрузчик датапаков | C | L | CMD-05, CMD-01, CMD-20 |
| CMD-22 | `/execute` (модификаторы, условия, store) | C | L | CMD-06, CMD-05, CMD-15, CMD-21 |

### Серверные операции и история (core)

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-23 | `/tps /mspt /gc /plugins /restart /debug /perf /jfr` | C | M | CMD-05, CMD-01 |
| CMD-24 | `/aether reload` и `/aether backup` (согласованный снапшот) | C | M | CMD-05, CMD-01, CMD-21 |
| CMD-25 | `/aether profile` — подтиковый профайлер | C | M | CMD-05, CMD-01, P1 |
| CMD-26 | `/aether world` — список/загрузка/выгрузка миров | C | M | CMD-05, CMD-01 |
| CMD-27 | История: `/co`, фильтры lookup, превью | C | M | CMD-06, CMD-05, CMD-01 |
| CMD-28 | История: `/restore` и `/purge` | C | M | CMD-27 |

### Core API для плагинов

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-29 | Индекс регионов (R-tree по чанкам) + проверка защиты | C | L | CMD-01, P1 |
| CMD-30 | Bulk-edit: пакетные правки блоков, один журнал, undo | C | L | CMD-01, P1 |
| CMD-31 | Телепорт + поиск безопасного места | C | M | CMD-01, P1 |
| CMD-32 | Player/UI API (окна, title/bossbar, эффекты, scheduler) — PA1 | C | L | CMD-01, P1 |

### Плагины

| id | что | C/P | размер | зависит |
|---|---|---|---|---|
| CMD-33 | Права-UI (`/lp`, треки, meta, веб-редактор) | P | L | P1, CMD-02 |
| CMD-34 | Регионы WorldGuard (модель, флаги, `/rg`) | P | L | P1, CMD-29 |
| CMD-35 | Клеймы GriefPrevention (`/claim`, доверие, золотая лопата) | P | L | P1, CMD-29 |
| CMD-36 | WorldEdit (выделение, clipboard, `//`, схемы, кисти) | P | L→split | P1, CMD-30, CMD-06 |
| CMD-37 | essentials-teleport (дома, варпы, `/tpa`, `/rtp`, `/back`) | P | L | P1, CMD-31 |
| CMD-38 | essentials-player (`/fly /god /speed /nick /afk` …) | P | L | P1, CMD-32 |
| CMD-39 | essentials-inv (`/invsee /ec /workbench /kit` …) | P | L | P1, CMD-32 |
| CMD-40 | essentials-chat (`/msg /r /mail /ignore /socialspy`, формат) | P | M | P1, CMD-32, CMD-33 |
| CMD-41 | Модерация (mute/warn/tempban/jail/vanish/freeze/report) | P | L→split | P1, CMD-32, CMD-40 |
| CMD-42 | Экономика (P2-порт на WASM + ledger) | P | M | P1, P2 |
| CMD-43 | Магазины (сундук-шоп, аукцион, admin-shop) | P | L | P1, CMD-42 |
| CMD-44 | Соцсети (`/party /friend /tpparty`) | P | S | P1, CMD-32, CMD-37 |
| CMD-45 | Интеграции (Discord, NuVotifier, `/link`, web-stats) | P | M | P1, PA3, CMD-32 |
| CMD-46 | Развлечения (firework/smite/tree/book/powertool/pet) | P | M | P1, CMD-32 |
| CMD-47 | Статистика (`/stats /leaderboard /playtime` через PA2) | P | M | P1, PA2 |

## Порядок и волны (что можно параллельно)

Зависимости CMD — строго назад (топологический порядок проверен). Внешние зависимости:
`oc/P1` (хост плагинов — **в main ещё нет**), `oc/P2` (экономика), `oc/PA2` (история-API),
`oc/PA3` (сеть).

- **Волна 0:** CMD-01.
- **Волна 1:** CMD-02, CMD-03 (параллельно).
- **Волна 2:** CMD-04, CMD-05, CMD-06, CMD-07 (после 03).
- **Волна 3 (ядро, параллельно):** CMD-08, 09, 10, 11, 12, 13, 14, 15, 16, 17, 19, 20, 23, 26, 27.
- **Волна 4:** CMD-18 (после 15), CMD-21 (после 20), CMD-28 (после 27).
- **Волна 5:** CMD-22 (после 15+21), CMD-24 (после 21).
- **После `oc/P1` (ядро-API, параллельно):** CMD-25, 29, 30, 31, 32.
- **Плагины после P1:** CMD-33/34/35/36/37/38/39/46 (параллельно, каждый за своим core-API),
  затем CMD-40 (после 33), CMD-41 (после 40), CMD-42 (после P2), CMD-43 (после 42),
  CMD-44 (после 37), CMD-45 (после PA3), CMD-47 (после PA2).

Ключевые цепочки: `CMD-01 → 03 → 06 → 08/10-22`; `CMD-20 → 21 → 22`;
`CMD-29 → 34/35`; `CMD-30 → 36`; `CMD-31 → 37`; `CMD-32 → 38/39/40/46`; `CMD-40 → 41`;
`CMD-42 → 43`.

## Открытые вопросы

1. **`/rotate` не существует в ванильной 1.21.11** — нечего повторять. Оставлен в каталоге и
   зарегистрирован как расширение Aether (CMD-11); предлагаю убрать или определить явно.
2. **`/publish` — команда встроенного сервера одиночной игры**; на дедике ваниль её не отдаёт.
   CMD-09 требует решить по фактическому поведению 1.21.11, а не выдумывать.
3. **`/visibility` числится «уже в Aether»**, но в исходниках main есть только `/cache`.
   В заготовках он не дублируется; нужно уточнить, существует ли он.
4. **Расширение схемы `needs`:** в брифе допускались только `oc/CMD-*` и `oc/P1`, но экономика,
   интеграции и статистика реально зависят от `P2`, `PA3`, `PA2` — в манифесте указаны эти ветки.
5. **`oc/P1` ещё не в main**, поэтому скелеты плагинов — только манифест+README, без кода. Полный
   код плагинов начнётся после слияния P1.
6. **Экономика: каталог относит её к плагинам, но P2** (WIT-сервис + ledger). CMD-42 — порт;
   встроенная остаётся fallback до прохождения conformance-набора.
7. **Крупные задачи** CMD-36 (WorldEdit) и CMD-41 (модерация) помечены L и требуют дробления на
   `CMD-36a/b` и `CMD-41a/b`; это описано внутри брифов, чтобы не плодить строки манифеста.
8. **Op level vs permission node:** в ядре сейчас только `is_operator` (список в конфиге). CMD-01
   вводит узлы и уровни; нужно решить, остаётся ли конфиг `operators` источником прав или
   становится лишь начальным наполнением групп.
