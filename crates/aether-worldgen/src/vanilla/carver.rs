//! Carvers: caves and canyons, a port of `CaveWorldCarver` and
//! `CanyonWorldCarver`.
//!
//! A carver *starts* in one chunk and carves into every chunk it reaches —
//! up to eight chunks away — so carving chunk C means replaying the carvers
//! of all 17×17 chunks around it, each from its own seeded random, and keeping
//! only the cells inside C. Every chunk replays the same tunnels, so the
//! pieces line up across chunk borders without any chunk seeing another's
//! blocks.
//!
//! Configured from the pack's `configured_carver/*.json` and each biome's
//! `carvers` list. The RNG is consumed draw for draw as in the game (legacy
//! LCG, `setLargeFeatureSeed`, the table-driven `Mth.sin`/`cos`), and the
//! carved cell asks the chunk's aquifer whether it floods, so caves below the
//! water table fill exactly where the game's do.

use std::collections::HashMap;
use std::sync::Arc;

use aether_world::BlockStateId;

use super::biome::BiomeRegistry;
use super::blockinfo;
use super::chunk::ProtoChunk;
use super::density::{BuildError, DataPack};
use super::generator::{Core, QuartCache};
use super::json::Json;
use super::mth;
use super::providers::{FloatProvider, HeightProvider};
use super::rng::{LegacyRandom, Rng, WorldgenRandom};
use super::surface::{Anchor, SurfaceEnv};
use super::tags::{BlockSet, Tags};
use super::terrain::{ChunkTerrain, Fill};

const PI: f64 = std::f64::consts::PI;

#[derive(Debug)]
enum Kind {
    Cave {
        horizontal: FloatProvider,
        vertical: FloatProvider,
        floor: FloatProvider,
    },
    Canyon {
        vertical_rotation: FloatProvider,
        distance_factor: FloatProvider,
        thickness: FloatProvider,
        width_smoothness: i32,
        horizontal_radius_factor: FloatProvider,
        vertical_radius_default_factor: f32,
        vertical_radius_center_factor: f32,
    },
}

/// One `configured_carver`.
#[derive(Debug)]
struct Configured {
    kind: Kind,
    probability: f32,
    y: HeightProvider,
    y_scale: FloatProvider,
    lava_level: i32,
    replaceable: BlockSet,
}

/// Every configured carver, and which biomes run which.
pub struct Carvers {
    /// Per biome id, its carvers in order.
    per_biome: Vec<Vec<Arc<Configured>>>,
    /// The biome each source chunk's carvers come from, memoized: every chunk
    /// asks about its 289 neighbours.
    source_biomes: std::sync::Mutex<super::FxHashMap<(i32, i32), super::biome::BiomeId>>,
    lava: BlockStateId,
    water: BlockStateId,
    air: BlockStateId,
    grass: u16,
    mycelium: u16,
    dirt: u16,
}

impl Carvers {
    /// Read the pack's carvers and biome carver lists.
    pub fn load(
        pack: &DataPack,
        biomes: &BiomeRegistry,
        min_y: i32,
        height: i32,
    ) -> Result<Self, BuildError> {
        let tags = Tags::new(pack);
        let mut by_name: HashMap<String, Option<Arc<Configured>>> = HashMap::new();
        let mut per_biome = Vec::new();
        for b in biomes.all() {
            let mut list = Vec::new();
            for name in &b.carvers {
                let c = match by_name.get(name) {
                    Some(c) => c.clone(),
                    None => {
                        let j = pack.read_json("configured_carver", name)?;
                        let c = parse(&j, &tags, min_y, height)
                            .map_err(|e| BuildError::new(format!("{name}: {e}")))?
                            .map(Arc::new);
                        by_name.insert(name.clone(), c.clone());
                        c
                    }
                };
                // A carver type this port does not know (the nether's) keeps
                // its slot: the index seeds the next carver's random.
                list.push(c);
            }
            per_biome.push(
                list.into_iter()
                    .map(|c| c.unwrap_or_else(|| Arc::new(noop())))
                    .collect(),
            );
        }
        let st = |n: &str| {
            blockinfo::parse_state(n).ok_or_else(|| BuildError::new(format!("no block `{n}`")))
        };
        Ok(Self {
            per_biome,
            source_biomes: std::sync::Mutex::new(Default::default()),
            lava: st("minecraft:lava[level=0]")?,
            water: st("minecraft:water[level=0]")?,
            air: st("minecraft:air")?,
            grass: blockinfo::block_of(st("minecraft:grass_block")?),
            mycelium: blockinfo::block_of(st("minecraft:mycelium")?),
            dirt: blockinfo::block_of(st("minecraft:dirt")?),
        })
    }

