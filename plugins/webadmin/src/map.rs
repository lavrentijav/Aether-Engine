//! Top-down map tiles: the top block of every column, coloured and shaded.
//!
//! A tile is a square of raw RGBA pixels, one pixel per column at zoom 0 and
//! one per `2^zoom` columns above it. Nothing is encoded (no PNG) and nothing
//! is drawn client-side beyond an `ImageData` blit: the browser asks for a
//! tile by `(zoom, tx, tz)` and paints the bytes, so zooming out is coarser
//! tiles rather than a scaled canvas and the palette stays sharp.
//!
//! Tiles are cached by key and **re-rendered on change**: when a block is
//! edited, [`MapRenderer::invalidate`] drops every cached tile, at every zoom,
//! whose bounds contain that column. The next request after that rebuilds the
//! tile from the live world, so the map follows edits without a full re-render
//! and without a dirty-region bookkeeping layer.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use aether_world::BlockStateId;

use crate::color;
use crate::source::{block_name_of, MapSource};

/// Default tile edge, in pixels (and thus columns at zoom 0).
pub const DEFAULT_TILE_PX: u32 = 128;
/// Highest zoom: `2^6 = 64` columns per pixel.
pub const MAX_ZOOM: u8 = 6;

/// Which tile, at which zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// `0` is one block per pixel; `z` is `2^z` blocks per pixel.
    pub zoom: u8,
    pub tx: i32,
    pub tz: i32,
}

impl TileKey {
    pub fn new(zoom: u8, tx: i32, tz: i32) -> Self {
        Self {
            zoom: zoom.min(MAX_ZOOM),
            tx,
            tz,
        }
    }

    /// The block column at the tile's north-west corner.
    pub fn origin(&self, px: u32) -> (i32, i32) {
        let span = px as i32 * blocks_per_pixel(self.zoom);
        (self.tx * span, self.tz * span)
    }
}

/// How many block columns one pixel covers at `zoom`.
pub fn blocks_per_pixel(zoom: u8) -> i32 {
    1 << zoom.min(MAX_ZOOM)
}

/// One rendered tile.
pub struct Tile {
    pub key: TileKey,
    /// Edge length in pixels.
    pub px: u32,
    /// `px * px * 4`, row-major, RGBA. A pixel with no surface has alpha 0.
    pub rgba: Vec<u8>,
    /// `px * px`, the surface Y of each pixel, `i16::MIN` when none.
    pub surface_y: Vec<i16>,
}

