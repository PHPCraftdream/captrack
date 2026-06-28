# Capacity-PGO — план реализации

> **Статус:** проектный план, к исполнению по порядку.
> **Дата:** 2026-06-28.
> **Контекст:** вырос из тупика «прозрачный макрос невозможен» (см. §0).

---

## 0. Почему этот план существует

Кампания capacity-telemetry уперлась в фундаментальное ограничение Rust:
**нельзя навесить Drop-хук на чужой тип (`Vec`) без обёртки, а обёртка ломает
прозрачность исходника.** Перебрали все механизмы (orphan rule на
`impl Drop for Vec`, inherent > trait, macro = one-expr, custom allocator) —
ни один не даёт одновременно (1) прозрачный код, (2) peak-after-grow,
(3) без unsafe/alloc-hook. Это математика типов, не дефект дизайна.

**Вывод:** перестаём держать инструментацию в продакшен-коде. Продакшен —
**голый Rust** (`Vec::with_capacity(N)`). Инструментация существует только
во время измерительного прогона, на throw-away состоянии дерева. Получаем
классический **PGO-цикл** (profile-guided optimization), но для capacity:

```
  measure ──► propose ──► (approve) ──► apply ──► verify
   dhat        diff-план                in-place    bench Δ
```

Ничего из captrack-wrapper'ов в проде не остаётся. Единственное, что
инструмент кладёт в код, — **числа** в `with_capacity(N)`.

---

## 1. Архитектурное решение

### 1.1 Где живёт инструмент

Новый **bin-crate `captrack-pgo`** как member captrack-workspace:

```toml
# captrack/Cargo.toml
[workspace]
members = [".", "captrack-macros", "captrack-pgo"]
```

Инструмент **generic** — работает на любом Rust-workspace, не только
shamir-db. Первый потребитель — shamir-db (`tx_pipeline` bench).

### 1.2 Measurement backend — pluggable

| Backend | Плюс | Минус | Роль |
|---|---|---|---|
| **dhat** | точный per-byte, file:line:col из backtrace, нулевая инженерия инструментации | global_allocator (только в bench/test-сборке) | **primary** |
| **captrack auto-instrument** | держит captrack-экосистему, без alloc-конфликта | сложный boundary-fix (TrackedVec↔Vec) при авто-вставке | optional / future |

**Решение:** primary = **dhat**. Он не требует переписывать исходники для
измерения (а значит — никакой boundary-инженерии, той самой, что нас и
утопила). captrack auto-instrument остаётся как опциональный backend на
будущее, через тот же интерфейс `Profile`.

### 1.3 Patcher — ядро, backend-agnostic

`apply` принимает абстрактный `SiteStats { key, peak, p50, p95, count }` и
не знает, откуда числа. Это разделяет «как измерили» и «как накатываем» —
оба backend'а кормят один patcher.

```
   profile/dhat.rs ─┐
                    ├─► Vec<SiteStats> ─► plan.rs ─► PatchPlan ─► apply.rs
 profile/captrack.rs┘                       ▲
                                       scan.rs (AST sites)
```

---

## 2. Структура файлов (целевая)

```
captrack-pgo/
├── Cargo.toml
└── src/
    ├── main.rs            # CLI entry: clap dispatch
    ├── cli.rs             # subcommands: measure | propose | apply | undo | auto
    ├── model.rs           # SiteKey, SiteStats, AllocSite, PatchPlan, PatchEntry
    ├── workspace.rs       # найти корень, перечислить .rs (respect .gitignore)
    ├── profile/
    │   ├── mod.rs         # trait Profile { fn sites(&self) -> Vec<SiteStats> }
    │   ├── dhat.rs        # dhat-heap.json → Vec<SiteStats>
    │   └── captrack.rs    # captrack JSON dump → Vec<SiteStats>
    ├── scan.rs            # syn AST walk → Vec<AllocSite> (конструкторы + spans)
    ├── rules.rs           # SiteStats → Option<proposed_cap> (фильтры/округление)
    ├── plan.rs            # match(sites ↔ stats) + rules → PatchPlan
    ├── report.rs          # человекочитаемый diff-вывод плана
    ├── apply.rs           # PatchPlan → in-place byte-edits по spans
    └── undo.rs            # откат последнего apply (manifest-based)
```

