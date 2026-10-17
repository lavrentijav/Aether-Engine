# CMD-09 — независимое ревью 1 (итог по 705ee2d)

Ветка: `oc/CMD09`. После `git fetch origin` и `origin/oc/CMD09`, и локальный
`oc/CMD09` указывают на **`705ee2d`** («CMD09: align server commands with
1.21.11 command data»). Это последний коммит; весь вердикт ниже относится к
нему, а не к `954f4c6`. Merge-base с `main`: `8247549`.
`git diff 8247549..705ee2d` — 30 файлов, +1034/−130. Follow-up
`954f4c6..705ee2d` — 13 файлов, +284/−66; он разобран отдельно в разделе
«Delta».

Ревью шло в worktree `/srv/dev/projects/Aether-Engine-wt/crit-CMD09`;
`HEAD = 705ee2d`. Окружение: общий
`CARGO_TARGET_DIR=/srv/dev/build/aether-target`, `CARGO_INCREMENTAL=0`,
только targeted-гейты.

Дополнительно поведение сверялось с **живым dedicated-сервером 1.21.11**
(`server.jar` из `/tmp/aetherversions/1.21.11`, offline-mode, flat-world):
`/version`, `/seed`, `/save-all`, `/save-off`, `/save-on`,
`/setidletimeout`, `/transfer`, `/publish`, `/list`, `/stop` выполнялись из
консоли после полного старта. Ниже «vanilla» — реальный вывод этого сервера,
а не только `commands.json`.

## Гейты (на 705ee2d)

* `cargo fmt --check` — **0**.
* `cargo clippy -p aether-server --all-targets -- -D warnings` — **0**.
* `cargo test -p aether-server --lib` — **1510 passed, 0 failed**.

Гейты зелёные, паник и падений тестов нет, но ряд пользовательских текстов и
поведений не соответствует 1.21.11 (и часть — самому брифу).

## Delta `954f4c6..705ee2d` — что проверено

1. **Bare `/seed`.** Дерево (`commands/server.rs:135-138`), каталог
   (`commands/catalogue.rs:406`) и обе таблицы `docs/COMMANDS*.md` больше не
   объявляют аргумент; лишнее слово даёт `Incorrect argument for command`.
   Это совпадает с 1.21.11 `commands.json` (у `/seed` нет children).
   **Но** текст ответа и шапка модуля не догнали изменение — см. находки 1 и 8.
2. **Целочисленные границы.** Добавлены `ArgKind::IntegerRange` и
   `ArgProperties::IntegerBounds` (`commands/tree.rs`, `commands/args.rs`);
   границы проверяются при парсинге и пишутся в declaration flags+int.
   Сверено с `commands.json`: `setidletimeout <minutes>` min 0; `transfer
   ... [port]` 1..=65535; `publish ... [port]` 0..=65535. Тесты на
   `-1`/`70000`/`0` дают точные `argument.integer.low/big`. Собственные
   проверки в `exec_setidletimeout`/`exec_transfer` после этого стали
   мёртвыми, но не мешают.
3. **`/transfer` players-only.** `ArgKind::Players` даёт flags `0x02`
   (`EntityArgument.players()` в vanilla — `(single=false, players=true)`);
   `@e[type=minecraft:cow]` отвергается с
   `Only players may be affected by this command, but the provided selector
   includes entities`, `@a[name=...]` принимается. Это верно. **Но** тексты
   ошибок при пустом/неизвестном выборе остались неверными — находка 2.
4. **`/save-all` пишет игроков.** `save_all` (`commands/server.rs:86-98`)
   сохраняет инвентарь и состояние каждого full-клиента до `flush_world`;
   тест `server.rs:553-566` проверяет, что мета игрока появилась. `/save-off`
   не мешает явному `/save-all`. Верно. **Но** промежуточная строка vanilla
   отсутствует — находка 3.
5. **Порядок `/list`.** Вместо алфавита теперь `sort_by_key(entity_id)`
   (`server.rs:252-254`). В обычном случае это порядок подключения, как в
   vanilla (`PlayerList.players` — список вставки). Оговорка: `entity_id`
   выделяется в `session.rs:253` до логина, а в реестр игрок попадает в
   `session.rs:477`; при одновременных медленном и быстром логинах порядок
   может отличаться от vanilla — находка 7 (LOW).
