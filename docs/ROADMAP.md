# Aether Engine — Roadmap / Роадмап

> Bilingual document. English first, Russian below. · Двуязычный документ: сначала English, ниже — Русский.
>
> This roadmap reflects spec **v1.1 (Production Candidate)** and its **Progressive Enhancement** strategy.
> Status legend: ✅ done · 🚧 in progress · 📋 planned · ❄️ deferred · ❌ cancelled.

---

## 🇬🇧 English

Aether follows a **phased delivery** model. Phases are sequenced by risk: the memory and
concurrency foundations must be proven before gameplay mechanics are layered on top.

### Phase 0 — Foundations & tooling
*Goal: a repository anyone can build, test and profile.*

- ✅ Cargo workspace skeleton (`aether-core`, `aether-world`, `aether-net`, `aether-convert`, `aether-telemetry`).
- ✅ Runtime SIMD dispatch scaffold (`Scalar / SSE4.2 / AVX2` real; `AVX-512` detected → AVX2 path until validated).
- ✅ CI: build + `cargo test` + `clippy` + `rustfmt` on x86-64.
- 🚧 Benchmark harness (Criterion) in place (mask ops, storage round-trip); the four QA stress fixtures still to add.
- 🚧 Prometheus telemetry exporter wired; Tracy client stubbed behind a feature.

### Phase 1 — Core Engine MVP  *(Target: 70–75% Vanilla compliance)*
*Goal: a world that ticks stably at 20 TPS with the hard performance work done.*

- ✅ **Memory model**: `Sub-Chunk`, `AVX-Cell` (64-byte cache line), Morton (Z-order) indexing, SoA masks.
- 🚧 **Palette compression** (u4 / u8 → u16 auto-expand) ✅; Block-Entity arena still 📋.
- 🚧 **Physics engine**: basic voxel AABB collision + movement/gravity shipped (`aether-physics`); staged pipeline, SIMD broad-phase and the `Cached Environment` O(1) fast path still 📋.
- 📋 **Redstone**: compiled Directed Dependency Graph (basic components; **QC deviations allowed** — see [Known Issues](KNOWN_ISSUES.md)).
- 📋 **Lighting**: async cell-based flood-fill with safe-point merges.
- 📋 **Entities/AI**: ECS storage, Flow-Field navigation, cached A\*, batched spawner.
- ✅ **Storage**: Fjall KV backend, Zstandard sub-chunk blobs, order-preserving keys (+ in-memory backend for tests).
- 📋 **World Engine** process model (one OS process per world).
- 📋 Work-stealing scheduler for generation / save / lighting off the main tick.

### Phase 2 — Hardcore Mechanical Target  *(Target: ≥ 95% Vanilla compliance)*
*Goal: close the deviation registry.*

- 📋 Full redstone dependency graph **with cross-chunk Quasi-Connectivity (QC)**.
- 📋 Deterministic fluid layering (1:1 with Vanilla tick timing).
- 📋 Strict directional-priority update order (replaces the parallel-graph ordering of Phase 1).
- 📋 Per-tick precise entity spawner (replaces batched spawner).
- 📋 Edge-case QA: Update Suppression micro-timings, historical engine quirks.

### Phase 3 — Network & multi-world
*Goal: real players, seamless transfers.*

- 📋 **Network Gateway** (Elixir/OTP or C++/Rust `epoll`/`io_uring`): TLS handshake, DDoS/rate-limit, packet sanitation.
- 📋 Internal Binary Protocol between Gateway and World Engines.
- 📋 Stateless seamless world hand-off (TCP stays open, route switches).
- 📋 Vanilla client protocol compatibility layer.

### Phase 4 — Extensibility, migration & ops
*Goal: production readiness.*

