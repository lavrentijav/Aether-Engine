//! Trees: `TreeFeature` with its trunk placers, foliage placers, feature
//! sizes and decorators, plus `FallenTreeFeature`. Ported from the game's
//! classes; the random source is drawn in the same order.

use aether_world::BlockStateId;

use super::super::blockinfo::{self, prop};
use super::super::density::BuildError;
use super::super::json::Json;
use super::super::providers::IntProvider;
use super::super::rng::Rng;
use super::blocks::{solid_render, sturdy, StateProvider};
use super::features::{shuffle, states};
use super::level::{Dir, Heightmap, JavaPosSet, Pos};
use super::{Ctx, Loader};

const PI: f64 = std::f64::consts::PI;

/// `TrunkPlacer` subtypes.
#[derive(Debug)]
pub enum Trunk {
    Straight,
    Forking,
    Fancy,
    DarkOak,
    Giant,
    MegaJungle,
    Bending {
        min_height_for_leaves: i32,
        bend_length: IntProvider,
    },
    Cherry {
        branch_count: IntProvider,
        branch_horizontal_length: IntProvider,
        branch_start: (i32, i32),
        branch_end: IntProvider,
    },
    UpwardsBranching {
        extra_branch_steps: IntProvider,
        place_branch_per_log_probability: f32,
        extra_branch_length: IntProvider,
        can_grow_through: super::super::tags::BlockSet,
    },
}

/// `FoliagePlacer` subtypes.
#[derive(Debug)]
pub enum Foliage {
    Blob(i32),
    Fancy(i32),
    Bush(i32),
    Spruce(IntProvider),
    Pine(IntProvider),
    MegaPine(IntProvider),
    Acacia,
    DarkOak,
    MegaJungle(i32),
    RandomSpread {
        height: IntProvider,
        attempts: i32,
    },
    Cherry {
        height: IntProvider,
        wide_bottom_hole: f32,
        corner_hole: f32,
        hanging: f32,
        hanging_ext: f32,
    },
}

/// `FeatureSize`.
#[derive(Debug)]
pub enum Size {
    Two {
        limit: i32,
        lower: i32,
        upper: i32,
        min_clipped: Option<i32>,
    },
    Three {
        limit: i32,
        upper_limit: i32,
        lower: i32,
        middle: i32,
        upper: i32,
        min_clipped: Option<i32>,
    },
}

impl Size {
    fn at(&self, h: i32, y: i32) -> i32 {
        match self {
            Size::Two {
                limit,
                lower,
                upper,
                ..
            } => {
                if y < *limit {
                    *lower
                } else {
                    *upper
                }
            }
            Size::Three {
                limit,
                upper_limit,
                lower,
                middle,
                upper,
                ..
            } => {
                if y < *limit {
                    *lower
                } else if y >= h - upper_limit {
                    *upper
                } else {
                    *middle
                }
            }
        }
    }

    fn min_clipped(&self) -> Option<i32> {
        match self {
            Size::Two { min_clipped, .. } | Size::Three { min_clipped, .. } => *min_clipped,
        }
    }
}

/// `TreeDecorator` subtypes.
#[derive(Debug)]
pub enum Decorator {
    LeaveVine(f32),
    TrunkVine,
    Cocoa(f32),
    Beehive(f32),
    AlterGround(StateProvider),
    PlaceOnGround {
        tries: i32,
        radius: i32,
        height: i32,
        provider: StateProvider,
    },
    AttachedToLeaves {
        probability: f32,
        exclusion_xz: i32,
        exclusion_y: i32,
        provider: StateProvider,
        required_empty: i32,
        directions: Vec<Dir>,
    },
    AttachedToLogs {
        probability: f32,
        provider: StateProvider,
        directions: Vec<Dir>,
    },
    /// A decorator type this port does not place.
    Unsupported(String),
}

/// A `tree` feature's config.
#[derive(Debug)]
pub struct TreeConfig {
    trunk_provider: StateProvider,
    dirt_provider: StateProvider,
    foliage_provider: StateProvider,
    base_height: i32,
    rand_a: i32,
    rand_b: i32,
    trunk: Trunk,
    radius: IntProvider,
    offset: IntProvider,
    foliage: Foliage,
    size: Size,
    decorators: Vec<Decorator>,
    ignore_vines: bool,
    force_dirt: bool,
}

/// A `fallen_tree` feature's config.
#[derive(Debug)]
pub struct FallenTree {
    trunk_provider: StateProvider,
    log_length: IntProvider,
    stump_decorators: Vec<Decorator>,
    log_decorators: Vec<Decorator>,
}

fn get<'j>(j: &'j Json, k: &str) -> Result<&'j Json, BuildError> {
    j.get(k)
        .ok_or_else(|| BuildError::new(format!("missing `{k}`")))
}

fn ty(j: &Json) -> &str {
    j.str_of("type")
        .unwrap_or("")
        .trim_start_matches("minecraft:")
}

fn ip(j: &Json, k: &str) -> Result<IntProvider, BuildError> {
    IntProvider::parse(get(j, k)?)
}

