//! More feature types: vegetation patches (moss, clay pools), corals and
//! icebergs. Ported from the game's classes, random draws in order.

use std::sync::Arc;

use aether_world::BlockStateId;

use super::super::blockinfo::{self, prop};
use super::super::providers::IntProvider;
use super::super::rng::Rng;
use super::super::tags::BlockSet;
use super::blocks::{sturdy, StateProvider};
use super::features::{shuffle, states};
use super::level::{Dir, JavaPosSet, Pos};
use super::{place_placed, Ctx, Placed};

/// `vegetation_patch` / `waterlogged_vegetation_patch` config.
#[derive(Debug)]
pub struct VegetationPatch {
    pub waterlogged: bool,
    pub replaceable: BlockSet,
    pub ground: StateProvider,
    pub vegetation: Arc<Placed>,
    pub ceiling: bool,
    pub depth: IntProvider,
    pub extra_bottom: f32,
    pub vertical_range: i32,
    pub vegetation_chance: f32,
    pub xz_radius: IntProvider,
    pub extra_edge: f32,
}

fn is(s: BlockStateId, n: &str) -> bool {
    blockinfo::name(s) == n
}

/// `VegetationPatchFeature.place`.
pub(crate) fn vegetation_patch(ctx: &mut Ctx, c: &VegetationPatch, origin: Pos) -> bool {
    let rx = c.xz_radius.sample(ctx.r) + 1;
    let rz = c.xz_radius.sample(ctx.r) + 1;
    let dir = if c.ceiling { Dir::Up } else { Dir::Down };
    let opp = dir.opposite();
    let mut ground = JavaPosSet::new();
    for dx in -rx..=rx {
        let ex = dx == -rx || dx == rx;
        for dz in -rz..=rz {
            let ez = dz == -rz || dz == rz;
            let edge = ex || ez;
            let corner = ex && ez;
            let side = edge && !corner;
            if corner {
                continue;
            }
            if side && !(c.extra_edge != 0.0 && ctx.r.next_float() <= c.extra_edge) {
                continue;
            }
            let mut p = origin.offset(dx, 0, dz);
            let mut i = 0;
            while ctx.lv.is_air(p) && i < c.vertical_range {
                p = p.rel(dir);
                i += 1;
            }
            let mut i = 0;
            while !ctx.lv.is_air(p) && i < c.vertical_range {
                p = p.rel(opp);
                i += 1;
            }
            let below = p.rel(dir);
            if ctx.lv.is_air(p) && sturdy(ctx.lv.get(below)) {
                let extra = if c.extra_bottom > 0.0 && ctx.r.next_float() < c.extra_bottom {
                    1
                } else {
                    0
                };
                let depth = c.depth.sample(ctx.r) + extra;
                if place_ground(ctx, c, below, dir, depth) {
                    ground.insert(below);
                }
            }
        }
    }
    let ground = if c.waterlogged {
        let mut kept = JavaPosSet::new();
        for p in ground.java_order() {
            let exposed = [Dir::North, Dir::East, Dir::South, Dir::West, Dir::Down]
                .iter()
                .any(|d| !sturdy(ctx.lv.get(p.rel(*d))));
            if !exposed {
                kept.insert(p);
            }
        }
        for p in kept.java_order() {
            ctx.lv.set(p, states().water);
        }
        kept
    } else {
        ground
    };
    for p in ground.java_order() {
        if c.vegetation_chance > 0.0 && ctx.r.next_float() < c.vegetation_chance {
            if c.waterlogged {
                if place_placed(ctx, &c.vegetation, p.rel(opp).below(1), None) {
                    let s = ctx.lv.get(p);
                    if prop(s, "waterlogged") == Some("false") {
                        ctx.lv
                            .set(p, blockinfo::with_prop(s, "waterlogged", "true"));
                    }
                }
            } else {
                place_placed(ctx, &c.vegetation, p.rel(opp), None);
            }
        }
    }
    !ground.is_empty()
}

