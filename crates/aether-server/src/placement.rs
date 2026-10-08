//! Choosing which *state* of a block a placement produces.
//!
//! A player who puts down a stair means a particular stair: facing away from
//! them, on the half they clicked. Until now every block was placed in its
//! default state, so every stair faced north — see `docs/DESIGN_NOTES.md` §9.4
//! and Known Issues A.6.
//!
//! # What this does and does not cover
//!
//! Everything here is derivable from **the placement alone** — the player's
//! yaw, the face they clicked, where on that face, and what was in the cell.
//! That covers the properties a player notices first: `facing`, `axis`,
//! `half`, `type`, `waterlogged`.
//!
//! One gap inside its own scope is worth naming: blocks whose facing comes
//! from **what they are attached to** rather than from where the player looked
//! — a ladder, a wall torch, a wall sign — are not distinguished from blocks
//! that face the player, because nothing in the block's own data says which
//! kind it is. They get the look-based answer, which is wrong for them. The
//! ones that *do* declare it, via a `face` property, are handled.
//!
//! It deliberately does **not** cover properties that are functions of
//! *neighbours* — a fence's connections, a redstone wire's shape, a door's
//! hinge side, a chest becoming a double. Those have to re-run when a
//! neighbour changes, not only when the block is placed, so they need a block
//! update pass that does not exist yet. A fence placed by this code is a
//! correct, unconnected fence.
//!
//! # Why it is data-driven
//!
//! There is no table of "blocks that have a facing" here. The block's own
//! property schema is asked what it has, and each rule applies only if the
//! block declares that property with a value it accepts. So a modded block
//! with a `facing` gets the same treatment as a stair, and a block without one
//! is left alone — rather than a list that has to be kept in step with the
//! game.

use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

/// Everything known at the moment of placement.
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// The player's yaw in degrees, as the client reports it.
    pub yaw: f32,
    /// The player's pitch in degrees.
    pub pitch: f32,
    /// Clicked face: 0 down, 1 up, 2 north, 3 south, 4 west, 5 east.
    pub face: u8,
    /// Where on the face, each `0.0..=1.0`.
    pub cursor: (f32, f32, f32),
    /// Whether the cell being filled already held water.
    pub into_water: bool,
}

/// The horizontal direction a player at `yaw` is facing.
///
/// Minecraft's yaw is 0 at south and increases clockwise, which is the
/// off-by-90-degrees everyone gets wrong once.
fn facing_of(yaw: f32) -> &'static str {
    let y = yaw.rem_euclid(360.0);
    match ((y + 45.0) / 90.0).floor() as i32 % 4 {
        0 => "south",
        1 => "west",
        2 => "north",
        _ => "east",
    }
}

/// The opposite of a horizontal direction.
fn opposite(d: &str) -> &'static str {
    match d {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        _ => "east",
    }
}

/// The direction a clicked face points.
fn face_name(face: u8) -> &'static str {
    match face {
        0 => "down",
        1 => "up",
        2 => "north",
        3 => "south",
        4 => "west",
        _ => "east",
    }
}

/// Whether the placement is in the upper half of the cell.
///
/// Clicking the underside of a block puts a slab on top; clicking the top puts
/// it on the bottom; clicking a *side* is decided by where on that side.
fn upper_half(ctx: &Context) -> bool {
    match ctx.face {
        1 => false, // clicked a top face: the new block sits on the bottom
        0 => true,  // clicked an underside: it hangs from the top
        _ => ctx.cursor.1 > 0.5,
    }
}

/// The block that goes in the cell *above*, for anything two blocks tall.
///
/// A door is two blocks that depend on each other, and placing only the lower
/// half — which is what happened until now — leaves a door that renders as half
/// a door and behaves as none of one. Tall grass, large ferns and sunflowers
/// are the same shape of problem.
///
/// Data-driven: a block is two tall exactly when it declares a `half` with an
/// `upper` value. No list.
pub fn upper_half_of(state: BlockStateId) -> Option<BlockStateId> {
    let (id, _) = blocks::block_of_state(state)?;
    let two_tall = props::schema_of(id).any(|(n, vals)| {
        n == "half"
            && vals
                .iter()
                .any(|v| props::VALUE_STRINGS[*v as usize] == "upper")
    });
    if !two_tall {
        return None;
    }
    // Keep every other property — a door's upper half has to agree with its
    // lower on facing and hinge or the two render as different doors.
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    let mut vals = props::values_of(id, state.raw() - lo);
    for (k, v) in vals.iter_mut() {
        if *k == "half" {
            *v = "upper";
        }
    }
    props::state_with(id, &vals).map(BlockStateId)
}

