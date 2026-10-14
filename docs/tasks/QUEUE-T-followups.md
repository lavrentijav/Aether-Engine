# Follow-up tasks T1–T5 (given to agents that already know the code)

Read `docs/tasks/COMMON.md`. Before you start: `git merge main` in your worktree (main moved a lot:
compat layer, gameplay for 1.20.5–1.21.10, durable ticks, hoppers). One commit per task id.
Other agents work on `crates/aether-redstone`, visibility (`S1`), brewing (`S2`), pathfinding (`S3`),
light (`S4`) — do not edit those areas.

## T1 — gameplay for 1.20.2–1.20.4 (the agent that wrote those codecs)
Queue J9 routed 1.20.5–1.21.10 (766–773) through the gameplay path (`full_gameplay() == true`; see
`docs/tasks/QO-notes.md` and the J9 commit). Do the same for 764–765, sharing the J9 code; gate the
pre-1.20.5 differences (item stacks are NBT-based `Slot`, not components; older entity metadata;
registry NBT blob). Tests per differing packet + the spear stand-in test on 765.

## T2 — enchanting, anvil, grindstone (the agent that wrote hoppers/containers)
Enchanting table (bookshelf power count incl. the air gap rule, 3 offers from the vanilla enchantment
algorithm with the player's enchantment seed, lapis + levels cost), anvil (rename, combine, repair
with material, prior-work penalty, "Too Expensive" at 40, anvil damage chance), grindstone (remove
non-curse enchantments, return XP). Enchantments are item components on the stack: store them on
`Stack` losslessly, show them through every codec's slot writer (component on 1.20.5+, NBT `Enchantments`
before). Apply at least: sharpness/protection/efficiency/unbreaking/fortune/silk touch in the existing
combat/mining code. Tests per mechanism.

## T3 — fluids: entity push and waterlogged flow (the agent that made ticks durable)
KNOWN_ISSUES: flowing water does not push mobs or items; waterlogged blocks do not flow; generated
water starts moving only when a neighbour changes. Implement the flow vector (vanilla
`FlowingFluid.getFlow`) and push entities/items in water (vanilla speeds), waterlogged blocks as
fluid sources that spread, and mark freshly generated fluid that can flow for a scheduled tick on
chunk load (bounded per tick). Tests.

## T4 — 1.8.9: fewer stone fallbacks (an agent that knows the codecs)
`compat/gen/v1_8.rs` `STONE_FALLBACKS` lists 422 engine blocks shown as stone on 1.8.9 (KNOWN_ISSUES
A.14). Extend `tools/gen/compat/blocks.json` rules for the pre-flattening id:meta model (e.g. colored
blocks → wool/stained clay/glass by color meta, logs/planks by wood type meta, stairs/slabs/fences of
new woods → oak/spruce equivalents with matching shape, deepslate/tuff/calcite → stone variants,
copper → iron/gold blocks), keep shapes matching (collision!). Target: < 50 fallbacks, each
remaining one listed with why. Test: no block with a rule falls back.

## T5 — one world, one server (the agent that fixed the review findings)
Two server processes opening the same world would both mint item uids and journal seqs from the same
bounds (review finding). Add an exclusive lock on the world directory at open (`flock` on a lock
file; held for the process lifetime; a clear error naming the other PID if taken), for the server and
the converter. Also update the key-space doc on `put_meta` (`aether-api/src/lib.rs`) to list every
top-level key prefix in use (C, V, K, REG, ITM, E, X, P, H, N, U, BT, BF, Wtime, …) — grep the code,
and add a test that fails if two subsystems claim the same prefix (central const list).

## Done
Per COMMON.md. Report hashes, tests, what is left.
