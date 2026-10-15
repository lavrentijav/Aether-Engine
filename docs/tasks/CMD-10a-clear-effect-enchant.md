# CMD-10a — V-player: clear, effect, enchant

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-10-player-effects.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/clear [<targets>] [<item>] [<maxCount>]` | `minecraft.command.clear` | 2 | `Removed <count> items from <player>` / `Found no items to remove` |
| `/effect give <targets> <effect> [<seconds>] [<amplifier>] [<hideParticles>]` | `minecraft.command.effect` | 2 | `Applied <effect> to <player>` |
| `/effect clear <targets> [<effect>]` | `minecraft.command.effect` | 2 | `Removed <effect> from <player>` / `Took <effect> from <player>` |
| `/enchant <targets> <enchantment> [<level>]` | `minecraft.command.enchant` | 2 | `Enchanted <item> with <enchantment> for <player>` |

## Files
- new `crates/aether-server/src/game/commands_player.rs`.
- `game/effects.rs` (apply/remove), `game/provenance.rs` (clear journals destruction),
  inventory model, enchantment-on-stack storage (T2).

## Edge cases to test explicitly
- `/clear` with an empty inventory → `Found no items to remove`; `maxCount` removes at most that
  many; an item filter removes only that item; destruction is journaled.
- `/effect give` duration 0 → effect default; amplifier beyond cap still applies; `hideParticles`
  toggles metadata for 1.21.11; infinite effect.
- `/effect clear` a single effect vs all.
- `/enchant` on an empty hand / non-enchantable item → vanilla error; level range 1..255.
- selectors may hit several targets.

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