/// The state `block`'s placement should produce.
///
/// `block` is any state of the block being placed — normally its default.
/// Falls back to that state whenever a rule does not apply, so a block with no
/// state-carrying properties passes through untouched.
pub fn state_for(block: BlockStateId, ctx: &Context) -> BlockStateId {
    let Some((id, _)) = blocks::block_of_state(block) else {
        return block;
    };
    let has = |name: &str| props::schema_of(id).any(|(n, _)| n == name);
    let accepts = |name: &str, value: &str| {
        props::schema_of(id).any(|(n, vals)| {
            n == name
                && vals.iter().any(|v| {
                    props::VALUE_STRINGS
                        [props::VALUES[*v as usize..].first().copied().unwrap_or(0) as usize]
                        == value
                })
        })
    };
    let _ = accepts;

    let mut chosen: Vec<(&str, &str)> = Vec::new();

    // What it is attached to. Buttons, levers and grindstones declare a
    // `face`, and it is decided by which surface was clicked, not by where the
    // player is looking: a button on the underside of a block is a ceiling
    // button however you were standing.
    let attached_to_wall = has("face") && ctx.face >= 2;
    if has("face") {
        chosen.push((
            "face",
            match ctx.face {
                1 => "floor",
                0 => "ceiling",
                _ => "wall",
            },
        ));
    }

    // Which way it points.
    if has("facing") {
        let want = if props::schema_of(id).any(|(n, vals)| n == "facing" && vals.len() > 4) {
            // Six-way facing (pistons, observers, droppers, end rods): vanilla
            // uses the direction the player is *looking*, not the face — which
            // is why a piston placed while looking down points down even when
            // you clicked the side of a block. Pitch is the whole difference.
            opposite_full(looking(ctx.yaw, ctx.pitch))
        } else if faces_the_players_own_way(id) {
            // Not every horizontal block faces the player. Read out of the
            // 1.21.11 server's own bytecode:
            //
            //   StairBlock, DoorBlock, FenceGateBlock -> getHorizontalDirection()
            //   ChestBlock, BedBlock, AbstractFurnaceBlock,
            //   DiodeBlock, TrapDoorBlock              -> ...getOpposite()
            //   AnvilBlock                             -> ...getClockWise()
            //
            // Nothing in the block's data says which family it is in, so this
            // is the one place a name-based list is unavoidable. It is short,
            // and every entry was checked against the game rather than
            // remembered. (Anvils are still wrong: they want a third rule.)
            facing_of(ctx.yaw)
        } else if attached_to_wall {
            // On a wall, the only direction that makes sense is out of it.
            face_name(ctx.face)
        } else {
            // Four-way: away from the player, which is how stairs and chests
            // read right side up.
            opposite(facing_of(ctx.yaw))
        };
        chosen.push(("facing", want));
    }

    // Which way it lies.
    if has("axis") {
        chosen.push((
            "axis",
            match ctx.face {
                0 | 1 => "y",
                2 | 3 => "z",
                _ => "x",
            },
        ));
    }

    // Which half of the cell it occupies.
    let upper = upper_half(ctx);
    if has("half") {
        // Stairs and trapdoors call it top/bottom; doors call it upper/lower
        // and the *lower* half is always what gets placed.
        chosen.push((
            "half",
            if props::schema_of(id).any(|(n, vals)| {
                n == "half"
                    && vals
                        .iter()
                        .any(|v| props::VALUE_STRINGS[*v as usize] == "lower")
            }) {
                "lower"
            } else if upper {
                "top"
            } else {
                "bottom"
            },
        ));
    }
    if has("type") {
        // Slabs. `double` is what happens when a slab is placed into a
        // matching one, which is a neighbour question and not this one.
        chosen.push(("type", if upper { "top" } else { "bottom" }));
    }

    // Placed into water.
    if ctx.into_water && has("waterlogged") {
        chosen.push(("waterlogged", "true"));
    }

    if chosen.is_empty() {
        return block;
    }
    // Anything the block does not actually accept is dropped rather than
    // failing the placement: a rule that does not fit must not cost the player
    // their block.
    for n in (1..=chosen.len()).rev() {
        if let Some(state) = props::state_with(id, &chosen[..n]) {
            return BlockStateId(state);
        }
    }
    block
}

/// The direction a player at `yaw`/`pitch` is looking, including up and down.
///
/// The threshold is vanilla's: past roughly 60 degrees of pitch the vertical
/// wins over the horizontal.
fn looking(yaw: f32, pitch: f32) -> &'static str {
    if pitch < -60.0 {
        "up"
    } else if pitch > 60.0 {
        "down"
    } else {
        facing_of(yaw)
    }
}

/// Whether this block faces the way the player does, rather than back at them.
///
/// See the citation at the call site. Matched by suffix because that is what
/// vanilla's class hierarchy amounts to here — every `*_stairs` is a
/// `StairBlock` — and `_door` does not catch `_trapdoor`, whose last five
/// characters are `pdoor`.
fn faces_the_players_own_way(block: u16) -> bool {
    let name = blocks::BLOCKS[block as usize].0;
    name.ends_with("_stairs") || name.ends_with("_door") || name.ends_with("_fence_gate")
}

