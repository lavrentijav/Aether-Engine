# Queue H — real block collision shapes (crates aether-physics, aether-server)
KNOWN_ISSUES item 11: "Block collision is per cube: slabs, stairs and fences collide as full blocks
for mobs and item entities". H1: collision boxes per block **state** from vanilla 1.21.11 data
(generate a table from minecraft-data `blockCollisionShapes` the way `registry/blocks.rs` was
generated — keep the generator script in `tools/gen/` and document it). H2: use the boxes in
`aether-physics` stepping for mobs and item entities (AABB vs list of boxes); open doors/gates
collide per their state. Tests: an entity walks up a slab, stands on a fence at 1.5, passes an open gate.