fn dirs(j: &Json) -> Vec<Dir> {
    j.get("directions")
        .and_then(Json::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter_map(|d| d.as_str().and_then(Dir::parse))
        .collect()
}

fn parse_decorators(l: &mut Loader, j: Option<&Json>) -> Result<Vec<Decorator>, BuildError> {
    let mut out = Vec::new();
    for d in j.and_then(Json::as_arr).unwrap_or(&[]) {
        let sp = |k: &str| StateProvider::parse(get(d, k)?, &l.noises);
        out.push(match ty(d) {
            "leave_vine" => Decorator::LeaveVine(d.f64_or("probability", 0.0) as f32),
            "trunk_vine" => Decorator::TrunkVine,
            "cocoa" => Decorator::Cocoa(d.f64_or("probability", 0.0) as f32),
            "beehive" => Decorator::Beehive(d.f64_or("probability", 0.0) as f32),
            "alter_ground" => Decorator::AlterGround(sp("provider")?),
            "place_on_ground" => Decorator::PlaceOnGround {
                tries: d.i32_or("tries", 128),
                radius: d.i32_or("radius", 2),
                height: d.i32_or("height", 1),
                provider: sp("block_state_provider")?,
            },
            "attached_to_leaves" => Decorator::AttachedToLeaves {
                probability: d.f64_or("probability", 0.0) as f32,
                exclusion_xz: d.i32_or("exclusion_radius_xz", 0),
                exclusion_y: d.i32_or("exclusion_radius_y", 0),
                provider: sp("block_provider")?,
                required_empty: d.i32_or("required_empty_blocks", 1),
                directions: dirs(d),
            },
            "attached_to_logs" => Decorator::AttachedToLogs {
                probability: d.f64_or("probability", 0.0) as f32,
                provider: sp("block_provider")?,
                directions: dirs(d),
            },
            other => {
                l.unsupported.insert(format!("tree_decorator:{other}"));
                Decorator::Unsupported(other.to_string())
            }
        });
    }
    Ok(out)
}

/// Parse a `tree` config.
pub(crate) fn parse_tree(l: &mut Loader, c: &Json) -> Result<TreeConfig, BuildError> {
    let tp = get(c, "trunk_placer")?;
    let trunk = match ty(tp) {
        "straight_trunk_placer" => Trunk::Straight,
        "forking_trunk_placer" => Trunk::Forking,
        "fancy_trunk_placer" => Trunk::Fancy,
        "dark_oak_trunk_placer" => Trunk::DarkOak,
        "giant_trunk_placer" => Trunk::Giant,
        "mega_jungle_trunk_placer" => Trunk::MegaJungle,
        "bending_trunk_placer" => Trunk::Bending {
            min_height_for_leaves: tp.i32_or("min_height_for_leaves", 1),
            bend_length: ip(tp, "bend_length")?,
        },
        "cherry_trunk_placer" => {
            let s = get(tp, "branch_start_offset_from_top")?;
            Trunk::Cherry {
                branch_count: ip(tp, "branch_count")?,
                branch_horizontal_length: ip(tp, "branch_horizontal_length")?,
                branch_start: (s.i32_or("min_inclusive", -4), s.i32_or("max_inclusive", -3)),
                branch_end: ip(tp, "branch_end_offset_from_top")?,
            }
        }
        "upwards_branching_trunk_placer" => Trunk::UpwardsBranching {
            extra_branch_steps: ip(tp, "extra_branch_steps")?,
            place_branch_per_log_probability: tp.f64_or("place_branch_per_log_probability", 0.0)
                as f32,
            extra_branch_length: ip(tp, "extra_branch_length")?,
            can_grow_through: l
                .tags
                .holder_set(tp.get("can_grow_through").unwrap_or(&Json::Null)),
        },
        other => return Err(BuildError::new(format!("unknown trunk placer `{other}`"))),
    };
    let fp = get(c, "foliage_placer")?;
    let foliage = match ty(fp) {
        "blob_foliage_placer" => Foliage::Blob(fp.i32_or("height", 3)),
        "fancy_foliage_placer" => Foliage::Fancy(fp.i32_or("height", 4)),
        "bush_foliage_placer" => Foliage::Bush(fp.i32_or("height", 2)),
        "spruce_foliage_placer" => Foliage::Spruce(ip(fp, "trunk_height")?),
        "pine_foliage_placer" => Foliage::Pine(ip(fp, "height")?),
        "mega_pine_foliage_placer" => Foliage::MegaPine(ip(fp, "crown_height")?),
        "acacia_foliage_placer" => Foliage::Acacia,
        "dark_oak_foliage_placer" => Foliage::DarkOak,
        "jungle_foliage_placer" => Foliage::MegaJungle(fp.i32_or("height", 2)),
        "random_spread_foliage_placer" => Foliage::RandomSpread {
            height: ip(fp, "foliage_height")?,
            attempts: fp.i32_or("leaf_placement_attempts", 128),
        },
        "cherry_foliage_placer" => Foliage::Cherry {
            height: ip(fp, "height")?,
            wide_bottom_hole: fp.f64_or("wide_bottom_layer_hole_chance", 0.0) as f32,
            corner_hole: fp.f64_or("corner_hole_chance", 0.0) as f32,
            hanging: fp.f64_or("hanging_leaves_chance", 0.0) as f32,
            hanging_ext: fp.f64_or("hanging_leaves_extension_chance", 0.0) as f32,
        },
        other => return Err(BuildError::new(format!("unknown foliage placer `{other}`"))),
    };
    let ms = get(c, "minimum_size")?;
    let min_clipped = ms
        .get("min_clipped_height")
        .and_then(Json::as_f64)
        .map(|v| v as i32);
    let size = match ty(ms) {
        "three_layers_feature_size" => Size::Three {
            limit: ms.i32_or("limit", 1),
            upper_limit: ms.i32_or("upper_limit", 1),
            lower: ms.i32_or("lower_size", 0),
            middle: ms.i32_or("middle_size", 1),
            upper: ms.i32_or("upper_size", 1),
            min_clipped,
        },
        _ => Size::Two {
            limit: ms.i32_or("limit", 1),
            lower: ms.i32_or("lower_size", 0),
            upper: ms.i32_or("upper_size", 1),
            min_clipped,
        },
    };
    if c.get("root_placer").is_some() {
        l.unsupported
            .insert("root_placer (mangrove roots)".to_string());
    }
    Ok(TreeConfig {
        trunk_provider: StateProvider::parse(get(c, "trunk_provider")?, &l.noises)?,
        dirt_provider: StateProvider::parse(get(c, "dirt_provider")?, &l.noises)?,
        foliage_provider: StateProvider::parse(get(c, "foliage_provider")?, &l.noises)?,
        base_height: tp.i32_or("base_height", 4),
        rand_a: tp.i32_or("height_rand_a", 2),
        rand_b: tp.i32_or("height_rand_b", 0),
        trunk,
        radius: ip(fp, "radius")?,
        offset: ip(fp, "offset")?,
        foliage,
        size,
        decorators: parse_decorators(l, c.get("decorators"))?,
        ignore_vines: c.bool_or("ignore_vines", false),
        force_dirt: c.bool_or("force_dirt", false),
    })
}

/// Parse a `fallen_tree` config.
pub(crate) fn parse_fallen(l: &mut Loader, c: &Json) -> Result<FallenTree, BuildError> {
    Ok(FallenTree {
        trunk_provider: StateProvider::parse(get(c, "trunk_provider")?, &l.noises)?,
        log_length: ip(c, "log_length")?,
        stump_decorators: parse_decorators(l, c.get("stump_decorators"))?,
        log_decorators: parse_decorators(l, c.get("log_decorators"))?,
    })
}

struct Attachment {
    pos: Pos,
    radius_offset: i32,
    double_trunk: bool,
}

/// The sets a tree's placement records, in insertion order.
struct Placement {
    logs: JavaPosSet,
    leaves: JavaPosSet,
    decorations: JavaPosSet,
}

fn is_air_or_leaves(ctx: &Ctx, p: Pos) -> bool {
    let s = ctx.lv.get(p);
    blockinfo::is_air(s) || ctx.tags().leaves.contains(s)
}

/// `TreeFeature.validTreePos`.
fn valid_tree_pos(ctx: &Ctx, p: Pos) -> bool {
    let s = ctx.lv.get(p);
    blockinfo::is_air(s) || ctx.tags().replaceable_by_trees.contains(s)
}

impl TreeConfig {
    fn valid_pos(&self, ctx: &Ctx, p: Pos) -> bool {
        if valid_tree_pos(ctx, p) {
            return true;
        }
        if let Trunk::UpwardsBranching {
            can_grow_through, ..
        } = &self.trunk
        {
            return can_grow_through.contains(ctx.lv.get(p));
        }
        false
    }

    /// `TrunkPlacer.isFree`.
    fn is_free(&self, ctx: &Ctx, p: Pos) -> bool {
        self.valid_pos(ctx, p) || ctx.tags().logs.contains(ctx.lv.get(p))
    }

    fn set_dirt(&self, ctx: &mut Ctx, pl: &mut Placement, p: Pos) {
        let s = ctx.lv.get(p);
        let is_dirt = ctx.tags().dirt.contains(s)
            && blockinfo::name(s) != "minecraft:grass_block"
            && blockinfo::name(s) != "minecraft:mycelium";
        if self.force_dirt || !is_dirt {
            let d = self.dirt_provider.get(ctx.r, p);
            set_root(ctx, pl, p, d);
        }
    }

    fn place_log_with(
        &self,
        ctx: &mut Ctx,
        pl: &mut Placement,
        p: Pos,
        axis: Option<&str>,
    ) -> bool {
        if self.valid_pos(ctx, p) {
            let mut s = self.trunk_provider.get(ctx.r, p);
            if let Some(a) = axis {
                s = blockinfo::with_prop(s, "axis", a);
            }
            pl.logs.insert(p);
            ctx.lv.set(p, s);
            true
        } else {
            false
        }
    }

    fn place_log(&self, ctx: &mut Ctx, pl: &mut Placement, p: Pos) -> bool {
        self.place_log_with(ctx, pl, p, None)
    }

    fn place_log_if_free(&self, ctx: &mut Ctx, pl: &mut Placement, p: Pos) {
        if self.is_free(ctx, p) {
            self.place_log(ctx, pl, p);
        }
    }

    fn tree_height(&self, r: &mut impl Rng) -> i32 {
        self.base_height + r.next_int_bounded(self.rand_a + 1) + r.next_int_bounded(self.rand_b + 1)
    }

    fn foliage_height(&self, r: &mut impl Rng, h: i32) -> i32 {
        match &self.foliage {
            Foliage::Blob(v) | Foliage::Fancy(v) | Foliage::Bush(v) | Foliage::MegaJungle(v) => *v,
            Foliage::Spruce(trunk) => 4.max(h - trunk.sample(r)),
            Foliage::Pine(p) | Foliage::MegaPine(p) => p.sample(r),
            Foliage::Acacia => 0,
            Foliage::DarkOak => 4,
            Foliage::RandomSpread { height, .. } => height.sample(r),
            Foliage::Cherry { height, .. } => height.sample(r),
        }
    }

    fn foliage_radius(&self, r: &mut impl Rng, trunk_left: i32) -> i32 {
        let base = self.radius.sample(r);
        match self.foliage {
            Foliage::Pine(_) => base + r.next_int_bounded((trunk_left + 1).max(1)),
            _ => base,
        }
    }
}

fn set_root(ctx: &mut Ctx, pl: &mut Placement, p: Pos, s: BlockStateId) {
    // `setDirtAt` writes through the trunk setter, so the dirt under a tree
    // is in its log set — which decorators then iterate, lowest first.
    pl.logs.insert(p);
    ctx.lv.set(p, s);
}

/// `TreeFeature.place`.
pub(crate) fn place_tree(ctx: &mut Ctx, cfg: &TreeConfig, origin: Pos) -> bool {
    let mut pl = Placement {
        logs: JavaPosSet::new(),
        leaves: JavaPosSet::new(),
        decorations: JavaPosSet::new(),
    };
    let roots = JavaPosSet::new();
    if !do_place(ctx, cfg, origin, &mut pl) {
        return false;
    }
    if pl.logs.is_empty() && pl.leaves.is_empty() {
        return false;
    }
    if !cfg.decorators.is_empty() {
        let mut logs = pl.logs.java_order();
        let mut leaves = pl.leaves.java_order();
        logs.sort_by_key(|p| p.y);
        leaves.sort_by_key(|p| p.y);
        let roots_sorted = roots.java_order();
        for d in &cfg.decorators {
            decorate(ctx, d, &logs, &leaves, &roots_sorted, &mut pl.decorations);
        }
    }
    update_leaves(ctx, &pl);
    true
}

fn do_place(ctx: &mut Ctx, cfg: &TreeConfig, origin: Pos, pl: &mut Placement) -> bool {
    let h = cfg.tree_height(ctx.r);
    let fh = cfg.foliage_height(ctx.r, h);
    let trunk_left = h - fh;
    let radius = cfg.foliage_radius(ctx.r, trunk_left);
    let trunk_origin = origin;
    let lo = origin.y.min(trunk_origin.y);
    let hi = origin.y.max(trunk_origin.y) + h + 1;
    if lo < ctx.lv.min_y() + 1 || hi > ctx.lv.max_y() - 1 + 1 {
        return false;
    }
    let free = max_free_height(ctx, cfg, h, trunk_origin);
    if !(free >= h || cfg.size.min_clipped().is_some_and(|m| free >= m)) {
        return false;
    }
    let attachments = place_trunk(ctx, cfg, pl, free, trunk_origin);
    for a in &attachments {
        create_foliage(ctx, cfg, pl, free, a, fh, radius);
    }
    true
}

fn max_free_height(ctx: &Ctx, cfg: &TreeConfig, h: i32, origin: Pos) -> i32 {
    for y in 0..=h + 1 {
        let s = cfg.size.at(h, y);
        for dx in -s..=s {
            for dz in -s..=s {
                let p = origin.offset(dx, y, dz);
                if !cfg.is_free(ctx, p)
                    || !cfg.ignore_vines && blockinfo::name(ctx.lv.get(p)) == "minecraft:vine"
                {
                    return y - 2;
                }
            }
        }
    }
    h
}

fn random_horizontal(ctx: &mut Ctx) -> Dir {
    Dir::HORIZONTAL[ctx.r.next_int_bounded(4) as usize]
}

fn place_trunk(
    ctx: &mut Ctx,
    cfg: &TreeConfig,
    pl: &mut Placement,
    h: i32,
    o: Pos,
) -> Vec<Attachment> {
    let att = |pos: Pos, ro: i32, dt: bool| Attachment {
        pos,
        radius_offset: ro,
        double_trunk: dt,
    };
    match &cfg.trunk {
        Trunk::Straight => {
            cfg.set_dirt(ctx, pl, o.below(1));
            for i in 0..h {
                cfg.place_log(ctx, pl, o.above(i));
            }
            vec![att(o.above(h), 0, false)]
        }
        Trunk::Forking => {
            cfg.set_dirt(ctx, pl, o.below(1));
            let mut out = Vec::new();
            let d1 = random_horizontal(ctx);
            let fork_at = h - ctx.r.next_int_bounded(4) - 1;
            let mut lean = 3 - ctx.r.next_int_bounded(3);
            let (mut x, mut z) = (o.x, o.z);
            let mut top: Option<i32> = None;
            for i in 0..h {
                let y = o.y + i;
                if i >= fork_at && lean > 0 {
                    let (sx, _, sz) = d1.step();
                    x += sx;
                    z += sz;
                    lean -= 1;
                }
                if cfg.place_log(ctx, pl, Pos::new(x, y, z)) {
                    top = Some(y + 1);
                }
            }
            if let Some(t) = top {
                out.push(att(Pos::new(x, t, z), 1, false));
            }
            let (mut x, mut z) = (o.x, o.z);
            let d2 = random_horizontal(ctx);
            if d2 != d1 {
                let start = fork_at - ctx.r.next_int_bounded(2) - 1;
                let mut len = 1 + ctx.r.next_int_bounded(3);
                let mut top = None;
                let mut i = start;
                while i < h && len > 0 {
                    if i >= 1 {
                        let y = o.y + i;
                        let (sx, _, sz) = d2.step();
                        x += sx;
                        z += sz;
                        if cfg.place_log(ctx, pl, Pos::new(x, y, z)) {
                            top = Some(y + 1);
                        }
                    }
                    i += 1;
                    len -= 1;
                }
                if let Some(t) = top {
                    out.push(att(Pos::new(x, t, z), 0, false));
                }
            }
            out
        }
        Trunk::Fancy => fancy_trunk(ctx, cfg, pl, h, o),
        Trunk::DarkOak => {
            let mut out = Vec::new();
            let b = o.below(1);
            cfg.set_dirt(ctx, pl, b);
            cfg.set_dirt(ctx, pl, b.rel(Dir::East));
            cfg.set_dirt(ctx, pl, b.rel(Dir::South));
            cfg.set_dirt(ctx, pl, b.rel(Dir::South).rel(Dir::East));
            let d = random_horizontal(ctx);
            let lean_at = h - ctx.r.next_int_bounded(4);
            let mut lean = 2 - ctx.r.next_int_bounded(3);
            let (x0, y0, z0) = (o.x, o.y, o.z);
            let (mut x, mut z) = (x0, z0);
            let top = y0 + h - 1;
            for i in 0..h {
                if i >= lean_at && lean > 0 {
                    let (sx, _, sz) = d.step();
                    x += sx;
                    z += sz;
                    lean -= 1;
                }
                let p = Pos::new(x, y0 + i, z);
                if is_air_or_leaves(ctx, p) {
                    cfg.place_log(ctx, pl, p);
                    cfg.place_log(ctx, pl, p.rel(Dir::East));
                    cfg.place_log(ctx, pl, p.rel(Dir::South));
                    cfg.place_log(ctx, pl, p.rel(Dir::East).rel(Dir::South));
                }
            }
            out.push(att(Pos::new(x, top, z), 0, true));
            for dx in -1..=2 {
                for dz in -1..=2 {
                    if (dx < 0 || dx > 1 || dz < 0 || dz > 1) && ctx.r.next_int_bounded(3) <= 0 {
                        let n = ctx.r.next_int_bounded(3) + 2;
                        for k in 0..n {
                            cfg.place_log(ctx, pl, Pos::new(x0 + dx, top - k - 1, z0 + dz));
                        }
                        out.push(att(Pos::new(x0 + dx, top, z0 + dz), 0, false));
                    }
                }
            }
            out
        }
        Trunk::Giant | Trunk::MegaJungle => {
            let b = o.below(1);
            cfg.set_dirt(ctx, pl, b);
            cfg.set_dirt(ctx, pl, b.rel(Dir::East));
            cfg.set_dirt(ctx, pl, b.rel(Dir::South));
            cfg.set_dirt(ctx, pl, b.rel(Dir::South).rel(Dir::East));
            for i in 0..h {
                cfg.place_log_if_free(ctx, pl, o.offset(0, i, 0));
                if i < h - 1 {
                    cfg.place_log_if_free(ctx, pl, o.offset(1, i, 0));
                    cfg.place_log_if_free(ctx, pl, o.offset(1, i, 1));
                    cfg.place_log_if_free(ctx, pl, o.offset(0, i, 1));
                }
            }
            let mut out = vec![att(o.above(h), 0, true)];
            if matches!(cfg.trunk, Trunk::MegaJungle) {
                let mut i = h - 2 - ctx.r.next_int_bounded(4);
                while i > h / 2 {
                    let a = ctx.r.next_float() * (PI * 2.0) as f32;
                    let (mut dx, mut dz) = (0, 0);
                    for k in 0..5 {
                        dx = (1.5 + super::super::mth::cos(a as f64) * k as f32) as i32;
                        dz = (1.5 + super::super::mth::sin(a as f64) * k as f32) as i32;
                        cfg.place_log(ctx, pl, o.offset(dx, i - 3 + k / 2, dz));
                    }
                    out.push(att(o.offset(dx, i, dz), -2, false));
                    i -= 2 + ctx.r.next_int_bounded(4);
                }
            }
            out
        }
        Trunk::Bending {
            min_height_for_leaves,
            bend_length,
        } => {
            let d = random_horizontal(ctx);
            let top = h - 1;
            let mut p = o;
            cfg.set_dirt(ctx, pl, p.below(1));
            let mut out = Vec::new();
            for i in 0..=top {
                if i + 1 >= top + ctx.r.next_int_bounded(2) {
                    p = p.rel(d);
                }
                if valid_tree_pos(ctx, p) {
                    cfg.place_log(ctx, pl, p);
                }
                if i >= *min_height_for_leaves {
                    out.push(att(p, 0, false));
                }
                p = p.above(1);
            }
            let n = bend_length.sample(ctx.r);
            for _ in 0..=n {
                if valid_tree_pos(ctx, p) {
                    cfg.place_log(ctx, pl, p);
                }
                out.push(att(p, 0, false));
                p = p.rel(d);
            }
            out
        }
        Trunk::Cherry {
            branch_count,
            branch_horizontal_length,
            branch_start,
            branch_end,
        } => {
            cfg.set_dirt(ctx, pl, o.below(1));
            let first = 0.max(
                h - 1
                    + (ctx.r.next_int_bounded(branch_start.1 - branch_start.0 + 1)
                        + branch_start.0),
            );
            let second_hi = branch_start.1 - 1;
            let mut second = 0.max(
                h - 1 + (ctx.r.next_int_bounded(second_hi - branch_start.0 + 1) + branch_start.0),
            );
            if second >= first {
                second += 1;
            }
            let count = branch_count.sample(ctx.r);
            let three = count == 3;
            let two = count >= 2;
            let trunk_h = if three {
                h
            } else if two {
                first.max(second) + 1
            } else {
                first + 1
            };
            for i in 0..trunk_h {
                cfg.place_log(ctx, pl, o.above(i));
            }
            let mut out = Vec::new();
            if three {
                out.push(att(o.above(trunk_h), 0, false));
            }
            let d = random_horizontal(ctx);
            out.push(cherry_branch(
                ctx,
                cfg,
                pl,
                h,
                o,
                d,
                first,
                first < trunk_h - 1,
                branch_horizontal_length,
                branch_end,
            ));
            if two {
                out.push(cherry_branch(
                    ctx,
                    cfg,
                    pl,
                    h,
                    o,
                    d.opposite(),
                    second,
                    second < trunk_h - 1,
                    branch_horizontal_length,
                    branch_end,
                ));
            }
            out
        }
        Trunk::UpwardsBranching {
            extra_branch_steps,
            place_branch_per_log_probability,
            extra_branch_length,
            ..
        } => {
            let mut out = Vec::new();
            for i in 0..h {
                let y = o.y + i;
                let p = Pos::new(o.x, y, o.z);
                if cfg.place_log(ctx, pl, p)
                    && i < h - 1
                    && ctx.r.next_float() < *place_branch_per_log_probability
                {
                    let d = random_horizontal(ctx);
                    let len = extra_branch_length.sample(ctx.r);
                    let start = 0.max(len - extra_branch_length.sample(ctx.r) - 1);
                    let steps = extra_branch_steps.sample(ctx.r);
                    // placeBranch
                    let mut top = y + start;
                    let (mut x, mut z) = (p.x, p.z);
                    let mut k = start;
                    let mut left = steps;
                    while k < h && left > 0 {
                        if k >= 1 {
                            let yy = y + k;
                            let (sx, _, sz) = d.step();
                            x += sx;
                            z += sz;
                            top = yy;
                            if cfg.place_log(ctx, pl, Pos::new(x, yy, z)) {
                                top = yy + 1;
                            }
                            out.push(att(Pos::new(x, yy, z), 0, false));
                        }
                        k += 1;
                        left -= 1;
                    }
                    if top - y > 1 {
                        let b = Pos::new(x, top, z);
                        out.push(att(b, 0, false));
                        out.push(att(b.below(2), 0, false));
                    }
                }
                if i == h - 1 {
                    out.push(att(Pos::new(o.x, y + 1, o.z), 0, false));
                }
            }
            out
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cherry_branch(
    ctx: &mut Ctx,
    cfg: &TreeConfig,
    pl: &mut Placement,
    h: i32,
    o: Pos,
    d: Dir,
    start: i32,
    below_top: bool,
    horizontal: &IntProvider,
    end_offset: &IntProvider,
) -> Attachment {
    let mut cur = o.above(start);
    let end_y = h - 1 + end_offset.sample(ctx.r);
    let long = below_top || end_y < start;
    let len = horizontal.sample(ctx.r) + if long { 1 } else { 0 };
    let target = o.rel_n(d, len).above(end_y);
    let n = if long { 2 } else { 1 };
    for _ in 0..n {
        cur = cur.rel(d);
        cfg.place_log_with(ctx, pl, cur, Some(d.axis()));
    }
    let vdir = if target.y > cur.y { Dir::Up } else { Dir::Down };
    loop {
        let dist = cur.manhattan(target);
        if dist == 0 {
            return Attachment {
                pos: target.above(1),
                radius_offset: 0,
                double_trunk: false,
            };
        }
        let f = (target.y - cur.y).abs() as f32 / dist as f32;
        let vertical = ctx.r.next_float() < f;
        cur = cur.rel(if vertical { vdir } else { d });
        cfg.place_log_with(ctx, pl, cur, if vertical { None } else { Some(d.axis()) });
    }
}

fn fancy_trunk(
    ctx: &mut Ctx,
    cfg: &TreeConfig,
    pl: &mut Placement,
    h: i32,
    o: Pos,
) -> Vec<Attachment> {
    let height = h + 2;
    let trunk_h = (height as f64 * 0.618).floor() as i32;
    cfg.set_dirt(ctx, pl, o.below(1));
    let clusters = 1.min((1.382 + (1.0 * height as f64 / 13.0).powf(2.0)).floor() as i32);
    let trunk_top = o.y + trunk_h;
    let mut y = height - 5;
    let mut coords: Vec<(Pos, i32)> = vec![(o.above(y), trunk_top)];
    while y >= 0 {
        let shape = tree_shape(height, y);
        if shape >= 0.0 {
            for _ in 0..clusters {
                let d = 1.0 * shape as f64 * (ctx.r.next_float() as f64 + 0.328);
                let a = (ctx.r.next_float() * 2.0) as f64 * PI;
                let ox = d * a.sin() + 0.5;
                let oz = d * a.cos() + 0.5;
                let base = o.offset(ox.floor() as i32, y - 1, oz.floor() as i32);
                let tip = base.above(5);
                if make_limb(ctx, cfg, pl, base, tip, false) {
                    let dx = o.x - base.x;
                    let dz = o.z - base.z;
                    let by = base.y as f64 - ((dx * dx + dz * dz) as f64).sqrt() * 0.381;
                    let branch_y = if by > trunk_top as f64 {
                        trunk_top
                    } else {
                        by as i32
                    };
                    let branch = Pos::new(o.x, branch_y, o.z);
                    if make_limb(ctx, cfg, pl, branch, base, false) {
                        coords.push((base, branch.y));
                    }
                }
            }
        }
        y -= 1;
    }
    make_limb(ctx, cfg, pl, o, o.above(trunk_h), true);
    // makeBranches
    for (pos, base_y) in &coords {
        let b = Pos::new(o.x, *base_y, o.z);
        if b != *pos && (*base_y - o.y) as f64 >= height as f64 * 0.2 {
            make_limb(ctx, cfg, pl, b, *pos, true);
        }
    }
    coords
        .into_iter()
        .filter(|(_, by)| (*by - o.y) as f64 >= height as f64 * 0.2)
        .map(|(pos, _)| Attachment {
            pos,
            radius_offset: 0,
            double_trunk: false,
        })
        .collect()
}

fn tree_shape(h: i32, y: i32) -> f32 {
    if (y as f32) < h as f32 * 0.3 {
        return -1.0;
    }
    let half = h as f32 / 2.0;
    let d = half - y as f32;
    let mut v = (half * half - d * d).sqrt();
    if d == 0.0 {
        v = half;
    } else if d.abs() >= half {
        return 0.0;
    }
    v * 0.5
}

fn make_limb(
    ctx: &mut Ctx,
    cfg: &TreeConfig,
    pl: &mut Placement,
    from: Pos,
    to: Pos,
    place: bool,
) -> bool {
    if !place && from == to {
        return true;
    }
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    let steps = dx.abs().max(dy.abs()).max(dz.abs());
    let fx = dx as f32 / steps as f32;
    let fy = dy as f32 / steps as f32;
    let fz = dz as f32 / steps as f32;
    for i in 0..=steps {
        let p = from.offset(
            (0.5 + i as f32 * fx).floor() as i32,
            (0.5 + i as f32 * fy).floor() as i32,
            (0.5 + i as f32 * fz).floor() as i32,
        );
        if place {
            let ax = (p.x - from.x).abs();
            let az = (p.z - from.z).abs();
            let m = ax.max(az);
            let axis = if m > 0 {
                if ax == m {
                    "x"
                } else {
                    "z"
                }
            } else {
                "y"
            };
            cfg.place_log_with(ctx, pl, p, Some(axis));
        } else if !cfg.is_free(ctx, p) {
            return false;
        }
    }
    true
}

/// `FoliagePlacer.tryPlaceLeaf`.
fn try_place_leaf(ctx: &mut Ctx, cfg: &TreeConfig, pl: &mut Placement, p: Pos) -> bool {
    let s = ctx.lv.get(p);
    let persistent = prop(s, "persistent") == Some("true");
    if !persistent && valid_tree_pos(ctx, p) {
        let mut leaf = cfg.foliage_provider.get(ctx.r, p);
        if prop(leaf, "waterlogged").is_some() {
            let wet = super::blocks::is_water_source(ctx.lv.get(p));
            leaf = blockinfo::with_prop(leaf, "waterlogged", if wet { "true" } else { "false" });
        }
        pl.leaves.insert(p);
        ctx.lv.set(p, leaf);
        true
    } else {
        false
    }
}

impl Foliage {
    fn skip(&self, r: &mut impl Rng, dx: i32, y: i32, dz: i32, range: i32, large: bool) -> bool {
        match self {
            Foliage::Blob(_) => {
                dx == range && dz == range && (r.next_int_bounded(2) == 0 || y == 0)
            }
            Foliage::Fancy(_) => {
                let a = dx as f32 + 0.5;
                let b = dz as f32 + 0.5;
                a * a + b * b > (range * range) as f32
            }
            Foliage::Bush(_) => dx == range && dz == range && r.next_int_bounded(2) == 0,
            Foliage::Spruce(_) | Foliage::Pine(_) => dx == range && dz == range && range > 0,
            Foliage::MegaPine(_) | Foliage::MegaJungle(_) => {
                if dx + dz >= 7 {
                    true
                } else {
                    dx * dx + dz * dz > range * range
                }
            }
            Foliage::Acacia => {
                if y == 0 {
                    (dx > 1 || dz > 1) && dx != 0 && dz != 0
                } else {
                    dx == range && dz == range && range > 0
                }
            }
            Foliage::DarkOak => {
                if y == -1 && !large {
                    dx == range && dz == range
                } else if y == 1 {
                    dx + dz > range * 2 - 2
                } else {
                    false
                }
            }
            Foliage::RandomSpread { .. } => false,
            Foliage::Cherry {
                wide_bottom_hole,
                corner_hole,
                ..
            } => {
                if y == -1 && (dx == range || dz == range) && r.next_float() < *wide_bottom_hole {
                    true
                } else {
                    let corner = dx == range && dz == range;
                    if range > 2 {
                        corner || dx + dz > range * 2 - 2 && r.next_float() < *corner_hole
                    } else {
                        corner && r.next_float() < *corner_hole
                    }
                }
            }
        }
    }

    fn skip_signed(
        &self,
        r: &mut impl Rng,
        dx: i32,
        y: i32,
        dz: i32,
        range: i32,
        large: bool,
    ) -> bool {
        if let Foliage::DarkOak = self {
            let special =
                y == 0 && large && (dx == -range || dx >= range) && (dz == -range || dz >= range);
            if special {
                return true;
            }
        }
        let (ax, az) = if large {
            (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs()))
        } else {
            (dx.abs(), dz.abs())
        };
        self.skip(r, ax, y, az, range, large)
    }
}

#[allow(clippy::too_many_arguments)]
fn leaves_row(
    ctx: &mut Ctx,
    cfg: &TreeConfig,
    pl: &mut Placement,
    center: Pos,
    range: i32,
    y: i32,
    large: bool,
) {
    let extra = if large { 1 } else { 0 };
    for dx in -range..=range + extra {
        for dz in -range..=range + extra {
            if !cfg.foliage.skip_signed(ctx.r, dx, y, dz, range, large) {
                try_place_leaf(ctx, cfg, pl, center.offset(dx, y, dz));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn leaves_row_hanging(
    ctx: &mut Ctx,
    cfg: &TreeConfig,
    pl: &mut Placement,
    center: Pos,
    range: i32,
    y: i32,
    large: bool,
    chance: f32,
    ext: f32,
) {
    leaves_row(ctx, cfg, pl, center, range, y, large);
    let extra = if large { 1 } else { 0 };
    let below = center.below(1);
    for d in Dir::HORIZONTAL {
        let cw = d.clockwise();
        let n = if cw.positive() { range + extra } else { range };
        let mut p = center.offset(0, y - 1, 0).rel_n(cw, n).rel_n(d, -range);
        let mut i = -range;
        while i < range + extra {
            let set_above = pl.leaves.contains(&p.above(1));
            if set_above && try_extension(ctx, cfg, pl, chance, below, p) {
                let q = p.below(1);
                try_extension(ctx, cfg, pl, ext, below, q);
            }
            i += 1;
            p = p.rel(d);
        }
    }
}

fn try_extension(
    ctx: &mut Ctx,
    cfg: &TreeConfig,
    pl: &mut Placement,
    chance: f32,
    base: Pos,
    p: Pos,
) -> bool {
    if p.manhattan(base) >= 7 {
        return false;
    }
    if ctx.r.next_float() > chance {
        return false;
    }
    try_place_leaf(ctx, cfg, pl, p)
}

fn create_foliage(
    ctx: &mut Ctx,
    cfg: &TreeConfig,
    pl: &mut Placement,
    _free: i32,
    a: &Attachment,
    fh: i32,
    radius: i32,
) {
    let offset = cfg.offset.sample(ctx.r);
    let large = a.double_trunk;
    match &cfg.foliage {
        Foliage::Blob(_) => {
            let mut y = offset;
            while y >= offset - fh {
                let r = (radius + a.radius_offset - 1 - y / 2).max(0);
                leaves_row(ctx, cfg, pl, a.pos, r, y, large);
                y -= 1;
            }
        }
        Foliage::Fancy(_) => {
            let mut y = offset;
            while y >= offset - fh {
                let r = radius
                    + if y != offset && y != offset - fh {
                        1
                    } else {
                        0
                    };
                leaves_row(ctx, cfg, pl, a.pos, r, y, large);
                y -= 1;
            }
        }
        Foliage::Bush(_) => {
            let mut y = offset;
            while y >= offset - fh {
                let r = radius + a.radius_offset - 1 - y;
                leaves_row(ctx, cfg, pl, a.pos, r, y, large);
                y -= 1;
            }
        }
        Foliage::Spruce(_) => {
            let mut r = ctx.r.next_int_bounded(2);
            let mut max_r = 1;
            let mut min_r = 0;
            let mut y = offset;
            while y >= -fh {
                leaves_row(ctx, cfg, pl, a.pos, r, y, large);
                if r >= max_r {
                    r = min_r;
                    min_r = 1;
                    max_r = (max_r + 1).min(radius + a.radius_offset);
                } else {
                    r += 1;
                }
                y -= 1;
            }
        }
        Foliage::Pine(_) => {
            let mut r = 0;
            let mut y = offset;
            while y >= offset - fh {
                leaves_row(ctx, cfg, pl, a.pos, r, y, large);
                if r >= 1 && y == offset - fh + 1 {
                    r -= 1;
                } else if r < radius + a.radius_offset {
                    r += 1;
                }
                y -= 1;
            }
        }
        Foliage::MegaPine(_) => {
            let mut prev = 0;
            let mut y = a.pos.y - fh + offset;
            while y <= a.pos.y + offset {
                let d = a.pos.y - y;
                let r0 = radius + a.radius_offset + (d as f32 / fh as f32 * 3.5).floor() as i32;
                let r = if d > 0 && r0 == prev && (y & 1) == 0 {
                    r0 + 1
                } else {
                    r0
                };
                leaves_row(ctx, cfg, pl, Pos::new(a.pos.x, y, a.pos.z), r, 0, large);
                prev = r0;
                y += 1;
            }
        }
        Foliage::Acacia => {
            let c = a.pos.above(offset);
            leaves_row(ctx, cfg, pl, c, radius + a.radius_offset, -1 - fh, large);
            leaves_row(ctx, cfg, pl, c, radius - 1, -fh, large);
            leaves_row(ctx, cfg, pl, c, radius + a.radius_offset - 1, 0, large);
        }
        Foliage::DarkOak => {
            let c = a.pos.above(offset);
            if large {
                leaves_row(ctx, cfg, pl, c, radius + 2, -1, large);
                leaves_row(ctx, cfg, pl, c, radius + 3, 0, large);
                leaves_row(ctx, cfg, pl, c, radius + 2, 1, large);
                if ctx.r.next_bool() {
                    leaves_row(ctx, cfg, pl, c, radius, 2, large);
                }
            } else {
                leaves_row(ctx, cfg, pl, c, radius + 2, -1, large);
                leaves_row(ctx, cfg, pl, c, radius + 1, 0, large);
            }
        }
        Foliage::MegaJungle(_) => {
            let n = if large {
                fh
            } else {
                1 + ctx.r.next_int_bounded(2)
            };
            let mut y = offset;
            while y >= offset - n {
                let r = radius + a.radius_offset + 1 - y;
                leaves_row(ctx, cfg, pl, a.pos, r, y, large);
                y -= 1;
            }
        }
        Foliage::RandomSpread { attempts, .. } => {
            for _ in 0..*attempts {
                let dx = ctx.r.next_int_bounded(radius) - ctx.r.next_int_bounded(radius);
                let dy = ctx.r.next_int_bounded(fh) - ctx.r.next_int_bounded(fh);
                let dz = ctx.r.next_int_bounded(radius) - ctx.r.next_int_bounded(radius);
                try_place_leaf(ctx, cfg, pl, a.pos.offset(dx, dy, dz));
            }
        }
        Foliage::Cherry {
            hanging,
            hanging_ext,
            ..
        } => {
            let c = a.pos.above(offset);
            let r = radius + a.radius_offset - 1;
            leaves_row(ctx, cfg, pl, c, r - 2, fh - 3, large);
            leaves_row(ctx, cfg, pl, c, r - 1, fh - 4, large);
            let mut y = fh - 5;
            while y >= 0 {
                leaves_row(ctx, cfg, pl, c, r, y, large);
                y -= 1;
            }
            leaves_row_hanging(ctx, cfg, pl, c, r, -1, large, *hanging, *hanging_ext);
            leaves_row_hanging(ctx, cfg, pl, c, r - 1, -2, large, *hanging, *hanging_ext);
        }
    }
}

/// `TreeFeature.updateLeaves`: leaf `distance` from the nearest log, by
/// breadth-first search through leaves, within the tree's bounding box.
fn update_leaves(ctx: &mut Ctx, pl: &Placement) {
    let all: Vec<Pos> = pl
        .logs
        .inserted()
        .iter()
        .chain(pl.leaves.inserted())
        .chain(pl.decorations.inserted())
        .copied()
        .collect();
    if all.is_empty() {
        return;
    }
    let (mut x0, mut y0, mut z0) = (i32::MAX, i32::MAX, i32::MAX);
    let (mut x1, mut y1, mut z1) = (i32::MIN, i32::MIN, i32::MIN);
    for p in &all {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        z0 = z0.min(p.z);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
        z1 = z1.max(p.z);
    }
    let inside =
        |p: &Pos| p.x >= x0 && p.x <= x1 && p.y >= y0 && p.y <= y1 && p.z >= z0 && p.z <= z1;
    let (w, h, d) = (
        (x1 - x0 + 1) as usize,
        (y1 - y0 + 1) as usize,
        (z1 - z0 + 1) as usize,
    );
    let idx = |p: &Pos| ((p.x - x0) as usize * h + (p.y - y0) as usize) * d + (p.z - z0) as usize;
    let mut filled = vec![false; w * h * d];
    // Seeded with decorations and roots (there are no roots here), not
    // leaves: leaves are what the search assigns distances to.
    for p in pl.decorations.inserted() {
        if inside(p) {
            filled[idx(p)] = true;
        }
    }
    let leaves_tag = ctx.tags().leaves.clone();
    let logs_tag = ctx.tags().logs.clone();
    let distance_at = |s: BlockStateId| -> Option<i32> {
        if logs_tag.contains(s) {
            Some(0)
        } else {
            prop(s, "distance").and_then(|v| v.parse().ok())
        }
    };
    let mut buckets: Vec<Vec<Pos>> = vec![Vec::new(); 7];
    buckets[0] = pl.logs.inserted().to_vec();
    let mut level = 0usize;
    loop {
        while level < 7 && buckets[level].is_empty() {
            level += 1;
        }
        if level >= 7 {
            break;
        }
        let p = buckets[level].pop().unwrap();
        if !inside(&p) {
            continue;
        }
        if level != 0 {
            let s = ctx.lv.get(p);
            if leaves_tag.contains(s) || prop(s, "distance").is_some() {
                ctx.lv
                    .set(p, blockinfo::with_prop(s, "distance", &level.to_string()));
            }
        }
        filled[idx(&p)] = true;
        for dir in Dir::ALL {
            let q = p.rel(dir);
            if !inside(&q) || filled[idx(&q)] {
                continue;
            }
            if let Some(dist) = distance_at(ctx.lv.get(q)) {
                let nd = dist.min(level as i32 + 1);
                if nd < 7 {
                    buckets[nd as usize].push(q);
                    level = level.min(nd as usize);
                }
            }
        }
    }
}

fn decorate(
    ctx: &mut Ctx,
    d: &Decorator,
    logs: &[Pos],
    leaves: &[Pos],
    roots: &[Pos],
    deco: &mut JavaPosSet,
) {
    let set = |ctx: &mut Ctx, deco: &mut JavaPosSet, p: Pos, s: BlockStateId| {
        deco.insert(p);
        ctx.lv.set(p, s);
    };
    let vine = |dir: Dir| blockinfo::with_prop(states().vine, dir.name(), "true");
    match d {
        Decorator::LeaveVine(prob) => {
            for l in leaves {
                for (side, face) in [
                    (Dir::West, Dir::East),
                    (Dir::East, Dir::West),
                    (Dir::North, Dir::South),
                    (Dir::South, Dir::North),
                ] {
                    if ctx.r.next_float() < *prob {
                        let p = l.rel(side);
                        if ctx.lv.is_air(p) {
                            set(ctx, deco, p, vine(face));
                            let mut q = p.below(1);
                            let mut left = 4;
                            while ctx.lv.is_air(q) && left > 0 {
                                set(ctx, deco, q, vine(face));
                                q = q.below(1);
                                left -= 1;
                            }
                        }
                    }
                }
            }
        }
        Decorator::TrunkVine => {
            for l in logs {
                for (side, face) in [
                    (Dir::West, Dir::East),
                    (Dir::East, Dir::West),
                    (Dir::North, Dir::South),
                    (Dir::South, Dir::North),
                ] {
                    if ctx.r.next_int_bounded(3) > 0 {
                        let p = l.rel(side);
                        if ctx.lv.is_air(p) {
                            set(ctx, deco, p, vine(face));
                        }
                    }
                }
            }
        }
        Decorator::Cocoa(prob) => {
            if ctx.r.next_float() >= *prob || logs.is_empty() {
                return;
            }
            let y0 = logs[0].y;
            for l in logs.iter().filter(|p| p.y - y0 <= 2) {
                for face in Dir::HORIZONTAL {
                    if ctx.r.next_float() <= 0.25 {
                        let (sx, _, sz) = face.opposite().step();
                        let p = l.offset(sx, 0, sz);
                        if ctx.lv.is_air(p) {
                            let age = ctx.r.next_int_bounded(3);
                            let s = blockinfo::parse_state(&format!(
                                "minecraft:cocoa[age={age},facing={}]",
                                face.name()
                            ));
                            if let Some(s) = s {
                                set(ctx, deco, p, s);
                            }
                        }
                    }
                }
            }
        }
        Decorator::Beehive(prob) => {
            if logs.is_empty() {
                return;
            }
            if ctx.r.next_float() >= *prob {
                return;
            }
            let y = if !leaves.is_empty() {
                (leaves[0].y - 1).max(logs[0].y + 1)
            } else {
                (logs[0].y + 1 + ctx.r.next_int_bounded(3)).min(logs[logs.len() - 1].y)
            };
            let spawn = [Dir::North, Dir::East, Dir::South, Dir::West]
                .iter()
                .copied()
                .filter(|d| *d != Dir::North);
            let spawn: Vec<Dir> = spawn.collect();
            let cands: Vec<Pos> = logs
                .iter()
                .filter(|p| p.y == y)
                .flat_map(|p| spawn.iter().map(move |d| p.rel(*d)))
                .collect();
            if cands.is_empty() {
                return;
            }
            let cands = shuffle(ctx, &cands);
            if let Some(p) = cands
                .into_iter()
                .find(|p| ctx.lv.is_air(*p) && ctx.lv.is_air(p.rel(Dir::South)))
            {
                if let Some(s) =
                    blockinfo::parse_state("minecraft:bee_nest[facing=south,honey_level=0]")
                {
                    set(ctx, deco, p, s);
                    let bees = 2 + ctx.r.next_int_bounded(2);
                    for _ in 0..bees {
                        ctx.r.next_int_bounded(599);
                    }
                }
            }
        }
        Decorator::AlterGround(provider) => {
            let base = lowest_trunk_or_root(logs, roots);
            if base.is_empty() {
                return;
            }
            let y0 = base[0].y;
            for p in base.iter().filter(|p| p.y == y0) {
                for c in [
                    p.rel(Dir::West).rel(Dir::North),
                    p.rel_n(Dir::East, 2).rel(Dir::North),
                    p.rel(Dir::West).rel_n(Dir::South, 2),
                    p.rel_n(Dir::East, 2).rel_n(Dir::South, 2),
                ] {
                    alter_circle(ctx, provider, c, deco);
                }
                for _ in 0..5 {
                    let k = ctx.r.next_int_bounded(64);
                    let (a, b) = (k % 8, k / 8);
                    if a == 0 || a == 7 || b == 0 || b == 7 {
                        alter_circle(ctx, provider, p.offset(-3 + a, 0, -3 + b), deco);
                    }
                }
            }
        }
        Decorator::PlaceOnGround {
            tries,
            radius,
            height,
            provider,
        } => {
            let base = lowest_trunk_or_root(logs, roots);
            if base.is_empty() {
                return;
            }
            let first = base[0];
            let y = first.y;
            let (mut x0, mut x1, mut z0, mut z1) = (first.x, first.x, first.z, first.z);
            for p in base.iter().filter(|p| p.y == y) {
                x0 = x0.min(p.x);
                x1 = x1.max(p.x);
                z0 = z0.min(p.z);
                z1 = z1.max(p.z);
            }
            let (bx0, bx1) = (x0 - radius, x1 + radius);
            let (by0, by1) = (y - height, y + height);
            let (bz0, bz1) = (z0 - radius, z1 + radius);
            for _ in 0..*tries {
                let x = ctx.r.next_int_between_inclusive(bx0, bx1);
                let yy = ctx.r.next_int_between_inclusive(by0, by1);
                let z = ctx.r.next_int_between_inclusive(bz0, bz1);
                let p = Pos::new(x, yy, z);
                let up = p.above(1);
                let us = ctx.lv.get(up);
                if (blockinfo::is_air(us) || blockinfo::name(us) == "minecraft:vine")
                    && solid_render(ctx.lv.get(p))
                    && ctx.lv.height(Heightmap::MotionBlockingNoLeaves, p.x, p.z) <= up.y
                {
                    let s = provider.get(ctx.r, up);
                    set(ctx, deco, up, s);
                }
            }
        }
        Decorator::AttachedToLeaves {
            probability,
            exclusion_xz,
            exclusion_y,
            provider,
            required_empty,
            directions,
        } => {
            let mut excluded = std::collections::HashSet::new();
            let shuffled = shuffle(ctx, leaves);
            for l in shuffled {
                let dir = directions[ctx.r.next_int_bounded(directions.len() as i32) as usize];
                let p = l.rel(dir);
                if !excluded.contains(&p) && ctx.r.next_float() < *probability {
                    let empty = (1..=*required_empty).all(|k| ctx.lv.is_air(l.rel_n(dir, k)));
                    if empty {
                        for dx in -exclusion_xz..=*exclusion_xz {
                            for dy in -exclusion_y..=*exclusion_y {
                                for dz in -exclusion_xz..=*exclusion_xz {
                                    excluded.insert(p.offset(dx, dy, dz));
                                }
                            }
                        }
                        let s = provider.get(ctx.r, p);
                        set(ctx, deco, p, s);
                    }
                }
            }
        }
        Decorator::AttachedToLogs {
            probability,
            provider,
            directions,
        } => {
            let shuffled = shuffle(ctx, logs);
            for l in shuffled {
                let dir = directions[ctx.r.next_int_bounded(directions.len() as i32) as usize];
                let p = l.rel(dir);
                if ctx.r.next_float() <= *probability && ctx.lv.is_air(p) {
                    let s = provider.get(ctx.r, p);
                    set(ctx, deco, p, s);
                }
            }
        }
        Decorator::Unsupported(name) => ctx.d.note_unsupported(&format!("tree_decorator:{name}")),
    }
}

fn lowest_trunk_or_root(logs: &[Pos], roots: &[Pos]) -> Vec<Pos> {
    if roots.is_empty() {
        logs.to_vec()
    } else if !logs.is_empty() && roots[0].y == logs[0].y {
        logs.iter().chain(roots).copied().collect()
    } else {
        roots.to_vec()
    }
}

fn alter_circle(ctx: &mut Ctx, provider: &StateProvider, c: Pos, deco: &mut JavaPosSet) {
    for dx in -2i32..=2 {
        for dz in -2i32..=2 {
            if dx.abs() != 2 || dz.abs() != 2 {
                let at = c.offset(dx, 0, dz);
                let mut k = 2;
                while k >= -3 {
                    let p = at.above(k);
                    if ctx.tags().dirt.contains(ctx.lv.get(p)) {
                        let s = provider.get(ctx.r, at);
                        deco.insert(p);
                        ctx.lv.set(p, s);
                        break;
                    }
                    if !ctx.lv.is_air(p) && k < 0 {
                        break;
                    }
                    k -= 1;
                }
            }
        }
    }
}

/// `FallenTreeFeature.place`.
pub(crate) fn place_fallen(ctx: &mut Ctx, cfg: &FallenTree, origin: Pos) -> bool {
    // Stump.
    let s = cfg.trunk_provider.get(ctx.r, origin);
    ctx.lv.set(origin, s);
    if !cfg.stump_decorators.is_empty() {
        let mut deco = JavaPosSet::new();
        for d in &cfg.stump_decorators {
            decorate(ctx, d, &[origin], &[], &[], &mut deco);
        }
    }
    let dir = random_horizontal(ctx);
    let len = cfg.log_length.sample(ctx.r) - 2;
    let mut p = origin.rel_n(dir, 2 + ctx.r.next_int_bounded(2));
    // setGroundHeightForFallenLogStartPos
    p = p.above(1);
    for _ in 0..6 {
        if valid_tree_pos(ctx, p) && sturdy(ctx.lv.get(p.below(1))) {
            break;
        }
        p = p.below(1);
    }
    // canPlaceEntireFallenLog
    let mut gap = 0;
    let mut q = p;
    for _ in 0..len {
        if !valid_tree_pos(ctx, q) {
            return true;
        }
        if !sturdy(ctx.lv.get(q.below(1))) {
            gap += 1;
            if gap > 2 {
                return true;
            }
        } else {
            gap = 0;
        }
        q = q.rel(dir);
    }
    let mut placed = JavaPosSet::new();
    let mut q = p;
    for _ in 0..len {
        let s = blockinfo::with_prop(cfg.trunk_provider.get(ctx.r, q), "axis", dir.axis());
        ctx.lv.set(q, s);
        placed.insert(q);
        q = q.rel(dir);
    }
    if !cfg.log_decorators.is_empty() {
        let mut logs = placed.java_order();
        logs.sort_by_key(|p| p.y);
        let mut deco = JavaPosSet::new();
        for d in &cfg.log_decorators {
            decorate(ctx, d, &logs, &[], &[], &mut deco);
        }
    }
    true
}