fn place_ground(ctx: &mut Ctx, c: &VegetationPatch, start: Pos, dir: Dir, depth: i32) -> bool {
    let mut p = start;
    for i in 0..depth {
        let g = c.ground.get(ctx.r, p);
        let cur = ctx.lv.get(p);
        if blockinfo::block_of(g) != blockinfo::block_of(cur) {
            if !c.replaceable.contains(cur) {
                return i != 0;
            }
            ctx.lv.set(p, g);
            p = p.rel(dir);
        }
    }
    true
}

/// Coral shapes.
#[derive(Debug, Clone, Copy)]
pub enum Coral {
    Tree,
    Claw,
    Mushroom,
}

/// Block lists the coral features pick from, in tag order.
#[derive(Debug)]
pub struct CoralBlocks {
    pub blocks: Vec<BlockStateId>,
    pub corals: Vec<BlockStateId>,
    pub wall_corals: Vec<BlockStateId>,
    pub corals_set: BlockSet,
}

fn pick(ctx: &mut Ctx, list: &[BlockStateId]) -> Option<BlockStateId> {
    if list.is_empty() {
        None
    } else {
        Some(list[ctx.r.next_int_bounded(list.len() as i32) as usize])
    }
}

/// `CoralFeature.place`.
pub(crate) fn coral(ctx: &mut Ctx, kind: Coral, b: &CoralBlocks, pos: Pos) -> bool {
    let Some(block) = pick(ctx, &b.blocks) else {
        return false;
    };
    match kind {
        Coral::Tree => {
            let mut p = pos;
            let n = ctx.r.next_int_bounded(3) + 1;
            for _ in 0..n {
                if !coral_block(ctx, b, p, block) {
                    return true;
                }
                p = p.above(1);
            }
            let top = p;
            let arms = ctx.r.next_int_bounded(3) + 2;
            let dirs = shuffle(ctx, &Dir::HORIZONTAL);
            for d in dirs.into_iter().take(arms as usize) {
                let mut q = top.rel(d);
                let len = ctx.r.next_int_bounded(5) + 2;
                let mut run = 0;
                let mut i = 0;
                while i < len && coral_block(ctx, b, q, block) {
                    run += 1;
                    q = q.above(1);
                    if i == 0 || run >= 2 && ctx.r.next_float() < 0.25 {
                        q = q.rel(d);
                        run = 0;
                    }
                    i += 1;
                }
            }
            true
        }
        Coral::Claw => {
            if !coral_block(ctx, b, pos, block) {
                return false;
            }
            let main = Dir::HORIZONTAL[ctx.r.next_int_bounded(4) as usize];
            let n = ctx.r.next_int_bounded(2) + 2;
            let ccw = main.clockwise().opposite();
            let dirs = shuffle(ctx, &[main, main.clockwise(), ccw]);
            for d in dirs.into_iter().take(n as usize) {
                let mut q = pos;
                let first = ctx.r.next_int_bounded(2) + 1;
                q = q.rel(d);
                let (step, len);
                if d == main {
                    step = main;
                    len = ctx.r.next_int_bounded(3) + 2;
                } else {
                    q = q.above(1);
                    let options = [d, Dir::Up];
                    step = options[ctx.r.next_int_bounded(2) as usize];
                    len = ctx.r.next_int_bounded(3) + 3;
                }
                let mut i = 0;
                while i < first && coral_block(ctx, b, q, block) {
                    q = q.rel(step);
                    i += 1;
                }
                q = q.rel(step.opposite()).above(1);
                for _ in 0..len {
                    q = q.rel(main);
                    if !coral_block(ctx, b, q, block) {
                        break;
                    }
                    if ctx.r.next_float() < 0.25 {
                        q = q.above(1);
                    }
                }
            }
            true
        }
        Coral::Mushroom => {
            let a = ctx.r.next_int_bounded(3) + 3;
            let bb = ctx.r.next_int_bounded(3) + 3;
            let c = ctx.r.next_int_bounded(3) + 3;
            let down = ctx.r.next_int_bounded(3) + 1;
            for x in 0..=bb {
                for y in 0..=a {
                    for z in 0..=c {
                        let p = pos.offset(x, y - down, z);
                        let xe = x == 0 || x == bb;
                        let ye = y == 0 || y == a;
                        let ze = z == 0 || z == c;
                        if (!xe || !ye)
                            && (!ze || !ye)
                            && (!xe || !ze)
                            && (xe || ye || ze)
                            && ctx.r.next_float() >= 0.1
                        {
                            coral_block(ctx, b, p, block);
                        }
                    }
                }
            }
            true
        }
    }
}