Тесты — по правилу проекта: `src/<mod>/tests/` или `tests/` рядом, не
inline. Фикстуры — крошечные `.rs`-сэмплы под `captrack-pgo/tests/fixtures/`.

---

## 3. Порядок выполнения

Каждый шаг = один коммит. Каждый завершается зелёным gate
(`cargo build -p captrack-pgo` + `cargo test -p captrack-pgo` +
`cargo clippy -p captrack-pgo -- -D warnings` + `cargo fmt`).

### Фаза 0 — каркас CLI

**Шаг 1 — `captrack-pgo/Cargo.toml` + workspace member.**
- Файлы: `captrack-pgo/Cargo.toml`, `captrack/Cargo.toml` (+member).
- Deps: `clap` (derive), `syn` (full, visit), `proc-macro2` (span-locations),
  `quote`, `serde`/`serde_json`, `walkdir`, `anyhow`, `ignore` (gitignore-aware).
- `proc-macro2` с feature `span-locations` — критично для file:line:col.
- Gate: `cargo build -p captrack-pgo`.

**Шаг 2 — `src/main.rs` + `src/cli.rs`.**
- `main.rs`: парс clap, dispatch.
- `cli.rs`: enum `Command { Measure, Propose, Apply, Undo, Auto }` со
  стаб-телами (печатают «not yet»). Каждая команда — заглушка.
- Gate: `cargo run -p captrack-pgo -- --help` показывает 5 подкоманд.

### Фаза 1 — модель данных + ввод профиля

**Шаг 3 — `src/model.rs`.** (зависит: 1)
- `SiteKey { file: PathBuf, line: u32, col: u32 }` — Eq/Hash/Ord.
- `SiteStats { key, peak: usize, p50, p95, count: u64 }`.
- `AllocSite { key, ctor: Ctor, current_cap: CapExpr, span_bytes: Range<usize> }`.
  - `Ctor` enum: `Vec`, `VecDeque`, `HashMap`, `HashSet`, `BTreeMap`, ...
  - `CapExpr`: `Literal(usize)` | `Zero` (Vec::new/vec![]) | `Dynamic(String)`.
- `PatchEntry { key, from: CapExpr, to: usize, reason: String }`.
- `PatchPlan { entries: Vec<PatchEntry>, skipped: Vec<(SiteKey, String)> }`.
- Тесты: сериализация round-trip.

**Шаг 4 — `src/profile/mod.rs` + `src/profile/dhat.rs`.** (зависит: 3)
- `trait Profile { fn sites(&self) -> anyhow::Result<Vec<SiteStats>>; }`.
- `dhat.rs`: парс `dhat-heap.json` (формат DHAT v2 — `pps` program-points
  с frames). Из каждого program-point достать вершину стека в нашем
  workspace (отфильтровать std/deps по пути), извлечь file:line:col,
  `tgmax`/`t_gmax_bytes` → peak. Если несколько аллокаций на сайт —
  агрегировать (max peak, sum count).
- ⚠ dhat JSON даёт байты, не элементы. Конверсия `bytes → cap` через
  `size_of::<T>()` — но T инструмент не знает. **Решение:** хранить и
  байты, и (если backend captrack) элементы; для dhat-режима patcher
  предлагает cap в ЭЛЕМЕНТАХ только если scan смог вывести `size_of` из
  типа на сайте; иначе оставляет в отчёте «peak=NNN bytes, нужен ручной
  divide». (См. открытый вопрос Q2.)
- Тесты: фикстура `tests/fixtures/dhat-heap.sample.json` → ожидаемые SiteStats.

**Шаг 5 — `src/profile/captrack.rs`.** (зависит: 3)
- Парс captrack-dump JSON (формат уже есть — `dump.rs`, версионированный,
  ключ по file:line:col, raw samples). Из samples посчитать p50/p95/peak.
- Это backend на случай auto-instrument; даёт элементы напрямую (не байты).
- Тесты: фикстура captrack-dump → SiteStats.

**Шаг 6 — `src/workspace.rs`.** (зависит: 1)
- Найти workspace-корень (вверх по дереву до `Cargo.toml` с `[workspace]`).
- Перечислить `.rs` через `ignore::WalkBuilder` (respect .gitignore,
  пропустить `target/`, `tests/fixtures/`).
- Тесты: на временном дереве.

### Фаза 2 — AST-скан исходников