6. **Packet ids.** Для 1.21.11 `cb_transfer = 127 (0x7F)` совпадает с
   `generated/reports/packets.json` (`minecraft:transfer` clientbound = 127).
   Кодирование — `string(host)` + `var_int(port)`, как в
   `ClientboundTransferPacket`. Для 766 `protocol/v766` падает в
   `modern::Codec` (`v766/mod.rs:315`), а его таблица `v1_20_5` содержит
   `cb_transfer: 115`; клиенты < 766 отсекаются. Проверен локальными данными
   только 774 (127); id для 766/767 (115) и 768–777 (122, 127, 129, 132)
   взяты из notes/PrismarineJS и по имеющемуся отчёту 1.21.11 независимо не
   перепроверены.

## Вердикт: CHANGES

## Находки

### 1. MEDIUM — `/seed` не печатает квадратные скобки

`crates/aether-server/src/commands/server.rs:45`:
`SEED = "Seed: %s"`; `exec_seed` (`server.rs:281-283`) отдаёт строку без
скобок. В 1.21.11 `commands.seed.success` — `Seed: [%s]`; живой сервер:

```text
Seed: [1827702531091598271]
```

Бриф (`docs/tasks/CMD-09-admin-server.md`) тоже указывает `Seed: [<seed>]`.
Тест `server.rs:626-639` закрепляет строку без скобок.

Дополнительно: `crate::seed()` — `u64` (`lib.rs:550-552`,
`config.rs:56`), а vanilla печатает signed `long`. Для seed > `i64::MAX`
вывод разойдётся, а отрицательный seed конфиг не принимает вовсе. Это не
регрессия CMD-09, но для точности стоит печатать `seed as i64`.

Fix: `"Seed: [%s]"` с `crate::seed() as i64`, тест обновить.

### 2. MEDIUM — у `/transfer` перепутаны два разных en_us-сообщения

`server.rs:342-344` переводит любое `No player was found` в
`commands.transfer.error.no_players` («Must specify at least one player to
transfer»), а `server.rs:347-353` для консоли без аргумента `players` отдаёт
`No player was found`.

Живой 1.21.11 ведёт себя иначе:

* `/transfer example.com` из консоли →
  `A player is required to run this command here` (`permissions.requires.player`,
  из `getPlayerOrException`), а не `No player was found`.
* `/transfer example.com 25565 @a[distance=1000..]` → `No player was found`
  (`argument.entity.notfound.player`; `EntityArgument.getPlayers` бросает
  исключение раньше, чем `TransferCommand` проверит пустой список).
* `/transfer example.com 25565 NoSuchPlayer` → `No player was found`.
* `/transfer example.com 25565 @a` на пустом сервере → `No player was found`.

То есть `commands.transfer.error.no_players` для этой команды в 1.21.11
практически недостижим. Тесты `server.rs:707-712` и
`server.rs:727-732` закрепляют неверный текст.

Дополнительно: vanilla `EntityArgument.players` принимает UUID игрока, а
`resolve_targets` для не-`@` текста ищет только по нику, поэтому
`/transfer host port <uuid>` даст `No player was found`.

Fix: для отсутствующего `players` у серверного источника отдавать
`A player is required to run this command here`; пустой/неизвестный
селектор и ник — `No player was found` (не переводить в
`TRANSFER_NO_PLAYERS`). Константа `TRANSFER_NO_PLAYERS` станет
неиспользуемой; тесты поправить.

### 3. LOW/MEDIUM — `/save-all` не печатает первую строку «Saving the game…»

`exec_save_all` (`server.rs:207-214`) отвечает только `Saved the game`.
1.21.11 сначала отправляет `commands.save.saving` =
`Saving the game (this may take a moment!)`, затем после записи —
`Saved the game`:

```text
Saving the game (this may take a moment!)
Saved the game
```

