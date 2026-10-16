# CMD-06b review 1 — entity selector parser + selection semantics

Reviewer: independent critic pass. Branch `oc/CMD06b`, commit `b44f59c`
("CMD06b: entity selector parser and selection semantics").
Base: merge-base with `main` is `ccd9507` (merge of `oc/CMD06a`), so
`git diff ccd9507..HEAD` is the real change set: 2 files, 2313 insertions
(`crates/aether-server/src/commands/mod.rs` +1, `selector.rs` new). The branch
is otherwise at `main`'s tip; no merge/rebase needed.

Reference used for "must match vanilla 1.21.11":
* decompiled 1.21.11 sources — `net/minecraft/command/EntitySelectorReader.java`,
  `EntitySelectorOptions.java`, `EntitySelector.java`,
  `argument/EntityArgumentType.java`, `predicate/NumberRange.java`;
* the 1.21.11 `en_us.json` lang file.

Checks run (all with `CARGO_TARGET_DIR=/srv/dev/build/aether-target`,
`CARGO_INCREMENTAL=0`, worktree `/srv/dev/projects/Aether-Engine-wt/crit-CMD06b`):

* `cargo fmt --all -- --check` — **clean** (exit 0)
* `cargo clippy -p aether-server --all-targets -- -D warnings` — **clean** (exit 0)
* `cargo-locked test -p aether-server` — **1319 passed, 0 failed** (30 of them
  `commands::selector::tests`), + 2 e2e tests.
* No files were edited, committed or pushed; no separate target dir created.
  (`--lib` link artifacts were already present, no `cargo clean` needed.)

---

## Verdict

**CHANGES.** The core is well built — variables/defaults, range syntax,
`find`/`select`/`select_one`, `@p`/`@r`/`@s` edge cases, tag/name/score/nbt/
advancement filters, sort/limit and the "snapshot players+entities once" lock
shape all hold up, fmt/clippy/tests are green, and the fuzz test is real. But
several **selector-option validation rules and error strings do not match the
1.21.11 reference**, and the brief makes vanilla parity (including error text)
explicit. The blockers are the mixed positive/negated filter options (1) and the
`@s`-inapplicable `limit`/`sort` (2); the rest are correctness/message gaps that
should be fixed or consciously waived.

---

## Findings

### 1. MEDIUM — Mixed positive/negated filter options are accepted; vanilla rejects them
`crates/aether-server/src/commands/selector.rs:553-583` (`type`),
`:546-551` (`name`), `:644-655` (`gamemode`), `:637-643` (`team`).

Each of these keys only guards against a **second positive** value (`once(seen)`
/ `type_equality`). Vanilla's handler also does `if (excludesX() && !negated)`
and its applicability condition is `!selectsX()`, so a positive and a negated
value can never coexist:

* `@e[type=creeper,type=!pig]` — code parses (selects creepers, excludes pigs);
  vanilla: `getHandler` condition `!selectsEntityType()` is false → inapplicable.
* `@e[type=!creeper,type=pig]` — code parses; vanilla: `excludesEntityType() &&
  !bl` → inapplicable.
* `@a[name=Alice,name=!Bob]` — code parses; vanilla inapplicable.
* `@a[gamemode=creative,gamemode=!survival]` — code parses; vanilla inapplicable.
* `@a[team=red,team=!blue]` — code parses; vanilla inapplicable (only
  `team=!a,team=b` is permitted, because the `team` handler has no
  `excludesTeam()` check — the code happens to match that one direction).

Negative-then-negative (`@e[type=!creeper,type=!pig]`) is correctly allowed.
Repeated positives are correctly rejected, but with the wrong message (see 5).
Verification source: `EntitySelectorOptions.register` `name`/`gamemode`/`team`/
`type` handlers and `getHandler`.

### 2. MEDIUM — `limit` and `sort` are accepted on `@s`
`crates/aether-server/src/commands/selector.rs:693-712`; `is_single` `:494-496`.

