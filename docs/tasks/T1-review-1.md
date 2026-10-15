# T1 hostile review — gameplay for 1.20.2–1.20.4 (764/765)

I checked every clientbound/serverbound packet the 764/765 codecs now forward into `modern::play` / `modern::Codec` against PrismarineJS `minecraft-data` (`pc/1.20.2`, `pc/1.20.3` = 765) and the repo's generated tables, and ran the suite (`cargo-locked test -p aether-server` → 803 passed). Packet ids, entity-type ids, block-state counts and item maps all check out; the defects are all in the pre-1.20.5 **layout** gates.

## Findings

### 1. HIGH — `update_mob_effect` is encoded with the 1.20.5+ body, so a 1.20.2–1.20.4 client disconnects
`crates/aether-server/src/protocol/modern/play.rs:475-491`

1.20.2/1.20.3 `packet_entity_effect` is `entityId varint, effectId varint, amplifier i8, duration varint, hideParticles i8, factorCodec option<anonymousNbt>`; 1.20.5 replaced the tail with a single `flags u8`. The arm has **no `gen` gate** and writes the 1.20.5 form (`var_int amplifier, var_int duration, u8 flags`). The `factorCodec` option boolean is never written, so the packet is exactly one byte short. Concrete scenario: a 1.20.2 player eats an enchanted golden apple (`game/effects.rs::give` → `ServerEvent::UpdateMobEffect`) → the client reads `hideParticles`, then tries to read the option bool past the end of the packet → `DecoderException`/kick. (The amplifier being `VarInt` is harmless only because values are <128.) The existing test `update_mob_effect_carries_id_amplifier_duration_and_flags` uses `V1_21_11` only, so it cannot see this.

### 2. HIGH — `OpenContainer` window title is sent as NBT for 1.20.2
`crates/aether-server/src/protocol/modern/mod.rs:422-434` (title at `:432`)

`ServerEvent::OpenWindow` in `play.rs:637` correctly gates the 1.20.2 JSON form, but `ServerEvent::OpenContainer` (the stash window, `stash/mod.rs:160`) writes `nbt::string(title).to_network()` unconditionally. For **1.20.2** `open_window.windowTitle` is a length-prefixed **JSON string**, so the client reads the NBT tag byte `0x08` as the string length 8 and then runs off the end / leaves trailing bytes. Concrete scenario: a 1.20.2 player opens `/stash` (now reachable because `supports_containers()` returns `true` at `v764/mod.rs:119`) → kick. The 1.20.2-only test `open_screen_and_death_messages_are_json_in_1_20_2` covers `OpenWindow`, not `OpenContainer`; the `OpenContainer` test (`modern/mod.rs:1315`) runs only on `V1_21_11`.

### 3. MEDIUM — pre-1.20.5 creative slot: the `present` bool is read as the count; the real `i8` count is dropped
`crates/aether-server/src/protocol/modern/mod.rs:507-534` (first read `:515`)

The shared decoder assumes the 1.20.5+ `Slot` (`count varint, item id, …`). For 764/765 the slot is `present bool, itemId varint, count i8, nbt`, so `pin.var_int()` at `:515` reads the presence byte (1), the id is read correctly, and `count` is forced to 1 (`:523`). Concrete scenario: a creative 1.20.2/1.20.4 player moves a 64-stack into a hotbar slot — the client reports count 64, `record_slot_change` (`session.rs:762`, `players.rs:130`) stores count 1, and the server syncs one item back: 63 items vanish. The v765 test `gameplay_packets_the_old_subset_never_read_decode_through_the_shared_path` only feeds count 1, so it passes while asserting the buggy path.

### 4. MEDIUM — `entity_action` action ids are the 1.21.6+ mapping; sprint/sneak mis-decode on 764/765
`crates/aether-server/src/protocol/modern/mod.rs:645-652`

1.20.2/1.20.3 `packet_entity_action` (`sb_player_command` = 33) maps `0=start_sneaking, 1=stop_sneaking, 2=stop_sleeping, 3=start_sprinting, 4=stop_sprinting`. The arm has no `gen` gate and maps `1→Sprint(true), 2→Sprint(false)` (the 1.21.6+ ids). Concrete scenario: a 1.20.2 player releases sneak (action 1) and the server calls `interact::set_stance(..., Some(true))` (start sprint); pressing sprint (action 3) is ignored; and because `sb_player_input` is `-1` for these protocols, sneaking is never handled at all. The correct mapping differs for every protocol <771 (this is inherited from J9's 766+ range, but T1 makes it live for 764/765).

### 5. LOW — 764/765 placement is hard-coded to stone
`crates/aether-server/src/protocol/v764/mod.rs:442`, `crates/aether-server/src/protocol/v765/mod.rs:427`

`ServerEvent::Place` from the v764/v765 decode always sets `block: block_ids::STONE`, rather than the modern path's `inventory::block_in_hotbar_slot(0)` fallback. Concrete scenario: a 1.20.2 survival player with dirt/oak planks selected right-clicks a face and a stone block is placed. Documented as a gap in the code comment, but it is a real gameplay loss versus the 1.20.5+ codecs T1 claims parity with.

## Verdict

**DO NOT MERGE.** Findings 1 and 2 are protocol-level defects that disconnect real 1.20.2–1.20.4 clients (any status effect; opening the stash), and both are exactly the class of pre-1.20.5 gate T1 was supposed to add. Finding 3 is silent item loss through the compat slot decode. The suite passes only because the new tests exercise the encoders/decoders on `V1_21_11` or on the happy-path values; per-version layout tests for `update_mob_effect`, `OpenContainer`, and a count>1 creative slot are missing.