/// The opposite of any of the six directions.
fn opposite_full(d: &str) -> &'static str {
    match d {
        "up" => "down",
        "down" => "up",
        "north" => "south",
        "south" => "north",
        "east" => "west",
        _ => "east",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(yaw: f32, face: u8, cy: f32) -> Context {
        // Pitch level unless a test says otherwise.
        Context {
            yaw,
            pitch: 0.0,
            face,
            cursor: (0.5, cy, 0.5),
            into_water: false,
        }
    }

    fn name(id: BlockStateId) -> String {
        props::state_name(id.raw()).unwrap()
    }

    fn place(block: &str, c: Context) -> String {
        name(state_for(blocks::default_state(block).unwrap(), &c))
    }

    #[test]
    fn yaw_maps_to_the_direction_the_player_faces() {
        // Yaw is 0 at *south* and increases clockwise. Getting this wrong by
        // 90 degrees is the classic, and it looks like working code.
        assert_eq!(facing_of(0.0), "south");
        assert_eq!(facing_of(90.0), "west");
        assert_eq!(facing_of(180.0), "north");
        assert_eq!(facing_of(270.0), "east");
        // ...and it wraps, in both directions.
        assert_eq!(facing_of(360.0), "south");
        assert_eq!(facing_of(-90.0), "east");
        assert_eq!(facing_of(44.0), "south");
        assert_eq!(facing_of(46.0), "west");
    }

    #[test]
    fn a_stair_follows_the_direction_the_player_is_facing() {
        // `StairBlock.getStateForPlacement` calls `getHorizontalDirection()`
        // with no `.getOpposite()` — verified in the 1.21.11 server's own
        // bytecode. This test asserted the opposite until that was checked.
        for (yaw, want) in [
            (0.0, "south"),
            (90.0, "west"),
            (180.0, "north"),
            (270.0, "east"),
        ] {
            let s = place("minecraft:oak_stairs", ctx(yaw, 1, 1.0));
            assert!(s.contains(&format!("facing={want}")), "yaw {yaw}: {s}");
        }
    }

    #[test]
    fn a_stair_clicked_underneath_is_upside_down() {
        assert!(place("minecraft:oak_stairs", ctx(0.0, 1, 1.0)).contains("half=bottom"));
        assert!(place("minecraft:oak_stairs", ctx(0.0, 0, 0.0)).contains("half=top"));
        // Clicking high on a *side* also gives a top stair.
        assert!(place("minecraft:oak_stairs", ctx(0.0, 2, 0.9)).contains("half=top"));
        assert!(place("minecraft:oak_stairs", ctx(0.0, 2, 0.1)).contains("half=bottom"));
    }

    #[test]
    fn a_slab_takes_the_half_that_was_clicked() {
        // The case a live client showed was broken: clicking a bottom face
        // gave a bottom slab.
        assert!(place("minecraft:oak_slab", ctx(0.0, 1, 1.0)).contains("type=bottom"));
        assert!(place("minecraft:oak_slab", ctx(0.0, 0, 0.0)).contains("type=top"));
    }

    #[test]
    fn a_log_lies_along_the_axis_it_was_clicked_on() {
        assert!(place("minecraft:oak_log", ctx(0.0, 1, 1.0)).contains("axis=y"));
        assert!(place("minecraft:oak_log", ctx(0.0, 2, 0.5)).contains("axis=z"));
        assert!(place("minecraft:oak_log", ctx(0.0, 4, 0.5)).contains("axis=x"));
    }

    #[test]
    fn a_door_is_placed_lower_half_first_with_an_upper_to_follow() {
        // `half` means top/bottom on a stair and upper/lower on a door.
        let lower = state_for(
            blocks::default_state("minecraft:oak_door").unwrap(),
            &ctx(0.0, 1, 1.0),
        );
        assert!(name(lower).contains("half=lower"), "{}", name(lower));

        let upper = upper_half_of(lower).expect("a door has an upper half");
        let un = name(upper);
        assert!(un.contains("half=upper"), "{un}");
        // The two halves must agree on everything else or they render as
        // different doors.
        assert!(un.contains("facing=south"), "{un}");
        assert_eq!(
            blocks::block_of_state(upper).map(|(b, _)| b),
            blocks::block_of_state(lower).map(|(b, _)| b)
        );
    }

    #[test]
    fn tall_plants_are_two_blocks_too_and_ordinary_ones_are_not() {
        // Data-driven, so the same rule that makes a door two tall makes a
        // sunflower two tall — with no list to keep in step.
        for tall in [
            "minecraft:sunflower",
            "minecraft:tall_grass",
            "minecraft:large_fern",
        ] {
            let s = blocks::default_state(tall).unwrap();
            assert!(upper_half_of(s).is_some(), "{tall} should be two tall");
        }
        for short in [
            "minecraft:stone",
            "minecraft:oak_stairs",
            "minecraft:oak_slab",
        ] {
            let s = blocks::default_state(short).unwrap();
            assert!(upper_half_of(s).is_none(), "{short} is one block");
        }
    }

    #[test]
    fn stairs_and_doors_face_the_players_own_way_and_chests_face_back() {
        // Checked against the 1.21.11 server's bytecode, not remembered:
        // StairBlock and DoorBlock call getHorizontalDirection(), ChestBlock
        // and AbstractFurnaceBlock call .getOpposite() on it. Getting this
        // backwards makes every staircase run the wrong way.
        let c = ctx(0.0, 1, 1.0); // yaw 0 is south
        for direct in [
            "minecraft:oak_stairs",
            "minecraft:oak_door",
            "minecraft:oak_fence_gate",
        ] {
            let s = place(direct, c);
            assert!(s.contains("facing=south"), "{direct}: {s}");
        }
        for opposite in ["minecraft:chest", "minecraft:furnace"] {
            let s = place(opposite, c);
            assert!(s.contains("facing=north"), "{opposite}: {s}");
        }
    }

    #[test]
    fn placing_into_water_waterlogs_what_can_be_waterlogged() {
        let mut c = ctx(0.0, 1, 1.0);
        c.into_water = true;
        assert!(place("minecraft:oak_stairs", c).contains("waterlogged=true"));
        assert!(place("minecraft:oak_slab", c).contains("waterlogged=true"));
        // A block with no such property is unaffected rather than refused.
        assert_eq!(place("minecraft:stone", c), "minecraft:stone");
    }

    #[test]
    fn a_six_way_block_follows_where_the_player_looks_including_up_and_down() {
        // The difference between four-way and six-way facing, and the reason
        // `pitch` is in the context at all: a piston placed while looking
        // down points down, whatever face was clicked.
        let mut c = ctx(0.0, 4, 0.5);
        assert!(place("minecraft:piston", c).contains("facing=north"));
        c.pitch = 80.0;
        assert!(
            place("minecraft:piston", c).contains("facing=up"),
            "looking down"
        );
        c.pitch = -80.0;
        assert!(
            place("minecraft:piston", c).contains("facing=down"),
            "looking up"
        );
    }

    #[test]
    fn a_button_is_attached_to_the_surface_that_was_clicked() {
        // Not to where the player was looking: a button on the underside of a
        // block is a ceiling button however you were standing.
        assert!(place("minecraft:stone_button", ctx(0.0, 1, 1.0)).contains("face=floor"));
        assert!(place("minecraft:stone_button", ctx(0.0, 0, 0.0)).contains("face=ceiling"));
        let wall = place("minecraft:stone_button", ctx(0.0, 5, 0.5));
        assert!(wall.contains("face=wall"), "{wall}");
        // ...and on a wall it points out of it, not at the player.
        assert!(wall.contains("facing=east"), "{wall}");
    }

    #[test]
    fn a_block_with_no_state_properties_passes_through_untouched() {
        for plain in ["minecraft:stone", "minecraft:dirt", "minecraft:bedrock"] {
            assert_eq!(place(plain, ctx(123.0, 3, 0.7)), plain);
        }
    }

    #[test]
    fn every_block_still_places_as_something_real() {
        // The rule that matters most: a rule that does not fit must never cost
        // a player their block. Across the whole set, from every face, the
        // result is always a valid state of the block asked for.
        for (i, row) in blocks::BLOCKS.iter().enumerate() {
            let def = BlockStateId(blocks::DEFAULT_STATE[i]);
            for face in 0..6u8 {
                let got = state_for(def, &ctx(37.0, face, 0.8));
                assert_eq!(
                    blocks::block_of_state(got).map(|(b, _)| b),
                    Some(i as u16),
                    "{} turned into a different block from face {face}",
                    row.0
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Interaction
// ---------------------------------------------------------------------------

/// What right-clicking a block does, if anything.
///
/// Vanilla checks this **before** placement: clicking a door opens it rather
/// than putting a block against it. Our server placed unconditionally, so a
/// door appeared to open — the client predicts it — and then snapped shut when
/// the server's block update and ack arrived, with the held block sitting in
/// front of it. That is the whole of the reported bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interaction {
    /// Replace the clicked block with this state.
    Toggle(BlockStateId),
}

/// The `open` property, flipped.
///
/// Rebuilt from the state's *own* properties rather than from the block's
/// default. `state_with` fills anything unnamed from the default, so passing it
/// only `open` would quietly reset facing and hinge — and the door would swing
/// round to face north as it opened.
fn flipped_open(state: BlockStateId) -> Option<BlockStateId> {
    let (id, _) = blocks::block_of_state(state)?;
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    let mut vals = props::values_of(id, state.raw() - lo);
    let mut found = false;
    for (k, v) in vals.iter_mut() {
        if *k == "open" {
            *v = if *v == "true" { "false" } else { "true" };
            found = true;
        }
    }
    if !found {
        return None;
    }
    props::state_with(id, &vals).map(BlockStateId)
}

/// What clicking `state` should do, or `None` to fall through to placing.
///
/// Data-driven, like the rest of this module: a block is openable exactly when
/// it declares an `open` property. That is doors, trapdoors and fence gates —
/// and any modded block that works the same way — with no list to keep in step.
///
/// Deliberately narrow. Chests, furnaces and crafting tables are also
/// right-clickable and are **not** handled: they open a screen, which is a
/// different mechanism, and silently swallowing the click would replace one
/// confusing behaviour with another. They still place a block, as before.
pub fn interact(state: BlockStateId, sneaking: bool) -> Option<Interaction> {
    // Sneaking is how a player says "place against this, do not use it" —
    // without it a door can never be built next to another door.
    if sneaking {
        return None;
    }
    flipped_open(state).map(Interaction::Toggle)
}

/// The other half of a two-tall block, if `state` is one.
///
/// Returns the cell offset and the state that belongs there. A door's halves
/// must agree on `open` or the two render as different doors and the top half
/// stays shut.
pub fn other_half(state: BlockStateId) -> Option<(i32, BlockStateId)> {
    let (id, _) = blocks::block_of_state(state)?;
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    let vals = props::values_of(id, state.raw() - lo);
    let half = vals.iter().find(|(k, _)| *k == "half")?.1;
    let (dy, want) = match half {
        "lower" => (1, "upper"),
        "upper" => (-1, "lower"),
        _ => return None,
    };
    let mut other = vals.clone();
    for (k, v) in other.iter_mut() {
        if *k == "half" {
            *v = want;
        }
    }
    props::state_with(id, &other).map(|s| (dy, BlockStateId(s)))
}

#[cfg(test)]
mod interaction_tests {
    use super::*;

    fn state(name: &str) -> BlockStateId {
        blocks::default_state(name).unwrap()
    }
    fn name(id: BlockStateId) -> String {
        props::state_name(id.raw()).unwrap()
    }

    #[test]
    fn clicking_a_door_opens_it_instead_of_placing() {
        let shut = state("minecraft:oak_door");
        assert!(name(shut).contains("open=false"));
        let Some(Interaction::Toggle(open)) = interact(shut, false) else {
            panic!("a door must be openable");
        };
        assert!(name(open).contains("open=true"), "{}", name(open));
        // ...and clicking again shuts it.
        let Some(Interaction::Toggle(again)) = interact(open, false) else {
            panic!("and closable")
        };
        assert_eq!(again, shut);
    }

    #[test]
    fn trapdoors_and_fence_gates_open_too_and_by_the_same_rule() {
        for openable in [
            "minecraft:oak_trapdoor",
            "minecraft:oak_fence_gate",
            "minecraft:iron_door",
        ] {
            assert!(
                interact(state(openable), false).is_some(),
                "{openable} should open"
            );
        }
    }

    #[test]
    fn a_plain_block_falls_through_to_being_built_against() {
        for plain in ["minecraft:stone", "minecraft:oak_stairs", "minecraft:chest"] {
            assert_eq!(interact(state(plain), false), None, "{plain}");
        }
    }

    #[test]
    fn sneaking_builds_against_a_door_rather_than_opening_it() {
        // Without this a player can never put a block against a door, or a
        // second door beside the first.
        assert_eq!(interact(state("minecraft:oak_door"), true), None);
    }

    #[test]
    fn a_doors_two_halves_point_at_each_other() {
        let lower = state("minecraft:oak_door");
        let (dy, upper) = other_half(lower).expect("a door has two halves");
        assert_eq!(dy, 1, "the other half of a lower is above it");
        assert!(name(upper).contains("half=upper"));

        let (dy_back, back) = other_half(upper).expect("and back again");
        assert_eq!(dy_back, -1);
        assert_eq!(back, lower, "the round trip is the identity");
    }

    #[test]
    fn opening_a_door_keeps_everything_but_the_open_flag() {
        // If the halves disagree on facing or hinge they render as two
        // different doors, and the top stays shut while the bottom swings.
        let block = blocks::block_id_of("minecraft:oak_door").unwrap();
        let east = BlockStateId(
            props::state_with(block, &[("facing", "east"), ("hinge", "right")]).unwrap(),
        );
        let Some(Interaction::Toggle(open)) = interact(east, false) else {
            panic!()
        };
        let n = name(open);
        assert!(
            n.contains("facing=east") && n.contains("hinge=right"),
            "{n}"
        );
    }

    #[test]
    fn a_one_block_tall_thing_has_no_other_half() {
        for short in ["minecraft:stone", "minecraft:oak_trapdoor"] {
            assert_eq!(other_half(state(short)), None, "{short}");
        }
    }
}

// ---------------------------------------------------------------------------
// Shapes that depend on a neighbour
// ---------------------------------------------------------------------------

/// Read a block by absolute coordinate.
pub type Neighbours<'a> = dyn Fn(i32, i32, i32) -> BlockStateId + 'a;

/// One property of a state, by name.
fn prop_of(state: BlockStateId, key: &str) -> Option<&'static str> {
    let (id, _) = blocks::block_of_state(state)?;
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    props::values_of(id, state.raw() - lo)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Whether two horizontal directions share an axis.
fn same_axis(a: &str, b: &str) -> bool {
    let axis = |d: &str| matches!(d, "north" | "south");
    axis(a) == axis(b)
}

/// The direction 90 degrees anticlockwise, looking down.
fn counter_clockwise(d: &str) -> &'static str {
    match d {
        "north" => "west",
        "west" => "south",
        "south" => "east",
        _ => "north",
    }
}

/// The unit offset of a horizontal direction.
fn step(d: &str) -> (i32, i32) {
    match d {
        "north" => (0, -1),
        "south" => (0, 1),
        "west" => (-1, 0),
        _ => (1, 0),
    }
}

/// Whether this state is a stair.
fn is_stairs(state: BlockStateId) -> bool {
    blocks::block_of_state(state).is_some_and(|(_, n)| n.ends_with("_stairs"))
}

/// Vanilla's `canTakeShape`: a corner is only formed when the block on the
/// other side is *not* a stair of the same facing and half.
///
/// Without it, a solid run of stairs would turn every one of them into a
/// corner.
fn can_take_shape(
    facing: &str,
    half: &str,
    x: i32,
    y: i32,
    z: i32,
    dir: &str,
    get: &Neighbours<'_>,
) -> bool {
    let (dx, dz) = step(dir);
    let other = get(x + dx, y, z + dz);
    !(is_stairs(other)
        && prop_of(other, "facing") == Some(facing)
        && prop_of(other, "half") == Some(half))
}

/// Give a placed stair its corner shape.
///
/// Transcribed from `StairBlock`'s own bytecode in the 1.21.11 server rather
/// than from memory — the four corner cases differ only in which of `left` and
/// `right` they name, and getting that backwards produces staircases that look
/// almost right:
///
/// ```text
/// front = block in the direction the stair faces
///   stair, same half, crossing axis, and the far side is clear
///     -> front faces anticlockwise of us ? OUTER_LEFT : OUTER_RIGHT
/// back  = block behind it
///   same conditions -> back faces anticlockwise of us ? INNER_LEFT : INNER_RIGHT
/// otherwise STRAIGHT
/// ```
///
/// Computed at placement, which is where vanilla computes it too, and again
/// whenever a neighbour changes — see [`neighbour_updates`].
pub fn stair_shape(
    state: BlockStateId,
    x: i32,
    y: i32,
    z: i32,
    get: &Neighbours<'_>,
) -> BlockStateId {
    if !is_stairs(state) {
        return state;
    }
    let (Some(facing), Some(half)) = (prop_of(state, "facing"), prop_of(state, "half")) else {
        return state;
    };
    let ccw = counter_clockwise(facing);

    let shape = {
        let (fx, fz) = step(facing);
        let front = get(x + fx, y, z + fz);
        let front_facing = prop_of(front, "facing");
        let corner = is_stairs(front)
            && prop_of(front, "half") == Some(half)
            && front_facing.is_some_and(|f| !same_axis(f, facing))
            && front_facing
                .is_some_and(|f| can_take_shape(facing, half, x, y, z, opposite(f), get));
        if corner {
            if front_facing == Some(ccw) {
                Some("outer_left")
            } else {
                Some("outer_right")
            }
        } else {
            let back_dir = opposite(facing);
            let (bx, bz) = step(back_dir);
            let back = get(x + bx, y, z + bz);
            let back_facing = prop_of(back, "facing");
            let corner = is_stairs(back)
                && prop_of(back, "half") == Some(half)
                && back_facing.is_some_and(|f| !same_axis(f, facing))
                && back_facing.is_some_and(|f| can_take_shape(facing, half, x, y, z, f, get));
            if corner {
                if back_facing == Some(ccw) {
                    Some("inner_left")
                } else {
                    Some("inner_right")
                }
            } else {
                None
            }
        }
    };

    let Some(shape) = shape else { return state };
    let (id, _) = blocks::block_of_state(state).expect("checked above");
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    let mut vals = props::values_of(id, state.raw() - lo);
    for (k, v) in vals.iter_mut() {
        if *k == "shape" {
            *v = shape;
        }
    }
    props::state_with(id, &vals)
        .map(BlockStateId)
        .unwrap_or(state)
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use std::collections::HashMap;

    struct World(HashMap<(i32, i32, i32), BlockStateId>);
    impl World {
        fn get(&self) -> impl Fn(i32, i32, i32) -> BlockStateId + '_ {
            move |x, y, z| {
                self.0
                    .get(&(x, y, z))
                    .copied()
                    .unwrap_or(blocks::default_state("minecraft:air").unwrap())
            }
        }
    }

    fn stair(facing: &str, half: &str) -> BlockStateId {
        let b = blocks::block_id_of("minecraft:oak_stairs").unwrap();
        BlockStateId(props::state_with(b, &[("facing", facing), ("half", half)]).unwrap())
    }

    fn shape_of(id: BlockStateId) -> &'static str {
        prop_of(id, "shape").unwrap()
    }

    #[test]
    fn a_lone_stair_is_straight() {
        let w = World(HashMap::new());
        let s = stair("north", "bottom");
        assert_eq!(shape_of(stair_shape(s, 0, 0, 0, &w.get())), "straight");
    }

    #[test]
    fn a_run_of_stairs_stays_straight() {
        // The case `canTakeShape` exists for: without it every stair in a
        // staircase becomes a corner.
        let mut m = HashMap::new();
        for z in -2..=2 {
            m.insert((0, 0, z), stair("north", "bottom"));
        }
        let w = World(m);
        for z in -1..=1 {
            assert_eq!(
                shape_of(stair_shape(stair("north", "bottom"), 0, 0, z, &w.get())),
                "straight",
                "at z={z}"
            );
        }
    }

    #[test]
    fn a_stair_meeting_a_crossing_one_in_front_makes_an_outer_corner() {
        let mut m = HashMap::new();
        // We face north (towards -z); the one in front faces west.
        m.insert((0, 0, -1), stair("west", "bottom"));
        let w = World(m);
        let got = shape_of(stair_shape(stair("north", "bottom"), 0, 0, 0, &w.get()));
        // West is anticlockwise of north.
        assert_eq!(counter_clockwise("north"), "west");
        assert_eq!(got, "outer_left");

        let mut m = HashMap::new();
        m.insert((0, 0, -1), stair("east", "bottom"));
        let w = World(m);
        assert_eq!(
            shape_of(stair_shape(stair("north", "bottom"), 0, 0, 0, &w.get())),
            "outer_right"
        );
    }

    #[test]
    fn a_stair_meeting_a_crossing_one_behind_makes_an_inner_corner() {
        let mut m = HashMap::new();
        m.insert((0, 0, 1), stair("west", "bottom")); // behind a north-facing stair
        let w = World(m);
        assert_eq!(
            shape_of(stair_shape(stair("north", "bottom"), 0, 0, 0, &w.get())),
            "inner_left"
        );

        let mut m = HashMap::new();
        m.insert((0, 0, 1), stair("east", "bottom"));
        let w = World(m);
        assert_eq!(
            shape_of(stair_shape(stair("north", "bottom"), 0, 0, 0, &w.get())),
            "inner_right"
        );
    }

    #[test]
    fn corners_only_form_between_stairs_of_the_same_half() {
        // An upside-down stair beside an upright one is not a corner, it is
        // two stairs.
        let mut m = HashMap::new();
        m.insert((0, 0, -1), stair("west", "top"));
        let w = World(m);
        assert_eq!(
            shape_of(stair_shape(stair("north", "bottom"), 0, 0, 0, &w.get())),
            "straight"
        );
    }

    #[test]
    fn upside_down_stairs_form_corners_among_themselves() {
        let mut m = HashMap::new();
        m.insert((0, 0, -1), stair("west", "top"));
        let w = World(m);
        let got = stair_shape(stair("north", "top"), 0, 0, 0, &w.get());
        assert_eq!(shape_of(got), "outer_left");
        assert_eq!(prop_of(got, "half"), Some("top"), "the half is untouched");
    }

    #[test]
    fn a_stair_facing_the_same_axis_is_not_a_corner() {
        // Two stairs nose to nose are a straight pair, not a corner.
        let mut m = HashMap::new();
        m.insert((0, 0, -1), stair("south", "bottom"));
        let w = World(m);
        assert_eq!(
            shape_of(stair_shape(stair("north", "bottom"), 0, 0, 0, &w.get())),
            "straight"
        );
    }

    #[test]
    fn anything_that_is_not_a_stair_is_returned_untouched() {
        let w = World(HashMap::new());
        let stone = blocks::default_state("minecraft:stone").unwrap();
        assert_eq!(stair_shape(stone, 0, 0, 0, &w.get()), stone);
    }
}

// ---------------------------------------------------------------------------
// The block-update pass
// ---------------------------------------------------------------------------
//
// Vanilla settles a shape twice: once when the block is placed, and again
// every time one of its neighbours changes. Only the first half of that was
// here, so a fence built *towards* an existing fence connected, and a fence
// built *away* from one did not — the older block kept the shape it was born
// with. [`neighbour_updates`] is the second half.
//
// What a shape is allowed to depend on is deliberately narrow: the six
// touching cells, plus the cell above for a wall post. Anything wider (a
// redstone graph, a piston line) is a propagation problem rather than a
// reshape, and is not attempted here.

/// The name of the block a state belongs to.
fn block_name(state: BlockStateId) -> &'static str {
    blocks::block_of_state(state).map(|(_, n)| n).unwrap_or("")
}