**Шаг 7 — `src/scan.rs`.** (зависит: 3, 6) ⭐ ключевой
- `syn::parse_file` каждого `.rs`. `Visit` ищет вызовы-конструкторы:
  - `Vec::with_capacity($e)`, `Vec::new()`, `vec![]` (пустой).
  - `VecDeque/HashMap/HashSet/BTreeMap/BTreeSet::with_capacity/new`.
  - `with_capacity_and_hasher(...)`.
- Для каждого — `SiteKey` из `span().start()` (нужен `span-locations`),
  `CapExpr` из аргумента, байтовый диапазон аргумента (для точечной замены).
- Пропустить: внутри `macro_rules!`-тел, `#[derive]`, тестовых модулей
  (опционально, флаг `--include-tests`).
- ⚠ proc-macro2 spans дают (line, col), но для byte-level edit нужен
  байтовый offset — держать карту «line → byte-offset» по файлу (один
  проход), конвертировать.
- Тесты: фикстуры с каждым видом конструктора → ожидаемые AllocSite.

### Фаза 3 — правила + план + отчёт

**Шаг 8 — `src/rules.rs`.** (зависит: 3)
- `fn propose_cap(s: &SiteStats, current: &CapExpr) -> Decision`.
- `Decision = Patch{to, reason} | Skip{reason}`.
- Пороги (конфигурируемы, дефолты):
  - `count < 10` → Skip("низкая частота, недостоверно").
  - `peak == 0` → Skip("фантомный сайт").
  - `current` уже `≥ peak` → Skip("достаточно").
  - `current == 0 && peak ≥ 4` → Patch(next_pow2(p95)).
  - `current > 0 && peak ≥ 4*current` → Patch(next_pow2(p95)).
  - иначе Skip("вариация в пределах нормы").
- Округление: `next_pow2(p95)` дефолт; альт — round-to-8 (флаг).
- ⚠ p95, не peak — peak ловит выброс, p95 устойчивее. Документировать.
- Тесты: табличные кейсы на каждое правило.

**Шаг 9 — `src/plan.rs`.** (зависит: 7, 8, 4/5)
- Вход: `Vec<AllocSite>` (scan) + `Vec<SiteStats>` (profile).
- Match по `SiteKey` (file:line:col). Несовпавшие профиль-сайты →
  warning «измерено, но не найдено в AST» (вероятно generated/macro).
  Несовпавшие AST-сайты → молча (не аллоцировали в этом прогоне).
- Прогнать `rules::propose_cap` → `PatchPlan`.
- Тесты: synthetic site+stats → ожидаемый план.

**Шаг 10 — `src/report.rs`.** (зависит: 9)
- Человекочитаемый вывод:
  ```
  crates/shamir-engine/.../write_exec.rs:158:38
    Vec::with_capacity(0)  →  Vec::with_capacity(64)
    peak=72 p95=61 count=4231  (next_pow2(p95))
  ────────────────────────────────────────────
  12 patch, 22 skip (8 low-count, 14 sufficient)
  ```
- `propose` команда = scan + profile + plan + report (без записи).
- Gate: `cargo run -p captrack-pgo -- propose --heap <json>` печатает план.

### Фаза 4 — apply + undo

