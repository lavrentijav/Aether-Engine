//! The vanilla recipe system, read from the operator's copy of the game data.
//!
//! Crafting, smelting, blasting, smoking, campfire cooking, stonecutting,
//! smithing and transmutation all live in `data/minecraft/recipe/*.json`, and
//! that is where this reads them from — the same pack the vanilla generator is
//! pointed at, so the two agree on what version the server speaks. Ingredients
//! may be item **tags** (`#minecraft:planks`), resolved recursively from
//! `data/minecraft/tags/item/*.json`; shaped recipes match either way round
//! (mirrored); and the crafting remainder (a milk bucket leaves a bucket) is
//! handled when the grid is consumed, in [`super::window`].
//!
//! When the server runs without a data pack — the built-in noise terrain — the
//! compiled-in table (`super::data::SHAPED` / `SHAPELESS`, generated from the
//! same source) answers instead, so crafting never silently stops working on a
//! packless server.
//!
//! # The recipe book
//!
//! The server deliberately does **not** send the client's recipe-book packets
//! (`Recipe Book Add/Remove`, and in modern releases the recipe registry is
//! part of the known pack the client already carries). A vanilla client is
//! happy with an empty book: it never unlocks a recipe, so it never offers to
//! place one. Crafting is server-authoritative anyway — the client's clicks are
//! checked here against the same recipes — so an empty book is a missing
//! convenience, not a correctness gap. The serverbound book packets
//! (`Place Recipe`, `Recipe Book Change Settings`, `Recipe Book Seen Recipe`)
//! are not recognised by any codec and decode to `ClientEvent::Ignored`, which
//! is the harmless outcome.

// Stonecutting and smithing recipes are loaded, indexed and tested, but no
// block UI drives them yet (there is no stonecutter or smithing-table window),
// so their accessors are dead code outside the tests. Transmutation, by
// contrast, is matched by the ordinary crafting grid (see `RecipeBook::craft`).
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aether_world::ItemTypeId;
use aether_worldgen::vanilla::json::Json;

use super::tables;

/// One ingredient: a concrete item, or a resolved item tag.
#[derive(Debug, Clone)]
enum Ing {
    Item(ItemTypeId),
    /// A tag's items, sorted and deduplicated for a binary search.
    Tag(Vec<ItemTypeId>),
}

impl Ing {
    fn matches(&self, item: ItemTypeId) -> bool {
        match self {
            Ing::Item(i) => *i == item,
            Ing::Tag(list) => list.binary_search(&item).is_ok(),
        }
    }
}

/// A shaped crafting recipe, its pattern trimmed to the occupied bounding box.
#[derive(Debug)]
struct Shaped {
    w: usize,
    h: usize,
    /// Row-major, `w * h`; `None` is a pattern cell that must be empty.
    cells: Vec<Option<Ing>>,
    result: ItemTypeId,
    count: u8,
}

/// A shapeless crafting recipe.
#[derive(Debug)]
struct Shapeless {
    ingredients: Vec<Ing>,
    result: ItemTypeId,
    count: u8,
}

/// The four cooking methods, each its own block family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cook {
    Smelting,
    Blasting,
    Smoking,
    Campfire,
}

/// What cooking an item produces.
#[derive(Debug, Clone, Copy)]
pub struct Cooked {
    pub result: ItemTypeId,
    pub count: u8,
    /// Ticks the recipe takes in its own block (200 / 100 / 100 / 600).
    pub ticks: u32,
    pub xp: f32,
}

#[derive(Debug)]
struct Cooking {
    method: Cook,
    ingredient: Ing,
    result: ItemTypeId,
    count: u8,
    ticks: u32,
    xp: f32,
}

#[derive(Debug)]
struct Stonecut {
    ingredient: Ing,
    result: ItemTypeId,
    count: u8,
}

#[derive(Debug)]
struct SmithTransform {
    template: Ing,
    base: Ing,
    addition: Ing,
    result: ItemTypeId,
}

#[derive(Debug)]
struct SmithTrim {
    template: Ing,
    base: Ing,
    addition: Ing,
}

#[derive(Debug)]
struct Transmute {
    input: Ing,
    material: Ing,
    result: ItemTypeId,
}

/// How many recipes of each kind the pack held, and what could not be run.
#[derive(Debug, Default, Clone)]
pub struct Counts {
    /// Every recipe file read.
    pub total: usize,
    pub shaped: usize,
    pub shapeless: usize,
    pub cooking: usize,
    pub stonecutting: usize,
    pub smithing: usize,
    pub transmute: usize,
    /// Recipes whose behaviour is dynamic and cannot be expressed as a table;
    /// listed in [`Counts::special`].
    pub special: usize,
    /// The special recipe ids, so the gap is visible rather than implied.
    pub special_list: Vec<String>,
    /// Recipe types this engine does not implement at all.
    pub unsupported: Vec<String>,
}

/// Every recipe of a pack, indexed for the lookups the game makes.
pub struct RecipeBook {
    shaped: Vec<Shaped>,
    shapeless: Vec<Shapeless>,
    cooking: Vec<Cooking>,
    stonecutting: Vec<Stonecut>,
    smith_transform: Vec<SmithTransform>,
    smith_trim: Vec<SmithTrim>,
    transmute: Vec<Transmute>,
    counts: Counts,
}

static BOOK: OnceLock<RecipeBook> = OnceLock::new();

/// A `Json::Null` with a stable address, for `get(...).unwrap_or(...)`.
static NULL: Json = Json::Null;

/// Member `key` of `j`, or a stable `null` when absent.
fn get<'a>(j: &'a Json, key: &str) -> &'a Json {
    j.get(key).unwrap_or(&NULL)
}

/// Load every recipe and item tag from the pack `root`.
pub fn load(root: &Path) -> Result<RecipeBook, String> {
    RecipeBook::load(root)
}

/// Install the recipes loaded from the game data, once, at startup.
pub fn install(book: RecipeBook) {
    let _ = BOOK.set(book);
}

/// The installed pack recipes, if a pack was loaded.
pub fn book() -> Option<&'static RecipeBook> {
    BOOK.get()
}

/// Locate `data/minecraft` from whatever the operator pointed `worldgen_data`
/// at — the pack root, `data/minecraft`, or `data/minecraft/worldgen`.
fn find_data_minecraft(root: &Path) -> Option<PathBuf> {
    let mut candidates = vec![root.join("data/minecraft"), root.to_path_buf()];
    if let Some(parent) = root.parent() {
        candidates.push(parent.to_path_buf());
    }
    candidates.into_iter().find(|c| c.join("recipe").is_dir())
}