/// Rewrite some properties of a state, keeping every other one.
///
/// [`props::state_with`] starts from the block's *default* state, so calling
/// it with only the changed keys would quietly reset everything unnamed —
/// which is how an opened door once swung round to face north. Start from the
/// state's own values instead.
fn with_props(state: BlockStateId, changes: &[(&str, &str)]) -> BlockStateId {
    let Some((id, _)) = blocks::block_of_state(state) else {
        return state;
    };
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    let mut vals = props::values_of(id, state.raw() - lo);
    for (k, v) in changes {
        if let Some(slot) = vals.iter_mut().find(|(n, _)| n == k) {
            slot.1 = v;
        }
    }
    props::state_with(id, &vals)
        .map(BlockStateId)
        .unwrap_or(state)
}

fn is_fence(name: &str) -> bool {
    name.ends_with("_fence")
}

fn is_fence_gate(name: &str) -> bool {
    name.ends_with("_fence_gate")
}

fn is_wall(name: &str) -> bool {
    name.ends_with("_wall") && !name.ends_with("_wall_sign") && !name.ends_with("_wall_torch")
}

/// Panes and bars share one connection rule and one property set.
fn is_pane(name: &str) -> bool {
    name.ends_with("_pane") || name == "minecraft:iron_bars"
}