**Шаг 11 — `src/apply.rs`.** (зависит: 9) ⭐ осторожно
- `PatchPlan` → правки. **Сверху вниз по убыванию byte-offset** в каждом
  файле (правка с конца не сдвигает offset'ы выше неотредактированных).
- Заменить только аргумент-диапазон: `with_capacity(0)` → `with_capacity(64)`;
  `Vec::new()` → `Vec::with_capacity(64)`; `vec![]` → `Vec::with_capacity(64)`.
- Записать `manifest` (что/где/из чего/во что) в
  `target/captrack-pgo/last-apply.json` для undo.
- ⚠ НЕ перезаписывать форматированием весь файл (никакого prettyplease —
  он переформатирует всё). Точечный byte-splice, diff минимальный.
- Тесты: фикстура до → apply → ожидаемый после (точный equality).

**Шаг 12 — `src/undo.rs`.** (зависит: 11)
- Прочитать manifest, откатить каждую правку (to → from), сверху вниз.
- Защита: если файл изменился после apply (hash mismatch) — отказ с
  предупреждением (пусть человек разрулит через git).
- Тесты: apply → undo → байт-в-байт исходник.

### Фаза 5 — оркестрация `auto`

**Шаг 13 — проводка `auto` в `src/cli.rs`.** (зависит: 10, 11)
- `auto --bench <name> [--apply]`:
  1. (если dhat) подсказать/запустить bench с dhat-feature, собрать json.
  2. propose → показать план.
  3. если `--apply` И план непуст → apply + печать manifest-пути.
  4. иначе — только план (dry-run по умолчанию).
- НЕ запускать bench сам по умолчанию (это тяжело и среда-зависимо) —
  принимать готовый `--heap <json>`; `--run-bench` опционально.
- Gate: end-to-end на фикстур-workspace.

---

## 4. Потребление в shamir-db (отдельная ветка работ)

Инструмент готов → применяем к shamir-db. Это **не** часть captrack-репо,
отдельные задачи в shamir-db:

- **S1.** Откатить #296 (Партия 1): вернуть 4 файла к голому
  `Vec::with_capacity()`, снять `captrack` dep из `shamir-engine`. Вернуть
  `tx_pipeline.rs` bench к `criterion_main!` (убрать captrack-dump).
  → дерево shamir-engine снова чистый Rust.
- **S2.** Включить dhat в `tx_pipeline` bench (cfg-gated `#[global_allocator]`
  + профайлер). `dhat = "0.3"` уже в dev-deps (сейчас unix-only — расширить
  на windows).
- **S3.** Прогнать → `dhat-heap.json`.
- **S4.** `captrack-pgo propose --heap dhat-heap.json` → ревью плана глазами
  (human-in-the-loop первую итерацию).
- **S5.** `apply` → коммит data-driven capacities.
- **S6.** Re-bench, замерить ms Δ.

После S1 вопросы «boundary / IntoInner / two-macros / прозрачность»
исчезают полностью — их источником была wrapper-инструментация в проде.

---

## 5. Решения (зафиксированы)

- **Продакшен-код — голый Rust.** Никаких captrack-обёрток/макросов в
  поставляемом коде. Инструментация живёт только в измерительном прогоне.
- **dhat — primary measurement backend.** Не требует переписывать исходник
  → нет boundary-инженерии. captrack-auto-instrument — опциональный backend
  на будущее через тот же `trait Profile`.
- **Patcher backend-agnostic.** `apply` работает от `SiteStats`, не знает
  источник чисел.
- **Точечный byte-splice, не reformat.** Diff правки = только изменённое
  число. Никакого prettyplease/rustfmt-прохода по файлу.
- **p95, не peak, для предложения.** Peak — выброс; p95 устойчив. Округление
  `next_pow2` по дефолту.
- **dry-run по умолчанию.** `apply` только по явному флагу; всегда пишет
  manifest для `undo`.

## 6. Открытые вопросы

- **Q1. captrack-репо или shamir-db/tools?** План кладёт `captrack-pgo` в
  captrack-workspace (member). Альтернатива — `shamir-db/tools/cap-pgo`
  (не публикуется). За captrack: переиспользуемость, OSS-ценность. За
  shamir-db: не раздувает OSS-крейт bin-зависимостями (clap/syn/walkdir).
  **Склонение:** captrack-member, но bin за feature-gate, чтобы
  `cargo install captrack` не тянул CLI-deps. Решить на Шаге 1.
- **Q2. dhat даёт байты, не элементы.** Конверсия `bytes/size_of::<T>()`
  требует знать T на сайте. Scan может вывести T из аннотации (`let v:
  Vec<Foo>`), но не всегда (inference). **План:** где T известен — делим и
  предлагаем cap в элементах; где нет — отчёт показывает байты и метит
  «manual». captrack-backend этой проблемы не имеет (считает элементы).
- **Q3. Округление — `next_pow2` или round-to-8?** Дефолт pow2, флаг
  `--round=8|pow2|exact`. Подтвердить на реальных данных Шага S4.
- **Q4. Судьба captrack-wrapper'ов.** Остаются ли `tvec!`/TrackedX в
  captrack как продукт? Да — это легитимный «continuous tracking» use-case
  для тех, кому нужно наблюдение в проде (не наш кейс). captrack-pgo —
  второй, независимый режим того же крейта.

## 7. Оценка объёма

| Фаза | Шаги | ~LOC | Заметка |
|---|---|---|---|
| 0 каркас | 1–2 | ~120 | clap-boilerplate |
| 1 модель+профиль | 3–6 | ~350 | dhat-парс самый ёмкий |
| 2 scan | 7 | ~250 | syn-visit, span→byte |
| 3 правила+план | 8–10 | ~250 | |
| 4 apply+undo | 11–12 | ~200 | byte-splice + manifest |
| 5 auto | 13 | ~80 | проводка |
| **Σ captrack-pgo** | | **~1250** | + тесты ~×0.6 |
| shamir-db S1–S6 | | ~небольшой | откат + dhat-setup + apply |

Против ~2000 LOC самопального instrument+de-instrument с boundary-fix
(который dhat убирает целиком) — экономия ~40% и точнее данные.

---

## Path B Migration (completed — 2026-06-28)

> The plan above (phases 0–5, steps 1–13) describes the **original Path A**
> implementation: a syn-based AST scanner matched profile sites to source
> locations and applied byte-splice patches.  That pipeline was completed
> through M4 but replaced in M5 with a semantically stronger approach.

### What changed and why

**Problem with Path A (syn-based):**
The syn AST matcher had three coverage gaps that blocked real-world adoption:

1. **Type aliases** — `type MyVec<T> = Vec<T>; MyVec::new()` was not
   recognised because `syn` only sees the surface syntax, not the resolved
   type.
2. **`Default::default()` calls** — constructors via the `Default` trait were
   invisible to pattern matching on constructor names.
3. **Macro-expanded constructors** — collections created inside `vec![]` or
   other macros after expansion were inaccessible to syn's pre-expansion AST.

**Solution — Path B (Dylint plugin):**

Replace the syn matcher with a Dylint lint plugin (`captrack-pgo-lint/`) that
operates on rustc's HIR after type-checking.  The plugin:

- Detects collection constructors via `clippy_utils` path resolution — works
  for aliases, `Default`, and macro expansions.
- Reads `CAPTRACK_PGO_PROFILE` to filter to matched sites.
- Emits `rustfix`-compatible `Suggestion`s that `cargo dylint --fix` applies.

**Trade-off accepted:**

The plugin must be compiled against a pinned nightly toolchain
(`nightly-2026-04-16` in `captrack-pgo-lint/rust-toolchain.toml`) because
`clippy_utils` is only available on nightly.  The CLI itself (`captrack-pgo`)
remains on stable Rust.  This nightly dependency is isolated to the plugin
compilation step and does not affect consumers of the `captrack` library.

### New pipeline (M5 onward)

```
captrack dump (profile.json)
        │
        ▼
captrack-pgo apply --profile profile.json
        │  sets CAPTRACK_PGO_PROFILE
        ▼
cargo dylint --path captrack-pgo-lint --fix
        │  HIR detection + rustfix suggestions
        ▼
source files rewritten in place
        │
        ▼
last-lint-apply.json  (before/after snapshot for undo)
```

### What was removed

- `src/scan.rs`, `src/plan.rs`, `src/rules.rs`, `src/report.rs`,
  `src/apply.rs`, `src/undo.rs` — syn pipeline.
- CLI subcommands `propose`, `auto`, and the old `apply` (syn-based).
- `last-apply.json` manifest format (byte-splice, v1) — no longer producible.
- Cargo deps: `syn`, `quote`, `proc-macro2`, `walkdir`.

---

## Path C — full auto-instrument + hasher (planned)

> Path B (M1–M5) delivered the `apply` half. Path C closes the loop: the user
> never touches collection constructors by hand; a single CLI flow instruments,
> profiles, restores, and applies in one round-trip.

### Target user workflow

```bash
# 1. Auto-wrap every bare Vec::new() / HashMap::new() etc. into TrackedX::with_capacity_named(...)
captrack-pgo instrument --workspace .

# 2. Run any bench/test as usual — captrack telemetry already records peaks
cargo test --features captrack/telemetry

# 3. Dump the captrack profile (the user's bench code calls dump_capacity_stats)
#    Then revert the instrumentation:
captrack-pgo uninstrument --workspace .

# 4. Apply final optimisations (capacity + chosen hasher) to the now-restored vanilla code
captrack-pgo apply --profile dump.json --workspace . --hasher fx
```

End result: source returns to vanilla shape, but every `Vec::new()` becomes
`Vec::with_capacity(N)` and every `HashMap::new()` becomes
`HashMap::with_capacity_and_hasher(N, ::fxhash::FxBuildHasher::default())` (or
the hasher chosen via `--hasher`).

### Design decisions (locked)

1. **Instrument form = direct constructor call**, not a macro.
   Replacement: `Vec::new()` → `::captrack::TrackedVec::<_>::with_capacity_named(0, file!(), line!(), column!())`.
   Rationale: no dependence on macro-import resolution, easier to debug, the
   `::captrack::` absolute path works regardless of the user's `use` lines.
   The downside (verbosity) is invisible to the human — code is restored
   afterwards by `uninstrument`.
2. **Hasher choice = CLI parameter**, defaulting to `none` (preserves `RandomState`).
   Options: `fx` (`::fxhash::FxBuildHasher::default()`), `ahash`
   (`::ahash::RandomState::new()`), `foldhash`
   (`::foldhash::fast::RandomState::default()`), `none` (skip).
   The chosen hasher path is emitted with a leading `::` (absolute) so the
   plugin never has to add `use` lines.
3. **Hasher swap is part of `apply`** (not a separate subcommand). For each
   matched `HashMap`/`HashSet` (and the third-party hash-keyed types we already
   handle) the plugin upgrades the constructor to the hasher-bearing form
   when `--hasher` is set to anything other than `none`.
4. **Instrument/uninstrument use the same manifest format** as `apply`:
   per-file before/after snapshot + sha256. `undo` becomes generic across all
   three operations.

### Milestones

- **M6 — `CAPTRACK_PGO_INSTRUMENT` lint.** New lint in `captrack-pgo-lint`,
  active when env var `CAPTRACK_PGO_INSTRUMENT=1` is set (mutually exclusive
  with `CAPTRACK_PGO_PROFILE`). Replaces every bare std collection
  constructor with the absolute `::captrack::TrackedX::with_capacity_named(...)`
  form. `MachineApplicable`. Skips sites already inside a `Tracked*` call.
  UI tests for Vec/VecDeque/HashMap/HashSet/BTreeMap/BTreeSet.

- **M7 — `captrack-pgo instrument` subcommand.** Orchestrates
  `cargo dylint --fix` with `CAPTRACK_PGO_INSTRUMENT=1`. Reuses the existing
  before/after manifest infrastructure (`last-lint-apply.json` renamed to
  `last-lint.json` or kept generic). Pre-flight: warns if `captrack` is not a
  dep of the target workspace, or its `telemetry` feature is not enabled.

- **M8 — `captrack-pgo uninstrument` subcommand.** Reverts the latest
  instrument manifest. Effectively a thin wrapper around the generic undo path
  with a clearer name; rejects with a helpful message if the latest manifest
  is from `apply` (those should go through `undo`).

- **M9 — `--hasher` flag in `apply`.** Extend `CAPTRACK_PGO_CAPACITY` lint to
  optionally emit hasher-bearing constructor forms based on a new env var
  `CAPTRACK_PGO_HASHER` ("fx" | "ahash" | "foldhash" | unset). For
  `HashMap::new()` with hasher=fx → suggest
  `HashMap::with_capacity_and_hasher(N, ::fxhash::FxBuildHasher::default())`.
  Extend `apply` CLI with `--hasher` flag that sets the env var. Type
  parameter handling: when adding a hasher to a `HashMap<K,V>`, the type
  parameter list must be extended to `HashMap<K, V, ::fxhash::FxBuildHasher>` —
  the plugin must either rewrite the variable's type ascription
  (`let m: HashMap<K,V> = ...` → `let m: HashMap<K,V,::fxhash::FxBuildHasher> = ...`)
  or accept that some sites compile only via inference. Document the
  limitation and emit the suggestion only when inference is guaranteed to
  work (no explicit `Map<K,V>` ascription).

### Open questions for Path C

- **Third-party hash-keyed types** (`IndexMap`, `DashMap`, `scc::HashMap`):
  do we also rewrite their constructors? Yes for consistency, but each
  third-party type has its own `with_hasher` signature variation — needs
  per-type rules.
- **Cargo.toml dep auto-add.** When `--hasher fx` is used, the workspace must
  depend on `fxhash`. `apply` could emit a warning rather than mutating
  `Cargo.toml`. Decision: **warn only**, user manages deps.
- **Hashing for BTree* / Vec*** — no hasher applies; `--hasher` skips them
  silently.