- 📋 **Plugin API**: Native C-ABI tier + WebAssembly (Wasmtime) sandbox + `engine.supports()` capability checks.
- 🚧 **Aether-Convert**: parallel Anvil `.mca` → KV migration with audit trail & checksums. **Pulled forward** — an initial version shipped alongside Phase 1 storage (both as the `aether-convert` crate and the standalone [Aether-Convert](https://github.com/lavrentijav/Aether-Convert) repo); Phase 4 hardens it (full block-state mapping, gzip/LZ4 chunks, resumable runs).
- 📋 In-game `/aether profile` visual sub-tick profiler.
- 📋 Ops hardening: graceful restart, backups, Grafana dashboards.

### Cross-cutting / continuous
- 📋 Benchmark gates enforced in CI (regressions block merge).
- 📋 Documentation kept in sync with each shipped subsystem.
- ❄️ Non-x86 targets (ARM/NEON, RISC-V) — deferred until after x86-64 AVX2 baseline is stable.

---

## 🇷🇺 Русский

Aether разрабатывается по модели **поэтапной поставки**. Порядок фаз задан риском:
фундамент памяти и параллелизма должен быть доказан до того, как поверх него
наслаиваются игровые механики.

### Фаза 0 — Фундамент и инструментарий
*Цель: репозиторий, который каждый может собрать, протестировать и профилировать.*

- ✅ Каркас Cargo-воркспейса (`aether-core`, `aether-world`, `aether-net`, `aether-convert`, `aether-telemetry`).
- ✅ Каркас runtime-диспетчеризации SIMD (`Scalar / SSE4.2 / AVX2` реально; `AVX-512` детектится → путь AVX2 до валидации).
- ✅ CI: сборка + `cargo test` + `clippy` + `rustfmt` на x86-64.
- 🚧 Бенчмарк-харнесс (Criterion) готов (маски, round-trip хранилища); четыре QA-фикстуры ещё предстоит добавить.
- 🚧 Экспортер телеметрии Prometheus вшит; клиент Tracy — заглушка за фичей.

### Фаза 1 — Core Engine MVP  *(Цель: 70–75% совместимости с Vanilla)*
*Цель: мир, который стабильно тикает на 20 TPS, с выполненной тяжёлой работой по производительности.*

- ✅ **Модель памяти**: `Sub-Chunk`, `AVX-Cell` (64-байтная строка кеша), индексация Morton, SoA-маски.
- 🚧 **Сжатие палитрой** (u4 / u8 → авто-разворот в u16) ✅; арена Block-Entity ещё 📋.
- 🚧 **Физический движок**: базовые воксельные AABB-коллизии + передвижение/гравитация реализованы (`aether-physics`); поэтапный конвейер, SIMD broad-phase и быстрый путь `Cached Environment` ещё 📋.
- 📋 **Редстоун**: компилируемый Directed Dependency Graph (базовые компоненты; **отклонения QC допустимы** — см. [Известные проблемы](KNOWN_ISSUES.md)).
- 📋 **Освещение**: асинхронный flood-fill по ячейкам со слиянием в safe-points.
- 📋 **Сущности/AI**: ECS-хранилище, Flow-Field навигация, кэшируемый A\*, пакетный спавнер.
- ✅ **Хранилище**: KV-бэкенд Fjall, блобы Sub-Chunk со сжатием Zstandard, порядко-сохраняющие ключи (+ in-memory бэкенд для тестов).
- 📋 Процессная модель **World Engine** (один процесс ОС на мир).
- 📋 Work-stealing планировщик для генерации / сохранения / света вне главного тика.

### Фаза 2 — Hardcore Mechanical Target  *(Цель: ≥ 95% совместимости с Vanilla)*
*Цель: закрыть реестр отклонений.*

- 📋 Полный граф зависимостей редстоуна **с меж-чанковой Quasi-Connectivity (QC)**.
- 📋 Детерминированный расчёт слоёв жидкостей (1:1 с таймингом тика Vanilla).
- 📋 Строгий порядок обновлений по направленному приоритету (заменяет параллельный граф Фазы 1).
- 📋 Потиковый точный спавнер сущностей (заменяет пакетный).
- 📋 QA граничных случаев: микро-тайминги Update Suppression, исторические особенности движка.

### Фаза 3 — Сеть и мультимир
*Цель: реальные игроки, бесшовные переходы.*

- 📋 **Network Gateway** (Elixir/OTP или C++/Rust `epoll`/`io_uring`): TLS-handshake, защита от DDoS/rate-limit, санитизация пакетов.
- 📋 Internal Binary Protocol между Gateway и World Engine.
- 📋 Бесшовный переход между мирами без состояния (TCP не рвётся, переключается маршрут).
- 📋 Слой совместимости с клиентским протоколом Vanilla.

### Фаза 4 — Расширяемость, миграция и эксплуатация
*Цель: готовность к продакшену.*

- 📋 **Plugin API**: уровень Native C-ABI + песочница WebAssembly (Wasmtime) + проверки `engine.supports()`.
- 🚧 **Aether-Convert**: параллельная миграция Anvil `.mca` → KV с аудит-трейлом и контрольными суммами. **Вынесено вперёд** — начальная версия поставлена вместе с хранилищем Фазы 1 (крейт `aether-convert` и отдельный репозиторий [Aether-Convert](https://github.com/lavrentijav/Aether-Convert)); Фаза 4 закаляет её (полный маппинг block-state, чанки gzip/LZ4, возобновляемые прогоны).
- 📋 Внутриигровой визуальный профайлер под-тиков `/aether profile`.
- 📋 Закалка эксплуатации: graceful restart, бэкапы, дашборды Grafana.

### Сквозное / непрерывное
- 📋 Бенчмарк-гейты в CI (регрессии блокируют merge).
- 📋 Документация синхронизируется с каждой выпущенной подсистемой.
- ❄️ Не-x86 таргеты (ARM/NEON, RISC-V) — отложены до стабилизации базовой линии x86-64 AVX2.