/// Which fences may connect to each other.
///
/// Vanilla splits them by block tag: every wooden fence is in `#fences` *and*
/// `#wooden_fences`, and `nether_brick_fence` is in the first only. Two fences
/// connect when they are in the same one of those two groups.
fn fence_group(name: &str) -> u8 {
    if name == "minecraft:nether_brick_fence" {
        1
    } else {
        0
    }
}

/// Whether this block offers a full, sturdy square face to lean against.
///
/// Vanilla asks the block's collision shape whether the whole face is covered.
/// We have per-block properties but not per-state shapes (see
/// `KNOWN_ISSUES` A.6), so this is a name-shaped approximation: solid, has
/// collision, and is not one of the block families that are known to be
/// partial. It errs towards *not* connecting, which reads as a missing
/// connection rather than a fence fused to a torch.
fn full_face(state: BlockStateId) -> bool {
    let name = block_name(state);
    if name.is_empty() || name == "minecraft:air" {
        return false;
    }
    const PARTIAL: [&str; 14] = [
        "_stairs",
        "_slab",
        "_fence",
        "_fence_gate",
        "_wall",
        "_pane",
        "_door",
        "_trapdoor",
        "_button",
        "_pressure_plate",
        "_sign",
        "_carpet",
        "_bed",
        "_candle",
    ];
    if PARTIAL.iter().any(|s| name.ends_with(s)) {
        return false;
    }
    blocks::props_of_state(state).is_some_and(|p| p.solid && p.collision)
}