    /// Carve one chunk: replay every carver started within eight chunks.
    pub(crate) fn carve(
        &self,
        core: &Core,
        chunk: &mut ProtoChunk,
        terrain: &ChunkTerrain,
        _quarts: &QuartCache,
        env: &impl SurfaceEnv,
    ) {
        let mut ctx = Ctx {
            carvers: self,
            core,
            terrain,
            env,
            mask: vec![false; 256 * chunk.height as usize],
        };
        let (cx, cz) = (chunk.cx, chunk.cz);
        for dx in -8..=8 {
            for dz in -8..=8 {
                let (sx, sz) = (cx + dx, cz + dz);
                let cached = self.source_biomes.lock().unwrap().get(&(sx, sz)).copied();
                let biome = match cached {
                    Some(b) => b,
                    None => {
                        let b = core.noise_biome(sx * 4, 0, sz * 4);
                        let mut m = self.source_biomes.lock().unwrap();
                        if m.len() > 200_000 {
                            m.clear();
                        }
                        m.insert((sx, sz), b);
                        b
                    }
                };
                for (i, c) in self.per_biome[biome as usize].iter().enumerate() {
                    let mut r = WorldgenRandom::legacy(0);
                    r.set_large_feature_seed(core.seed.wrapping_add(i as i64), sx, sz);
                    if r.next_float() <= c.probability {
                        ctx.run(chunk, c, &mut r, sx, sz);
                    }
                }
            }
        }
    }
}

fn noop() -> Configured {
    Configured {
        kind: Kind::Cave {
            horizontal: FloatProvider::Constant(0.0),
            vertical: FloatProvider::Constant(0.0),
            floor: FloatProvider::Constant(0.0),
        },
        probability: -1.0,
        y: HeightProvider::Constant(0),
        y_scale: FloatProvider::Constant(0.0),
        lava_level: 0,
        replaceable: BlockSet::empty(),
    }
}

