# Rевью: `oc/WEB1` (с влитым `oc/WEB2`) против `main`

Проверил: worktree `/srv/dev/projects/Aether-Engine-wt/crit-WEB` (detached `0f656e4`). Тесты гонял `cargo-locked` (target `/srv/dev/build/aether-target`, `CARGO_INCREMENTAL=0`). Ничего не коммитил.

**Прогоны**
- `cargo-locked test -p aether-webadmin-plugin -p aether-ops` → 40 + 4 ok
- `cargo-locked test -p aether-server --no-default-features web::` → 4 ok
- `cargo fmt --all -- --check` → чисто
- `cargo clippy -p aether-webadmin-plugin -p aether-ops --all-targets -D warnings` → чисто. Полный clippy `aether-server` не успел (таймаут 10 мин на wasmtime; тесты края компилируются/проходят).

## Что сделано хорошо
- **Аутентификация покрывает все маршруты.** Гейт в `http.rs:289-357`: после `GET/POST /login` при невалидном токене возвращается логин/401 до матча роутов. Значит `/api/*`, `/tile`, `/tile_at`, `/map`, `/info`, `/head`, `/overlay`, `/surface`, `/history`, `/changes`, `/player`, `/regions`, `/players`, `/profile`, `/inventory` — все за токеном.
- **Constant-time сравнение** — `auth.rs:128-137` (length + XOR-accumulate), без early-exit.
- **Cookie** — `HttpOnly; SameSite=Strict; Path=/; Max-Age` (`http.rs:373-380`); `Cache-Control: no-store` на всех ответах (`http.rs:762`).
- **Path traversal нет** — файлов со статикой нет, все роуты хардкод.
- **Body ограничен** `MAX_BODY=64 KiB` (`http.rs:37,680`); read/write-таймауты 5с (`http.rs:644-645`).
- **bind по умолчанию `127.0.0.1:8090`** (`lib.rs:DEFAULT_BIND`); в примере конфига тоже loopback.
- **Секретов в git нет**; one-time токен из `/dev/urandom` (`auth.rs:76`).
- **XSS в серверных страницах закрыт** — `pages::esc` применяется к никам/IP/предметам/game_mode (`pages.rs:6-17,270-300`).
- **Один плагин, один роутер** — единственная регистрация `aether_webadmin::registration()` в `crates/aether-server/src/plugins.rs:347-361`; дублей нет.
- **Рендер/replay не в tick-потоке** — HTTP-сервер поднимает свои потоки (`http.rs:617-631`).

## Находки

### Блокеры (безопасность/DoS — прямо требуемое брифом)
1. **Нет лимита числа соединений; поток на каждый accept, бесконечно.** `http.rs:609-631`. Один клиент открывает тысячи сокетов → разрастание потоков/памяти. Бриф требует «лимиты … числа соединений (DoS одним клиентом)».
2. **Нет лимита размера/числа заголовков.** `http.rs:648` (request line) и `http.rs:657-676` (цикл `read_line` в неограниченную `String`). Клиент может слать неограниченный поток заголовков (в пределах 5с-per-read), вызывая рост памяти. Ограничен только body.
3. **Slowloris не закрыт полностью:** только per-syscall timeout 5с, общего дедлайна на запрос нет, при этом соединения не лимитированы → `http.rs:644-645` + п.1. Клиент, шлющий байт каждые ~4с, держит соединения вечно.
4. **Unbounded area radius в `/changes`.** `http.rs:519` (`r` из query без клампа) → `timemachine.rs:146-153`: вложенный цикл по ~`(2r/16)^2` чанкам, на каждый — полный `column_events`, а `out` копится без ограничения (лимит режет только в конце, `timemachine.rs:176-178`). `r=2^31` → практически вечный цикл/OOM от одного запроса.

### Средние
5. **`/tile_at` — O(пикселей × column_events).** `timemachine.rs:238-263` вызывает `surface_at` на каждый пиксель → `column_timeline` (`:266`) → `column_events` на каждый пиксель; тайл 128×128 = 16 384 сканирований чанка, кэша нет. CPU-DoS на «таймслайдере», рейт-лимита нет.
6. **Overlay не подключён.** `with_overlay`/`NameResolver` нигде не вызываются (`git grep` пуст), сервер зовёт только `with_world` (`plugins.rs:359`), поэтому `/overlay` всегда пуст и слой игроков/сущностей на карте (требование WEB2) не работает; в `KNOWN_ISSUES` это не отражено. См. `http.rs:261-268`.
7. **Рендер живого тайла бьёт по глобальному локy.** `map.rs:239-263` сканирует всю вертикаль на пиксель через `World::get_block`, а `ensure_column` берёт `columns.write()` на каждый вызов (`crates/aether-api/src/lib.rs:299`) — конкуренция/джиттер с tick-потоком на больших тайлах.
8. **Сэмплер работает на tick-потоке.** `game/tick.rs:47-49` вызывает `sample()`, который собирает `pending_positions()` (копия всего планировщика block/fluid ticks), `positions()`, `loaded_positions()` — несколько игровых локов и аллокаций раз в секунду (`web.rs`). Может давать пропуски тика при большом числе отложенных тиков.
9. **XSS-гигиена на карте.** `APP_JS` вставляет ответы в `innerHTML` без клиентского экранирования (`http.rs:1016,1027`: `surf.name`, `r.from_name/to_name`, `r.actor`). Сейчас `actor_label` — всегда UUID-hex (resolver не подключён), а имена блоков берутся из глобального реестра, поэтому не эксплуатируется; но требование «ники/названия экранированы в HTML» для этого пути не выполнено, и при подключении `NameResolver` станет stored-XSS.

### Мелкие
10. Док/README говорит «non-GET … 405», но реализация отдаёт 404 (`http.rs:355`). Рассинхрон.
11. `TileKey::origin` (`map.rs:57`) переполняет i32 при `tx/tz≈i32::MAX` (паникует поток соединения в debug, в release wrap). Вход не валидируется.
12. `?token=` не работает для автообновляемых страниц: JS-фетчи идут без токена и без cookie (`http.rs:729+`), т.е. только для curl — как и в доках, но стоит явно оговорить.
13. `source::block_name_of` (глобальный реестр) vs `MapSource::block_name` (реестр мира) могут расходиться для модовых миров — только отображение.

## Вердикт

**CHANGES.** Критичные функции аутентификации/секретов/экранирования сделаны корректно, но перечисленные пункты 1–4 — прямые пробелы против «главное — безопасность» брифа (лимиты соединений/заголовков, slowloris, ограничение дорогих query), п.5 — CPU-DoS без рейт-лимита, п.6 — нерабочая заявленная функция overlay. После их устранения (или явного сужения периметра с rate-limit/semaphore и клампа `r`) — approve.