/// Whether a fence at `name` connects to `other` in direction `dir`.
fn fence_connects(name: &str, other: BlockStateId, dir: &str) -> bool {
    let on = block_name(other);
    if is_fence(on) {
        return fence_group(on) == fence_group(name);
    }
    if is_fence_gate(on) {
        // `FenceBlock.connectsTo`: the gate's facing must be on the axis
        // *across* the connection — a gate faces along the fence line it
        // interrupts, so it presents its side to us.
        return prop_of(other, "facing").is_some_and(|f| !same_axis(f, dir));
    }
    full_face(other)
}

/// Whether a pane or bars connects to `other`.
fn pane_connects(other: BlockStateId) -> bool {
    let on = block_name(other);
    is_pane(on) || is_wall(on) || full_face(other)
}

/// Whether a wall connects to `other` in direction `dir`.
fn wall_connects(other: BlockStateId, dir: &str) -> bool {
    let on = block_name(other);
    if is_wall(on) || is_pane(on) {
        return true;
    }
    if is_fence_gate(on) {
        return prop_of(other, "facing").is_some_and(|f| !same_axis(f, dir));
    }
    full_face(other)
}

/// The four horizontal directions, in the order the properties are named.
const HORIZONTAL: [&str; 4] = ["north", "east", "south", "west"];

