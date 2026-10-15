# CMD0 — каталог команд → задачи, заготовки, очередь

Статус: ✅ сделано (ветка `oc/CMD0`), обновлено по ревью владельца (3 уровня плагинов). Источник:
`docs/design/commands.md`, `docs/design/plugins.md`, `CONTRIBUTING.md` («Core or plugin?»).

## Что сделано в CMD0

Каталог команд разложен на **74 задачи** — по одной на сессию агента (крупные группы раздроблены).
Каждая задача — бриф `docs/tasks/CMD-<id>-<slug>.md`: цель, синтаксис и поведение (ваниль 1.21.11
или референс-плагин: permission node, op level, текст ответа), файлы, **явные краевые случаи для
тестов**, зависимости (`needs:`), критерии готовности.

Три артефакта:

1. **Заготовки в ядре** — `crates/aether-server/src/commands/registry.rs`: запись на каждую ещё не
   реализованную core-команду с permission node, op level и id задачи. Команда из каталога отвечает
   `not implemented yet (CMD-<id>)` с проверкой прав; `/help` перечисляет их по группам.
2. **Заготовки плагинов** — `plugins/<name>/plugin.toml` + `README.md` для 15 плагинов, помеченных
   уровнем (**native** или **WASM**). Хоста (`oc/P1`) и нативного загрузчика (`oc/CMD-00N`) в main
   нет, поэтому кода нет — только манифест и README.
3. **Манифест очереди** — `docs/tasks/CMD-queue.json`: `{id, title, brief, needs, size, tier}` в
   топологически верном порядке (`tier` = `C` / `N` / `W`).

Задачи **не запускались** — это планирование.

## Три уровня: C / N / W

| тир | где | как | для чего |
|---|---|---|---|
| **C — ядро** | `crates/` | код движка | только то, что нужно **каждому** серверу, и горячие механизмы **без политики** |
| **N — нативный плагин** | `plugins/<name>/` (Rust-крейт этого workspace) | линкуется **в процесс**, тот же API/события, что у WASM, **без границы**, полная скорость | необязательное, но **чувствительное к скорости или глубоко интегрированное** |
| **W — WASM-плагин** | `plugins/<name>/` (`.wasm`) | песочница Wasmtime, недоверенный/сторонний код | необязательное и **не** критичное по скорости |

- **Ядро без политики.** Решение, которое оператор захочет менять, — это политика, и она не в ядре.
  Там, где ядро обязано выполнить код на горячем пути, оно вызывает фиксированный **хук**; у хука
  нет поведения, кроме «разрешить». Ядро собирается и работает при **всех** выключенных плагинах.
- **Нативный плагин** — обычный Rust-крейт; тот же мир `aether:plugin` (lifecycle, события, импорты
  хоста), но вызов **прямой**, без сериализации и песочницы. Включается/выключается в конфиге,
  **никогда не требуется ядром**. Им доверяют больше, чем WASM: он в процессе.
- **WASM-плагин** — недоверенный, в песочнице (epoch/fuel/память, capabilities). Дороже по границе;
  для необязательного и не горячего.
- Подробности — `docs/design/plugins.md` («Tiers») и `CONTRIBUTING.md` / `CONTRIBUTING.ru.md`.

**Что уехало из ядра в нативные плагины** по решению владельца: хранилище прав + `/lp` (CMD-02,
CMD-33), профайлер (CMD-25), команды истории `/co` `/restore` `/purge` (CMD-27/28), модель и
проверки регионов/клеймов (CMD-29…35), bulk-edit/WorldEdit (CMD-30, CMD-36), модерация (CMD-41),
essentials-teleport/player/inv (CMD-37/38/39). **Ядро сохранило только минимальный хук** (события
P1 + примитивы записи блока/журнала); модели и политика — в плагине.

**Что осталось WASM:** экономика, магазины, чат-форматирование, соцсети, развлечения, интеграции,
статистика.

**Внимание:** каждая **нативная** задача требует загрузчика нативного уровня — **CMD-00N**
(`docs/tasks/CMD-00N-native-loader.md`, на API P1). Он стоит в манифесте **первым**, и все N-задачи
имеют `needs: oc/CMD-00N`. Пока P1 не влит, ни нативные, ни WASM-плагины не запускаются.

## Правила владения именами

