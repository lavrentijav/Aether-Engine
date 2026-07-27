# Aether Engine — Known Issues & Deviations / Известные проблемы и отклонения

> Current problems, engineering risks, and the **Known Deviations Registry** (Phase 1
> compatibility gaps that are accepted on purpose).
> Текущие проблемы, инженерные риски и **реестр допустимых отклонений** (осознанно
> принятые пробелы совместимости на Фазе 1).
>
> Last reviewed / Последняя ревизия: 2026-07-27

---

## 🇬🇧 English

### A. Current blocking problems (design/pre-alpha)
1. **No implementation yet.** Only the specification exists; every subsystem below is unbuilt. The primary "problem" right now is bootstrapping Phase 0.
2. **No CI / build gate.** Nothing enforces build, tests, or formatting yet (planned in Phase 0).
3. **SIMD backends unverified.** The runtime-dispatch design is on paper; correctness parity between `Scalar / SSE / AVX2 / AVX-512` paths is untested.
4. **Determinism unproven.** Parallel subsystems must merge at Safe Points; the deterministic merge points are specified but not validated.

### B. Known Deviations Registry (Phase 1 — accepted on purpose)
These are **not bugs** in Phase 1 — they are documented, temporary compatibility gaps that must be recorded in the engine config and closed in Phase 2.

| Subsystem | Phase 1 deviation | Phase 2 target |
|-----------|-------------------|----------------|
| **Redstone** | Quasi-Connectivity (QC) ignored across inactive chunk borders | Full dependency graph with cross-chunk QC |
| **Fluids** | Parallel simplified spread; Java tick timing not preserved | Deterministic fluid layers 1:1 with Vanilla |
| **Update order** | Simultaneous redstone updates ordered by the parallel graph | Strict deterministic directional-priority queue |
| **Entity spawn** | Batched async spawn every N ticks | Per-tick precise spawner |

### C. Engineering risks & trade-offs
- **RAM overhead: +15–25%.** SoA masks, Morton order, and redstone graphs cost more memory than classic structures. Accepted for cache-locality and SIMD wins.
- **x86-64 first.** The first version targets x86-64 with AVX2; other ISAs are deferred. Users on ARM/RISC-V are unsupported until later.
- **Determinism vs. parallelism.** Aggressive parallelism increases the risk of non-deterministic results if Safe-Point merges are wrong — needs strong test coverage.
- **Compatibility ceiling in Phase 1.** 70–75% Vanilla compliance means some contraptions (QC-dependent redstone, tick-perfect fluids) will behave differently until Phase 2.
- **External dependency choices.** RocksDB vs. Fjall, Elixir/OTP vs. C++/Rust for the Gateway — not finalized; a wrong pick is expensive to reverse.

### D. How to report a problem
Open a GitHub issue with: the subsystem, whether it's a **bug** or an **accepted deviation** (check the table above first), reproduction steps, and — for performance issues — a Tracy capture or the relevant benchmark scenario. See [CONTRIBUTING.md](../CONTRIBUTING.md).

---

## 🇷🇺 Русский

### A. Текущие блокирующие проблемы (design/pre-alpha)
1. **Реализации ещё нет.** Есть только спецификация; все подсистемы ниже не построены. Главная «проблема» сейчас — запуск Фазы 0.
2. **Нет CI / гейта сборки.** Пока ничто не проверяет сборку, тесты и форматирование (запланировано в Фазе 0).
3. **SIMD-бэкенды не проверены.** Дизайн runtime-диспетчеризации — на бумаге; паритет корректности между путями `Scalar / SSE / AVX2 / AVX-512` не протестирован.
4. **Детерминизм не доказан.** Параллельные подсистемы должны сливаться в Safe Points; точки детерминированного слияния описаны, но не валидированы.

### B. Реестр допустимых отклонений (Фаза 1 — приняты осознанно)
На Фазе 1 это **не баги** — это задокументированные временные пробелы совместимости, которые обязаны быть зафиксированы в конфиге движка и закрыты в Фазе 2.

| Подсистема | Отклонение на Фазе 1 | Цель Фазы 2 |
|------------|----------------------|-------------|
| **Редстоун** | Quasi-Connectivity (QC) игнорируется на границах неактивных чанков | Полный граф зависимостей с меж-чанковой QC |
| **Жидкости** | Параллельное упрощённое распространение; тайминг тика Java не сохраняется | Детерминированные слои жидкостей 1:1 с Vanilla |
| **Порядок обновлений** | Одновременные обновления редстоуна упорядочены параллельным графом | Строгая детерминированная очередь по направленному приоритету |
| **Спавн сущностей** | Пакетный асинхронный спавн раз в N тиков | Потиковый точный спавнер |

### C. Инженерные риски и компромиссы
- **Оверхед RAM: +15–25%.** SoA-маски, Morton order и графы редстоуна требуют больше памяти, чем классические структуры. Принято ради кеш-локальности и выигрыша от SIMD.
- **Сначала x86-64.** Первая версия — под x86-64 с AVX2; другие ISA отложены. Пользователи ARM/RISC-V не поддерживаются до более позднего этапа.
- **Детерминизм против параллелизма.** Агрессивный параллелизм повышает риск недетерминированных результатов при неправильных слияниях в Safe Points — нужно сильное тестовое покрытие.
- **Потолок совместимости на Фазе 1.** 70–75% совместимости с Vanilla означает, что часть механизмов (QC-зависимый редстоун, тик-точные жидкости) будет вести себя иначе до Фазы 2.
- **Выбор внешних зависимостей.** RocksDB против Fjall, Elixir/OTP против C++/Rust для Gateway — не финализировано; неправильный выбор дорого откатывать.

### D. Как сообщить о проблеме
Заведите issue на GitHub с указанием: подсистемы, тип — **баг** или **допустимое отклонение** (сначала сверьтесь с таблицей выше), шаги воспроизведения и — для проблем производительности — захват Tracy или соответствующий бенчмарк-сценарий. См. [CONTRIBUTING.md](../CONTRIBUTING.md).