fn coral_block(ctx: &mut Ctx, b: &CoralBlocks, p: Pos, block: BlockStateId) -> bool {
    let up = p.above(1);
    let s = ctx.lv.get(p);
    if !((is(s, "minecraft:water") || b.corals_set.contains(s))
        && is(ctx.lv.get(up), "minecraft:water"))
    {
        return false;
    }
    ctx.lv.set(p, block);
    if ctx.r.next_float() < 0.25 {
        if let Some(c) = pick(ctx, &b.corals) {
            ctx.lv.set(up, c);
        }
    } else if ctx.r.next_float() < 0.05 {
        let n = ctx.r.next_int_bounded(4) + 1;
        ctx.lv.set(
            up,
            blockinfo::with_prop(states().sea_pickle, "pickles", &n.to_string()),
        );
    }
    for d in Dir::HORIZONTAL {
        if ctx.r.next_float() < 0.2 {
            let q = p.rel(d);
            if is(ctx.lv.get(q), "minecraft:water") {
                if let Some(w) = pick(ctx, &b.wall_corals) {
                    let w = if prop(w, "facing").is_some() {
                        blockinfo::with_prop(w, "facing", d.name())
                    } else {
                        w
                    };
                    ctx.lv.set(q, w);
                }
            }
        }
    }
    true
}

fn iceberg_state(s: BlockStateId) -> bool {
    matches!(
        blockinfo::name(s),
        "minecraft:packed_ice" | "minecraft:snow_block" | "minecraft:blue_ice"
    )
}

fn ceil(f: f32) -> i32 {
    f.ceil() as i32
}