- **Ванильное имя принадлежит ядру.** Плагин **не** перерегистрирует команду ядра; он вешает хук
  или добавляет **новое** имя.
  - `/msg /tell /w /say /me /tellraw /title /bossbar /dialog` — ядро (CMD-17); essentials-chat
    (CMD-40, WASM) добавляет только `/r /mail /ignore /socialspy /broadcast /motd /rules` и
    форматирует чат хуком на ядровый `/msg`.
  - `/tp /teleport` — ядро (CMD-11a); essentials-teleport (CMD-37, native) добавляет `/home /warp
    /tpa` и т.п., но `/spawn` не переопределяет (см. открытые вопросы).
- **Один диспетчер.** После CMD-03 не остаётся старого `match`-диспетчера: все существующие
  команды мигрированы на дерево в той же задаче, старый путь удалён.

## Все 74 задачи

**Легенда:**

- **тир** — `C` = ядро, `N` = нативный плагин, `W` = WASM-плагин.
- **размер** — `S` ≈ одна сессия, `M` ≈ 2–4 коммита, `L` — большой (остался один, CMD-03 — миграция
  существующих команд обязана быть в той же задаче).
- **зависит** — id задач этого манифеста, готовые раньше (граф строго назад, топологический порядок).
- **внешние зависимости** (не CMD): **P1** — хост плагинов (в main ещё нет), **CMD-00N** — нативный
  загрузчик, **P2** — сервис экономики, **PA1/PA2/PA3** — UI-API / история-API / сеть.
- **op level / permission node** — уровень 0–4 и узел прав; проверяет ядро прав (CMD-01).

### Платформа плагинов

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-00N | Нативный загрузчик (in-process тир на API P1) | N | M | P1 |

### Инфраструктура команд

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-01 | Ядро прав: узлы, op-level, default; `operators` — seed группы `op` | C | M | — |
| CMD-02 | Группы, наследование, per-world хранилище прав, группа `op` | N | M | CMD-00N, CMD-01 |
| CMD-03 | Brigadier-дерево + миграция ВСЕХ существующих команд, один диспетчер | C | L | CMD-01 |
| CMD-04 | Пакет Declare Commands для каждого кодека (из дерева) | C | M | CMD-03 |
| CMD-05 | Фидбек команд, `/help`, алиасы, `sendCommandFeedback` | C | M | CMD-03 |
| CMD-06a | Парсер аргументов и координат `~ ^`, повороты | C | M | CMD-03 |
| CMD-06b | Парсер селекторов `@a/@p/@r/@s/@e` + семантика выбора | C | M | CMD-03, CMD-06a |
| CMD-07 | Консоль и RCON как источники команд | C | M | CMD-03, CMD-01 |

### V-admin (core)

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-08 | `/op /deop /ban /ban-ip /banlist /pardon /pardon-ip /kick /whitelist` | C | M | CMD-05, CMD-01, CMD-06b |
| CMD-09 | `/stop /save-all /save-on /save-off /list /seed /version /setidletimeout /transfer /publish` | C | M | CMD-05 |

### V-player (core)

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-10a | `/clear /effect /enchant` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-10b | `/experience /xp /attribute /damage` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-11a | `/teleport /tp /spreadplayers /spawnpoint` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-11b | `/spectate /ride /item /kill /rotate` | C | M | CMD-06b, CMD-05, CMD-01 |

### V-world (core)

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-12a | `/setblock /fill /clone` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-12b | `/fillbiome /place /forceload` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-13a | `/time /weather /worldborder /setworldspawn /defaultgamemode` | C | M | CMD-06b, CMD-05, CMD-01 |
| CMD-13b | `/gamerule /difficulty /tick /locate /waypoint` | C | M | CMD-06b, CMD-05, CMD-01 |

### V-data (core)

| id | что | тир | размер | зависит |
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

### Серверные операции (core)

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-23 | `/tps /mspt /gc /plugins /restart /debug /perf /jfr` | C | M | CMD-05, CMD-01 |
| CMD-24 | `/aether reload` и `/aether backup` | C | M | CMD-05, CMD-01, CMD-21a |
| CMD-26 | `/aether world` — список/загрузка/выгрузка миров | C | M | CMD-05, CMD-01 |

### Core API для плагинов (core)

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-31 | телепорт + поиск безопасного места | C | M | CMD-01, P1 |
| CMD-32a | player API: состояние, эффекты/полёт/скорость, scheduler | C | M | CMD-01, P1 |
| CMD-32b | оконный/контейнерный API (PA1) | C | M | CMD-01, P1, CMD-32a |
| CMD-32c | UI-примитивы + поиск сервисов | C | M | CMD-01, P1, CMD-32a |