Vanilla's conditions are `!reader.isSenderOnly() && !hasLimit()` and
`!reader.isSenderOnly() && !hasSorter()`, so `@s[limit=2]` and `@s[sort=nearest]`
are **inapplicable**. Code accepts both. (`@s` defaults `limit=1`, so
`is_single` masks this for `parse_single`, but the options should still be
rejected at parse time.)

### 3. MEDIUM — `scores` accepts decimal values; vanilla requires integer ranges
`crates/aether-server/src/commands/selector.rs:656-660` → `parse_scores`
`:1329-1341` → `parse_range` `:1295-1327` (f64).

Vanilla parses scores with `NumberRange.IntRange`. `@e[scores={foo=1.5}]` must
fail ("Invalid integer '1.5'"); the code accepts it and compares as a float.
Also, a `scores` objective key is not validated (that part is fine: vanilla just
returns `false` for an unknown objective).

### 4. MEDIUM — `x_rotation`/`y_rotation`: swapped ranges rejected, angles not wrapped
`crates/aether-server/src/commands/selector.rs:625-632` (parse),
`:878-888` (match), `parse_range` `:1315-1317` (`SwappedRange`).

Vanilla uses `NumberRange.AngleRange`, which does **not** reject `min > max`, and
the predicate wraps both bounds and the entity angle with `MathHelper.wrapDegrees`
(`minAngle > maxAngle ? angle >= minAngle || angle <= maxAngle : …`). So
`@e[y_rotation=170..-170]` is valid in vanilla; the code returns `SwappedRange`.
Conversely the code compares raw `e.rotation.pitch`/`yaw` without wrapping.

### 5. MEDIUM — Error strings that must match vanilla do not
The brief requires "совпадение текстов ошибок с ванилью"; several differ. The
string source is `ParseError::message` `selector.rs:1065-1095`.

* **Duplicate options** (`:1074`): `Duplicate(k)` → `"Option '{k}' is already
  set"`. Vanilla has no such key and reports duplicates through the option's
  applicability predicate → `argument.entity.options.inapplicable` =
  `"Option '%s' isn't applicable here"`. This hits duplicate `distance`, `level`,
  `x/y/z/dx/dy/dz`, `x_rotation/y_rotation`, `limit`, `sort`, `scores`,
  `advancements`, `name`, `gamemode`, `team`, `type`. The unit tests
  `:1663-1669` lock in the wrong string.
* **`level` negative** (`:1086-1090`): code → `"Option 'level' shouldn't be
  negative"`; vanilla `argument.entity.options.level.negative` =
  `"Level shouldn't be negative"`. (`distance` is correct: `"Distance cannot be
  negative"`.)
* **Non-integer range** (`:1090-1092`): code → `"Only whole numbers are allowed
  for option '{key}': found '{value}'"`. Vanilla's `IntRange.parse` surfaces the
  Brigadier built-in `"Invalid integer '%s'"` (the lang also carries
  `argument.range.ints` = `"Only whole numbers are allowed; not decimals"`).
  Neither equals the code's string.
* Correct strings worth noting: `Missing selector type`, `Unknown selector type
  '%s'`, `Unknown option '%s'`, `Expected value for option '%s'`,
  `Option '%s' isn't applicable here`, `Invalid or unknown sort type '%s'`,
  `Invalid or unknown game mode '%s'`, `Invalid or unknown entity type '%s'`,
  `Expected end of options`, `Limit must be at least 1`, `Min cannot be bigger
  than max`, `No entity/player was found`, `Only one entity/player is allowed…`.

### 6. LOW — `type=` value is never checked against the entity-type registry
`crates/aether-server/src/commands/selector.rs:553-583` + `valid_resource`
`:1429-1446`. Only lexical validity is checked, so `@e[type=notarealtype]`
parses and silently matches nothing. Vanilla throws
`argument.entity.options.type.invalid` ("Invalid or unknown entity type '%s'").
This needs a registry hook (or an explicit "deferred to resolution" note), since
the parser currently has no way to know the valid ids.

