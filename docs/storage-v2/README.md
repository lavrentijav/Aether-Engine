# Storage v2 — decisions and phases

Status: **accepted direction, 2026-10-13.** Base design: [`a082-champion.md`](a082-champion.md)
(Russian; winner of a 128-agent design tournament — redo journal as the single commit point,
stores as materialized views, guarded rollback, item type/instance ids). This file records what we
take from it, what we change, and in what order it lands. Where this file and the champion disagree,
**this file wins**.

## Amendments to the champion

1. **Scale.** Defaults are tuned for **50–100 players**; formats and layout must not break at
   1000 (no per-player O(n²), no global locks on the tick path, bounded memory per player).
2. **No migration.** Existing worlds and player data may be wiped. New formats start at version 1;
   old keys are not read. (Keep the format-version + refuse-newer rule for the future.)
3. **History tiers.**
   - *Local (hot)*: redo journal + operational history for `history.local_days` (default **14**).
     Needed for startup and normal rollback. Authoritative.
   - *Archive (cold, optional)*: **ClickHouse on another server** keeps all history (years).
     Never needed to start or play. See "Archive" below.
4. **Simplify first, then grow.** Phase order below puts correctness and the commit protocol first;
   causal (`ActionId`) attribution, worldgen-derived ids and census come later and are toggles.

## Archive (ClickHouse)

- Exporter thread ships **sealed** local journal segments in batches (RowBinary/Native, zstd)
  over TLS with an INSERT-only user; insert dedup token = segment id, so re-sends are harmless.
- A local segment is deleted only after verification: `count()` and a checksum over the segment's
  seq range in ClickHouse match the local segment.
- ClickHouse down → local history simply grows past `local_days`; at a disk threshold raise an
  alert. **Never** slow the tick, never block startup.
- Tables: `events` (MergeTree, `ORDER BY (world, toDate(ts), cx, cz, seq)`, codecs `Delta`/
  `DoubleDelta` + ZSTD, `LowCardinality` for kinds), projections by `actor` and by `item_instance`.
- Old rollback (older than `local_days`) = admin operation: the plan is built from a ClickHouse
  query, applied through the same guarded rollback path (conflicts reported, never overwritten).
- State (players, inventories, economy) is **never** archived there — only history.
- Replaces the PostgreSQL history mirror (`journal/sink.rs`) for analytics; Postgres stays for the
  economy only if still needed.

## Phases → tasks

| Phase | Content | Owner |
|---|---|---|
| 0 | Correctness of today's code: unreadable save never becomes empty (C01), journal seq reservation (C01), connection limits (T02), repo hygiene (T01) | Claude / opencode |
| 1 | Registries: `ItemTypeId u16`, append-only name tables, replace item `String`s in hot structs and events | split |
| 2 | Journal v2 format: batched segments with CRC, group fsync off-tick, `journal.level`, retention by days | Claude (format) + opencode (tests, tooling) |
| 3 | `ItemInstanceId u64` allocator (block reservation) replacing random `u128` uids | opencode |
| 4 | Cross-domain atomic ops (block ↔ inventory in one journal record) + idempotent replay on startup | Claude |
| 5 | Guarded rollback (CAS on post-image), conflict report, tainted policy | Claude |
| 6 | ClickHouse exporter + verification + `/history` cold queries | opencode (+ review) |
| 7 | Causal `ActionId`, worldgen-derived ids, census/dupe detectors | later |

Each task gets a self-contained brief in `docs/tasks/`.
