# Aether Engine — Project Status / Статус проекта

> Single source of truth for **what exists**, **what is planned**, and **what is cancelled**.
> Единый источник правды: **что есть**, **что в планах**, **что отменено**.
>
> Legend / Легенда: ✅ done · 🚧 in progress · 📋 planned · ❄️ deferred · ❌ cancelled.
>
> Last reviewed / Последняя ревизия: 2026-07-27 · Spec / Спецификация: v1.1

---

## 🇬🇧 English

### ✅ What exists today
The project is in the **design / pre-alpha** phase. Concretely present in the repository:

| Item | State |
|------|-------|
| Technical specification (v1.0 & v1.1) | ✅ Written (source PDFs) |
| README (English + Russian) | ✅ |
| Roadmap, Status, Known Issues, Contributing docs | ✅ |
| License | ✅ |
| `.gitignore` for a Cargo project | ✅ |
| **Engine source code (any crate)** | ❌ Not yet started |

### 📋 What is planned
Grouped by roadmap phase (see [ROADMAP.md](ROADMAP.md) for the full sequence).

**Phase 0 — Foundations**
- 📋 Cargo workspace + crate skeletons
- 📋 Runtime SIMD dispatch trait & backends
- 📋 CI (build, test, clippy, fmt) + Criterion benches
- 📋 Telemetry hooks (Tracy + Prometheus)

**Phase 1 — Core Engine MVP (70–75% Vanilla)**
- 📋 SoA memory model: Sub-Chunk, AVX-Cell, Morton order, masks
- 📋 Palette compression + Block-Entity arena
- 📋 Physics pipeline (SIMD broad-phase, cached environment)
- 📋 Graph-based redstone (DDG)
- 📋 Async lighting (cell flood-fill)
- 📋 ECS entities + Flow-Field AI
- 📋 KV storage (RocksDB / Fjall + Zstd)
- 📋 Per-world process isolation + work-stealing scheduler

**Phase 2 — Hardcore Mechanical Target (≥ 95% Vanilla)**
- 📋 Cross-chunk Quasi-Connectivity redstone
- 📋 Deterministic fluids, strict update order, per-tick spawner

**Phase 3 — Network & multi-world**
- 📋 Network Gateway, Internal Binary Protocol, seamless hand-off, Vanilla protocol

**Phase 4 — Extensibility & ops**
- 📋 Native C-ABI + WASM plugin API
- 📋 Aether-Convert migration tool
- 📋 In-game profiler, ops hardening

### ❌ What is cancelled / superseded
Decisions explicitly dropped or replaced during design. Kept here so the history is not lost.

| Cancelled / changed | Reason | Replaced by |
|---------------------|--------|-------------|
| **"100% Vanilla-accurate from day one"** (spec v1.0: *"priority is given to perfect Vanilla mechanics"*) | Blocked MVP on exotic edge-cases | **Progressive Enhancement** (v1.1): 70–75% in Phase 1, 95%+ in Phase 2 |
| **Per-tick precise entity spawner in Phase 1** | Too costly for the MVP tick budget | Batched async spawner in Phase 1; per-tick spawner deferred to Phase 2 |
| **Exact QC (Quasi-Connectivity) across chunk borders in Phase 1** | Requires the full cross-chunk dependency graph | Ignored in Phase 1 (registered deviation); implemented in Phase 2 |
| **OOP entity model** (`struct Player { ... }`) | Poor cache locality, blocks SIMD | Structure-of-Arrays (SoA) storage |
| **Fork of an existing Java server** (Paper/Purpur/Folia/Fabric) | Locks in Java internals; defeats the DOD/SIMD goal | Clean-room Rust engine |
| ❄️ **Non-x86 (ARM/NEON, RISC-V) targets** | Not cancelled — **deferred** until x86-64 AVX2 baseline is stable | Revisit post-Phase 1 |

---

## 🇷🇺 Русский