/// Recompute every shape property of the block at `(x, y, z)` from its
/// neighbours, returning the state it should now be in.
///
/// Returns the state unchanged for blocks whose shape is not a function of
/// their neighbours, which is nearly all of them.
pub fn reshape(state: BlockStateId, x: i32, y: i32, z: i32, get: &Neighbours<'_>) -> BlockStateId {
    let name = block_name(state);
    if is_stairs(state) {
        return stair_shape(state, x, y, z, get);
    }
    if is_fence(name) || is_pane(name) {
        let mut changes: Vec<(&str, &str)> = Vec::with_capacity(4);
        for dir in HORIZONTAL {
            let (dx, dz) = step(dir);
            let other = get(x + dx, y, z + dz);
            let on = if is_fence(name) {
                fence_connects(name, other, dir)
            } else {
                pane_connects(other)
            };
            changes.push((dir, if on { "true" } else { "false" }));
        }
        return with_props(state, &changes);
    }
    if is_wall(name) {
        let mut sides: [bool; 4] = [false; 4];
        let mut changes: Vec<(&str, &str)> = Vec::with_capacity(5);
        for (i, dir) in HORIZONTAL.iter().enumerate() {
            let (dx, dz) = step(dir);
            sides[i] = wall_connects(get(x + dx, y, z + dz), dir);
        }
        // `tall` when something sits on top of the wall for the run to reach
        // up to; `low` otherwise. Vanilla derives this from the collision
        // shape of the block above, which we do not have per state.
        let above = get(x, y + 1, z);
        let raised = full_face(above) || is_wall(block_name(above));
        let side = |on: bool| {
            if !on {
                "none"
            } else if raised {
                "tall"
            } else {
                "low"
            }
        };
        for (i, dir) in HORIZONTAL.iter().enumerate() {
            changes.push((dir, side(sides[i])));
        }
        // The centre post is dropped only when the wall is a straight run
        // through and nothing above needs supporting.
        let straight = (sides[0] && sides[2] && !sides[1] && !sides[3])
            || (sides[1] && sides[3] && !sides[0] && !sides[2]);
        let up = !straight || raised;
        changes.push(("up", if up { "true" } else { "false" }));
        return with_props(state, &changes);
    }
    state
}

/// The reshapes a change at `(x, y, z)` forces on the blocks around it.
///
/// Only cells whose state actually changes are returned, so the caller can
/// broadcast exactly what moved. The changed cell itself is *not* included:
/// its own shape was settled when it was placed.
pub fn neighbour_updates(
    x: i32,
    y: i32,
    z: i32,
    get: &Neighbours<'_>,
) -> Vec<(i32, i32, i32, BlockStateId)> {
    const AROUND: [(i32, i32, i32); 6] = [
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, -1),
        (1, 0, 0),
        (0, 0, 1),
        (-1, 0, 0),
    ];
    let mut out = Vec::new();
    for (dx, dy, dz) in AROUND {
        let (nx, ny, nz) = (x + dx, y + dy, z + dz);
        let was = get(nx, ny, nz);
        let now = reshape(was, nx, ny, nz, get);
        if now != was {
            out.push((nx, ny, nz, now));
        }
    }
    out
}

#[cfg(test)]
mod neighbour_tests {
    use super::*;
    use std::collections::HashMap;