### Нативные плагины (N)

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-25 | `/aether profile` — подтиковый профайлер | N | M | CMD-00N, CMD-05, CMD-01 |
| CMD-27 | История: `/co`, фильтры lookup, превью | N | M | CMD-00N, CMD-06b, CMD-05, CMD-01 |
| CMD-28 | История: `/restore` и `/purge` | N | M | CMD-00N, CMD-27 |
| CMD-29a | индекс регионов (per-chunk, снапшоты) | N | M | CMD-00N, CMD-01 |
| CMD-29b | проверка защиты + хук-точки + bypass | N | M | CMD-00N, CMD-01, CMD-29a |
| CMD-30a | BulkEdit API + группировка журнала + undo | N | M | CMD-00N, CMD-01 |
| CMD-30b | публикация bulk-edit: resend/light/бюджет тика | N | M | CMD-00N, CMD-01, CMD-30a |
| CMD-33a | permissions: узлы и родители | N | M | CMD-00N, CMD-02 |
| CMD-33b | permissions: meta, треки, веб-редактор | N | M | CMD-00N, CMD-02, CMD-33a |
| CMD-34a | regions: модель + `/rg define/redefine/remove/priority/info/list` | N | M | CMD-00N, CMD-29b |
| CMD-34b | regions: флаги, участники/владельцы, bypass | N | M | CMD-00N, CMD-29b, CMD-34a |
| CMD-35a | claims: модель + `/claim /abandonclaim /claimslist` + лопата | N | M | CMD-00N, CMD-29b |
| CMD-35b | claims: доверие (access/container/build/manage) + лимиты | N | S | CMD-00N, CMD-29b, CMD-35a |
| CMD-36a | WorldEdit: выделение, `//set/replace/walls/outline`, `//undo/redo/expand/contract` | N | M | CMD-00N, CMD-30b, CMD-06b |
| CMD-36b | WorldEdit: clipboard, `//rotate/flip/stack/move`, `//schem`, кисти | N | M | CMD-00N, CMD-30b, CMD-36a |
| CMD-37a | essentials-teleport: дома, варпы, `/setspawn` | N | M | CMD-00N, CMD-31 |
| CMD-37b | essentials-teleport: `/tpa`, `/back`, вертикаль, `/rtp` | N | M | CMD-00N, CMD-31, CMD-37a |
| CMD-38a | essentials-player: `/fly /god /speed /feed /heal /repair /hat` | N | M | CMD-00N, CMD-32a |
| CMD-38b | essentials-player: ник/инфо/afk/позиция/прочее | N | M | CMD-00N, CMD-32a, CMD-32c, CMD-38a |
| CMD-39a | essentials-inv: `/invsee /enderchest /clearinventory` | N | M | CMD-00N, CMD-32b |
| CMD-39b | essentials-inv: станции, `/trash`, киты | N | M | CMD-00N, CMD-32b, CMD-39a |
| CMD-41a | moderation: mute/warn/tempban, контроль чата | N | M | CMD-00N, CMD-32a, CMD-40 |
| CMD-41b | moderation: jail/vanish/freeze/sudo/report/history | N | M | CMD-00N, CMD-32a, CMD-40, CMD-41a |

### WASM-плагины (W)

| id | что | тир | размер | зависит |
|---|---|---|---|---|
| CMD-40 | essentials-chat: только новые имена, хук на ядровый `/msg` | W | M | P1, CMD-32c, CMD-33b |
| CMD-42 | economy (порт P2 на WASM + ledger) | W | M | P1, P2 |
| CMD-43a | shops: сундук-шоп и admin-shop | W | M | P1, CMD-42 |
| CMD-43b | shops: аукцион и `/sell` | W | M | P1, CMD-42, CMD-43a |
| CMD-44 | social (`/party /friend /tpparty`) | W | S | P1, CMD-32a, CMD-37b |
| CMD-45 | integrations (Discord, NuVotifier, `/link`, web-stats) | W | M | P1, PA3, CMD-32c |
| CMD-46 | fun/cosmetic (firework/smite/tree/book/powertool/pet) | W | M | P1, CMD-32a, CMD-32c |
| CMD-47 | stats (`/stats /leaderboard /playtime` через PA2) | W | M | P1, PA2 |

## Порядок и волны (что можно параллельно)

Граф зависимостей строго назад (топология проверена). Внешние: `oc/P1` (**в main нет**),
`oc/CMD-00N` (нативный загрузчик, после P1), `oc/P2`, `oc/PA2`, `oc/PA3`.

- **Волна 0:** CMD-01.
- **Волна 1:** CMD-03.
- **Волна 2:** CMD-04, CMD-05, CMD-06a, CMD-06b, CMD-07.
- **Волна 3 (ядро, параллельно):** CMD-08, 09, 10a, 10b, 11a, 11b, 12a, 12b, 13a, 13b, 14a, 15a,
  16, 17a, 17b, 19, 20a, 20b, 20c, 23, 26.
