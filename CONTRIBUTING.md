# Contributing to Aether Engine / Как внести вклад в Aether Engine

> Bilingual guide. English first, Russian below. · Двуязычный гайд: сначала English, ниже — Русский.

---

## 🇬🇧 English

Thanks for your interest in Aether! The project is in **design / pre-alpha**, so the most
valuable contributions right now are foundational: the Phase 0 scaffolding, benchmarks, and
correctness tests described in the [Roadmap](docs/ROADMAP.md).

### Before you start
1. Read the [README](README.md), the [Roadmap](docs/ROADMAP.md), and the [Status](docs/STATUS.md).
2. Check [Known Issues](docs/KNOWN_ISSUES.md) — some "bugs" are **accepted Phase 1 deviations**, not defects.
3. Open (or comment on) a GitHub issue before large changes, so effort isn't duplicated.

### Development principles (non-negotiable)
These come straight from the spec and are what makes Aether *Aether*:
- **Data-Oriented Design.** Prefer Structure-of-Arrays over OOP object graphs.
- **Cache-locality first.** Keep data sequential; avoid pointer chasing.
- **SIMD-first.** Any hot math path needs a scalar fallback **and** vectorized SSE4.2/AVX2/AVX-512 branches selected via runtime dispatch.
- **No global locks.** Heavy work goes to the work-stealing pool and merges at deterministic Safe Points.
- **Determinism.** Parallel results must merge identically every run.

### Workflow
1. Fork and branch from the default branch: `git checkout -b feature/short-description`.
2. Keep changes focused; one logical change per pull request.
3. Run the local checks before pushing (see below).
4. Open a **draft** pull request early; describe *what* and *why*, link the issue.
5. Update the relevant docs (`docs/STATUS.md`, `docs/ROADMAP.md`) when a feature lands or a plan changes.

### Local checks (once the workspace exists)
```bash
cargo fmt --all -- --check     # formatting
cargo clippy --all-targets     # lints
cargo test --all               # correctness
cargo bench                    # performance (no regressions vs. QA gates)
```
Performance-sensitive PRs should include a Tracy capture or the relevant benchmark result. The QA gates (redstone/entity/explosion/migration) live in the [README](README.md#benchmark-targets-qa-gates).

### Commit & PR style
- Clear, imperative commit subjects (e.g. `physics: add SIMD broad-phase mask check`).
- Reference issues (`Closes #123`).
- CI must be green; **do not** disable a failing check to merge.

### CI note for maintainers
When GitHub Actions are added: after a build passes, there is **no need to re-poll the pull
request on every run** — a green build is sufficient signal. (See `CLAUDE.md`.)

---

## 🇷🇺 Русский

Спасибо за интерес к Aether! Проект на стадии **проектирования / pre-alpha**, поэтому самые
ценные вклады сейчас — фундаментальные: каркас Фазы 0, бенчмарки и тесты корректности,
описанные в [Роадмапе](docs/ROADMAP.md).

### Перед началом
1. Прочитайте [README](README.ru.md), [Роадмап](docs/ROADMAP.md) и [Статус](docs/STATUS.md).
2. Загляните в [Известные проблемы](docs/KNOWN_ISSUES.md) — часть «багов» это **допустимые отклонения Фазы 1**, а не дефекты.
3. Перед крупными изменениями заведите (или прокомментируйте) issue на GitHub, чтобы не дублировать усилия.

### Принципы разработки (не обсуждаются)
Они идут прямо из спецификации и делают Aether тем, что он есть:
- **Data-Oriented Design.** Предпочитайте Structure-of-Arrays графам OOP-объектов.
- **Cache-locality first.** Держите данные последовательно; избегайте pointer chasing.
- **SIMD-first.** Любой горячий путь вычислений нуждается в scalar-фолбэке **и** векторизованных ветках SSE4.2/AVX2/AVX-512 с выбором через runtime-диспетчеризацию.
- **Никаких глобальных блокировок.** Тяжёлая работа уходит в work-stealing пул и сливается в детерминированных Safe Points.
- **Детерминизм.** Параллельные результаты должны сливаться одинаково при каждом запуске.

### Рабочий процесс
1. Форкните и создайте ветку от дефолтной: `git checkout -b feature/short-description`.
2. Держите изменения сфокусированными; одно логическое изменение на pull request.
3. Прогоните локальные проверки перед push (см. ниже).
4. Открывайте **draft** pull request рано; опишите *что* и *почему*, привяжите issue.
5. Обновляйте соответствующие документы (`docs/STATUS.md`, `docs/ROADMAP.md`), когда функция готова или план меняется.

### Локальные проверки (когда появится воркспейс)
```bash
cargo fmt --all -- --check     # форматирование
cargo clippy --all-targets     # линты
cargo test --all               # корректность
cargo bench                    # производительность (без регрессий против QA-гейтов)
```
PR, влияющие на производительность, должны включать захват Tracy или соответствующий результат бенчмарка. QA-гейты (редстоун/сущности/взрыв/миграция) — в [README](README.md#benchmark-targets-qa-gates).

### Стиль коммитов и PR
- Ясные императивные заголовки коммитов (например, `physics: add SIMD broad-phase mask check`).
- Ссылайтесь на issue (`Closes #123`).
- CI должен быть зелёным; **не** отключайте падающую проверку ради merge.

### Замечание по CI для мейнтейнеров
Когда добавляются GitHub Actions: после успешной сборки **не нужно повторно опрашивать
pull request на каждом запуске** — зелёная сборка это достаточный сигнал. (См. `CLAUDE.md`.)
