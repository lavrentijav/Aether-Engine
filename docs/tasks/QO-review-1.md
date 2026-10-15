## Review — J9 (branch `oc/QO`)

I verified the generated tables against the checked-in PrismarineJS `minecraft-data` (v3.117.0), the version tables, and the codec constants. Five real defects, strongest first. All tests pass (`765 passed`), so none of these are caught.

---

### 1. FATAL — 1.21.3–1.21.10 send a malformed `entity_teleport`
`crates/aether-server/src/protocol/v768/mod.rs:305` (identical in `v769:305`, `v770:305`, `v771:305`, `v772:305`, `v773:321`)

`ServerEvent::EntityMove` hand-rolls `PLAY_ENTITY_TELEPORT` with the **pre-1.21.2** body: `entityId, x, y, z (3×f64), yaw (i8), pitch (i8), onGround (bool)`.

In 1.21.2+ that packet (`packet_entity_teleport`, id `0x77`/`0x76`/`0x7B` for 768/770/773) was changed to `entityId, x, y, z, dx, dy, dz (6×f64), yaw, pitch (f32), flags (PositionUpdateRelatives), onGround`. `position_sync()` already encodes this correctly and even switches to `sync_entity_position`; the codec's own arm bypasses it.

Concrete: any 1.21.3 client sees another player move. The server broadcasts `EntityMove` (`session.rs:539`) → packet body is 27 bytes where the client expects ~58 → protodef hits end-of-buffer → **client disconnect/desync on every other-player movement**. Should delegate to `modern::play::position_sync` or be gated like `MoveEntity` is.

---

### 2. FATAL — 1.21.9 player spawn uses an entity type that does not exist
`crates/aether-server/src/protocol/v773/mod.rs:126` — `const PLAYER_ENTITY_TYPE: i32 = 155;`

`minecraft-data` `pc/1.21.9`: `minecraft:player` = **151**, max entity id = 152. `155` is the **1.21.11** value (generated `v1_21_11.rs`), not 1.21.9. The codec's hand-rolled `SpawnPlayer` uses this constant instead of `version().entity_type("minecraft:player")` (which the generated `v1_21_9::ENTITY_TYPES` gets right).

Concrete: a second player joins; the 1.21.9/1.21.10 client receives `spawn_entity` with type 155 → unknown entity type → **disconnect**. Also `v768/mod.rs:109` is `147` (1.21.4's value) where 1.21.2/1.21.3 needs **148** (`147` is `zombified_piglin` there) → other players spawn as the wrong entity.

---

### 3. MAJOR — generated damage-type table is truncated and mis-indexed
`tools/gen/gen_legacy_play.js:133`; output `crates/aether-server/src/protocol/modern/legacy_gen.rs:231` (v1_21_3), `:471` (v1_21_1) etc.

The capture regex `\("([a-z0-9_]+)", "([A-Za-z]+)", (ALIVE|ALWAYS)...` cannot match message ids containing a `.` or `_`, so it silently drops:
- `player_explosion` (`"explosion.player"`),
- `sonic_boom`,
- `mace_smash` (768–772).

The codec's own registry (`v767/registry.rs:152,153`, `v768/registry.rs:145,154,155`) does send all 47/49 entries, so the client's wire ids are 0..46/48. The generated `DAMAGE_TYPES` the encoder consults (`play.rs:366`, `damage_type`) is 45/46 entries, so every damage type after the omission is off by 1–2.

Concrete: 1.21.1 starvation (`"minecraft:starve"`, true registry id 36) sends `sourceTypeId` 34 because `player_explosion`+`sonic_boom` were skipped → wrong damage/death type. `v768` starvation is off by 3. No test covers `damage_type()` for any of 766–773.

---

### 4. MINOR — `Time` writes a field the packet does not have before 1.21.3
`crates/aether-server/src/protocol/modern/play.rs:499`

`update_time` gained `tickDayTime: bool` in 1.21.3; for `gen < V1_21_3` the branch still writes `p.i64(time_of_day).bool(true)`. Concrete: 1.20.5/1.21.1 client gets one trailing `0x01` byte after `age,time`. Usually ignored, but it is a wrong layout and violates the "gate by gen" contract the change claims.

---

### 5. MINOR — drop path bypasses the compat stand-in
`crates/aether-server/src/protocol/modern/mod.rs:374`

`ServerEvent::DropItem` uses `self.0.wire_item(*item)` (raw id) instead of `wire_stack_item`, so dropping an iron spear on 1.21.1 spawns a **genuine trident** item entity (no `custom_name`/`lore` marker) — the exact disguise the item agent's work is meant to preserve. Not exploitable (pickup is server-owned), but the dropped item is laundered in appearance, unlike every other slot path which goes through `write_stack`.

---

Not reported (checked, not defects): pose serializer ids 21→20 are correct; entity/attribute/component tables and the v1_20_5 menu `-1` shift match `minecraft-data`; 766/767 `EntityMove` old layout is correct; `position_sync`/`write_velocity` gating is correct.
