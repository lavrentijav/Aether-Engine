# CMD-34a — независимое ревью ветки `oc/CMD34a`

VERDICT: **CHANGES**

## Гейты

- `cargo fmt --all -- --check` — OK.
- `cargo-locked clippy --workspace --all-targets -j4 -- -D warnings` — OK.
  (Первый прогон упал на stale rlib в общем target: `aether-plugin` был собран
  из другого worktree без новых методов. После
  `cargo clean -p aether-plugin -p aether-plugin-sdk -p aether-region -p aether-regions-plugin -p aether-server`
  clippy прошёл; к коду ветки это отношения не имеет.)
- `cargo-locked test --workspace --locked` — OK: **2213 passed, 0 failed, 2 ignored**.

## CMD-29a/29b: регрессий не найдено

Модель, `RegionIndex`, `RegionStore` и проверка перенесены в `aether-region`
дословно (diff старого `crates/aether-server/src/region.rs` против нового
`crates/aether-region/src/lib.rs` — только doc-комментарии и `store()`).
Серверные хуки `denial_at` / `acting` / `bypass` остались прежними, точки
вызова break/place/interact/use-item/explosion не менялись. Старые тесты
перенесены (21 в `aether-region`), серверные bypass/denial-тесты сохранены.
Поведение `RegionStore` (snapshot-swap, priority desc/id asc, `Defer`,
world-filter, membership, bypass) не изменилось.

Архитектурно новый крейт оправдан: `aether-plugin` не зависит от
`aether-region`, сервер и плагин зависят от одного крейта, цикла нет.
`aether-region` действительно без зависимостей. Один пространственный
store (`RegionStore`) — требования брифа соблюдены; `StoredState` плагина —
это persistence-модель, а не второй индекс.

## Findings

1. **Wand point 2 не работает на 1.8.9 (protocol 47) с дефолтным
   `minecraft:golden_axe`.**
   `crates/aether-server/src/protocol/v47/mod.rs:253-255` отбрасывает пакет
   `0x08` (`return ClientEvent::Ignored`) для любого held item, который не
   маппится в блок. `before_block_interact` вызывается только из ветки
   `ClientEvent::Place` (`crates/aether-server/src/session.rs:873-880`), а для
   инструмента (golden axe, id 286) `Place` вообще не порождается. Итог: на
   основном 1.8.9-протоколе второй клик палочки не ставит точку, `/rg define`
   и `/rg redefine` недоступны. Фикс: для `0x08` с non-block item эмитить
   отдельное событие взаимодействия (и роутить его в
   `before_block_interact`), либо не привязывать выбор к `Place`. Нужен
   codec→hook интеграционный тест; сейчас `plugins/regions/src/tests.rs`
   вызывает метод плагина напрямую.

2. **Point 1 привязан к `before-block-break`, который срабатывает только после
   реального разрушения блока и только после проверки региона.**
   В survival `StartDig` лишь запускает добычу
   (`crates/aether-server/src/session.rs:845-854`,
   `crates/aether-server/src/game/interact.rs:54-82`), а
   `before_block_break` вызывается уже из `break_block`
   (`crates/aether-server/src/session.rs:1278-1279`) — после
   `region::denial_at`. Следствия: левый клик не ставит точку сразу; неразрушимые
   блоки выбрать нельзя; в защищённой зоне, где нет build-прав, point 1 не
   ставится. Это же противоречит README: «Both clicks are cancelled … it never
   breaks» (`plugins/regions/README.md:55-56`) — в survival блок успевает
   сломаться и затем восстанавливается; реализация в
   `plugins/regions/src/lib.rs:759-779`. Нужен start-interact/start-dig хук или
   кнопка в `before-block-interact`.

3. **Права `worldguard.region.*` для `/rg` зарегистрированы неверно.**
   В манифесте `/rg` и `/region` объявлены с `node = worldguard.region.info`,
   `op = 0` (`plugins/regions/src/lib.rs:107-117`). `register_plugin` объявляет
   этот node как `Allow` (`crates/aether-server/src/commands/registry.rs:71-78`,
   `crates/aether-server/src/permissions.rs:1126-1135`). Поэтому `/rg info`
   доступен всем, а не гейтится нодой, и явный deny на
   `worldguard.region.info` блокирует весь `/rg` (включая `define`/`redefine`)
   ещё в `crates/aether-server/src/commands/mod.rs:251-255`, до
   per-subcommand проверок в `plugins/regions/src/lib.rs:564-720`. Фикс:
   регистрировать верхнеуровневую команду с нейтральной/публичной нодой
   (`CommandDecl::Plain`), а `worldguard.region.*` проверять только в
   подкомандах; либо объявлять info с op > 0.