impl Tile {
    /// The pixel at `(px, pz)`, or `[0, 0, 0, 0]` when out of range.
    pub fn pixel(&self, px: u32, pz: u32) -> [u8; 4] {
        if px >= self.px || pz >= self.px {
            return [0, 0, 0, 0];
        }
        let i = ((pz * self.px + px) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// The surface Y at `(px, pz)`, or `i16::MIN` for no surface.
    pub fn surface(&self, px: u32, pz: u32) -> i16 {
        if px >= self.px || pz >= self.px {
            return i16::MIN;
        }
        self.surface_y[(pz * self.px + px) as usize]
    }
}

/// The surface of one column as the map sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    pub id: BlockStateId,
    pub y: i32,
    /// Water depth above the floor, for tinting.
    pub water_depth: i32,
    /// Whether the surface is water (tinted by depth).
    pub is_water: bool,
}

impl Surface {
    /// The map pixel for this surface, shaded against a north-west neighbour.
    pub fn color(&self, shade: f32) -> [u8; 4] {
        let mut c = color::color_of(self.id);
        let mut f = shade;
        if self.is_water {
            f *= (1.0 - self.water_depth as f32 * 0.03).max(0.45);
        }
        for v in &mut c {
            *v = (*v as f32 * f).clamp(0.0, 255.0) as u8;
        }
        [c[0], c[1], c[2], 255]
    }
}

/// Renders and caches map tiles over a [`MapSource`].
pub struct MapRenderer {
    source: Arc<dyn MapSource>,
    y_min: i32,
    y_max: i32,
    tile_px: u32,
    /// Whether to shade by height difference from the north-west neighbour.
    shade: bool,
    cache: RwLock<HashMap<TileKey, Arc<Tile>>>,
}

impl MapRenderer {
    /// A renderer over `source`, at the default tile size and height band.
    pub fn new(source: Arc<dyn MapSource>) -> Self {
        let (y_min, y_max) = (source.y_min(), source.y_max());
        Self {
            source,
            y_min,
            y_max,
            tile_px: DEFAULT_TILE_PX,
            shade: true,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Override the tile edge, in pixels.
    pub fn with_tile_px(mut self, px: u32) -> Self {
        self.tile_px = px.max(1);
        self
    }

    /// Override the vertical band scanned for the surface.
    pub fn with_height_range(mut self, y_min: i32, y_max: i32) -> Self {
        self.y_min = y_min;
        self.y_max = y_max.max(y_min);
        self
    }

    /// Turn height shading off (a flat, purely per-block palette).
    pub fn with_shading(mut self, shade: bool) -> Self {
        self.shade = shade;
        self
    }

    /// The tile edge in pixels.
    pub fn tile_px(&self) -> u32 {
        self.tile_px
    }

    /// How many tiles are cached.
    pub fn cached_tiles(&self) -> usize {
        self.cache.read().unwrap().len()
    }

    /// Drop every cached tile that contains block `(x, z)`, at every zoom.
    pub fn invalidate(&self, x: i32, z: i32) {
        let mut cache = self.cache.write().unwrap();
        cache.retain(|key, _| {
            let (ox, oz) = key.origin(self.tile_px);
            let span = self.tile_px as i32 * blocks_per_pixel(key.zoom);
            !(x >= ox && x < ox + span && z >= oz && z < oz + span)
        });
    }

    /// Drop the whole cache.
    pub fn invalidate_all(&self) {
        self.cache.write().unwrap().clear();
    }

    /// Render (or return the cached) tile.
    pub fn render(&self, key: TileKey) -> Arc<Tile> {
        if let Some(t) = self.cache.read().unwrap().get(&key) {
            return Arc::clone(t);
        }
        let tile = Arc::new(self.render_uncached(key));
        self.cache.write().unwrap().insert(key, Arc::clone(&tile));
        tile
    }

    /// The surface of the column at `(x, z)` in the configured band.
    pub fn surface(&self, x: i32, z: i32) -> Option<Surface> {
        // A bounded scan rather than `MapSource::surface`, so the renderer's
        // configured band wins and a mock need not implement the default.
        let mut y = self.y_max;
        while y >= self.y_min {
            let id = self.source.block(x, y, z);
            let name = self
                .source
                .block_name(id)
                .unwrap_or_else(|| block_name_of(id));
            if !crate::source::is_air_state(&name) {
                let is_water = name.starts_with("minecraft:water");
                let mut depth = 0;
                if is_water {
                    let mut d = y;
                    while d >= self.y_min {
                        let below = self.source.block(x, d, z);
                        let bn = self
                            .source
                            .block_name(below)
                            .unwrap_or_else(|| block_name_of(below));
                        if bn.starts_with("minecraft:water") {
                            d -= 1;
                            depth += 1;
                        } else {
                            break;
                        }
                    }
                }
                return Some(Surface {
                    id,
                    y,
                    water_depth: depth,
                    is_water,
                });
            }
            y -= 1;
        }
        None
    }

    fn render_uncached(&self, key: TileKey) -> Tile {
        let px = self.tile_px;
        let bpp = blocks_per_pixel(key.zoom);
        let (ox, oz) = key.origin(px);
        let mut rgba = vec![0u8; (px * px * 4) as usize];
        let mut surface_y = vec![i16::MIN; (px * px) as usize];
        for pz in 0..px {
            for pix in 0..px {
                let x = ox + pix as i32 * bpp;
                let z = oz + pz as i32 * bpp;
                let Some(s) = self.surface(x, z) else {
                    continue;
                };
                let idx = (pz * px + pix) as usize;
                surface_y[idx] = s.y.clamp(i16::MIN as i32 + 1, i16::MAX as i32) as i16;
                let shade = self.shade_factor(&surface_y, px, pix, pz, s.y);
                let c = s.color(shade);
                rgba[idx * 4..idx * 4 + 4].copy_from_slice(&c);
            }
        }
        Tile {
            key,
            px,
            rgba,
            surface_y,
        }
    }

    fn shade_factor(&self, surface_y: &[i16], px: u32, pix: u32, pz: u32, y: i32) -> f32 {
        if !self.shade || pix == 0 || pz == 0 {
            return 1.0;
        }
        let nw = surface_y[((pz - 1) * px + pix - 1) as usize];
        if nw == i16::MIN {
            return 1.0;
        }
        let d = (y - nw as i32).clamp(-4, 4) as f32;
        1.0 + d * 0.06
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_world::registry::ids;
    use aether_world::BlockStateId;
    use std::collections::HashMap as Map;
    use std::sync::Mutex;

    /// A column-height mock: `x, z` -> `(block, top_y)`.
    struct MockSource {
        columns: Mutex<Map<(i32, i32), (BlockStateId, i32)>>,
    }

    impl MapSource for MockSource {
        fn block(&self, x: i32, y: i32, z: i32) -> BlockStateId {
            match self.columns.lock().unwrap().get(&(x, z)) {
                Some(&(id, top)) => {
                    // Bedrock at 0, dirt below the surface block, surface at top.
                    if y == 0 {
                        ids::BEDROCK
                    } else if y > 0 && y < top {
                        ids::DIRT
                    } else if y == top {
                        id
                    } else {
                        BlockStateId::AIR
                    }
                }
                None => BlockStateId::AIR,
            }
        }
        fn block_name(&self, id: BlockStateId) -> Option<String> {
            if id == ids::GRASS_BLOCK {
                Some("minecraft:grass_block".to_string())
            } else if id == ids::WATER {
                Some("minecraft:water".to_string())
            } else if id == ids::DIRT {
                Some("minecraft:dirt".to_string())
            } else if id == ids::BEDROCK {
                Some("minecraft:bedrock".to_string())
            } else {
                Some("minecraft:air".to_string())
            }
        }
        fn y_min(&self) -> i32 {
            0
        }
        fn y_max(&self) -> i32 {
            8
        }
    }

    fn mock() -> Arc<MockSource> {
        let mut m = Map::new();
        m.insert((0, 0), (ids::GRASS_BLOCK, 4));
        m.insert((1, 0), (ids::WATER, 3));
        Arc::new(MockSource {
            columns: Mutex::new(m),
        })
    }

    #[test]
    fn renders_top_block_colors() {
        let r = MapRenderer::new(mock()).with_tile_px(2).with_shading(false);
        let t = r.render(TileKey::new(0, 0, 0));
        assert_eq!(t.pixel(0, 0), [96, 158, 64, 255]);
        // (1,0) is one-block-deep water: the base blue tinted by depth.
        assert_eq!(t.pixel(1, 0), [50, 87, 203, 255]);
        assert_eq!(t.pixel(1, 1), [0, 0, 0, 0]);
        assert_eq!(t.surface(0, 0), 4);
    }

    #[test]
    fn surfaces_are_cached_until_invalidated() {
        let r = MapRenderer::new(mock()).with_tile_px(2);
        let _ = r.render(TileKey::new(0, 0, 0));
        assert_eq!(r.cached_tiles(), 1);
        r.invalidate(0, 0);
        assert_eq!(r.cached_tiles(), 0);
    }

    #[test]
    fn invalidate_only_drops_covering_tiles() {
        let r = MapRenderer::new(mock()).with_tile_px(2);
        r.render(TileKey::new(0, 0, 0)); // covers x,z in 0..2
        r.render(TileKey::new(0, 1, 0)); // covers x in 2..4
        assert_eq!(r.cached_tiles(), 2);
        r.invalidate(3, 0);
        assert_eq!(r.cached_tiles(), 1);
        assert!(r
            .render(TileKey::new(0, 0, 0))
            .pixel(0, 0)
            .eq(&[96, 158, 64, 255]));
    }

    #[test]
    fn zoom_coarsens_and_shares_blocks() {
        assert_eq!(blocks_per_pixel(0), 1);
        assert_eq!(blocks_per_pixel(3), 8);
        // At zoom 1, one pixel covers columns 0 and 1; the north-west sample
        // (column 0) wins.
        let r = MapRenderer::new(mock()).with_tile_px(2).with_shading(false);
        let t = r.render(TileKey::new(1, 0, 0));
        assert_eq!(t.pixel(0, 0), [96, 158, 64, 255]);
    }

    #[test]
    fn water_is_tinted_by_depth_even_without_shading() {
        let r = MapRenderer::new(mock()).with_tile_px(2).with_shading(false);
        let deep = r.surface(1, 0).unwrap();
        let c = deep.color(1.0);
        assert!(c[2] > c[0], "deep water stays blue: {c:?}");
    }
}