/// `IcebergFeature.place`.
pub(crate) fn iceberg(ctx: &mut Ctx, state: BlockStateId, origin: Pos) -> bool {
    use std::f64::consts::PI;
    let o = Pos::new(origin.x, ctx.lv.sea_level(), origin.z);
    let snow_on_top = ctx.r.next_double() > 0.7;
    let angle = ctx.r.next_double() * 2.0 * PI;
    let shape_e = 11 - ctx.r.next_int_bounded(5);
    let shape_c = 3 + ctx.r.next_int_bounded(3);
    let ellipse = ctx.r.next_double() > 0.7;
    let mut height = if ellipse {
        ctx.r.next_int_bounded(6) + 6
    } else {
        ctx.r.next_int_bounded(15) + 3
    };
    if !ellipse && ctx.r.next_double() > 0.9 {
        height += ctx.r.next_int_bounded(19) + 7;
    }
    let depth = (height + ctx.r.next_int_bounded(11)).min(18);
    let width = (height + ctx.r.next_int_bounded(7) - ctx.r.next_int_bounded(5)).min(11);
    let r = if ellipse { shape_e } else { 11 };
    for x in -r..r {
        for z in -r..r {
            for y in 0..height {
                let rr = if ellipse {
                    radius_ellipse(y, height, width)
                } else {
                    radius_round(ctx, y, height, width)
                };
                if ellipse || x < rr {
                    ice_block(
                        ctx,
                        o,
                        height,
                        x,
                        y,
                        z,
                        rr,
                        r,
                        ellipse,
                        shape_c,
                        angle,
                        snow_on_top,
                        state,
                    );
                }
            }
        }
    }
    smooth(ctx, o, width, height, ellipse, shape_e);
    for x in -r..r {
        for z in -r..r {
            let mut y = -1;
            while y > -depth {
                let el = if ellipse {
                    ceil(r as f32 * (1.0 - (y as f64).powf(2.0) as f32 / (depth as f32 * 8.0)))
                } else {
                    r
                };
                let rr = radius_steep(ctx, -y, depth, width);
                if x < rr {
                    ice_block(
                        ctx,
                        o,
                        depth,
                        x,
                        y,
                        z,
                        rr,
                        el,
                        ellipse,
                        shape_c,
                        angle,
                        snow_on_top,
                        state,
                    );
                }
                y -= 1;
            }
        }
    }
    let cut = if ellipse {
        ctx.r.next_double() > 0.1
    } else {
        ctx.r.next_double() > 0.7
    };
    if cut {
        cut_out(ctx, width, height, o, ellipse, shape_e, angle, shape_c);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn cut_out(
    ctx: &mut Ctx,
    width: i32,
    height: i32,
    o: Pos,
    ellipse: bool,
    shape_e: i32,
    angle: f64,
    shape_c: i32,
) {
    use std::f64::consts::PI;
    let sx = if ctx.r.next_bool() { -1 } else { 1 };
    let sz = if ctx.r.next_bool() { -1 } else { 1 };
    let mut ox = ctx.r.next_int_bounded((width / 2 - 2).max(1));
    if ctx.r.next_bool() {
        ox = width / 2 + 1 - ctx.r.next_int_bounded((width - width / 2 - 1).max(1));
    }
    let mut oz = ctx.r.next_int_bounded((width / 2 - 2).max(1));
    if ctx.r.next_bool() {
        oz = width / 2 + 1 - ctx.r.next_int_bounded((width - width / 2 - 1).max(1));
    }
    if ellipse {
        ox = ctx.r.next_int_bounded((shape_e - 5).max(1));
        oz = ox;
    }
    let off = (sx * ox, sz * oz);
    let a = if ellipse {
        angle + PI / 2.0
    } else {
        ctx.r.next_double() * 2.0 * PI
    };
    for y in 0..height - 3 {
        let rr = radius_round(ctx, y, height, width);
        carve(ctx, rr, y, o, false, a, off, shape_e, shape_c);
    }
    let mut y = -1;
    loop {
        let lim = -height + ctx.r.next_int_bounded(5);
        if y <= lim {
            break;
        }
        let rr = radius_steep(ctx, -y, height, width);
        carve(ctx, rr, y, o, true, a, off, shape_e, shape_c);
        y -= 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn carve(
    ctx: &mut Ctx,
    r: i32,
    y: i32,
    o: Pos,
    under: bool,
    angle: f64,
    off: (i32, i32),
    shape_e: i32,
    shape_c: i32,
) {
    let a = r + 1 + shape_e / 3;
    let c = (r - 3).min(3) + shape_c / 2 - 1;
    for x in -a..a {
        for z in -a..a {
            let d = signed_ellipse(x, z, off, a, c, angle);
            if d < 0.0 {
                let p = o.offset(x, y, z);
                let s = ctx.lv.get(p);
                if iceberg_state(s) || is(s, "minecraft:snow_block") {
                    if under {
                        ctx.lv.set(p, states().water);
                    } else {
                        ctx.lv.set(p, BlockStateId::AIR);
                        if is(ctx.lv.get(p.above(1)), "minecraft:snow") {
                            ctx.lv.set(p.above(1), BlockStateId::AIR);
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ice_block(
    ctx: &mut Ctx,
    o: Pos,
    height: i32,
    x: i32,
    y: i32,
    z: i32,
    rr: i32,
    el: i32,
    ellipse: bool,
    shape_c: i32,
    angle: f64,
    snow_on_top: bool,
    state: BlockStateId,
) {
    let d = if ellipse {
        let c = {
            let mut c = shape_c;
            if y > 0 && height - y <= 3 {
                c = shape_c - (4 - (height - y));
            }
            c
        };
        signed_ellipse(x, z, (0, 0), el, c, angle)
    } else {
        let f = 10.0 * ctx.r.next_float().clamp(0.2, 0.8) / rr as f32;
        f as f64 + (x as f64).powf(2.0) + (z as f64).powf(2.0) - (rr as f64).powf(2.0)
    };
    if d < 0.0 {
        let p = o.offset(x, y, z);
        let edge = if ellipse {
            -0.5
        } else {
            (-6 - ctx.r.next_int_bounded(3)) as f64
        };
        if d > edge && ctx.r.next_double() > 0.9 {
            return;
        }
        let s = ctx.lv.get(p);
        if blockinfo::is_air(s)
            || is(s, "minecraft:snow_block")
            || is(s, "minecraft:ice")
            || is(s, "minecraft:water")
        {
            let ok = !ellipse || ctx.r.next_double() > 0.05;
            let div = if ellipse { 3 } else { 2 };
            let from_top = height - y;
            let snow = snow_on_top
                && !is(s, "minecraft:water")
                && (from_top as f64)
                    <= ctx.r.next_int_bounded((height / div).max(1)) as f64 + height as f64 * 0.6
                && ok;
            ctx.lv
                .set(p, if snow { states().snow_block } else { state });
        }
    }
}

fn signed_ellipse(x: i32, z: i32, c: (i32, i32), a: i32, b: i32, angle: f64) -> f64 {
    let dx = (x - c.0) as f64;
    let dz = (z - c.1) as f64;
    ((dx * angle.cos() - dz * angle.sin()) / a as f64).powf(2.0)
        + ((dx * angle.sin() + dz * angle.cos()) / b as f64).powf(2.0)
        - 1.0
}

fn radius_round(ctx: &mut Ctx, y: i32, h: i32, w: i32) -> i32 {
    let f = 3.5 - ctx.r.next_float();
    let mut v = (1.0 - (y as f64).powf(2.0) as f32 / (h as f32 * f)) * w as f32;
    if h > 15 + ctx.r.next_int_bounded(5) {
        let yy = if y < 3 + ctx.r.next_int_bounded(6) {
            y / 2
        } else {
            y
        };
        v = (1.0 - yy as f32 / (h as f32 * f * 0.4)) * w as f32;
    }
    ceil(v / 2.0)
}

fn radius_ellipse(y: i32, h: i32, w: i32) -> i32 {
    let v = (1.0 - (y as f64).powf(2.0) as f32 / (h as f32 * 1.0)) * w as f32;
    ceil(v / 2.0)
}

fn radius_steep(ctx: &mut Ctx, y: i32, h: i32, w: i32) -> i32 {
    let f = 1.0 + ctx.r.next_float() / 2.0;
    let v = (1.0 - y as f32 / (h as f32 * f)) * w as f32;
    ceil(v / 2.0)
}

fn smooth(ctx: &mut Ctx, o: Pos, width: i32, height: i32, ellipse: bool, shape_e: i32) {
    let r = if ellipse { shape_e } else { width / 2 };
    for x in -r..=r {
        for z in -r..=r {
            for y in 0..=height {
                let p = o.offset(x, y, z);
                let s = ctx.lv.get(p);
                if iceberg_state(s) || is(s, "minecraft:snow") {
                    if ctx.lv.is_air(p.below(1)) {
                        ctx.lv.set(p, BlockStateId::AIR);
                        ctx.lv.set(p.above(1), BlockStateId::AIR);
                    } else if iceberg_state(s) {
                        let n = [Dir::West, Dir::East, Dir::North, Dir::South]
                            .iter()
                            .filter(|d| !iceberg_state(ctx.lv.get(p.rel(**d))))
                            .count();
                        if n >= 3 {
                            ctx.lv.set(p, BlockStateId::AIR);
                        }
                    }
                }
            }
        }
    }
}