### 7. LOW — Volume filter uses the feet point, not the bounding box
`crates/aether-server/src/commands/selector.rs:843-856`, `span` `:935-940`.
The `…+1` span itself is correct (matches vanilla `createBox`), but vanilla tests
`box.intersects(entity.getBoundingBox())`, while the code tests the entity's feet
point with inclusive bounds. Entities straddling a boundary (or large hitboxes)
can differ. Documented and acceptable while `EntityRef` carries only `pos`, but
worth a note.

### 8. LOW — No alive filter for `@e`/`@n`
`crates/aether-server/src/commands/selector.rs:733-779`. Vanilla sets
`requiresAlive` for `@e` and `@n` (and `@s` may select a dead entity; `@a`/`@p`/
`@r` may select dead players). `EntityRef` has no alive flag, so the distinction
is currently lost. Flag it so a later `EntityRef` change doesn't silently
diverge.

### 9. LOW — Single/player wording gaps
`is_single` `:494-496`, `parse_single`/`parse_with_amount` `:460-486`,
`select_one` `:795-813`. `parse_single` always yields
`TooManyEntities` ("Only one entity…"); vanilla uses `argument.player.toomany`
("Only one player…") when the argument is player-only. Also
`argument.player.entities` ("Only players may be affected by this command, but
the provided selector includes entities") — raised when a player-only *multiple*
argument gets `@e` — has no equivalent. Probably the command layer's job, but
nothing records the players-only intent on the selector.

### 10. LOW — `name=` ignores the vanilla display-name fallback
`crates/aether-server/src/commands/selector.rs:823-828` compares
`EntityRef.name` (`None` → `""`). Vanilla compares `entity.getStringifiedName()`,
so an unnamed non-player (or one whose name has not been captured) cannot be
matched and mob display names never match. Player paths are fine.

### 11. LOW — Test gaps matching the bugs above
`crates/aether-server/src/commands/selector.rs` `mod tests`:
* no case for mixed positive/negated `name`/`team`/`gamemode`/`type` (§1);
* no case for `@s[limit=…]` / `@s[sort=…]` (§2);
* no case for decimal `scores` (§3);
* no case for wrapped angle ranges (§4);
* `find_snapshots_the_source_once_each` `:2169` counts only `players()`/
  `entities()`, not the per-entity `entity_type_in_tag` callback used by
  `type=#tag` (§ below).

### 12. LOW — Module doc overstates the lock-order guarantee
`crates/aether-server/src/commands/selector.rs:24-33` says filtering "never
reaches back into a locked source per entity", but `matches` calls
`source.entity_type_in_tag(&e.kind, t)` once per entity per `type=#tag` filter
(`:829-835`). The player/entity snapshot guarantee holds; the doc sentence should
be qualified.

---

## What is correct (for the record)

* `find` (raw, silent on empty), `select` (empty → `No entity`/`No player`),
  `select_one` (empty + >1) semantics; `@p`/`@r` with no players; `@s` with no
  executor; `parse_single` rejecting `@a`/`@e`/`@e[limit=2]` but allowing
  `@e[limit=1]`/`@a[limit=1]`.
* Defaults: `@a`/`@e` unlimited, `@p`/`@r`/`@n` limit 1; sorts
  nearest/nearest/random/arbitrary.
* Range forms `n`, `n..`, `..n`, `n..m`, swapped-min-max, negative
  `distance`/`level`, integer `level`; `distance` measured from `origin` with
  `x/y/z` overrides (console → `(0,0,0)`).
* Volume box construction including the mandatory `+1.0` positive extension
  (`createBox` parity), negative deltas, and unspecified deltas = 0.
* `type` inapplicable on `@a`/`@p`/`@r` (vanilla pre-seeds `EntityType.PLAYER`);
  `type=#tag` resolution via the source; `!` on `name`/`tag`/`team`/`gamemode`/
  `type`/`predicate`/`nbt`; multiple `tag`/`nbt`/`predicate`; `tag=`/`tag=!`
  empty semantics; `team=` empty.
* `scores` unknown objective → no match; `advancements` boolean and
  criterion-map forms; NBT subset matching (compounds subset, lists exact);
  sort-then-truncate with a seedable RNG.
* Snapshot-once shape for `players()`/`entities()`; no panics found on garbage
  (fuzz + mutated corpora pass); parse never touches the source.