impl RecipeBook {
    /// Load every recipe and item tag under the pack `root`.
    pub fn load(root: &Path) -> Result<Self, String> {
        if !root.exists() {
            return Err(format!("{} does not exist", root.display()));
        }
        let data_mc = find_data_minecraft(root).ok_or_else(|| {
            format!(
                "{} has no data/minecraft/recipe directory; point worldgen_data at an \
                 unpacked server jar (tools/fetch-vanilla-data.sh)",
                root.display()
            )
        })?;
        let tags = load_item_tags(&data_mc)?;
        let entries = read_json_dir(&data_mc.join("recipe"))?;
        Ok(Self::build(entries, &tags))
    }

    /// Build from already-parsed `(id, json)` pairs and resolved item tags.
    fn build(entries: Vec<(String, Json)>, tags: &HashMap<String, Vec<ItemTypeId>>) -> Self {
        let mut book = RecipeBook {
            shaped: Vec::new(),
            shapeless: Vec::new(),
            cooking: Vec::new(),
            stonecutting: Vec::new(),
            smith_transform: Vec::new(),
            smith_trim: Vec::new(),
            transmute: Vec::new(),
            counts: Counts::default(),
        };
        for (id, j) in &entries {
            book.counts.total += 1;
            book.add(id, j, tags);
        }
        book
    }

    fn ing(&self, j: &Json, tags: &HashMap<String, Vec<ItemTypeId>>) -> Option<Ing> {
        ing_of(j, tags)
    }

    /// Add one recipe. Unrecognised types are recorded, not fatal.
    fn add(&mut self, id: &str, j: &Json, tags: &HashMap<String, Vec<ItemTypeId>>) {
        let ty = j.str_of("type").unwrap_or("");
        match ty {
            "minecraft:crafting_shaped" => {
                if let Some(r) = self.shaped(j, tags) {
                    self.shaped.push(r);
                    self.counts.shaped += 1;
                } else {
                    self.counts.unsupported.push(id.to_string());
                }
            }
            "minecraft:crafting_shapeless" => {
                if let Some(r) = self.shapeless(j, tags) {
                    self.shapeless.push(r);
                    self.counts.shapeless += 1;
                } else {
                    self.counts.unsupported.push(id.to_string());
                }
            }
            "minecraft:smelting"
            | "minecraft:blasting"
            | "minecraft:smoking"
            | "minecraft:campfire_cooking" => {
                let method = match ty {
                    "minecraft:blasting" => Cook::Blasting,
                    "minecraft:smoking" => Cook::Smoking,
                    "minecraft:campfire_cooking" => Cook::Campfire,
                    _ => Cook::Smelting,
                };
                if let Some(r) = self.cooking(j, tags, method) {
                    self.cooking.push(r);
                    self.counts.cooking += 1;
                } else {
                    self.counts.unsupported.push(id.to_string());
                }
            }
            "minecraft:stonecutting" => {
                if let Some(r) = self.parse_stonecut(j, tags) {
                    self.stonecutting.push(r);
                    self.counts.stonecutting += 1;
                } else {
                    self.counts.unsupported.push(id.to_string());
                }
            }
            "minecraft:smithing_transform" => {
                if let (Some(template), Some(base), Some(addition), Some((result, _))) = (
                    self.ing(get(j, "template"), tags),
                    self.ing(get(j, "base"), tags),
                    self.ing(get(j, "addition"), tags),
                    result_of(j),
                ) {
                    self.smith_transform.push(SmithTransform {
                        template,
                        base,
                        addition,
                        result,
                    });
                    self.counts.smithing += 1;
                } else {
                    self.counts.unsupported.push(id.to_string());
                }
            }
            "minecraft:smithing_trim" => {
                if let (Some(template), Some(base), Some(addition)) = (
                    self.ing(get(j, "template"), tags),
                    self.ing(get(j, "base"), tags),
                    self.ing(get(j, "addition"), tags),
                ) {
                    self.smith_trim.push(SmithTrim {
                        template,
                        base,
                        addition,
                    });
                    self.counts.smithing += 1;
                } else {
                    self.counts.unsupported.push(id.to_string());
                }
            }
            "minecraft:crafting_transmute" => {
                if let (Some(input), Some(material), Some((result, _))) = (
                    self.ing(get(j, "input"), tags),
                    self.ing(get(j, "material"), tags),
                    result_of(j),
                ) {
                    self.transmute.push(Transmute {
                        input,
                        material,
                        result,
                    });
                    self.counts.transmute += 1;
                } else {
                    self.counts.unsupported.push(id.to_string());
                }
            }
            // Dynamic recipes with a real algorithm behind them (dye a piece
            // of armour, copy a book, extend a map, …). Recorded so the gap is
            // counted and named rather than silently dropped; a handful of the
            // families also ship a plain shaped/shapeless fallback which *is*
            // loaded above.
            ty if ty.starts_with("minecraft:crafting_special_")
                || ty == "minecraft:crafting_decorated_pot" =>
            {
                self.counts.special += 1;
                self.counts.special_list.push(id.to_string());
            }
            _ => self.counts.unsupported.push(id.to_string()),
        }
    }

    fn shaped(&self, j: &Json, tags: &HashMap<String, Vec<ItemTypeId>>) -> Option<Shaped> {
        let (result, count) = result_of(j)?;
        let key = j.get("key").and_then(Json::as_obj)?;
        let rows: Vec<&str> = j
            .get("pattern")
            .and_then(Json::as_arr)?
            .iter()
            .filter_map(Json::as_str)
            .collect();
        if rows.is_empty() {
            return None;
        }
        let w0 = rows.iter().map(|r| r.chars().count()).max()?;
        if w0 == 0 {
            return None;
        }
        // Trim to the bounding box of non-space characters, the way vanilla's
        // `ShapedRecipe` does before matching.
        let occupied = |r: &str| r.chars().any(|c| c != ' ');
        let min_y = rows.iter().position(|r| occupied(r))?;
        let max_y = rows.iter().rposition(|r| occupied(r))?;
        let mut min_x = w0;
        let mut max_x = 0;
        let mut any = false;
        for r in &rows[min_y..=max_y] {
            for (x, c) in r.chars().enumerate() {
                if c != ' ' {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    any = true;
                }
            }
        }
        if !any {
            return None;
        }
        let (w, h) = (max_x - min_x + 1, max_y - min_y + 1);
        let mut cells = Vec::with_capacity(w * h);
        for r in &rows[min_y..=max_y] {
            let chars: Vec<char> = r.chars().collect();
            for x in min_x..=max_x {
                let c = chars.get(x).copied().unwrap_or(' ');
                if c == ' ' {
                    cells.push(None);
                } else {
                    let k = key.get(&c.to_string())?;
                    cells.push(Some(self.ing(k, tags)?));
                }
            }
        }
        Some(Shaped {
            w,
            h,
            cells,
            result,
            count,
        })
    }