### ✅ Что есть сейчас
Проект на стадии **проектирования / pre-alpha**. Конкретно присутствует в репозитории:

| Пункт | Состояние |
|-------|-----------|
| Техническое задание (v1.0 и v1.1) | ✅ Написано (исходные PDF) |
| README (English + Русский) | ✅ |
| Документы Роадмап, Статус, Известные проблемы, Контрибьютинг | ✅ |
| Лицензия | ✅ |
| `.gitignore` для Cargo-проекта | ✅ |
| **Исходный код движка (любой крейт)** | ❌ Ещё не начат |

### 📋 Что в планах
Сгруппировано по фазам роадмапа (полная последовательность — в [ROADMAP.md](ROADMAP.md)).

**Фаза 0 — Фундамент**
- 📋 Cargo-воркспейс + каркасы крейтов
- 📋 Трейт runtime-диспетчеризации SIMD и бэкенды
- 📋 CI (сборка, тесты, clippy, fmt) + бенчи Criterion
- 📋 Хуки телеметрии (Tracy + Prometheus)

**Фаза 1 — Core Engine MVP (70–75% Vanilla)**
- 📋 SoA-модель памяти: Sub-Chunk, AVX-Cell, Morton order, маски
- 📋 Сжатие палитрой + арена Block-Entity
- 📋 Конвейер физики (SIMD broad-phase, кэш окружения)
- 📋 Граф-редстоун (DDG)
- 📋 Асинхронное освещение (flood-fill по ячейкам)
- 📋 ECS-сущности + Flow-Field AI
- 📋 KV-хранилище (RocksDB / Fjall + Zstd)
- 📋 Изоляция процессов по мирам + work-stealing планировщик

**Фаза 2 — Hardcore Mechanical Target (≥ 95% Vanilla)**
- 📋 Меж-чанковая Quasi-Connectivity в редстоуне
- 📋 Детерминированные жидкости, строгий порядок обновлений, потиковый спавнер

**Фаза 3 — Сеть и мультимир**
- 📋 Network Gateway, Internal Binary Protocol, бесшовный переход, протокол Vanilla

**Фаза 4 — Расширяемость и эксплуатация**
- 📋 Plugin API: Native C-ABI + WASM
- 📋 Инструмент миграции Aether-Convert
- 📋 Внутриигровой профайлер, закалка эксплуатации

### ❌ Что отменено / заменено
Решения, явно отброшенные или заменённые в ходе проектирования. Сохранены здесь, чтобы не потерять историю.

| Отменено / изменено | Причина | Заменено на |
|---------------------|---------|-------------|
| **«100% точность Vanilla с первого дня»** (спец. v1.0: *«приоритет отдаётся идеальной точности механики Vanilla»*) | Блокировало MVP экзотическими граничными случаями | **Progressive Enhancement** (v1.1): 70–75% в Фазе 1, 95%+ в Фазе 2 |
| **Потиковый точный спавнер сущностей в Фазе 1** | Слишком дорого для бюджета тика MVP | Пакетный асинхронный спавнер в Фазе 1; потиковый — отложен в Фазу 2 |
| **Точная QC (Quasi-Connectivity) на границах чанков в Фазе 1** | Требует полного меж-чанкового графа зависимостей | Игнорируется в Фазе 1 (зарегистрированное отклонение); реализуется в Фазе 2 |
| **OOP-модель сущностей** (`struct Player { ... }`) | Плохая кеш-локальность, мешает SIMD | Хранение по Structure-of-Arrays (SoA) |
| **Форк существующего Java-сервера** (Paper/Purpur/Folia/Fabric) | Привязывает к внутренностям Java; убивает цель DOD/SIMD | Clean-room движок на Rust |
| ❄️ **Не-x86 таргеты (ARM/NEON, RISC-V)** | Не отменено — **отложено** до стабилизации базы x86-64 AVX2 | Вернуться после Фазы 1 |