4. **Нет сброса/отмены выбора wand; выбор не очищается после успешных
   define/redefine/remove и не чистится при выходе игрока.**
   `selections` — `HashMap<u64, Selection>` без удаления
   (`plugins/regions/src/lib.rs:257,451-462`); успешный `define` не сбрасывает
   точки (`plugins/regions/src/lib.rs:564-596`). `after-player-join` заявлен в
   подписках (`plugins/regions/src/lib.rs:104`), но обработчика нет. Entity id
   переиспользуются, поэтому новый игрок может унаследовать чужой выбор.
   Пункт чек-листа «wand (2 клика) сброс/отмена» не выполнен. Нужны явная
   отмена (`/rg wand cancel`/повторный клик/сброс после define) и очистка
   выбора при leave/rejoin.

5. **Ошибка записи KV не возвращается пользователю; возможна рассинхронизация
   памяти и персистентности.**
   `save` только логирует ошибку (`plugins/regions/src/lib.rs:298-303`), а
   `define`/`redefine`/`remove`/`priority` публикуют изменение в core и
   отвечают успехом до/независимо от записи
   (`plugins/regions/src/lib.rs:368-369,400-402,419-421,446-448`). При сбое
   fjall регион живёт в памяти до рестарта, после рестарта изменение
   теряется. Хуже: `kv_get` глотает ошибку чтения
   (`crates/aether-server/src/plugins.rs:112-119`), а `load` в этом случае
   делает `rebuild()` из пустого состояния
   (`plugins/regions/src/lib.rs:281-295`) — то есть при ошибке чтения защита
   молча исчезает (fail-open). Для protection-плагина стоит вернуть ошибку
   команде и не затирать store при ошибке чтения.

6. **Тест «state survives a restart» не проверяет пересборку нового store.**
   `Fixture::restart` создаёт новый `RegionStore`, но не обновляет
   `self.store` (`plugins/regions/src/tests.rs:140-150`); затем проверка
   `count_at(&f.store, …) == 1` (`plugins/regions/src/tests.rs:428-434`)
   смотрит на старый store, где регион остался ещё до рестарта. Тест пройдёт
   даже если `load` перестанет вызывать `rebuild`. Нужно заменить `f.store` на
   новый store и проверять именно его индекс.

7. **`redefine` всегда переносит регион в `OVERWORLD`.**
   Команда передаёт `OVERWORLD` (`plugins/regions/src/lib.rs:617`), а
   `redefine` перезаписывает `world` (`plugins/regions/src/lib.rs:390-395`).
   Загруженный из KV регион другого мира (а `list`/persistence уже per-world)
   при `/rg redefine` молча переедет в overworld. `define` тоже всегда
   overworld (`plugins/regions/src/lib.rs:589`). Для текущего one-world среза
   это низкий риск, но для per-world CMD-34b — баг. `redefine` должен сохранять
   `previous.world` (или брать мир игрока).

## Nits

- `crates/aether-region/src/lib.rs:52-59` — устаревший раздел «Module home»:
  индекс уже не «lives in the core», а в этом крейте; единственный consumer
  уже не только core.
- `plugins/regions/src/lib.rs:24-25` и `plugins/regions/README.md:55` —
  утверждение «never breaks» неверно для survival (см. finding 2).
- `plugins/regions/src/lib.rs:98` и README запрашивают `world.read`, но крейт
  его нигде не использует (`player_position` идёт через `players`).
- `deny_on_fault` для protection-плагина оставлен `false`
  (`plugins/regions/src/lib.rs:120`); для самого плагина это пока некритично
  (защита в core), но стоит осознанно зафиксировать.

## Итог

Перенос модели в `aether-region` чистый, CMD-29a/29b не сломаны, гейты
зелёные. Но заявленный wand не работает на 1.8.9 с дефолтным предметом, point 1
имеет неверную семантику, а регистрация прав `/rg` и persist-ошибки требуют
исправления. Поэтому **CHANGES**.