    fn shapeless(&self, j: &Json, tags: &HashMap<String, Vec<ItemTypeId>>) -> Option<Shapeless> {
        let (result, count) = result_of(j)?;
        let mut ingredients = Vec::new();
        for v in j.get("ingredients").and_then(Json::as_arr)? {
            ingredients.push(self.ing(v, tags)?);
        }
        if ingredients.is_empty() {
            return None;
        }
        Some(Shapeless {
            ingredients,
            result,
            count,
        })
    }

    fn cooking(
        &self,
        j: &Json,
        tags: &HashMap<String, Vec<ItemTypeId>>,
        method: Cook,
    ) -> Option<Cooking> {
        let (result, count) = result_of(j)?;
        let ingredient = self.ing(get(j, "ingredient"), tags)?;
        // The pack's `cookingtime` is already per method (200/100/100/600);
        // fall back to the vanilla default when it is absent.
        let default = match method {
            Cook::Smelting => 200,
            Cook::Blasting | Cook::Smoking => 100,
            Cook::Campfire => 600,
        };
        let ticks = j.i32_or("cookingtime", default).clamp(1, 100_000) as u32;
        let xp = j.get("experience").and_then(Json::as_f64).unwrap_or(0.0) as f32;
        Some(Cooking {
            method,
            ingredient,
            result,
            count,
            ticks,
            xp,
        })
    }

    fn parse_stonecut(
        &self,
        j: &Json,
        tags: &HashMap<String, Vec<ItemTypeId>>,
    ) -> Option<Stonecut> {
        let (result, count) = result_of(j)?;
        let ingredient = self.ing(get(j, "ingredient"), tags)?;
        Some(Stonecut {
            ingredient,
            result,
            count,
        })
    }

    /// How many recipes of each kind were loaded.
    pub fn counts(&self) -> &Counts {
        &self.counts
    }

    /// Match a crafting grid against every recipe. `grid` is row-major,
    /// `width` wide (2 for the player grid, 3 for a table); `None` is empty.
    pub fn craft(&self, grid: &[Option<ItemTypeId>], width: usize) -> Option<(ItemTypeId, u8)> {
        if width == 0 || grid.len() % width != 0 {
            return None;
        }
        let height = grid.len() / width;
        let (min_x, max_x, min_y, max_y, n) = bounding_box(grid, width, height)?;
        let (bw, bh) = (max_x - min_x + 1, max_y - min_y + 1);
        let cell = |x: usize, y: usize| grid[(min_y + y) * width + min_x + x];

        for r in &self.shaped {
            if r.w == bw && r.h == bh && shaped_matches(r, cell, bw, bh) {
                return Some((r.result, r.count));
            }
        }
        let items: Vec<ItemTypeId> = grid.iter().flatten().copied().collect();
        if items.len() != n {
            return None;
        }
        for r in &self.shapeless {
            if r.ingredients.len() == items.len() && shapeless_matches(&r.ingredients, &items) {
                return Some((r.result, r.count));
            }
        }
        // Transmutation (`crafting_transmute`) is a crafting-grid recipe with
        // exactly two items, one matching the input and one the material.
        if items.len() == 2 {
            for t in &self.transmute {
                let (a, b) = (items[0], items[1]);
                let input = if t.input.matches(a) && t.material.matches(b) {
                    a
                } else if t.input.matches(b) && t.material.matches(a) {
                    b
                } else {
                    continue;
                };
                // Vanilla refuses the recipe when it would return the input
                // unchanged (dyeing an already-black bundle black).
                if input != t.result {
                    return Some((t.result, 1));
                }
            }
        }
        None
    }

    /// What cooking `input` by `method` produces.
    pub fn cook(&self, method: Cook, input: ItemTypeId) -> Option<Cooked> {
        self.cooking
            .iter()
            .find(|c| c.method == method && c.ingredient.matches(input))
            .map(|c| Cooked {
                result: c.result,
                count: c.count,
                ticks: c.ticks,
                xp: c.xp,
            })
    }

    /// Whether any method cooks `input`.
    pub fn cooks(&self, input: ItemTypeId) -> bool {
        self.cooking.iter().any(|c| c.ingredient.matches(input))
    }

    /// Every stonecutter output for `input`, in pack order.
    pub fn stonecut(&self, input: ItemTypeId) -> Vec<(ItemTypeId, u8)> {
        self.stonecutting
            .iter()
            .filter(|s| s.ingredient.matches(input))
            .map(|s| (s.result, s.count))
            .collect()
    }

    /// The result of applying `addition` to `base` with `template` at a
    /// smithing table (transform recipes), or the base itself for a trim.
    pub fn smithing(
        &self,
        template: ItemTypeId,
        base: ItemTypeId,
        addition: ItemTypeId,
    ) -> Option<ItemTypeId> {
        if let Some(r) = self.smith_transform.iter().find(|r| {
            r.template.matches(template) && r.base.matches(base) && r.addition.matches(addition)
        }) {
            return Some(r.result);
        }
        self.smith_trim
            .iter()
            .find(|r| {
                r.template.matches(template) && r.base.matches(base) && r.addition.matches(addition)
            })
            .map(|_| base)
    }

    /// The result of dyeing/transmuting `input` with `material`.
    pub fn transmute(&self, input: ItemTypeId, material: ItemTypeId) -> Option<ItemTypeId> {
        self.transmute
            .iter()
            .find(|t| t.input.matches(input) && t.material.matches(material))
            .map(|t| t.result)
    }
}

/// The occupied bounding box of a grid and how many cells it holds.
fn bounding_box(
    grid: &[Option<ItemTypeId>],
    width: usize,
    height: usize,
) -> Option<(usize, usize, usize, usize, usize)> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (width, height, 0, 0);
    let mut n = 0;
    for y in 0..height {
        for x in 0..width {
            if grid[y * width + x].is_some() {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                n += 1;
            }
        }
    }
    if n == 0 {
        None
    } else {
        Some((min_x, max_x, min_y, max_y, n))
    }
}