- **Волна 4:** CMD-14b (после 14a), CMD-15b (после 15a), CMD-18 (после 15a), CMD-21a (после 20a/b/c).
- **Волна 5:** CMD-21b, CMD-21c (после 21a), CMD-22a (после 21b), CMD-24 (после 21a).
- **Волна 6:** CMD-22b (после 15a и 22a).
- **После `oc/P1`:** **CMD-00N** (нативный загрузчик), затем ядро-API CMD-31/32a/32b/32c.
- **Нативные плагины (после CMD-00N):** CMD-02, 25, 27, 29a, 30a, 33a, 34a/35a (после 29b) — первая
  волна, затем CMD-28, 29b, 30b, 33b, 34b, 35b, 36a, 37a, 38a, 39a, 41a, далее 36b, 37b, 38b, 39b,
  41b. (CMD-34a/35a ждут CMD-29b; CMD-36a ждёт CMD-30b; CMD-41a ждёт CMD-40.)
- **WASM-плагины (после P1):** CMD-40 (после CMD-32c и CMD-33b), CMD-42 (после P2) → CMD-43a → 43b,
  CMD-44 (после CMD-37b), CMD-45 (после PA3), CMD-46, CMD-47 (после PA2).

Ключевые цепочки: `CMD-01 → 03 → 06a → 06b → 08…22`; `CMD-20a/b/c → 21a → 21b → 22a → 22b`;
`P1 → CMD-00N → {CMD-02, 25, 27, 29a, 30a, 33a}`; `CMD-29a → 29b → 34a/35a`;
`CMD-30a → 30b → 36a → 36b`; `CMD-31 → 37a → 37b`; `CMD-32a → 32b/32c`;
`CMD-32c → 40 → 41a → 41b`; `CMD-42 → 43a → 43b`.

## Открытые вопросы

1. **`/rotate` — ванильная с 1.21.2** (`/rotate <target> <rotation>` / `facing …`, узел
   `minecraft.command.rotate`, op 2). Специфицирована в CMD-11b; на кодеках < 1.21.2 гейтится.
   Вопрос снят.
2. **`/publish` — команда встроенного сервера одиночной игры**; на дедике ваниль её не отдаёт.
   CMD-09 требует решить по фактическому поведению 1.21.11.
3. **`/visibility` существует в main** (`game/commands.rs:212`, `protocol/commands.rs:75`, влит с
   S1). В этой ветке его ещё нет (main новее); планирование считает его существующим, в заготовках
   не дублируется, CMD-03 мигрирует его на дерево как рабочую команду. Вопрос снят.
4. **Уровни `needs` расширены:** сверх `oc/CMD-*`/`oc/P1` используются `oc/CMD-00N`, `oc/P2`,
   `oc/PA2`, `oc/PA3`.
5. **`oc/P1` ещё не в main**, нативного загрузчика `oc/CMD-00N` тоже — скелеты плагинов только
   манифест+README; полный код после их слияния.
6. **Экономика** — WASM-плагин, но её сервис — P2 (WIT + ledger). CMD-42 — порт; встроенная остаётся
   fallback до conformance-набора.
7. **`/spawn` и essentials-teleport (native).** По правилу владения ядро оставляет `/spawn` себе;
   чтобы плагин задавал свою точку, ядровый `/spawn` должен читать **сервис-провайдер точки
   спавна** (нативный плагин), а не отвечать тем же именем. Нужно решить, вводим ли такой сервис.
8. **`/heal` / `/feed` у essentials-player (native)** пересекаются с существующей ядровой `/heal`.
   По правилу ядро владеет именем; плагин либо вешает хук, либо берёт другое имя. Нужно решение.
9. **`operators` как seed:** конфиг применяется как членство в группе `op` (уровень 4) при старте,
   дальше источник истины — хранилище прав (нативный CMD-02). Решить правило при удалении имени из
   конфига (не деопать задним числом) и запрет удаления группы `op`.
10. **Граница CMD-00N ↔ P1.** Нативные и WASM-плагины должны делить один порядок приоритетов и один
    реестр сервисов. Нужно решить, где живёт общий код (в `crates/aether-plugin`) и как нативный
    крейт объявляет манифест (растить `plugin.toml` или link-time регистрация) — это зафиксирует
    CMD-00N.
11. **Профайлер (CMD-25, native) и ядро.** Ядро предоставляет именованные scope-хуки (без политики);
    нативный плагин их собирает. Решить, что именно остаётся в ядре: только API scope, не UI.