Это видно и для `/save-all`, и для `/save-all flush`. Тест
`server.rs:553-558` ожидает одну строку. Важен и порядок: vanilla отправляет
«Saving…» **до** записи; текущий `Reply` формируется только после
`save_all`, поэтому даже две строки в одном ответе будут обеими «после».
Бриф в таблице перечисляет только success-строку, но раздел «real effects» и
требование точных en_us 1.21.11 говорят за первую строку.

Fix: отправить `Saving the game (this may take a moment!)` перед `save_all`
(для игрока — через `handle.emit`, для консоли/RCON — первой строкой
ответа), `Saved the game` — после успеха; при ошибке — первая строка и
`Unable to save the game (is there enough disk space?)`.

### 4. MEDIUM — `/version` печатает старый однострочный формат вместо 1.21.11

`server.rs:46` содержит
`"This server is running Aether Engine version %s (MC: 1.21.11)"`;
`exec_version` (`server.rs:285-287`) отдаёт ровно одну строку. В 1.21.11
`/version` переработан: живой сервер печатает заголовок и девять полей
(`commands.version.header`, `.id`, `.name`, `.data`, `.series`, `.protocol`,
`.build_time`, `.pack.resource`, `.pack.data`, `.stable.yes/no`):

```text
Server version info:
id = 1.21.11
name = 1.21.11
data = 4671
series = main
protocol = 774 (0x306)
build_time = Tue Dec 09 12:20:42 UTC 2025
pack_resource = 75.0
pack_data = 94.1
stable = yes
```

Строки `This server is running … (MC: …)` в 1.21.11 больше нет. Тест
`server.rs:642-651` закрепляет старый формат.

Здесь есть конфликт документов: таблица брифа перечисляет именно старую
однострочную строку, и код следует ей. Но бриф озаглавлен «vanilla 1.21.11»,
а ревью требует точных en_us 1.21.11 — значит, либо реализовать формат
релиза (и поправить бриф/notes), либо явно записать однострочник как
сознательное отклонение. Сейчас `CMD-09-notes.md` заявляет выравнивание по
1.21.11, а `STATUS.md` — «at their vanilla levels», что неверно.

### 5. LOW/MEDIUM — `/publish` на dedicated 1.21.11 отсутствует, а не «падает»

`server.rs:177-198` регистрирует `/publish`, `exec_publish`
(`server.rs:307-309`) всегда отвечает `Unable to host local game`.
`docs/tasks/CMD-09-notes.md:10-12` утверждает, что `/publish` «is present on
the dedicated server and fails there», `docs/STATUS.md:80` — «`/publish`
fails like a dedicated server».

Живой dedicated 1.21.11 на `/publish`, `/publish true creative 25565`
отвечает `Unknown or incomplete command`: команда **отсутствует** в дереве.
Бриф допускает «absent from the tree or fail like vanilla» и требует
«decide from the 1.21.11 behaviour and record it». Записанное поведение
фактически неверно; правильный вариант для dedicated — отсутствие. Сейчас
клиенты видят лишний узел, `/help` показывает команду, которой у vanilla нет.

Fix: убрать узел `/publish` (и отметить в каталоге/notes, что на dedicated
её нет) либо явно записать сознательное отклонение, не называя его
«vanilla dedicated».

### 6. LOW — idle-kick не отправляет `multiplayer.disconnect.idling`

`session.rs:739-744` при истечении idle-таймаута вызывает
`handle.disconnect("You have been idle for too long!")`, а
`PlayerHandle::disconnect` (`players.rs:477-479`) лишь закрывает outbox и
shutdown’ит сокет. Play-phase Disconnect-пакета нет вообще (в
`ServerEvent` есть только login-phase Disconnect). Клиент увидит обрыв
соединения, а не сообщение об idle; причина остаётся только в логе.

`docs/STATUS.md:80` при этом утверждает «idle timeouts kick with
`multiplayer.disconnect.idling`». Это неверно. Сам kick (закрытие
соединения) работает, поэтому LOW; но либо нужен play-phase Disconnect с
`multiplayer.disconnect.idling`, либо STATUS нужно исправить.

### 7. LOW — `/list` сортирует по `entity_id` (время accept), а не по вставке в реестр