/// Whether a shaped recipe's pattern matches the grid's bounding box, straight
/// or mirrored.
fn shaped_matches(
    r: &Shaped,
    cell: impl Fn(usize, usize) -> Option<ItemTypeId>,
    bw: usize,
    bh: usize,
) -> bool {
    let mut straight = true;
    let mut mirrored = true;
    for y in 0..bh {
        for x in 0..bw {
            let item = cell(x, y);
            if !pattern_cell_matches(&r.cells[y * bw + x], item) {
                straight = false;
            }
            if !pattern_cell_matches(&r.cells[y * bw + (bw - 1 - x)], item) {
                mirrored = false;
            }
        }
    }
    straight || mirrored
}

fn pattern_cell_matches(pattern: &Option<Ing>, item: Option<ItemTypeId>) -> bool {
    match pattern {
        None => item.is_none(),
        Some(_) => item.is_some_and(|i| pattern.as_ref().is_some_and(|p| p.matches(i))),
    }
}

/// Whether every ingredient can be paired with a distinct grid item — a
/// perfect bipartite matching, not a greedy assignment. Greedy is unsound when
/// an ingredient's set overlaps a later one: ingredients `[{a,b}, {a}]` with
/// items `[a, b]` place `a→{a,b}`, leaving `b` nowhere, yet `a→{a}, b→{a,b}`
/// works. Kuhn's augmenting-path algorithm finds it (≤ 9 ingredients).
fn shapeless_matches(ings: &[Ing], items: &[ItemTypeId]) -> bool {
    if ings.len() != items.len() {
        return false;
    }
    // `item_of_ing[i]` is the grid item currently paired with ingredient `i`.
    let mut item_of_ing = vec![usize::MAX; ings.len()];
    for i in 0..ings.len() {
        let mut seen = vec![false; items.len()];
        if !augment(ings, items, i, &mut seen, &mut item_of_ing) {
            return false;
        }
    }
    true
}

fn augment(
    ings: &[Ing],
    items: &[ItemTypeId],
    ing: usize,
    seen: &mut [bool],
    item_of_ing: &mut [usize],
) -> bool {
    for (j, item) in items.iter().enumerate() {
        if seen[j] || !ings[ing].matches(*item) {
            continue;
        }
        seen[j] = true;
        if item_of_ing[j] == usize::MAX || augment(ings, items, item_of_ing[j], seen, item_of_ing) {
            item_of_ing[j] = ing;
            return true;
        }
    }
    false
}

/// Parse a recipe `result` object (`{id, count?}`; `{item, count?}` on older
/// packs).
fn result_of(j: &Json) -> Option<(ItemTypeId, u8)> {
    let r = j.get("result")?;
    let name = r.str_of("id").or_else(|| r.str_of("item"))?;
    let item = ItemTypeId::vanilla(name)?;
    let count = r.i32_or("count", 1).clamp(1, 64) as u8;
    Some((item, count))
}

/// Parse one ingredient: a string (`minecraft:x` or `#minecraft:tag`), an
/// object (`{"item":…}`, `{"tag":…}`, `{"items":[…]}`), or a list of items.
fn ing_of(j: &Json, tags: &HashMap<String, Vec<ItemTypeId>>) -> Option<Ing> {
    match j {
        Json::Str(s) => ing_str(s, tags),
        Json::Arr(items) => {
            let mut ids = Vec::new();
            for v in items {
                collect_items(v, tags, &mut ids);
            }
            finish(ids)
        }
        Json::Obj(_) => {
            if let Some(items) = j.get("items").and_then(Json::as_arr) {
                let mut ids = Vec::new();
                for v in items {
                    collect_items(v, tags, &mut ids);
                }
                return finish(ids);
            }
            if let Some(s) = j.str_of("item") {
                return ing_str(s, tags);
            }
            if let Some(s) = j.str_of("tag") {
                return tag_ing(s, tags);
            }
            None
        }
        _ => None,
    }
}

fn ing_str(s: &str, tags: &HashMap<String, Vec<ItemTypeId>>) -> Option<Ing> {
    match s.strip_prefix('#') {
        Some(tag) => tag_ing(tag, tags),
        None => ItemTypeId::vanilla(s).map(Ing::Item),
    }
}

fn tag_ing(tag: &str, tags: &HashMap<String, Vec<ItemTypeId>>) -> Option<Ing> {
    let key = with_ns(tag);
    tags.get(&key)
        .filter(|v| !v.is_empty())
        .map(|v| Ing::Tag(v.clone()))
}

fn collect_items(j: &Json, tags: &HashMap<String, Vec<ItemTypeId>>, out: &mut Vec<ItemTypeId>) {
    match ing_of(j, tags) {
        Some(Ing::Item(i)) => out.push(i),
        Some(Ing::Tag(list)) => out.extend(list),
        None => {}
    }
}

fn finish(mut ids: Vec<ItemTypeId>) -> Option<Ing> {
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        None
    } else {
        Some(Ing::Tag(ids))
    }
}