    struct W(HashMap<(i32, i32, i32), BlockStateId>);
    impl W {
        fn get(&self) -> impl Fn(i32, i32, i32) -> BlockStateId + '_ {
            move |x, y, z| {
                self.0
                    .get(&(x, y, z))
                    .copied()
                    .unwrap_or(blocks::default_state("minecraft:air").unwrap())
            }
        }
    }

    fn b(name: &str) -> BlockStateId {
        blocks::default_state(name).unwrap()
    }

    #[test]
    fn a_fence_connects_towards_a_neighbour_placed_later() {
        // The whole point of the pass: the *older* block has to change.
        let mut m = HashMap::new();
        m.insert((0, 0, 0), b("minecraft:oak_fence"));
        m.insert((1, 0, 0), b("minecraft:oak_fence"));
        let w = W(m);
        let ups = neighbour_updates(1, 0, 0, &w.get());
        let (_, _, _, s) = ups
            .iter()
            .find(|(x, _, _, _)| *x == 0)
            .expect("the fence at x=0 must be reshaped");
        assert_eq!(prop_of(*s, "east"), Some("true"));
        assert_eq!(prop_of(*s, "west"), Some("false"));
    }

    #[test]
    fn breaking_a_fence_disconnects_the_one_beside_it() {
        let mut m = HashMap::new();
        m.insert(
            (0, 0, 0),
            with_props(b("minecraft:oak_fence"), &[("east", "true")]),
        );
        // x=1 is now air — the neighbour was broken.
        let w = W(m);
        let ups = neighbour_updates(1, 0, 0, &w.get());
        let (_, _, _, s) = ups.iter().find(|(x, _, _, _)| *x == 0).expect("reshaped");
        assert_eq!(prop_of(*s, "east"), Some("false"));
    }

    #[test]
    fn wooden_and_nether_brick_fences_do_not_connect() {
        let mut m = HashMap::new();
        m.insert((0, 0, 0), b("minecraft:oak_fence"));
        m.insert((1, 0, 0), b("minecraft:nether_brick_fence"));
        let w = W(m);
        assert!(
            neighbour_updates(1, 0, 0, &w.get()).is_empty(),
            "nothing may connect across the two fence groups"
        );
    }

    #[test]
    fn a_fence_connects_to_a_solid_block_but_not_to_a_torch() {
        let mut m = HashMap::new();
        m.insert((0, 0, 0), b("minecraft:oak_fence"));
        m.insert((1, 0, 0), b("minecraft:stone"));
        let w = W(m);
        let ups = neighbour_updates(1, 0, 0, &w.get());
        let (_, _, _, s) = ups.iter().find(|(x, _, _, _)| *x == 0).expect("reshaped");
        assert_eq!(prop_of(*s, "east"), Some("true"));

        let mut m = HashMap::new();
        m.insert((0, 0, 0), b("minecraft:oak_fence"));
        m.insert((1, 0, 0), b("minecraft:torch"));
        let w = W(m);
        assert!(neighbour_updates(1, 0, 0, &w.get()).is_empty());
    }

    #[test]
    fn a_fence_connects_to_a_gate_that_presents_its_side() {
        // A gate facing north/south interrupts a north/south fence line, so
        // it connects along z and not along x.
        let mut m = HashMap::new();
        m.insert((0, 0, 0), b("minecraft:oak_fence"));
        m.insert(
            (0, 0, 1),
            with_props(b("minecraft:oak_fence_gate"), &[("facing", "east")]),
        );
        let w = W(m);
        let ups = neighbour_updates(0, 0, 1, &w.get());
        let (_, _, _, s) = ups.iter().find(|(_, _, z, _)| *z == 0).expect("reshaped");
        assert_eq!(prop_of(*s, "south"), Some("true"));

        let mut m = HashMap::new();
        m.insert((0, 0, 0), b("minecraft:oak_fence"));
        m.insert(
            (0, 0, 1),
            with_props(b("minecraft:oak_fence_gate"), &[("facing", "south")]),
        );
        let w = W(m);
        assert!(
            neighbour_updates(0, 0, 1, &w.get()).is_empty(),
            "a gate facing along the connection is end-on, not side-on"
        );
    }

    #[test]
    fn glass_panes_join_up() {
        let mut m = HashMap::new();
        m.insert((0, 0, 0), b("minecraft:glass_pane"));
        m.insert((0, 0, 1), b("minecraft:glass_pane"));
        let w = W(m);
        let ups = neighbour_updates(0, 0, 1, &w.get());
        let (_, _, _, s) = ups.iter().find(|(_, _, z, _)| *z == 0).expect("reshaped");
        assert_eq!(prop_of(*s, "south"), Some("true"));
        assert_eq!(prop_of(*s, "north"), Some("false"));
    }

    #[test]
    fn a_wall_run_drops_its_post_and_a_corner_keeps_it() {
        let mut m = HashMap::new();
        for z in -1..=1 {
            m.insert((0, 0, z), b("minecraft:cobblestone_wall"));
        }
        let w = W(m);
        let mid = reshape(b("minecraft:cobblestone_wall"), 0, 0, 0, &w.get());
        assert_eq!(prop_of(mid, "north"), Some("low"));
        assert_eq!(prop_of(mid, "south"), Some("low"));
        assert_eq!(
            prop_of(mid, "up"),
            Some("false"),
            "a straight run has no post"
        );

        // Bend it: the same block with one arm going east keeps the post.
        let mut m = HashMap::new();
        m.insert((0, 0, -1), b("minecraft:cobblestone_wall"));
        m.insert((1, 0, 0), b("minecraft:cobblestone_wall"));
        let w = W(m);
        let corner = reshape(b("minecraft:cobblestone_wall"), 0, 0, 0, &w.get());
        assert_eq!(prop_of(corner, "up"), Some("true"));
    }

    #[test]
    fn a_wall_under_a_block_goes_tall() {
        let mut m = HashMap::new();
        m.insert((0, 0, -1), b("minecraft:cobblestone_wall"));
        m.insert((0, 1, 0), b("minecraft:stone"));
        let w = W(m);
        let s = reshape(b("minecraft:cobblestone_wall"), 0, 0, 0, &w.get());
        assert_eq!(prop_of(s, "north"), Some("tall"));
        assert_eq!(prop_of(s, "up"), Some("true"));
    }

    #[test]
    fn a_stair_reshapes_when_the_stair_beside_it_appears() {
        let mut m = HashMap::new();
        let north = with_props(b("minecraft:oak_stairs"), &[("facing", "north")]);
        let west = with_props(b("minecraft:oak_stairs"), &[("facing", "west")]);
        m.insert((0, 0, 0), north);
        m.insert((0, 0, -1), west);
        let w = W(m);
        // The west-facing stair was placed second; the north-facing one below
        // it must become an outer corner.
        let ups = neighbour_updates(0, 0, -1, &w.get());
        let (_, _, _, s) = ups.iter().find(|(_, _, z, _)| *z == 0).expect("reshaped");
        assert_eq!(prop_of(*s, "shape"), Some("outer_left"));
    }

    #[test]
    fn ordinary_blocks_are_never_reshaped() {
        let mut m = HashMap::new();
        m.insert((0, 0, 0), b("minecraft:stone"));
        m.insert((1, 0, 0), b("minecraft:dirt"));
        let w = W(m);
        assert!(neighbour_updates(1, 0, 0, &w.get()).is_empty());
    }
}