`server.rs:252-254` комментирует «entity ids are allocated in that order».
`entity_id` выдаётся в `session.rs:253`, до логина, а `registry.join` —
в `session.rs:477`. Vanilla печатает `PlayerList.players` в порядке
добавления после логина. При одновременных медленном и быстром логинах
порядок может отличаться. Это лучше прежнего алфавита, но не строго
vanilla; если важна точность — хранить sequence при `join`.

### 8. LOW — drift в документации и гарантиях

* `server.rs:9` (шапка модуля) всё ещё перечисляет `/seed [<world>]`, хотя
  дерево и `server.rs:16-18` уже bare `/seed`.
* `server.rs:20-22` и `STATUS.md:80` говорят, что сохранения
  «serialised», но `SAVE_LOCK` (`server.rs:62`) берётся только в
  `save_all` (`server.rs:90`). Shutdown-путь `ops.rs:90-93`
  (`save_time`/`blockticks::save`/`fluids::save`/`world.flush`) и тиковый
  save идут мимо него. Два flush одновременно (например, `/save-all` во
  время `/stop`) возможны. Тест `server.rs:570-579` проверяет только два
  `save_all`, а не конкуренцию с shutdown.
* `docs/tasks/CMD-09-notes.md` заявляет выравнивание по 1.21.11, хотя
  находки 1–4 показывают обратное.

Fix: синхронизировать шапку, notes и STATUS; либо провести shutdown через
`SAVE_LOCK`, либо смягчить формулировку «serialised».

## Что проверено и работает

* Все команды брифа зарегистрированы с нужными op-уровнями: `/stop`,
  `/save-all`, `/save-on`, `/save-off`, `/publish` — 4; `/seed`,
  `/version` — 2; `/setidletimeout`, `/transfer` — 3; `/list` — 0 без
  requirement. Отказы уровня 2/3 и доступ level 3 к player-командам покрыты
  тестами (`server.rs:765+`).
* `/list`/`/list uuids`: формат
  `There are <n> of a max of <m> players online: <names>` совпадает с
  vanilla, пустой список даёт хвостовой пробел; UUID — hyphenated;
  staff-vanish фильтруется для игрока.
* `/save-all` синхронный: пишет состояние онлайн-игроков и мир
  (`blockticks`, `fluids`, время, `World::flush`) и только потом отвечает;
  `/save-off` не мешает явному `/save-all`; `/stop` сохраняет и завершает
  процесс. `/save-all` выполняется на потоке источника (сессия/консоль), не
  на тик-потоке; но `World::flush` держит `cache.read()` на время записи
  (`aether-api/src/lib.rs:812`), а `set_block` берёт `cache.write()`
  (`:625`), поэтому большой save может подтормозить тик. Это свойство
  существующего flush, не новое в CMD-09, но «не блокирует тик» не
  гарантируется.
* `/save-on`/`/save-off` идемпотентны с правильными текстами
  (`Automatic saving is now …` / `Saving is already turned …`); autosave
  (поток и тик) и 30-секундный player-save уважают `/save-off`.
* `/setidletimeout`: min 0, `0` выключает, отрицательное даёт
  `Integer must not be less than 0: found -1`; idle-таймер не сбрасывается
  keep-alive-ответами (только действиями), kick с
  `You have been idle for too long!` (но без пакета — находка 6).
* `/seed` больше не принимает аргумент (лишний — `Incorrect argument for
  command`), что соответствует 1.21.11.
* `/transfer`: пакет `Transfer` — правильный id (`0x7F` для 774), host
  string + port VarInt; для клиентов < 766 команда честно отказывает; пакет
  ставится в outbox с закрытием после записи (`push_then_close`), не теряясь
  при shutdown сокета; селектор ограничен игроками. Тексты ошибок — находка 2.
* `/stop` отвечает `Stopping the server` (совпадает с 1.21.11,
  `commands.stop.stopping`), затем shutdown-поток сохраняет и выходит.
* `/reload` не входит в CMD-09 (остаётся pending CMD-21), `/kick` — CMD-08;
  регрессий в них не найдено.