fn with_ns(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

/// Read every `*.json` in a directory as `(stem, value)`, sorted by stem.
fn read_json_dir(dir: &Path) -> Result<Vec<(String, Json)>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for e in rd {
        let e = e.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let j = Json::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push((stem, j));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Read every `*.json` under `dir` **recursively**, keying each by its path
/// relative to `dir` (subdirectories become path segments, e.g.
/// `enchantable/sharp_weapon`), sorted. Item tags live in subdirectories, so a
/// flat listing would miss `tags/item/enchantable/*`.
fn read_json_tree(dir: &Path, prefix: &str, out: &mut Vec<(String, Json)>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for e in rd {
        let e = e.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        let path = e.path();
        if path.is_dir() {
            let name = path.file_name().map(|s| s.to_string_lossy().into_owned());
            read_json_tree(
                &path,
                &format!("{prefix}{}/", name.unwrap_or_default()),
                out,
            )?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let j = Json::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push((format!("{prefix}{stem}"), j));
    }
    Ok(())
}

/// Every item tag under `data/minecraft/tags/item`, resolved recursively.
///
/// A malformed tag file is an error, not a silently empty tag: a recipe that
/// references a tag the pack failed to parse would otherwise become
/// unsupported with no sign of why.
fn load_item_tags(data_mc: &Path) -> Result<HashMap<String, Vec<ItemTypeId>>, String> {
    // Raw values first: a tag entry is an item name or a nested `#tag`.
    let mut raw: HashMap<String, Vec<RawValue>> = HashMap::new();
    let dir = data_mc.join("tags/item");
    if dir.is_dir() {
        let mut entries = Vec::new();
        read_json_tree(&dir, "", &mut entries)?;
        for (name, j) in entries {
            let mut vals = Vec::new();
            for v in j.get("values").and_then(Json::as_arr).unwrap_or(&[]) {
                let name = match v {
                    Json::Str(s) => s.as_str(),
                    Json::Obj(_) => v.str_of("id").unwrap_or(""),
                    _ => continue,
                };
                if name.is_empty() {
                    continue;
                }
                if let Some(tag) = name.strip_prefix('#') {
                    vals.push(RawValue::Tag(with_ns(tag)));
                } else {
                    vals.push(RawValue::Item(with_ns(name)));
                }
            }
            raw.insert(format!("minecraft:{name}"), vals);
        }
    }

    let mut memo: HashMap<String, Vec<ItemTypeId>> = HashMap::new();
    let keys: Vec<String> = raw.keys().cloned().collect();
    let mut resolving = Vec::new();
    for k in keys {
        resolve_tag(&k, &raw, &mut memo, &mut resolving);
    }
    Ok(memo)
}

enum RawValue {
    Item(String),
    Tag(String),
}

fn resolve_tag(
    key: &str,
    raw: &HashMap<String, Vec<RawValue>>,
    memo: &mut HashMap<String, Vec<ItemTypeId>>,
    resolving: &mut Vec<String>,
) -> Vec<ItemTypeId> {
    if let Some(v) = memo.get(key) {
        return v.clone();
    }
    // A cycle (there are none in vanilla) yields nothing rather than looping.
    if resolving.iter().any(|k| k == key) {
        return Vec::new();
    }
    resolving.push(key.to_string());
    let mut ids = Vec::new();
    if let Some(values) = raw.get(key) {
        for v in values {
            match v {
                RawValue::Item(name) => {
                    if let Some(id) = ItemTypeId::vanilla(name) {
                        ids.push(id);
                    }
                }
                RawValue::Tag(tag) => ids.extend(resolve_tag(tag, raw, memo, resolving)),
            }
        }
    }
    resolving.pop();
    ids.sort_unstable();
    ids.dedup();
    memo.insert(key.to_string(), ids.clone());
    ids
}

// --- global accessors, with the compiled-in table as the no-pack fallback ----

/// Match a crafting grid, from the pack when one is loaded, else the
/// compiled-in table.
pub fn craft(grid: &[Option<ItemTypeId>], width: usize) -> Option<(ItemTypeId, u8)> {
    if let Some(b) = book() {
        return b.craft(grid, width);
    }
    let refs: Vec<Option<&str>> = grid
        .iter()
        .map(|c| c.and_then(aether_world::ItemTypeId::vanilla_name))
        .collect();
    tables::craft(&refs, width).map(|(name, n)| (crate::inventory::item(name), n))
}

/// The cooking method a block name uses.
pub fn method_for_block(short: &str) -> Cook {
    match short {
        "smoker" => Cook::Smoking,
        "blast_furnace" => Cook::Blasting,
        _ => Cook::Smelting,
    }
}

/// What cooking `input` by `method` produces, from the pack when one is
/// loaded, else [`fallback_cook`].
pub fn cook(method: Cook, input: ItemTypeId) -> Option<Cooked> {
    if let Some(b) = book() {
        return b.cook(method, input);
    }
    fallback_cook(method, input)
}

/// The no-pack answer from the compiled-in smelting table.
///
/// The compiled table does not distinguish smelting-only from smoking- or
/// blasting-only recipes, so — as before recipes were loaded from a pack —
/// every furnace cooks whatever the table can smelt, at its block's pace. A
/// blast furnace or smoker must not stop cooking just because no pack is
/// configured (the default).
fn fallback_cook(method: Cook, input: ItemTypeId) -> Option<Cooked> {
    tables::smelt_id(input).map(|result| Cooked {
        result,
        count: 1,
        ticks: match method {
            Cook::Blasting | Cook::Smoking => 100,
            Cook::Campfire => 600,
            Cook::Smelting => 200,
        },
        xp: input.vanilla_name().map_or(0.0, tables::smelt_xp),
    })
}

/// The item a crafting ingredient leaves behind when it is consumed — vanilla's
/// `crafting_remaining_item`: a bucket from a filled bucket, a glass bottle from
/// a potion or honey bottle, a bowl from a stew.
pub fn remainder(item: ItemTypeId) -> Option<ItemTypeId> {
    let short = item.vanilla_name()?.strip_prefix("minecraft:")?;
    let out = if short.ends_with("_bucket") {
        "minecraft:bucket"
    } else if matches!(
        short,
        "potion" | "splash_potion" | "lingering_potion" | "honey_bottle" | "dragon_breath"
    ) {
        "minecraft:glass_bottle"
    } else if matches!(
        short,
        "mushroom_stew" | "beetroot_soup" | "rabbit_stew" | "suspicious_stew"
    ) {
        "minecraft:bowl"
    } else {
        return None;
    };
    ItemTypeId::vanilla(out)
}

/// Whether any method cooks `input` (for routing a shift-click).
pub fn cooks(input: ItemTypeId) -> bool {
    if let Some(b) = book() {
        return b.cooks(input);
    }
    tables::smelt_id(input).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_tags(src: &[(&str, &str)]) -> HashMap<String, Vec<ItemTypeId>> {
        let mut raw: HashMap<String, Vec<RawValue>> = HashMap::new();
        for (name, json) in src {
            let j = Json::parse(json).unwrap();
            let mut vals = Vec::new();
            for v in j.get("values").and_then(Json::as_arr).unwrap_or(&[]) {
                let s = v.as_str().unwrap_or("");
                if let Some(tag) = s.strip_prefix('#') {
                    vals.push(RawValue::Tag(with_ns(tag)));
                } else {
                    vals.push(RawValue::Item(with_ns(s)));
                }
            }
            raw.insert(format!("minecraft:{name}"), vals);
        }
        let mut memo = HashMap::new();
        let keys: Vec<String> = raw.keys().cloned().collect();
        let mut stack = Vec::new();
        for k in keys {
            resolve_tag(&k, &raw, &mut memo, &mut stack);
        }
        memo
    }

    fn book_of(recipes: &[&str], tags: Vec<(&str, &str)>) -> RecipeBook {
        let tags = parse_tags(&tags);
        let entries: Vec<(String, Json)> = recipes
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("r{i}"), Json::parse(s).unwrap()))
            .collect();
        RecipeBook::build(entries, &tags)
    }

    #[test]
    fn a_shaped_recipe_matches_straight() {
        // A pattern symmetric under mirror, drawn straight.
        let book = book_of(
            &[r##"{
                "type": "minecraft:crafting_shaped",
                "key": {"X": "minecraft:oak_planks", "S": "minecraft:stick"},
                "pattern": ["X X", " S "],
                "result": {"id": "minecraft:oak_boat", "count": 1}
            }"##],
            vec![],
        );
        let p = ItemTypeId::vanilla("minecraft:oak_planks").unwrap();
        let s = ItemTypeId::vanilla("minecraft:stick").unwrap();
        let boat = ItemTypeId::vanilla("minecraft:oak_boat").unwrap();
        // Pattern occupies 3x2; the grid is 3 wide.
        let straight = [Some(p), None, Some(p), None, Some(s), None];
        assert_eq!(book.craft(&straight, 3), Some((boat, 1)));
    }

    #[test]
    fn an_asymmetric_shaped_recipe_matches_only_when_mirrored() {
        // `["X  ", "SS "]` trims to a 2x2 pattern `[X, _, S, S]` — genuinely
        // asymmetric. A grid placed the other way round must still match.
        let book = book_of(
            &[r##"{
                "type": "minecraft:crafting_shaped",
                "key": {"X": "minecraft:oak_planks", "S": "minecraft:stick"},
                "pattern": ["X  ", "SS "],
                "result": {"id": "minecraft:oak_boat", "count": 1}
            }"##],
            vec![],
        );
        let p = ItemTypeId::vanilla("minecraft:oak_planks").unwrap();
        let s = ItemTypeId::vanilla("minecraft:stick").unwrap();
        let boat = ItemTypeId::vanilla("minecraft:oak_boat").unwrap();
        // Straight: X top-left, two sticks below.
        let straight = [Some(p), None, Some(s), Some(s)];
        assert_eq!(book.craft(&straight, 2), Some((boat, 1)));
        // Mirrored: the X sits top-right; the straight test fails and only the
        // mirror branch can match.
        let mirrored = [None, Some(p), Some(s), Some(s)];
        assert_eq!(book.craft(&mirrored, 2), Some((boat, 1)));
    }

    #[test]
    fn a_tag_ingredient_resolves_through_the_item_tags() {
        let book = book_of(
            &[r##"{
                "type": "minecraft:crafting_shapeless",
                "ingredients": ["#minecraft:planks"],
                "result": {"id": "minecraft:oak_boat", "count": 4}
            }"##],
            vec![(
                "planks",
                r##"{"values": ["minecraft:oak_planks", "minecraft:birch_planks"]}"##,
            )],
        );
        let oak = ItemTypeId::vanilla("minecraft:oak_planks").unwrap();
        let birch = ItemTypeId::vanilla("minecraft:birch_planks").unwrap();
        let stick = ItemTypeId::vanilla("minecraft:stick").unwrap();
        let boat = ItemTypeId::vanilla("minecraft:oak_boat").unwrap();
        assert_eq!(
            book.craft(&[Some(oak), None, None, None], 2),
            Some((boat, 4))
        );
        assert_eq!(
            book.craft(&[Some(birch), None, None, None], 2),
            Some((boat, 4))
        );
        assert_eq!(book.craft(&[Some(stick), None, None, None], 2), None);
    }

    #[test]
    fn nested_item_tags_resolve() {
        let book = book_of(
            &[r##"{
                "type": "minecraft:crafting_shapeless",
                "ingredients": ["#minecraft:wooden_slabs"],
                "result": {"id": "minecraft:oak_boat", "count": 1}
            }"##],
            vec![
                ("wooden_slabs", r##"{"values": ["#minecraft:oak_slabs"]}"##),
                ("oak_slabs", r##"{"values": ["minecraft:oak_slab"]}"##),
            ],
        );
        let slab = ItemTypeId::vanilla("minecraft:oak_slab").unwrap();
        let boat = ItemTypeId::vanilla("minecraft:oak_boat").unwrap();
        assert_eq!(
            book.craft(&[Some(slab), None, None, None], 2),
            Some((boat, 1))
        );
    }

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn item_tags_are_read_from_subdirectories() {
        let root =
            std::env::temp_dir().join(format!("aether-recipes-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_file(
            &root.join("data/minecraft/recipe/thing.json"),
            r##"{"type":"minecraft:crafting_shapeless",
                "ingredients":["#minecraft:enchantable/sharp"],
                "result":{"id":"minecraft:stick"}}"##,
        );
        write_file(
            &root.join("data/minecraft/tags/item/enchantable/sharp.json"),
            r##"{"values":["minecraft:oak_planks"]}"##,
        );
        let book = RecipeBook::load(&root).unwrap();
        let oak = ItemTypeId::vanilla("minecraft:oak_planks").unwrap();
        assert_eq!(
            book.craft(&[Some(oak), None, None, None], 2),
            Some((ItemTypeId::vanilla("minecraft:stick").unwrap(), 1)),
            "a tag under a subdirectory must resolve"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_tag_is_an_error_not_an_empty_tag() {
        let root = std::env::temp_dir().join(format!("aether-recipes-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_file(
            &root.join("data/minecraft/recipe/thing.json"),
            r##"{"type":"minecraft:crafting_shapeless",
                "ingredients":["#minecraft:bad"],"result":{"id":"minecraft:stick"}}"##,
        );
        write_file(&root.join("data/minecraft/tags/item/bad.json"), "{not json");
        assert!(
            RecipeBook::load(&root).is_err(),
            "a bad tag file must fail the load, not silently empty every tag"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shapeless_matching_is_not_greedy() {
        // Ingredients [{oak, birch}, {oak}], items [oak, birch]. A greedy
        // assignment sends oak to the tag first and then cannot place birch;
        // a real matching sends oak to the specific ingredient and birch to
        // the tag.
        let book = book_of(
            &[r##"{
                "type": "minecraft:crafting_shapeless",
                "ingredients": ["#minecraft:overlap", "minecraft:oak_planks"],
                "result": {"id": "minecraft:stick", "count": 1}
            }"##],
            vec![(
                "overlap",
                r##"{"values": ["minecraft:oak_planks", "minecraft:birch_planks"]}"##,
            )],
        );
        let oak = ItemTypeId::vanilla("minecraft:oak_planks").unwrap();
        let birch = ItemTypeId::vanilla("minecraft:birch_planks").unwrap();
        let stick = ItemTypeId::vanilla("minecraft:stick").unwrap();
        assert_eq!(
            book.craft(&[Some(oak), Some(birch), None, None], 2),
            Some((stick, 1)),
            "a specific ingredient must be able to take the item a tag also matches"
        );
    }

    #[test]
    fn a_sample_of_every_cooking_method() {
        let book = book_of(
            &[
                r##"{"type":"minecraft:smelting","ingredient":"minecraft:raw_iron",
                    "result":{"id":"minecraft:iron_ingot"},"cookingtime":200,"experience":0.7}"##,
                r##"{"type":"minecraft:blasting","ingredient":"minecraft:raw_iron",
                    "result":{"id":"minecraft:iron_ingot"},"cookingtime":100,"experience":0.7}"##,
                r##"{"type":"minecraft:smoking","ingredient":"minecraft:beef",
                    "result":{"id":"minecraft:cooked_beef"},"cookingtime":100,"experience":0.35}"##,
                r##"{"type":"minecraft:campfire_cooking","ingredient":"minecraft:potato",
                    "result":{"id":"minecraft:baked_potato"},"cookingtime":600,"experience":0.35}"##,
            ],
            vec![],
        );
        let raw = ItemTypeId::vanilla("minecraft:raw_iron").unwrap();
        let beef = ItemTypeId::vanilla("minecraft:beef").unwrap();
        let potato = ItemTypeId::vanilla("minecraft:potato").unwrap();

        let smelt = book.cook(Cook::Smelting, raw).unwrap();
        assert_eq!(
            smelt.result,
            ItemTypeId::vanilla("minecraft:iron_ingot").unwrap()
        );
        assert_eq!(smelt.ticks, 200);
        assert!((smelt.xp - 0.7).abs() < 1e-6);

        let blast = book.cook(Cook::Blasting, raw).unwrap();
        assert_eq!(blast.ticks, 100);
        // A smoker has no raw-iron recipe.
        assert!(book.cook(Cook::Smoking, raw).is_none());

        let smoke = book.cook(Cook::Smoking, beef).unwrap();
        assert_eq!(
            smoke.result,
            ItemTypeId::vanilla("minecraft:cooked_beef").unwrap()
        );
        assert_eq!(smoke.ticks, 100);

        let camp = book.cook(Cook::Campfire, potato).unwrap();
        assert_eq!(
            camp.result,
            ItemTypeId::vanilla("minecraft:baked_potato").unwrap()
        );
        assert_eq!(camp.ticks, 600);
    }

    #[test]
    fn stonecutting_and_smithing_and_transmute() {
        let book = book_of(
            &[
                r##"{"type":"minecraft:stonecutting","ingredient":"minecraft:andesite",
                    "result":{"id":"minecraft:andesite_slab","count":2}}"##,
                r##"{"type":"minecraft:smithing_transform",
                    "template":"minecraft:netherite_upgrade_smithing_template",
                    "base":"minecraft:diamond_helmet",
                    "addition":"#minecraft:netherite_tool_materials",
                    "result":{"id":"minecraft:netherite_helmet"}}"##,
                r##"{"type":"minecraft:crafting_transmute",
                    "input":"#minecraft:bundles","material":"minecraft:black_dye",
                    "result":{"id":"minecraft:black_bundle"}}"##,
            ],
            vec![(
                "netherite_tool_materials",
                r##"{"values": ["minecraft:netherite_ingot"]}"##,
            )],
        );
        let andesite = ItemTypeId::vanilla("minecraft:andesite").unwrap();
        let slab = ItemTypeId::vanilla("minecraft:andesite_slab").unwrap();
        assert_eq!(book.stonecut(andesite), vec![(slab, 2)]);
        assert!(book
            .stonecut(ItemTypeId::vanilla("minecraft:stone").unwrap())
            .is_empty());

        let template =
            ItemTypeId::vanilla("minecraft:netherite_upgrade_smithing_template").unwrap();
        let helmet = ItemTypeId::vanilla("minecraft:diamond_helmet").unwrap();
        let ingot = ItemTypeId::vanilla("minecraft:netherite_ingot").unwrap();
        let netherite = ItemTypeId::vanilla("minecraft:netherite_helmet").unwrap();
        assert_eq!(book.smithing(template, helmet, ingot), Some(netherite));
    }

    #[test]
    fn a_transmute_recipe_crafts_in_the_grid() {
        let book = book_of(
            &[r##"{"type":"minecraft:crafting_transmute",
                "input":"#minecraft:bundles","material":"minecraft:black_dye",
                "result":{"id":"minecraft:black_bundle"}}"##],
            vec![(
                "bundles",
                r##"{"values": ["minecraft:white_bundle", "minecraft:black_bundle"]}"##,
            )],
        );
        let white = ItemTypeId::vanilla("minecraft:white_bundle").unwrap();
        let black = ItemTypeId::vanilla("minecraft:black_bundle").unwrap();
        let dye = ItemTypeId::vanilla("minecraft:black_dye").unwrap();
        let either = |a, b| {
            matches!(
                book.craft(&[Some(a), Some(b), None, None], 2),
                Some((r, 1)) if r == black
            )
        };
        assert!(
            either(white, dye),
            "white bundle + black dye -> black bundle"
        );
        assert!(either(dye, white), "order does not matter");
        // Dyeing an already-black bundle black returns it unchanged, which
        // vanilla refuses.
        assert_eq!(book.craft(&[Some(black), Some(dye), None, None], 2), None);
    }

    #[test]
    fn special_and_unknown_types_are_counted_not_run() {
        let book = book_of(
            &[
                r##"{"type":"minecraft:crafting_special_repairitem","category":"misc"}"##,
                r##"{"type":"minecraft:crafting_decorated_pot","category":"misc"}"##,
                r##"{"type":"minecraft:something_else"}"##,
            ],
            vec![],
        );
        assert_eq!(book.counts.special, 2);
        assert_eq!(book.counts.unsupported, vec!["r2".to_string()]);
        assert_eq!(book.counts.total, 3);
    }

    #[test]
    fn a_wide_shaped_pattern_is_trimmed_to_its_bounding_box() {
        // Leading spaces (empty columns) must not shift the match.
        let book = book_of(
            &[r##"{
                "type": "minecraft:crafting_shaped",
                "key": {"X": "minecraft:oak_planks"},
                "pattern": ["  X", "  X"],
                "result": {"id": "minecraft:stick", "count": 1}
            }"##],
            vec![],
        );
        let p = ItemTypeId::vanilla("minecraft:oak_planks").unwrap();
        let stick = ItemTypeId::vanilla("minecraft:stick").unwrap();
        // Two planks stacked in a 2x2 grid still match the trimmed 1x2 shape.
        let grid = [Some(p), None, Some(p), None];
        assert_eq!(book.craft(&grid, 2), Some((stick, 1)));
    }

    #[test]
    fn the_real_pack_loads_and_answers_samples() {
        let Some(root) = std::env::var("AETHER_VANILLA_PACK").ok() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let book = RecipeBook::load(std::path::Path::new(&root)).unwrap();
        let c = book.counts();
        eprintln!(
            "recipes: {} total ({} shaped, {} shapeless, {} cooking, {} stonecutting, \
             {} smithing, {} transmute; {} special, {} unsupported)",
            c.total,
            c.shaped,
            c.shapeless,
            c.cooking,
            c.stonecutting,
            c.smithing,
            c.transmute,
            c.special,
            c.unsupported.len()
        );
        assert!(c.shaped > 500, "{} shaped", c.shaped);
        assert!(c.shapeless > 250, "{} shapeless", c.shapeless);
        assert!(c.cooking > 100, "{} cooking", c.cooking);
        assert!(c.stonecutting > 200, "{} stonecutting", c.stonecutting);
        assert!(c.smithing > 20, "{} smithing", c.smithing);
        assert!(c.transmute > 20, "{} transmute", c.transmute);
        assert!(c.unsupported.is_empty(), "unsupported: {:?}", c.unsupported);

        // Shapeless over a real tag: any log makes 4 planks.
        let oak = ItemTypeId::vanilla("minecraft:oak_log").unwrap();
        let planks = ItemTypeId::vanilla("minecraft:oak_planks").unwrap();
        assert_eq!(
            book.craft(&[Some(oak), None, None, None], 2),
            Some((planks, 4))
        );
        // Shaped over a real tag (`#stone_tool_materials`): a stone pickaxe.
        let cobble = ItemTypeId::vanilla("minecraft:cobblestone").unwrap();
        let stick = ItemTypeId::vanilla("minecraft:stick").unwrap();
        let pick = ItemTypeId::vanilla("minecraft:stone_pickaxe").unwrap();
        let grid = [
            Some(cobble),
            Some(cobble),
            Some(cobble),
            None,
            Some(stick),
            None,
            None,
            Some(stick),
            None,
        ];
        assert_eq!(book.craft(&grid, 3), Some((pick, 1)));
        // Transmutation: a white bundle plus black dye makes a black bundle.
        let white = ItemTypeId::vanilla("minecraft:white_bundle").unwrap();
        let black = ItemTypeId::vanilla("minecraft:black_bundle").unwrap();
        let dye = ItemTypeId::vanilla("minecraft:black_dye").unwrap();
        assert_eq!(
            book.craft(&[Some(white), Some(dye), None, None], 2),
            Some((black, 1))
        );
        // Cooking: smelting and blasting raw iron share a result but not a time.
        let raw = ItemTypeId::vanilla("minecraft:raw_iron").unwrap();
        let ingot = ItemTypeId::vanilla("minecraft:iron_ingot").unwrap();
        assert_eq!(book.cook(Cook::Smelting, raw).unwrap().result, ingot);
        assert_eq!(book.cook(Cook::Smelting, raw).unwrap().ticks, 200);
        assert_eq!(book.cook(Cook::Blasting, raw).unwrap().ticks, 100);
        // Stonecutting.
        let stone = ItemTypeId::vanilla("minecraft:stone").unwrap();
        let slab = ItemTypeId::vanilla("minecraft:stone_slab").unwrap();
        assert!(book.stonecut(stone).iter().any(|(i, _)| *i == slab));
        // Smithing transform with the netherite upgrade.
        let template =
            ItemTypeId::vanilla("minecraft:netherite_upgrade_smithing_template").unwrap();
        let helmet = ItemTypeId::vanilla("minecraft:diamond_helmet").unwrap();
        let n_ingot = ItemTypeId::vanilla("minecraft:netherite_ingot").unwrap();
        let n_helmet = ItemTypeId::vanilla("minecraft:netherite_helmet").unwrap();
        assert_eq!(book.smithing(template, helmet, n_ingot), Some(n_helmet));
    }

    #[test]
    fn the_compiled_in_table_is_the_no_pack_fallback() {
        // Without a pack installed, `craft` uses `super::data`, which the
        // generated data has: planks from a log.
        if BOOK.get().is_some() {
            return; // a pack was installed by another test; the fallback is moot
        }
        let log = Some(crate::inventory::item("minecraft:oak_log"));
        assert_eq!(
            craft(&[log, None, None, None], 2),
            Some((crate::inventory::item("minecraft:oak_planks"), 4))
        );
    }

    #[test]
    fn the_no_pack_fallback_cooks_in_every_furnace() {
        // No test installs a pack, so `book()` is None: every step goes
        // through the compiled-in table. A blast furnace or smoker must
        // still cook (the pre-recipe behaviour), not return nothing.
        assert!(book().is_none());
        let raw = crate::inventory::item("minecraft:raw_iron");
        let ingot = crate::inventory::item("minecraft:iron_ingot");
        assert_eq!(
            cook(Cook::Smelting, raw).map(|c| (c.result, c.ticks)),
            Some((ingot, 200))
        );
        assert_eq!(
            cook(Cook::Blasting, raw).map(|c| (c.result, c.ticks)),
            Some((ingot, 100))
        );
        assert_eq!(
            cook(Cook::Smoking, raw).map(|c| (c.result, c.ticks)),
            Some((ingot, 100))
        );
        assert_eq!(
            cook(Cook::Campfire, raw).map(|c| (c.result, c.ticks)),
            Some((ingot, 600))
        );
        // An item the table cannot smelt cooks nowhere.
        let stick = crate::inventory::item("minecraft:stick");
        assert!(cook(Cook::Smelting, stick).is_none());
    }
}
