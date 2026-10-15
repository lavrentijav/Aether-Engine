# CMD-34b — Plugin: regions flags, members, bypass

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/regions`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-29b`, `oc/CMD-34a`
Parent/umbrella: `docs/tasks/CMD-34-regions-plugin.md`

## Commands (`/rg` = `/region`)
| command | syntax | node |
|---|---|---|
| flags | `/rg flag <id> <flag> [value]` (`pvp`, `build`, `use`, `entry`, `greeting`, …) | `worldguard.region.flag` |
| members | `/rg addmember <id> <player>` / `/rg removemember` | `worldguard.region.member` |
| owners | `/rg addowner <id> <player>` / `/rg removeowner` | `worldguard.region.owner` |
| bypass | (permission) | `worldguard.region.bypass.<world>` |

Flags are typed and extend the core flag registry (CMD-29b); `build`/`pvp`/`use` map to `Action`.

## Files
- `plugins/regions/`: manifest, `src/`, README.
- region model (CMD-34a), region check (CMD-29b).

## Edge cases to test explicitly
- overlapping regions with priorities; a deny high-priority beats an allow below.
- `flag pvp deny` blocks player-vs-player damage but not mob damage.
- addmember/addowner grant build; a non-member is denied with the configured message.
- bypass is per-world; without it an operator is still subject to flags.

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
