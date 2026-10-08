#!/bin/bash
# Regenerate crates/aether-server/src/protocol/modern/gen/ from the releases
# fetched by fetch.sh (into $AETHER_VERSIONS, default ./versions).
#
# A new release is: fetch it, add a line below with its synchronized
# registries (the game's RegistrySynchronization / RegistryDataLoader
# SYNCHRONIZED_REGISTRIES, in that order) and the core-pack versions that
# share its protocol, add its module to gen/mod.rs and a `Version` in
# version.rs, and register the codec in protocol::codecs().
set -e
ROOT=${AETHER_VERSIONS:-$PWD/versions}
cd "$(dirname "$0")/../.."
G=crates/aether-server/src/protocol/modern/gen
B=$ROOT/1.21.11/generated/reports
SY26_1="minecraft:worldgen/biome,minecraft:chat_type,minecraft:trim_pattern,minecraft:trim_material,minecraft:wolf_variant,minecraft:wolf_sound_variant,minecraft:pig_variant,minecraft:pig_sound_variant,minecraft:frog_variant,minecraft:cat_variant,minecraft:cat_sound_variant,minecraft:cow_sound_variant,minecraft:cow_variant,minecraft:chicken_sound_variant,minecraft:chicken_variant,minecraft:zombie_nautilus_variant,minecraft:painting_variant,minecraft:dimension_type,minecraft:damage_type,minecraft:banner_pattern,minecraft:enchantment,minecraft:jukebox_song,minecraft:instrument,minecraft:test_environment,minecraft:test_instance,minecraft:dialog,minecraft:world_clock,minecraft:timeline"
SY26_2=$(echo "$SY26_1" | sed 's/minecraft:painting_variant,/minecraft:painting_variant,minecraft:sulfur_cube_archetype,/')
SY26_3="$SY26_2,minecraft:decorated_pot_pattern,minecraft:block_transformer,minecraft:worldgen/block_state_provider"
gen() { # <version> <synced> <core packs>
  python3 tools/gen/versions.py "$1" "$ROOT/$1/generated/reports" "$ROOT/$1/data/data/minecraft" "$B" "$2" "$3" "$G" \
    > "$G/v${1//./_}.rs"
}
gen 1.21.11 "" "1.21.11"
gen 26.1 "$SY26_1" "26.1,26.1.1,26.1.2"
gen 26.2 "$SY26_2" "26.2"
gen 26.3 "$SY26_3" "26.3"