fn parse(j: &Json, tags: &Tags, min_y: i32, height: i32) -> Result<Option<Configured>, BuildError> {
    let cfg = j
        .get("config")
        .ok_or_else(|| BuildError::new("no config"))?;
    let fp = |k: &str| -> Result<FloatProvider, BuildError> {
        FloatProvider::parse(
            cfg.get(k)
                .ok_or_else(|| BuildError::new(format!("no `{k}`")))?,
        )
    };
    let kind = match j.str_of("type").unwrap_or("") {
        "minecraft:cave" => Kind::Cave {
            horizontal: fp("horizontal_radius_multiplier")?,
            vertical: fp("vertical_radius_multiplier")?,
            floor: fp("floor_level")?,
        },
        "minecraft:canyon" => {
            let shape = cfg
                .get("shape")
                .ok_or_else(|| BuildError::new("canyon: no shape"))?;
            let sp = |k: &str| -> Result<FloatProvider, BuildError> {
                FloatProvider::parse(
                    shape
                        .get(k)
                        .ok_or_else(|| BuildError::new(format!("no `{k}`")))?,
                )
            };
            Kind::Canyon {
                vertical_rotation: fp("vertical_rotation")?,
                distance_factor: sp("distance_factor")?,
                thickness: sp("thickness")?,
                width_smoothness: shape.i32_or("width_smoothness", 3),
                horizontal_radius_factor: sp("horizontal_radius_factor")?,
                vertical_radius_default_factor: shape.f64_or("vertical_radius_default_factor", 1.0)
                    as f32,
                vertical_radius_center_factor: shape.f64_or("vertical_radius_center_factor", 0.0)
                    as f32,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(Configured {
        kind,
        probability: cfg.f64_or("probability", 0.0) as f32,
        y: HeightProvider::parse(
            cfg.get("y").ok_or_else(|| BuildError::new("no `y`"))?,
            min_y,
            height,
        )?,
        y_scale: fp("yScale")?,
        lava_level: Anchor::parse(
            cfg.get("lava_level")
                .ok_or_else(|| BuildError::new("no `lava_level`"))?,
        )?
        .resolve(min_y, height),
        replaceable: tags.holder_set(cfg.get("replaceable").unwrap_or(&Json::Null)),
    }))
}

/// How a carve step decides to leave a cell alone.
enum Skip<'a> {
    Cave { floor: f64 },
    Canyon { widths: &'a [f32] },
}

struct Ctx<'a, E: SurfaceEnv> {
    carvers: &'a Carvers,
    core: &'a Core,
    terrain: &'a ChunkTerrain<'a>,
    env: &'a E,
    mask: Vec<bool>,
}

impl<E: SurfaceEnv> Ctx<'_, E> {
    fn run(
        &mut self,
        chunk: &mut ProtoChunk,
        c: &Configured,
        r: &mut WorldgenRandom,
        sx: i32,
        sz: i32,
    ) {
        match &c.kind {
            Kind::Cave {
                horizontal,
                vertical,
                floor,
            } => {
                let range = (4 * 2 - 1) * 16;
                let n0 = r.next_int_bounded(15) + 1;
                let n1 = r.next_int_bounded(n0) + 1;
                let count = r.next_int_bounded(n1);
                for _ in 0..count {
                    let x = (sx * 16 + r.next_int_bounded(16)) as f64;
                    let y = c.y.sample(r) as f64;
                    let z = (sz * 16 + r.next_int_bounded(16)) as f64;
                    let hr = horizontal.sample(r) as f64;
                    let vr = vertical.sample(r) as f64;
                    let fl = floor.sample(r) as f64;
                    let skip = Skip::Cave { floor: fl };
                    let mut tunnels = 1;
                    if r.next_int_bounded(4) == 0 {
                        let ys = c.y_scale.sample(r) as f64;
                        let thickness = 1.0 + r.next_float() * 6.0;
                        let h = 1.5 + (mth::sin((PI / 2.0) as f32 as f64) * thickness) as f64;
                        self.ellipsoid(chunk, c, x + 1.0, y, z, h, h * ys, &skip);
                        tunnels += r.next_int_bounded(4);
                    }
                    for _ in 0..tunnels {
                        let yaw = r.next_float() * (PI * 2.0) as f32;
                        let pitch = (r.next_float() - 0.5) / 4.0;
                        let mut thickness = r.next_float() * 2.0 + r.next_float();
                        if r.next_int_bounded(10) == 0 {
                            thickness *= r.next_float() * r.next_float() * 3.0 + 1.0;
                        }
                        let end = range - r.next_int_bounded(range / 4);
                        let seed = r.next_long();
                        self.tunnel(
                            chunk, c, seed, x, y, z, hr, vr, thickness, yaw, pitch, 0, end, 1.0,
                            &skip,
                        );
                    }
                }
            }
            Kind::Canyon {
                vertical_rotation,
                distance_factor,
                thickness,
                width_smoothness,
                horizontal_radius_factor,
                vertical_radius_default_factor,
                vertical_radius_center_factor,
            } => {
                let range = (4 * 2 - 1) * 16;
                let x = (sx * 16 + r.next_int_bounded(16)) as f64;
                let y = c.y.sample(r);
                let z = (sz * 16 + r.next_int_bounded(16)) as f64;
                let yaw = r.next_float() * (PI * 2.0) as f32;
                let pitch = vertical_rotation.sample(r);
                let y_scale = c.y_scale.sample(r) as f64;
                let thick = thickness.sample(r);
                let end = (range as f32 * distance_factor.sample(r)) as i32;
                let seed = r.next_long();
                self.canyon(
                    chunk,
                    c,
                    seed,
                    x,
                    y as f64,
                    z,
                    thick,
                    yaw,
                    pitch,
                    end,
                    y_scale,
                    *width_smoothness,
                    horizontal_radius_factor,
                    *vertical_radius_default_factor,
                    *vertical_radius_center_factor,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tunnel(
        &mut self,
        chunk: &mut ProtoChunk,
        c: &Configured,
        seed: i64,
        mut x: f64,
        mut y: f64,
        mut z: f64,
        hrm: f64,
        vrm: f64,
        thickness: f32,
        mut yaw: f32,
        mut pitch: f32,
        start: i32,
        end: i32,
        y_scale: f64,
        skip: &Skip,
    ) {
        let mut r = LegacyRandom::new(seed);
        let branch = r.next_int_bounded(end / 2) + end / 4;
        let steep = r.next_int_bounded(6) == 0;
        let mut yaw_v = 0.0f32;
        let mut pitch_v = 0.0f32;
        for i in start..end {
            let hr = 1.5
                + (mth::sin((std::f32::consts::PI * i as f32 / end as f32) as f64) * thickness)
                    as f64;
            let vr = hr * y_scale;
            let cp = mth::cos(pitch as f64);
            x += (mth::cos(yaw as f64) * cp) as f64;
            y += mth::sin(pitch as f64) as f64;
            z += (mth::sin(yaw as f64) * cp) as f64;
            pitch *= if steep { 0.92 } else { 0.7 };
            pitch += pitch_v * 0.1;
            yaw += yaw_v * 0.1;
            pitch_v *= 0.9;
            yaw_v *= 0.75;
            pitch_v += (r.next_float() - r.next_float()) * r.next_float() * 2.0;
            yaw_v += (r.next_float() - r.next_float()) * r.next_float() * 4.0;
            if i == branch && thickness > 1.0 {
                let s1 = r.next_long();
                let t1 = r.next_float() * 0.5 + 0.5;
                self.tunnel(
                    chunk,
                    c,
                    s1,
                    x,
                    y,
                    z,
                    hrm,
                    vrm,
                    t1,
                    yaw - (PI / 2.0) as f32,
                    pitch / 3.0,
                    i,
                    end,
                    1.0,
                    skip,
                );
                let s2 = r.next_long();
                let t2 = r.next_float() * 0.5 + 0.5;
                self.tunnel(
                    chunk,
                    c,
                    s2,
                    x,
                    y,
                    z,
                    hrm,
                    vrm,
                    t2,
                    yaw + (PI / 2.0) as f32,
                    pitch / 3.0,
                    i,
                    end,
                    1.0,
                    skip,
                );
                return;
            }
            if r.next_int_bounded(4) != 0 {
                if !can_reach(chunk, x, z, i, end, thickness) {
                    return;
                }
                self.ellipsoid(chunk, c, x, y, z, hr * hrm, vr * vrm, skip);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn canyon(
        &mut self,
        chunk: &mut ProtoChunk,
        c: &Configured,
        seed: i64,
        mut x: f64,
        mut y: f64,
        mut z: f64,
        thickness: f32,
        mut yaw: f32,
        mut pitch: f32,
        end: i32,
        y_scale: f64,
        width_smoothness: i32,
        hrf: &FloatProvider,
        vrd: f32,
        vrc: f32,
    ) {
        let mut r = LegacyRandom::new(seed);
        let depth = chunk.height as usize;
        let mut widths = vec![0f32; depth];
        let mut f = 1.0f32;
        for (i, w) in widths.iter_mut().enumerate() {
            if i == 0 || r.next_int_bounded(width_smoothness) == 0 {
                f = 1.0 + r.next_float() * r.next_float();
            }
            *w = f * f;
        }
        let skip = Skip::Canyon { widths: &widths };
        let mut yaw_v = 0.0f32;
        let mut pitch_v = 0.0f32;
        for i in 0..end {
            let mut hr = 1.5
                + (mth::sin((i as f32 * std::f32::consts::PI / end as f32) as f64) * thickness)
                    as f64;
            let mut vr = hr * y_scale;
            hr *= hrf.sample(&mut r) as f64;
            // updateVerticalRadius
            let t = 1.0 - (0.5 - i as f32 / end as f32).abs() * 2.0;
            let factor = vrd + vrc * t;
            vr = factor as f64 * vr * (r.next_float() * (1.0 - 0.75) + 0.75) as f64;
            let cp = mth::cos(pitch as f64);
            let sp = mth::sin(pitch as f64);
            x += (mth::cos(yaw as f64) * cp) as f64;
            y += sp as f64;
            z += (mth::sin(yaw as f64) * cp) as f64;
            pitch *= 0.7;
            pitch += pitch_v * 0.05;
            yaw += yaw_v * 0.05;
            pitch_v *= 0.8;
            yaw_v *= 0.5;
            pitch_v += (r.next_float() - r.next_float()) * r.next_float() * 2.0;
            yaw_v += (r.next_float() - r.next_float()) * r.next_float() * 4.0;
            if r.next_int_bounded(4) != 0 {
                if !can_reach(chunk, x, z, i, end, thickness) {
                    return;
                }
                self.ellipsoid(chunk, c, x, y, z, hr, vr, &skip);
            }
        }
    }

    /// `carveEllipsoid`.
    #[allow(clippy::too_many_arguments)]
    fn ellipsoid(
        &mut self,
        chunk: &mut ProtoChunk,
        c: &Configured,
        x: f64,
        y: f64,
        z: f64,
        hr: f64,
        vr: f64,
        skip: &Skip,
    ) {
        let mid_x = (chunk.min_x() + 8) as f64;
        let mid_z = (chunk.min_z() + 8) as f64;
        let reach = 16.0 + hr * 2.0;
        if (x - mid_x).abs() > reach || (z - mid_z).abs() > reach {
            return;
        }
        let (min_x, min_z) = (chunk.min_x(), chunk.min_z());
        let min_gen_y = chunk.min_y;
        let x0 = (mth::floor(x - hr) - min_x - 1).max(0);
        let x1 = (mth::floor(x + hr) - min_x).min(15);
        let y0 = (mth::floor(y - vr) - 1).max(min_gen_y + 1);
        let y1 = (mth::floor(y + vr) + 1).min(min_gen_y + chunk.height - 1 - 7);
        let z0 = (mth::floor(z - hr) - min_z - 1).max(0);
        let z1 = (mth::floor(z + hr) - min_z).min(15);
        for lx in x0..=x1 {
            let wx = min_x + lx;
            let dx = (wx as f64 + 0.5 - x) / hr;
            for lz in z0..=z1 {
                let wz = min_z + lz;
                let dz = (wz as f64 + 0.5 - z) / hr;
                if dx * dx + dz * dz >= 1.0 {
                    continue;
                }
                let mut hit_grass = false;
                let mut yy = y1;
                while yy > y0 {
                    let dy = (yy as f64 - 0.5 - y) / vr;
                    let skipped = match skip {
                        Skip::Cave { floor } => dy <= *floor || dx * dx + dy * dy + dz * dz >= 1.0,
                        Skip::Canyon { widths } => {
                            let k = (yy - min_gen_y) as usize;
                            (dx * dx + dz * dz) * widths[k - 1] as f64 + dy * dy / 6.0 >= 1.0
                        }
                    };
                    let mi = ((((yy - min_gen_y) as usize) * 16) + lz as usize) * 16 + lx as usize;
                    if !skipped && !self.mask[mi] {
                        self.mask[mi] = true;
                        self.carve_block(chunk, c, lx as usize, yy, lz as usize, &mut hit_grass);
                    }
                    yy -= 1;
                }
            }
        }
    }

    /// `carveBlock`.
    fn carve_block(
        &mut self,
        chunk: &mut ProtoChunk,
        c: &Configured,
        lx: usize,
        y: i32,
        lz: usize,
        hit_grass: &mut bool,
    ) {
        let cv = self.carvers;
        let s = chunk.get(lx, y, lz);
        let b = blockinfo::block_of(s);
        if b == cv.grass || b == cv.mycelium {
            *hit_grass = true;
        }
        if !c.replaceable.contains(s) {
            return;
        }
        let (wx, wz) = (chunk.min_x() + lx as i32, chunk.min_z() + lz as i32);
        let carved = if y <= c.lava_level {
            cv.lava
        } else {
            match self.terrain.aquifer_fill(wx, y, wz, 0.0) {
                Fill::Solid => return,
                Fill::Air => cv.air,
                Fill::Water => cv.water,
                Fill::Lava => cv.lava,
            }
        };
        chunk.set(lx, y, lz, carved);
        if *hit_grass && blockinfo::block_of(chunk.get(lx, y - 1, lz)) == cv.dirt {
            let fluid = blockinfo::has_fluid(carved);
            if let Some(top) = self.core.surface.top_material(
                chunk,
                self.env,
                &self.core.biomes,
                wx,
                y - 1,
                wz,
                fluid,
            ) {
                chunk.set(lx, y - 1, lz, top);
            }
        }
    }
}

/// `WorldCarver.canReach`.
fn can_reach(chunk: &ProtoChunk, x: f64, z: f64, i: i32, end: i32, thickness: f32) -> bool {
    let dx = x - (chunk.min_x() + 8) as f64;
    let dz = z - (chunk.min_z() + 8) as f64;
    let left = (end - i) as f64;
    let reach = (thickness + 2.0 + 16.0) as f64;
    dx * dx + dz * dz - left * left <= reach * reach
}
