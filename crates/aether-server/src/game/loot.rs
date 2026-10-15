//! The vanilla loot table system, read from the pack's
//! `data/minecraft/loot_table/**.json`.
//!
//! This is the same content the worldgen reads: a file per table, parsed on
//! first use and cached. The runtime implements the subset of the format the
//! game uses for block drops, mob drops and structure chests — pools, rolls,
//! weighted entries, alternatives/groups/sequences, the common conditions and
//! functions. Anything the parser does not understand is recorded in
//! [`LootTables::unsupported`] rather than silently dropped, and a table that
//! fails to parse is reported as absent so the caller can fall back.
//!
//! Enchantments are not modelled as item components yet, so functions that
//! would *write* an enchantment (`enchant_randomly`, `enchant_with_levels`)
//! are parsed and applied as no-ops, and `set_damage`/`set_components` are
//! dropped because drops carry no components. The context still carries the
//! *reader* side — `silk_touch`, `fortune`, `looting` and arbitrary
//! enchantment levels — because that is what the conditions and `apply_bonus`
//! read.
//!
//! Determinism: chest loot is seeded from the world seed, the table id and the
//! chest position, so the same world always rolls the same chest once it is
//! first opened. Vanilla seeds a chest from its `random_sequence`, which is a
//! different hash; matching it exactly needs the sequence registry and is left
//! as a TODO. Block and mob drops draw from the caller's RNG.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use aether_worldgen::vanilla::json::Json;

use super::tables::Rng;
use crate::session::DemoWorld;

/// One stack a table produced. `count` is already clamped to a byte, which is
/// what the item entity layer wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LootStack {
    pub item: String,
    pub count: u8,
}

/// A number provider, the `rolls`/`count` fields.
#[derive(Debug, Clone, Copy)]
pub enum NumberProvider {
    Constant(f32),
    Uniform { min: f32, max: f32 },
    Binomial { n: i32, p: f32 },
}

impl NumberProvider {
    /// The provider as an integer draw (uniform inclusive on the floored
    /// bounds, as vanilla's `getInt`).
    pub fn get_i32(&self, rng: &mut Rng) -> i32 {
        match *self {
            Self::Constant(v) => v.floor() as i32,
            Self::Uniform { min, max } => {
                let (lo, hi) = (min.floor() as i32, max.floor() as i32);
                if hi <= lo {
                    lo
                } else {
                    rng.range(lo, hi)
                }
            }
            Self::Binomial { n, p } => {
                let mut k = 0;
                for _ in 0..n.max(0) {
                    if rng.f32() < p {
                        k += 1;
                    }
                }
                k
            }
        }
    }

    /// A float draw (vanilla's `getFloat`).
    pub fn get_f32(&self, rng: &mut Rng) -> f32 {
        match *self {
            Self::Constant(v) => v,
            Self::Uniform { min, max } => min + rng.f32() * (max - min),
            Self::Binomial { .. } => self.get_i32(rng) as f32,
        }
    }
}

/// A level-based value, `enchanted_chance` inside
/// `random_chance_with_enchanted_bonus`.
#[derive(Debug, Clone, Copy)]
pub enum LevelBasedValue {
    Constant(f32),
    Linear {
        base: f32,
        per_level_above_first: f32,
    },
}

impl LevelBasedValue {
    fn at(&self, level: i32) -> f32 {
        match *self {
            Self::Constant(v) => v,
            Self::Linear {
                base,
                per_level_above_first,
            } => base + per_level_above_first * (level.max(1) - 1) as f32,
        }
    }
}

/// A condition on a pool or entry.
#[derive(Debug, Clone)]
pub enum Condition {
    /// A condition whose predicate the engine cannot evaluate — `vehicle`,
    /// `equipment`, `nbt`, `location_check`… Treated as failing, so a table
    /// that needs one never produces the wrong drop.
    Impossible,
    RandomChance(f32),
    RandomChanceWithEnchantedBonus {
        enchantment: String,
        unenchanted: f32,
        enchanted: LevelBasedValue,
    },
    /// `table_bonus`: `chances[enchantment level]`, clamped to the last entry.
    TableBonus {
        enchantment: String,
        chances: Vec<f32>,
    },
    KilledByPlayer,
    SurvivesExplosion,
    MatchTool {
        /// Item names the tool must be, or `None` for any.
        items: Option<Vec<String>>,
        /// `(enchantment, min_level)` pairs.
        enchants: Vec<(String, i32)>,
    },
    BlockStateProperty {
        block: Option<String>,
        properties: Vec<(String, String)>,
    },
    EntityProperties {
        entity: EntityRef,
        kind: Option<String>,
        kind_tag: Option<Vec<String>>,
        on_fire: bool,
        is_baby: bool,
        sneaking: bool,
        /// `components`: expected component id → value, e.g.
        /// `minecraft:sheep/color` → `white`.
        components: Vec<(String, String)>,
        /// `type_specific`: the entity type and its named fields (`sheared`,
        /// `size`, …) as integers (booleans as 0/1).
        type_specific: Option<(String, Vec<(String, PredValue)>)>,
    },
    AnyOf(Vec<Condition>),
    AllOf(Vec<Condition>),
    Inverted(Box<Condition>),
}

/// Which entity a condition names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityRef {
    This,
    Killer,
    Attacker,
    DirectAttacker,
}

/// A `type_specific` field value from a predicate.
#[derive(Debug, Clone, Copy)]
pub enum PredValue {
    Int(i64),
    Bool(bool),
}

impl PredValue {
    fn as_i64(self) -> i64 {
        match self {
            Self::Int(v) => v,
            Self::Bool(b) => b as i64,
        }
    }
}

/// A function applied to a produced stack or count.
#[derive(Debug, Clone)]
pub enum Function {
    SetCount {
        count: NumberProvider,
        add: bool,
    },
    LimitCount {
        min: Option<i32>,
        max: Option<i32>,
    },
    ApplyBonus {
        enchantment: String,
        formula: BonusFormula,
    },
    ExplosionDecay,
    FurnaceSmelt,
    EnchantedCountIncrease {
        enchantment: String,
        count: NumberProvider,
        limit: u32,
    },
    /// Parsed but a no-op: enchantments are not item components yet.
    EnchantRandomly,
    /// Parsed but a no-op (see [`EnchantRandomly`]).
    EnchantWithLevels,
    /// Parsed but a no-op: drops carry no component data.
    SetDamage,
    /// Parsed but a no-op: drops carry no component data.
    SetComponents,
    /// A function guarded by its own `conditions` array.
    Conditional {
        conditions: Vec<Condition>,
        inner: Box<Function>,
    },
}

/// The three `apply_bonus` formulas.
#[derive(Debug, Clone, Copy)]
pub enum BonusFormula {
    OreDrops,
    UniformBonusCount { bonus_multiplier: i32 },
    BinomialWithBonusCount { probability: f32, extra: i32 },
}

/// An entry that can produce stacks.
#[derive(Debug, Clone)]
pub struct Entry {
    pub weight: i32,
    pub conditions: Vec<Condition>,
    pub functions: Vec<Function>,
    pub kind: EntryKind,
}

/// What an entry is.
#[derive(Debug, Clone)]
pub enum EntryKind {
    Item(String),
    /// A tag: `items` are the resolved members.
    Tag {
        items: Vec<String>,
        expand: bool,
    },
    /// A reference to another table, resolved at load time.
    Nested(Arc<LootTable>),
    /// A reference that could not be resolved.
    LootTable(String),
    Alternatives(Vec<Entry>),
    Group(Vec<Entry>),
    Sequence(Vec<Entry>),
    Empty,
}

/// One pool of the table.
#[derive(Debug, Clone)]
pub struct Pool {
    pub rolls: NumberProvider,
    pub bonus_rolls: NumberProvider,
    pub conditions: Vec<Condition>,
    pub functions: Vec<Function>,
    pub entries: Vec<Entry>,
}

/// A parsed loot table.
#[derive(Debug, Clone)]
pub struct LootTable {
    /// Table-level functions, applied to every stack the table produces.
    pub functions: Vec<Function>,
    pub pools: Vec<Pool>,
}

/// What a roll knows about the world.
#[derive(Debug, Clone, Default)]
pub struct LootContext {
    /// The held tool's item name, if any.
    pub tool: Option<String>,
    /// Enchantment levels on the killer's/breaker's tool.
    pub enchants: HashMap<String, i32>,
    /// A player made the kill.
    pub player_kill: bool,
    /// The block being broken, with its state properties.
    pub block: Option<(String, Vec<(String, String)>)>,
    /// Explosion radius, when the drop comes from one.
    pub explosion_radius: Option<f32>,
    /// The entity's kind (mobs), for `entity_properties`.
    pub entity_kind: Option<String>,
    pub entity_on_fire: bool,
    pub entity_is_baby: bool,
    pub attacker_kind: Option<String>,
    /// The entity's components (`minecraft:sheep/color` → `white`), for
    /// `entity_properties.components`.
    pub entity_components: Vec<(String, String)>,
    /// The entity's `type_specific` fields (`minecraft:sheep`, `sheared`),
    /// integers with booleans as 0/1.
    pub entity_type_specific: Option<(String, Vec<(String, i64)>)>,
    /// Luck, scaling `bonus_rolls` and `quality`; zero in this engine.
    pub luck: f32,
}

impl LootContext {
    /// A bare context for a block broken by `tool`.
    pub fn block(tool: Option<&str>) -> Self {
        Self {
            tool: tool.map(str::to_owned),
            ..Default::default()
        }
    }

    /// A context with one enchantment at `level`.
    #[cfg(test)]
    pub fn with_enchant(mut self, name: &str, level: i32) -> Self {
        self.enchants.insert(name.to_owned(), level);
        self
    }

    fn enchant_level(&self, name: &str) -> i32 {
        self.enchants.get(name).copied().unwrap_or(0)
    }
}

impl LootTable {
    /// Roll every pool of the table.
    ///
    /// Vanilla expands a pool's entries into singleton candidates first, drops
    /// the ones whose conditions fail — each condition evaluated exactly once
    /// — and only then draws one candidate by weight. Reproducing that order
    /// matters: a `random_chance` on an `alternatives` child or a
    /// `table_bonus` must consume the RNG once, not once to select the child
    /// and again to run it.
    pub fn roll(&self, ctx: &LootContext, rng: &mut Rng) -> Vec<LootStack> {
        let mut out = Vec::new();
        for pool in &self.pools {
            if !matches_all(&pool.conditions, ctx, rng) {
                continue;
            }
            let rolls = pool.rolls.get_i32(rng);
            // `bonus_rolls` is "per level of luck", floored after the
            // multiplication, so without a luck source it adds nothing.
            let bonus = (ctx.luck * pool.bonus_rolls.get_f32(rng)).floor() as i32;
            for _ in 0..(rolls + bonus).max(0) {
                let mut candidates = Vec::new();
                for entry in &pool.entries {
                    expand_entry(entry, ctx, rng, &[], &mut candidates);
                }
                let mut produced = Vec::new();
                if let Some(c) = pick_candidate(&candidates, rng) {
                    produce(c, ctx, rng, &mut produced);
                }
                for f in &pool.functions {
                    for st in produced.iter_mut() {
                        apply_function(f, st, ctx, rng);
                    }
                }
                out.extend(produced.into_iter().filter(|s| s.count > 0));
            }
        }
        // Table-level functions run last, over everything the table produced.
        for f in &self.functions {
            for st in out.iter_mut() {
                apply_function(f, st, ctx, rng);
            }
        }
        out.retain(|s| s.count > 0);
        out
    }
}

/// A singleton entry after composite expansion: one weighted candidate the
/// pool draws from.
struct Candidate {
    weight: i32,
    produce: Produce,
    functions: Vec<Function>,
}

/// What a selected candidate makes.
enum Produce {
    /// One stack per item name (an item, or every member of an unexpanded tag).
    Items(Vec<String>),
    /// The loot of another table.
    Table(Arc<LootTable>),
}

/// Expand `entry` into singleton candidates, testing its conditions first.
fn expand_entry(
    entry: &Entry,
    ctx: &LootContext,
    rng: &mut Rng,
    inherited: &[Function],
    out: &mut Vec<Candidate>,
) {
    if !matches_all(&entry.conditions, ctx, rng) {
        return;
    }
    expand_entry_unchecked(entry, ctx, rng, inherited, out);
}

/// Expand an entry whose own conditions have already passed, so they are not
/// evaluated a second time — which for an RNG-driven condition would burn an
/// extra draw.
fn expand_entry_unchecked(
    entry: &Entry,
    ctx: &LootContext,
    rng: &mut Rng,
    inherited: &[Function],
    out: &mut Vec<Candidate>,
) {
    // Innermost functions run first; `inherited` is everything the enclosing
    // composites carry, already ordered parent-first.
    let mut functions = entry.functions.clone();
    functions.extend(inherited.iter().cloned());
    match &entry.kind {
        EntryKind::Item(name) => out.push(Candidate {
            weight: entry.weight,
            produce: Produce::Items(vec![name.clone()]),
            functions,
        }),
        EntryKind::Empty => out.push(Candidate {
            weight: entry.weight,
            produce: Produce::Items(Vec::new()),
            functions,
        }),
        EntryKind::Tag { items, expand } => {
            if *expand {
                // Composite: one candidate per member. Vanilla drops the tag
                // entry's own modifiers here (MC-212671); the pack's only
                // expanded tag carries none.
                for it in items {
                    out.push(Candidate {
                        weight: entry.weight,
                        produce: Produce::Items(vec![it.clone()]),
                        functions: inherited.to_vec(),
                    });
                }
            } else {
                // Singleton: every member drops together.
                out.push(Candidate {
                    weight: entry.weight,
                    produce: Produce::Items(items.clone()),
                    functions,
                });
            }
        }
        EntryKind::Nested(table) => out.push(Candidate {
            weight: entry.weight,
            produce: Produce::Table(Arc::clone(table)),
            functions,
        }),
        EntryKind::LootTable(_) => {}
        EntryKind::Alternatives(children) => {
            for c in children {
                if matches_all(&c.conditions, ctx, rng) {
                    expand_entry_unchecked(c, ctx, rng, &functions, out);
                    break;
                }
            }
        }
        EntryKind::Sequence(children) => {
            for c in children {
                if matches_all(&c.conditions, ctx, rng) {
                    expand_entry_unchecked(c, ctx, rng, &functions, out);
                } else {
                    break;
                }
            }
        }
        EntryKind::Group(children) => {
            for c in children {
                expand_entry(c, ctx, rng, &functions, out);
            }
        }
    }
}

/// Make the stacks a selected candidate produces, then run its functions.
fn produce(c: &Candidate, ctx: &LootContext, rng: &mut Rng, out: &mut Vec<LootStack>) {
    match &c.produce {
        Produce::Items(names) => {
            for n in names {
                out.push(LootStack {
                    item: n.clone(),
                    count: 1,
                });
            }
        }
        Produce::Table(t) => out.extend(t.roll(ctx, rng)),
    }
    for f in &c.functions {
        for st in out.iter_mut() {
            apply_function(f, st, ctx, rng);
        }
    }
}

/// Draw one candidate by weight; `None` for an empty pool.
fn pick_candidate<'a>(candidates: &'a [Candidate], rng: &mut Rng) -> Option<&'a Candidate> {
    if candidates.is_empty() {
        return None;
    }
    let total: i32 = candidates.iter().map(|c| c.weight.max(0)).sum();
    if total <= 0 {
        return candidates.first();
    }
    let mut n = rng.range(1, total);
    for c in candidates {
        n -= c.weight.max(0);
        if n <= 0 {
            return Some(c);
        }
    }
    candidates.last()
}

fn matches_all(conditions: &[Condition], ctx: &LootContext, rng: &mut Rng) -> bool {
    conditions.iter().all(|c| test(c, ctx, rng))
}

fn test(c: &Condition, ctx: &LootContext, rng: &mut Rng) -> bool {
    match c {
        Condition::Impossible => false,
        Condition::RandomChance(p) => rng.f32() < *p,
        Condition::RandomChanceWithEnchantedBonus {
            enchantment,
            unenchanted,
            enchanted,
        } => {
            let level = ctx.enchant_level(enchantment);
            let chance = if level == 0 {
                *unenchanted
            } else {
                enchanted.at(level)
            };
            rng.f32() < chance
        }
        Condition::TableBonus {
            enchantment,
            chances,
        } => {
            if chances.is_empty() {
                return false;
            }
            let level = ctx.enchant_level(enchantment).max(0) as usize;
            let idx = level.min(chances.len() - 1);
            rng.f32() < chances[idx]
        }
        Condition::KilledByPlayer => ctx.player_kill,
        Condition::SurvivesExplosion => match ctx.explosion_radius {
            None => true,
            Some(r) if r <= 1.0 => true,
            Some(r) => rng.f32() < 1.0 / r,
        },
        Condition::MatchTool { items, enchants } => {
            if let Some(items) = items {
                match &ctx.tool {
                    Some(t) => {
                        if !items.iter().any(|i| i == t) {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
            enchants.iter().all(|(e, min)| ctx.enchant_level(e) >= *min)
        }
        Condition::BlockStateProperty { block, properties } => {
            let Some((name, props)) = &ctx.block else {
                return false;
            };
            if let Some(b) = block {
                if b != name {
                    return false;
                }
            }
            properties
                .iter()
                .all(|(k, v)| props.iter().any(|(pk, pv)| pk == k && pv == v))
        }
        Condition::EntityProperties {
            entity,
            kind,
            kind_tag,
            on_fire,
            is_baby,
            sneaking,
            components,
            type_specific,
        } => {
            let is_this = matches!(entity, EntityRef::This);
            let (this_kind, this_fire, this_baby, this_sneak) = match entity {
                EntityRef::This => (
                    ctx.entity_kind.as_deref(),
                    ctx.entity_on_fire,
                    ctx.entity_is_baby,
                    false,
                ),
                EntityRef::Killer | EntityRef::DirectAttacker | EntityRef::Attacker => {
                    (ctx.attacker_kind.as_deref(), false, false, false)
                }
            };
            if let Some(k) = kind {
                if this_kind != Some(k.as_str()) {
                    return false;
                }
            }
            if let Some(tag) = kind_tag {
                match this_kind {
                    Some(k) if tag.iter().any(|t| t == k) => {}
                    _ => return false,
                }
            }
            if *on_fire && !this_fire {
                return false;
            }
            if *is_baby && !this_baby {
                return false;
            }
            if *sneaking && !this_sneak {
                return false;
            }
            // Components and `type_specific` are only known for `this`.
            if !components.is_empty()
                && (!is_this
                    || !components.iter().all(|(k, v)| {
                        ctx.entity_components
                            .iter()
                            .any(|(ck, cv)| ck == k && cv == v)
                    }))
            {
                return false;
            }
            if let Some((ty, fields)) = type_specific {
                if !is_this {
                    return false;
                }
                let Some((ctx_ty, ctx_fields)) = &ctx.entity_type_specific else {
                    return false;
                };
                if ctx_ty != ty {
                    return false;
                }
                if !fields.iter().all(|(k, want)| {
                    ctx_fields
                        .iter()
                        .any(|(ck, got)| ck == k && got == &want.as_i64())
                }) {
                    return false;
                }
            }
            true
        }
        Condition::AnyOf(terms) => terms.iter().any(|t| test(t, ctx, rng)),
        Condition::AllOf(terms) => terms.iter().all(|t| test(t, ctx, rng)),
        Condition::Inverted(term) => !test(term, ctx, rng),
    }
}

fn apply_function(f: &Function, st: &mut LootStack, ctx: &LootContext, rng: &mut Rng) {
    match f {
        Function::SetCount { count, add } => {
            let n = count.get_i32(rng).max(0);
            let base = if *add { st.count as i32 } else { 0 };
            st.count = (base + n).clamp(0, 255) as u8;
        }
        Function::LimitCount { min, max } => {
            let mut n = st.count as i32;
            if let Some(m) = min {
                n = n.max(*m);
            }
            if let Some(m) = max {
                n = n.min(*m);
            }
            st.count = n.clamp(0, 255) as u8;
        }
        Function::ApplyBonus {
            enchantment,
            formula,
        } => {
            let level = ctx.enchant_level(enchantment);
            if level <= 0 {
                return;
            }
            let base = st.count as i32;
            let n = match formula {
                BonusFormula::OreDrops => {
                    // `nextInt(level + 2) - 1`, clamped at zero.
                    let i = (rng.range(0, level + 1) - 1).max(0);
                    base * (i + 1)
                }
                BonusFormula::UniformBonusCount { bonus_multiplier } => {
                    base + rng.range(0, (*bonus_multiplier).max(0) * level)
                }
                BonusFormula::BinomialWithBonusCount { probability, extra } => {
                    // Vanilla: `level + extra` rounds, each adding one.
                    let mut n = base;
                    for _ in 0..(level + extra).max(0) {
                        if rng.f32() < *probability {
                            n += 1;
                        }
                    }
                    n
                }
            };
            st.count = n.clamp(0, 255) as u8;
        }
        Function::ExplosionDecay => {
            if let Some(r) = ctx.explosion_radius {
                if r > 1.0 && rng.f32() >= 1.0 / r {
                    st.count = 0;
                }
            }
        }
        Function::FurnaceSmelt => {
            if let Some(smelted) = super::tables::smelt(&st.item) {
                st.item = smelted.to_owned();
            }
        }
        Function::EnchantedCountIncrease {
            enchantment,
            count,
            limit,
        } => {
            let level = ctx.enchant_level(enchantment);
            if level <= 0 {
                return;
            }
            // Vanilla: `grow(round(level * count.getFloat))`, the bonus
            // clamped to `limit` (0 = unlimited). Drawing the float and
            // scaling it gives 0..level, not `getInt * level` (which is only
            // ever 0 or level).
            let extra = (count.get_f32(rng) * level as f32).round() as i32;
            let cap = if *limit == 0 { i32::MAX } else { *limit as i32 };
            let add = extra.clamp(0, cap);
            st.count = (st.count as i32 + add).clamp(0, 255) as u8;
        }
        Function::EnchantRandomly
        | Function::EnchantWithLevels
        | Function::SetDamage
        | Function::SetComponents => {}
        Function::Conditional { conditions, inner } => {
            if matches_all(conditions, ctx, rng) {
                apply_function(inner, st, ctx, rng);
            }
        }
    }
}

// --- the loader ------------------------------------------------------------

/// A source of resolved tag members. Tags are flattened at load time so the
/// rolling path never touches the filesystem.
struct TagLoader {
    root: PathBuf,
    cache: Mutex<HashMap<(String, String), Arc<Vec<String>>>>,
}

impl TagLoader {
    fn resolve(&self, kind: &str, id: &str) -> Vec<String> {
        let id = strip_ns(id).to_owned();
        self.resolve_inner(kind, &id, &mut BTreeSet::new())
    }

    fn resolve_inner(&self, kind: &str, id: &str, seen: &mut BTreeSet<String>) -> Vec<String> {
        if !seen.insert(id.to_owned()) {
            return Vec::new();
        }
        if let Some(v) = self
            .cache
            .lock()
            .unwrap()
            .get(&(kind.to_owned(), id.to_owned()))
        {
            return (**v).clone();
        }
        let path = self.root.join("tags").join(kind).join(format!("{id}.json"));
        let mut out: BTreeSet<String> = BTreeSet::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(json) = Json::parse(&text) {
                if let Some(values) = json.get("values").and_then(Json::as_arr) {
                    for v in values {
                        match v {
                            Json::Str(s) => {
                                if let Some(tag) = s.strip_prefix('#') {
                                    for m in self.resolve_inner(kind, strip_ns(tag), seen) {
                                        out.insert(m);
                                    }
                                } else {
                                    out.insert(with_ns(s));
                                }
                            }
                            Json::Obj(o) => {
                                if let Some(Json::Str(s)) = o.get("id") {
                                    if let Some(tag) = s.strip_prefix('#') {
                                        for m in self.resolve_inner(kind, strip_ns(tag), seen) {
                                            out.insert(m);
                                        }
                                    } else {
                                        out.insert(with_ns(s));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        let v = Arc::new(out.into_iter().collect::<Vec<_>>());
        self.cache
            .lock()
            .unwrap()
            .insert((kind.to_owned(), id.to_owned()), Arc::clone(&v));
        (*v).clone()
    }
}

/// Every loaded loot table, plus the tag loader behind it.
pub struct LootTables {
    root: Option<PathBuf>,
    tags: Option<TagLoader>,
    cache: Mutex<HashMap<String, Option<Arc<LootTable>>>>,
    unsupported: Mutex<BTreeSet<String>>,
}

impl LootTables {
    /// Open the pack rooted at `worldgen_data`. A missing or unrecognised
    /// path yields a registry whose every lookup returns `None`, so callers
    /// fall back to their built-in tables.
    pub fn open(worldgen_data: &str) -> Self {
        let root = pack_data_dir(worldgen_data);
        let tags = root.as_ref().map(|r| TagLoader {
            root: r.clone(),
            cache: Mutex::new(HashMap::new()),
        });
        Self {
            root,
            tags,
            cache: Mutex::new(HashMap::new()),
            unsupported: Mutex::new(BTreeSet::new()),
        }
    }

    /// The process-wide registry, opened once from the configuration.
    pub fn global() -> &'static LootTables {
        static T: OnceLock<LootTables> = OnceLock::new();
        T.get_or_init(|| Self::open(crate::worldgen_data()))
    }

    /// A table by id (`minecraft:blocks/diamond_ore`), parsed and cached.
    pub fn table(&self, id: &str) -> Option<Arc<LootTable>> {
        self.table_impl(id, &mut Vec::new())
    }

    /// [`Self::table`] carrying the chain of ids being resolved, so a
    /// `loot_table` reference that loops back on itself is cut instead of
    /// recursing without bound.
    fn table_impl(&self, id: &str, stack: &mut Vec<String>) -> Option<Arc<LootTable>> {
        if let Some(v) = self.cache.lock().unwrap().get(id) {
            return v.clone();
        }
        if stack.iter().any(|s| s == id) {
            self.unsupported
                .lock()
                .unwrap()
                .insert(format!("{id}: cyclic loot_table reference"));
            return None;
        }
        let mut t = self.parse(id)?;
        stack.push(id.to_owned());
        self.resolve_nested(&mut t, stack);
        stack.pop();
        let arc = Arc::new(t);
        self.cache
            .lock()
            .unwrap()
            .insert(id.to_owned(), Some(Arc::clone(&arc)));
        Some(arc)
    }

    /// Types the parser skipped, for the startup report.
    pub fn unsupported(&self) -> Vec<String> {
        self.unsupported.lock().unwrap().iter().cloned().collect()
    }

    /// Parse every table in the pack, so [`Self::unsupported`] is populated
    /// before the server logs it. A no-op without a pack.
    pub fn warm(&self) {
        let Some(root) = &self.root else {
            return;
        };
        let mut ids = Vec::new();
        collect_ids(&root.join("loot_table"), "", &mut ids);
        for id in ids {
            let _ = self.table(&id);
        }
    }

    /// The table for a block (`minecraft:blocks/<short name>`).
    pub fn block_table(&self, block: &str) -> Option<Arc<LootTable>> {
        let short = block.strip_prefix("minecraft:").unwrap_or(block);
        self.table(&format!("minecraft:blocks/{short}"))
    }

    /// The table for an entity (`minecraft:entities/<short name>`).
    pub fn entity_table(&self, kind: &str) -> Option<Arc<LootTable>> {
        let short = kind.strip_prefix("minecraft:").unwrap_or(kind);
        self.table(&format!("minecraft:entities/{short}"))
    }

    /// Roll the named table with a seed derived from the world seed and the
    /// position — the way a structure chest is rolled once, on first open.
    pub fn roll_seeded(
        &self,
        id: &str,
        seed: u64,
        pos: (i32, i32, i32),
        ctx: &LootContext,
    ) -> Option<Vec<LootStack>> {
        let table = self.table(id)?;
        let mut rng = Rng::new(seed ^ hash_table(id) ^ hash_pos(pos));
        Some(table.roll(ctx, &mut rng))
    }

    fn parse(&self, id: &str) -> Option<LootTable> {
        let root = self.root.as_ref()?;
        let path = root
            .join("loot_table")
            .join(format!("{}.json", strip_ns(id)));
        let text = std::fs::read_to_string(&path).ok()?;
        let json = Json::parse(&text).ok()?;
        let tags = self.tags.as_ref();
        let mut warn = BTreeSet::new();
        match parse_table(id, &json, tags, &mut warn) {
            Ok(t) => {
                if !warn.is_empty() {
                    self.unsupported.lock().unwrap().extend(warn);
                }
                Some(t)
            }
            Err(e) => {
                self.unsupported
                    .lock()
                    .unwrap()
                    .insert(format!("{id}: {e}"));
                None
            }
        }
    }

    /// Replace `loot_table` references with their parsed tables, so the
    /// rolling path needs no registry.
    fn resolve_nested(&self, table: &mut LootTable, stack: &mut Vec<String>) {
        for pool in &mut table.pools {
            for entry in &mut pool.entries {
                resolve_entry_nested(entry, self, stack);
            }
        }
    }
}

fn resolve_entry_nested(entry: &mut Entry, tables: &LootTables, stack: &mut Vec<String>) {
    match &mut entry.kind {
        EntryKind::LootTable(id) => {
            let id = id.clone();
            if let Some(t) = tables.table_impl(&id, stack) {
                entry.kind = EntryKind::Nested(t);
            }
        }
        EntryKind::Alternatives(c) | EntryKind::Group(c) | EntryKind::Sequence(c) => {
            for child in c {
                resolve_entry_nested(child, tables, stack);
            }
        }
        _ => {}
    }
}

/// `minecraft:foo/bar` → `foo/bar`.
fn strip_ns(id: &str) -> &str {
    id.split_once(':').map(|(_, r)| r).unwrap_or(id)
}

/// `foo` → `minecraft:foo`, leaving a namespaced id alone.
fn with_ns(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn hash_table(id: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn hash_pos((x, y, z): (i32, i32, i32)) -> u64 {
    (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
}

/// Find `data/minecraft` from the configured `worldgen_data`, which may be the
/// pack root or the `data/minecraft/worldgen` directory itself.
fn pack_data_dir(worldgen_data: &str) -> Option<PathBuf> {
    if worldgen_data.is_empty() {
        return None;
    }
    let p = Path::new(worldgen_data);
    let candidate = p.join("data/minecraft");
    if candidate.join("loot_table").is_dir() {
        return Some(candidate);
    }
    // Already `.../data/minecraft`.
    if p.join("loot_table").is_dir() && p.file_name().and_then(|n| n.to_str()) == Some("minecraft")
    {
        return Some(p.to_path_buf());
    }
    None
}

/// Every table id (`minecraft:<path>`) under `dir`, recursively.
fn collect_ids(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            collect_ids(&p, &format!("{prefix}{name}/"), out);
        } else if let Some(stem) = name.strip_suffix(".json") {
            out.push(format!("minecraft:{prefix}{stem}"));
        }
    }
}

// --- structure chests ------------------------------------------------------

/// The loot table a naturally generated chest at `pos` carries, if the engine
/// can tell.
///
/// The engine's worldgen places the dungeon (`monster_room`) and igloo
/// laboratory chests but keeps no block entity on them, so which table a chest
/// should roll is decided from the structure's signature: a dungeon surrounds
/// its chest with a `spawner`, the igloo laboratory with a `brewing_stand` and
/// `water_cauldron`. A position the journal has ever touched is a player's own
/// chest and rolls nothing. This is a deviation from vanilla, which stores the
/// `LootTable` string in the chest's block entity; it is recorded in
/// `KNOWN_ISSUES`.
pub fn generated_chest_table(world: &DemoWorld, pos: (i32, i32, i32)) -> Option<&'static str> {
    let touched = world
        .journal()
        .last_change_at(pos.0, pos.1, pos.2)
        .ok()
        .flatten()
        .is_some();
    chest_table_from_blocks(touched, pos, |dx, dy, dz| {
        crate::game::block_name(world, world.get_block(dx, dy, dz))
    })
}

/// The structure-chest decision on its own, so it can be tested without a
/// world: `touched` says the journal has seen this position (so it is not a
/// generated chest), and `get` returns a block's name.
fn chest_table_from_blocks(
    touched: bool,
    (x, y, z): (i32, i32, i32),
    get: impl Fn(i32, i32, i32) -> Option<String>,
) -> Option<&'static str> {
    if touched {
        return None;
    }
    let mut igloo = false;
    for dy in -3..=3 {
        for dz in -6..=6 {
            for dx in -6..=6 {
                let Some(name) = get(x + dx, y + dy, z + dz) else {
                    continue;
                };
                match name.as_str() {
                    "minecraft:spawner" => return Some("minecraft:chests/simple_dungeon"),
                    "minecraft:brewing_stand" | "minecraft:water_cauldron" => igloo = true,
                    _ => {}
                }
            }
        }
    }
    igloo.then_some("minecraft:chests/igloo_chest")
}

/// Roll the loot a freshly generated chest should start with, and where in its
/// `slots` each stack goes. `None` when the position is not a generated chest;
/// `Some(vec![])` when it is but rolled empty. `tables` is passed in so a test
/// can supply a pack.
pub fn generated_chest_loot(
    tables: &LootTables,
    id: &str,
    seed: u64,
    pos: (i32, i32, i32),
    slots: usize,
) -> Option<Vec<(usize, LootStack)>> {
    let ctx = LootContext::default();
    let stacks = tables.roll_seeded(id, seed, pos, &ctx)?;
    if slots == 0 {
        return Some(Vec::new());
    }
    let mut rng = Rng::new(seed ^ hash_table(id) ^ hash_pos(pos) ^ 0xA5A5_A5A5);
    let mut used = vec![false; slots];
    let mut out = Vec::new();
    for s in stacks {
        if s.count == 0 || out.len() >= slots {
            break;
        }
        let mut slot = rng.range(0, slots as i32 - 1) as usize;
        while used[slot] {
            slot = (slot + 1) % slots;
        }
        used[slot] = true;
        out.push((slot, s));
    }
    Some(out)
}

/// Roll the loot a freshly generated chest at `pos` should start with, and
/// where in its `slots` each stack goes. `None` when the position is not a
/// generated chest or no pack is configured.
pub fn seed_generated_chest(
    world: &DemoWorld,
    pos: (i32, i32, i32),
    seed: u64,
    slots: usize,
) -> Option<Vec<(usize, LootStack)>> {
    let id = generated_chest_table(world, pos)?;
    generated_chest_loot(LootTables::global(), id, seed, pos, slots)
}

// --- parsing ---------------------------------------------------------------

/// Record a construct the parser skipped, once per unique `kind:type`.
fn note(warn: &mut BTreeSet<String>, kind: &str, ty: &str) {
    warn.insert(format!("{kind}:{ty}"));
}

fn parse_table(
    _id: &str,
    j: &Json,
    tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Result<LootTable, String> {
    let functions = parse_functions(j.get("functions"), tags, warn);
    let mut pools = Vec::new();
    if let Some(arr) = j.get("pools").and_then(Json::as_arr) {
        for (i, p) in arr.iter().enumerate() {
            pools.push(parse_pool(p, tags, warn).map_err(|e| format!("pool {i}: {e}"))?);
        }
    }
    Ok(LootTable { functions, pools })
}

fn parse_pool(
    j: &Json,
    tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Result<Pool, String> {
    let rolls = j
        .get("rolls")
        .map(parse_number)
        .unwrap_or(NumberProvider::Constant(1.0));
    let bonus_rolls = j
        .get("bonus_rolls")
        .map(parse_number)
        .unwrap_or(NumberProvider::Constant(0.0));
    let conditions = parse_conditions(j.get("conditions"), tags, warn);
    let functions = parse_functions(j.get("functions"), tags, warn);
    let mut entries = Vec::new();
    if let Some(arr) = j.get("entries").and_then(Json::as_arr) {
        for e in arr {
            entries.push(parse_entry(e, tags, warn)?);
        }
    }
    Ok(Pool {
        rolls,
        bonus_rolls,
        conditions,
        functions,
        entries,
    })
}

fn parse_entry(
    j: &Json,
    tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Result<Entry, String> {
    let weight = j.i32_or("weight", 1);
    let conditions = parse_conditions(j.get("conditions"), tags, warn);
    let functions = parse_functions(j.get("functions"), tags, warn);
    let ty = j.str_of("type").unwrap_or("minecraft:item");
    let kind = match ty {
        "minecraft:item" => EntryKind::Item(
            j.str_of("name")
                .ok_or("item entry without a name")?
                .to_owned(),
        ),
        "minecraft:tag" => {
            let name = j.str_of("name").ok_or("tag entry without a name")?;
            let items = tags.map(|t| t.resolve("item", name)).unwrap_or_default();
            EntryKind::Tag {
                items,
                expand: j.bool_or("expand", false),
            }
        }
        "minecraft:loot_table" => EntryKind::LootTable(
            j.str_of("value")
                .or_else(|| j.str_of("name"))
                .unwrap_or("")
                .to_owned(),
        ),
        "minecraft:alternatives" => EntryKind::Alternatives(parse_children(j, tags, warn)?),
        "minecraft:group" => EntryKind::Group(parse_children(j, tags, warn)?),
        "minecraft:sequence" => EntryKind::Sequence(parse_children(j, tags, warn)?),
        "minecraft:empty" => EntryKind::Empty,
        // `dynamic` (decorated pot sherds, shulker boxes, …) reads the block
        // entity; the engine keeps none, so it produces nothing.
        "minecraft:dynamic" | "minecraft:slots" => {
            note(warn, "entry", ty);
            EntryKind::Empty
        }
        other => return Err(format!("entry type `{other}`")),
    };
    Ok(Entry {
        weight,
        conditions,
        functions,
        kind,
    })
}

fn parse_children(
    j: &Json,
    tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Result<Vec<Entry>, String> {
    let arr = j
        .get("children")
        .and_then(Json::as_arr)
        .ok_or("composite entry without children")?;
    arr.iter().map(|c| parse_entry(c, tags, warn)).collect()
}

fn parse_number(j: &Json) -> NumberProvider {
    match j {
        Json::Num(n) => NumberProvider::Constant(*n as f32),
        Json::Obj(_) => match j.str_of("type").unwrap_or("minecraft:constant") {
            "minecraft:constant" => NumberProvider::Constant(j.f64_or("value", 0.0) as f32),
            "minecraft:uniform" => NumberProvider::Uniform {
                min: j.f64_or("min", 0.0) as f32,
                max: j.f64_or("max", 0.0) as f32,
            },
            "minecraft:binomial" => NumberProvider::Binomial {
                n: j.i32_or("n", 0),
                p: j.f64_or("p", 0.0) as f32,
            },
            _ => NumberProvider::Constant(field_or(j, "value", "count", 1.0) as f32),
        },
        _ => NumberProvider::Constant(0.0),
    }
}

/// Some providers (e.g. `score`) carry their value under another key; this
/// keeps a best-effort default.
fn field_or(j: &Json, a: &str, b: &str, default: f64) -> f64 {
    j.get(a)
        .and_then(Json::as_f64)
        .or_else(|| j.get(b).and_then(Json::as_f64))
        .unwrap_or(default)
}

fn parse_conditions(
    j: Option<&Json>,
    tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Vec<Condition> {
    j.and_then(Json::as_arr)
        .map(|arr| {
            arr.iter()
                // An unparsed condition fails closed: better no drop than the
                // wrong one. It is recorded so the gap is visible.
                .map(|c| parse_condition(c, tags, warn).unwrap_or(Condition::Impossible))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_condition(
    j: &Json,
    tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Option<Condition> {
    let ty = j.str_of("condition")?;
    Some(match ty {
        "minecraft:random_chance" => Condition::RandomChance(j.f64_or("chance", 0.0) as f32),
        "minecraft:random_chance_with_enchanted_bonus" => {
            Condition::RandomChanceWithEnchantedBonus {
                enchantment: with_ns(j.str_of("enchantment").unwrap_or("")),
                unenchanted: j.f64_or("unenchanted_chance", 0.0) as f32,
                enchanted: j
                    .get("enchanted_chance")
                    .map(parse_level_value)
                    .unwrap_or(LevelBasedValue::Constant(0.0)),
            }
        }
        "minecraft:table_bonus" => Condition::TableBonus {
            enchantment: with_ns(j.str_of("enchantment").unwrap_or("")),
            chances: j
                .get("chances")
                .and_then(Json::as_arr)
                .map(|a| {
                    a.iter()
                        .filter_map(Json::as_f64)
                        .map(|v| v as f32)
                        .collect()
                })
                .unwrap_or_default(),
        },
        "minecraft:killed_by_player" => Condition::KilledByPlayer,
        "minecraft:survives_explosion" => Condition::SurvivesExplosion,
        "minecraft:match_tool" => {
            let pred = j.get("predicate")?;
            // Any predicate key we do not model makes the condition impossible
            // rather than permissive.
            let known = ["items", "predicates"];
            if pred
                .as_obj()
                .is_some_and(|m| m.keys().any(|k| !known.contains(&k.as_str())))
            {
                return Some(Condition::Impossible);
            }
            let items = pred.get("items").map(|v| match v {
                Json::Str(s) => vec![s.to_owned()],
                Json::Arr(a) => a
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            });
            let items = items.map(|v| {
                let mut out = Vec::new();
                for s in v {
                    if let Some(tag) = s.strip_prefix('#') {
                        if let Some(t) = tags {
                            out.extend(t.resolve("item", tag));
                        }
                    } else {
                        out.push(with_ns(&s));
                    }
                }
                out
            });
            let predicates = pred.get("predicates");
            if predicates
                .and_then(Json::as_obj)
                .is_some_and(|m| m.keys().any(|k| k != "minecraft:enchantments"))
            {
                return Some(Condition::Impossible);
            }
            let enchants = predicates
                .and_then(|p| p.get("minecraft:enchantments"))
                .and_then(Json::as_arr)
                .map(|a| {
                    a.iter()
                        .filter_map(|e| {
                            let name = e.str_of("enchantments")?;
                            let min = e
                                .get("levels")
                                .and_then(|l| l.get("min"))
                                .and_then(Json::as_f64)
                                .unwrap_or(1.0) as i32;
                            let mut names = Vec::new();
                            if let Some(tag) = name.strip_prefix('#') {
                                if let Some(t) = tags {
                                    names = t.resolve("enchantment", tag);
                                }
                            } else {
                                names.push(with_ns(name));
                            }
                            Some(names.into_iter().map(move |n| (n, min)))
                        })
                        .flatten()
                        .collect()
                })
                .unwrap_or_default();
            Condition::MatchTool { items, enchants }
        }
        "minecraft:block_state_property" => Condition::BlockStateProperty {
            block: j.str_of("block").map(with_ns),
            properties: j
                .get("properties")
                .and_then(Json::as_obj)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default(),
        },
        "minecraft:entity_properties" => {
            let entity = match j.str_of("entity").unwrap_or("this") {
                "killer" => EntityRef::Killer,
                "attacker" => EntityRef::Attacker,
                "direct_attacker" => EntityRef::DirectAttacker,
                _ => EntityRef::This,
            };
            let pred = j.get("predicate").cloned().unwrap_or(Json::Null);
            // `vehicle`, `equipment`, `nbt`, `effects`, `slots`, … cannot be
            // evaluated here; fail closed rather than match by accident.
            // `type`, `flags`, `components` and `type_specific` are handled.
            let known = ["type", "flags", "components", "type_specific"];
            if pred
                .as_obj()
                .is_some_and(|m| m.keys().any(|k| !known.contains(&k.as_str())))
            {
                return Some(Condition::Impossible);
            }
            if pred.get("flags").and_then(Json::as_obj).is_some_and(|m| {
                m.keys()
                    .any(|k| !["is_on_fire", "is_baby", "is_sneaking"].contains(&k.as_str()))
            }) {
                return Some(Condition::Impossible);
            }
            let (kind, kind_tag) = match pred.get("type") {
                Some(Json::Str(s)) => {
                    if let Some(tag) = s.strip_prefix('#') {
                        (None, tags.map(|t| t.resolve("entity_type", tag)))
                    } else {
                        (Some(with_ns(s)), None)
                    }
                }
                Some(Json::Arr(a)) => {
                    let mut names = Vec::new();
                    for v in a {
                        if let Some(s) = v.as_str() {
                            if let Some(tag) = s.strip_prefix('#') {
                                if let Some(t) = tags {
                                    names.extend(t.resolve("entity_type", tag));
                                }
                            } else {
                                names.push(with_ns(s));
                            }
                        }
                    }
                    (None, Some(names))
                }
                _ => (None, None),
            };
            // Components must be simple name → string pairs; anything else
            // (nested predicates) is not understood.
            let mut components = Vec::new();
            if let Some(obj) = pred.get("components").and_then(Json::as_obj) {
                for (k, v) in obj {
                    match v {
                        Json::Str(s) => components.push((k.clone(), s.clone())),
                        _ => return Some(Condition::Impossible),
                    }
                }
            }
            // `type_specific` must name a concrete entity type and carry only
            // int/bool fields.
            let type_specific = match pred.get("type_specific").and_then(Json::as_obj) {
                None => None,
                Some(obj) => {
                    let Some(ty) = obj.get("type").and_then(Json::as_str) else {
                        return Some(Condition::Impossible);
                    };
                    if ty.starts_with('#') {
                        return Some(Condition::Impossible);
                    }
                    let mut fields = Vec::new();
                    for (k, v) in obj {
                        if k == "type" {
                            continue;
                        }
                        match v {
                            Json::Num(n) => fields.push((k.clone(), PredValue::Int(*n as i64))),
                            Json::Bool(b) => fields.push((k.clone(), PredValue::Bool(*b))),
                            _ => return Some(Condition::Impossible),
                        }
                    }
                    Some((with_ns(ty), fields))
                }
            };
            let flags = pred.get("flags");
            Condition::EntityProperties {
                entity,
                kind,
                kind_tag,
                on_fire: flags.is_some_and(|f| f.bool_or("is_on_fire", false)),
                is_baby: flags.is_some_and(|f| f.bool_or("is_baby", false)),
                sneaking: flags.is_some_and(|f| f.bool_or("is_sneaking", false)),
                components,
                type_specific,
            }
        }
        "minecraft:any_of" => Condition::AnyOf(parse_conditions(j.get("terms"), tags, warn)),
        "minecraft:all_of" => Condition::AllOf(parse_conditions(j.get("terms"), tags, warn)),
        "minecraft:inverted" => Condition::Inverted(Box::new(
            j.get("term").and_then(|t| parse_condition(t, tags, warn))?,
        )),
        // `location_check`, `weather_check`, `value_check`, `time_check`,
        // `reference`, `damage_source_properties`, … — not modelled; the
        // condition is treated as failing so a table that needs one does not
        // produce the wrong drop.
        _ => {
            note(warn, "condition", ty);
            return None;
        }
    })
}

fn parse_level_value(j: &Json) -> LevelBasedValue {
    match j.str_of("type").unwrap_or("minecraft:constant") {
        "minecraft:linear" => LevelBasedValue::Linear {
            base: j.f64_or("base", 0.0) as f32,
            per_level_above_first: j.f64_or("per_level_above_first", 0.0) as f32,
        },
        _ => LevelBasedValue::Constant(j.f64_or("value", 0.0) as f32),
    }
}

fn parse_functions(
    j: Option<&Json>,
    tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Vec<Function> {
    j.and_then(Json::as_arr)
        .map(|arr| {
            arr.iter()
                .filter_map(|f| parse_function(f, tags, warn))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_function(
    j: &Json,
    tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Option<Function> {
    let inner = parse_function_inner(j, tags, warn)?;
    let conditions = parse_conditions(j.get("conditions"), tags, warn);
    if conditions.is_empty() {
        Some(inner)
    } else {
        Some(Function::Conditional {
            conditions,
            inner: Box::new(inner),
        })
    }
}

fn parse_function_inner(
    j: &Json,
    _tags: Option<&TagLoader>,
    warn: &mut BTreeSet<String>,
) -> Option<Function> {
    let ty = j.str_of("function")?;
    Some(match ty {
        "minecraft:set_count" => Function::SetCount {
            count: j
                .get("count")
                .map(parse_number)
                .unwrap_or(NumberProvider::Constant(1.0)),
            // Vanilla defaults `add` to false (replace the count).
            add: j.bool_or("add", false),
        },
        "minecraft:limit_count" => {
            // `LimitCount.CODEC` is `IntRange.CODEC.fieldOf("limit")`, so the
            // bounds live under `limit`, not at the top level.
            let limit = j.get("limit");
            Function::LimitCount {
                min: limit
                    .and_then(|l| l.get("min"))
                    .and_then(Json::as_f64)
                    .map(|v| v as i32),
                max: limit
                    .and_then(|l| l.get("max"))
                    .and_then(Json::as_f64)
                    .map(|v| v as i32),
            }
        }
        "minecraft:apply_bonus" => Function::ApplyBonus {
            enchantment: with_ns(j.str_of("enchantment").unwrap_or("")),
            formula: match j
                .str_of("formula")
                .unwrap_or("minecraft:uniform_bonus_count")
            {
                "minecraft:ore_drops" => BonusFormula::OreDrops,
                "minecraft:binomial_with_bonus_count" => BonusFormula::BinomialWithBonusCount {
                    probability: j
                        .get("parameters")
                        .map(|p| p.f64_or("probability", 0.0) as f32)
                        .unwrap_or(j.f64_or("probability", 0.0) as f32),
                    extra: j
                        .get("parameters")
                        .map(|p| p.i32_or("extra", 0))
                        .unwrap_or(j.i32_or("extra", 0)),
                },
                _ => BonusFormula::UniformBonusCount {
                    bonus_multiplier: j.i32_or("bonusMultiplier", 1),
                },
            },
        },
        "minecraft:explosion_decay" => Function::ExplosionDecay,
        "minecraft:furnace_smelt" => Function::FurnaceSmelt,
        "minecraft:enchanted_count_increase" | "minecraft:looting_enchant" => {
            let enchantment = with_ns(j.str_of("enchantment").unwrap_or("minecraft:looting"));
            let count = j
                .get("count")
                .map(parse_number)
                .unwrap_or(NumberProvider::Constant(1.0));
            let limit = j.i32_or("limit", 0);
            Function::EnchantedCountIncrease {
                enchantment,
                count,
                limit: limit.max(0) as u32,
            }
        }
        "minecraft:enchant_randomly" => {
            note(warn, "noop-function", ty);
            Function::EnchantRandomly
        }
        "minecraft:enchant_with_levels" => {
            note(warn, "noop-function", ty);
            Function::EnchantWithLevels
        }
        "minecraft:set_damage" => {
            note(warn, "noop-function", ty);
            Function::SetDamage
        }
        "minecraft:set_components" | "minecraft:set_name" | "minecraft:set_lore" => {
            note(warn, "noop-function", ty);
            Function::SetComponents
        }
        // `set_potion`, `set_stew_effect`, `set_banner_pattern`, `copy_name`,
        // `copy_components`, `set_enchantments`, `set_instrument`, … — not
        // modelled; skipped, and recorded so the gap is visible.
        _ => {
            note(warn, "function", ty);
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_from(json: &str) -> LootTable {
        let j = Json::parse(json).unwrap();
        parse_table("minecraft:test", &j, None, &mut BTreeSet::new()).unwrap()
    }

    /// A `loot_table` reference that loops back on itself must be cut, not
    /// recursed until the stack overflows.
    #[test]
    fn cyclic_loot_table_references_do_not_recurse() {
        let dir = std::env::temp_dir().join(format!("aether-loot-cycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lt = dir.join("data/minecraft/loot_table");
        std::fs::create_dir_all(&lt).unwrap();
        std::fs::write(
            lt.join("a.json"),
            r#"{"pools":[{"rolls":1,"entries":[
                {"type":"minecraft:loot_table","value":"minecraft:b"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            lt.join("b.json"),
            r#"{"pools":[{"rolls":1,"entries":[
                {"type":"minecraft:loot_table","value":"minecraft:a"}]}]}"#,
        )
        .unwrap();
        let tables = LootTables::open(dir.to_str().unwrap());
        assert!(tables.table("minecraft:a").is_some());
        assert!(
            tables.unsupported().iter().any(|s| s.contains("cyclic")),
            "the cycle should be recorded: {:?}",
            tables.unsupported()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_level_functions_apply() {
        // A root-level function runs over everything the table produced.
        let t = table_from(
            r#"{"functions":[{"function":"minecraft:limit_count","limit":{"max":0}}],
                "pools":[{"rolls":1,"entries":[
                    {"type":"minecraft:item","name":"minecraft:apple"}]}]}"#,
        );
        let mut rng = Rng::new(1);
        assert!(t.roll(&LootContext::default(), &mut rng).is_empty());
    }

    #[test]
    fn ore_drops_follow_fortune() {
        // The real diamond-ore table, trimmed to its essentials.
        let t = table_from(
            r#"{
              "pools": [{
                "rolls": 1.0,
                "bonus_rolls": 0.0,
                "entries": [{
                  "type": "minecraft:alternatives",
                  "children": [
                    {"type":"minecraft:item","name":"minecraft:diamond_ore",
                     "conditions":[{"condition":"minecraft:match_tool",
                       "predicate":{"predicates":{"minecraft:enchantments":[
                         {"enchantments":"minecraft:silk_touch","levels":{"min":1}}]}}}]},
                    {"type":"minecraft:item","name":"minecraft:diamond",
                     "functions":[{"function":"minecraft:apply_bonus",
                       "enchantment":"minecraft:fortune","formula":"minecraft:ore_drops"}]}
                  ]
                }]
              }]
            }"#,
        );

        let mut rng = Rng::new(7);
        // Silk touch: the ore itself.
        let ctx = LootContext {
            tool: Some("minecraft:diamond_pickaxe".into()),
            ..Default::default()
        }
        .with_enchant("minecraft:silk_touch", 1);
        let out = t.roll(&ctx, &mut rng);
        assert_eq!(
            out,
            vec![LootStack {
                item: "minecraft:diamond_ore".into(),
                count: 1
            }]
        );

        // No silk, no fortune: exactly one diamond.
        let ctx = LootContext::block(Some("minecraft:iron_pickaxe"));
        let out = t.roll(&ctx, &mut rng);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item, "minecraft:diamond");
        assert_eq!(out[0].count, 1);

        // Fortune III: between one and four, averaging 2.2.
        let ctx = LootContext::block(Some("minecraft:diamond_pickaxe"))
            .with_enchant("minecraft:fortune", 3);
        let mut total = 0u64;
        let mut counts = [0u64; 5];
        for _ in 0..100_000 {
            let out = t.roll(&ctx, &mut rng);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].item, "minecraft:diamond");
            total += out[0].count as u64;
            counts[out[0].count as usize] += 1;
        }
        let mean = total as f64 / 100_000.0;
        assert!((mean - 2.2).abs() < 0.02, "mean was {mean}");
        // oredrops: P(1)=2/5, P(2)=P(3)=P(4)=1/5.
        assert!((counts[1] as f64 / 100_000.0 - 0.4).abs() < 0.01);
        assert!((counts[2] as f64 / 100_000.0 - 0.2).abs() < 0.01);
        assert!((counts[3] as f64 / 100_000.0 - 0.2).abs() < 0.01);
        assert!((counts[4] as f64 / 100_000.0 - 0.2).abs() < 0.01);
    }

    #[test]
    fn enchanted_count_increase_is_round_level_times_float() {
        // Vanilla grows by `round(level * count.getFloat)`. With a `uniform
        // 0..1` provider and looting III that is 0/1/2/3 at 1/6, 1/3, 1/3,
        // 1/6 — not `getInt * level` (only 0 or 3).
        let t = table_from(
            r#"{"pools":[{"rolls":1,"entries":[
                {"type":"minecraft:item","name":"minecraft:diamond",
                 "functions":[
                   {"function":"minecraft:set_count","add":false,"count":10},
                   {"function":"minecraft:enchanted_count_increase",
                    "enchantment":"minecraft:looting",
                    "count":{"type":"minecraft:uniform","min":0,"max":1}}]}]}]}"#,
        );
        let ctx = LootContext {
            enchants: [("minecraft:looting".to_owned(), 3)].into_iter().collect(),
            ..Default::default()
        };
        let mut rng = Rng::new(17);
        let mut counts = [0u64; 4];
        for _ in 0..120_000 {
            let out = t.roll(&ctx, &mut rng);
            let n = out[0].count;
            assert!((10..=13).contains(&n), "count {n}");
            counts[(n - 10) as usize] += 1;
        }
        let total = 120_000f64;
        assert!((counts[0] as f64 / total - 1.0 / 6.0).abs() < 0.02);
        assert!((counts[1] as f64 / total - 1.0 / 3.0).abs() < 0.02);
        assert!((counts[2] as f64 / total - 1.0 / 3.0).abs() < 0.02);
        assert!((counts[3] as f64 / total - 1.0 / 6.0).abs() < 0.02);
    }

    #[test]
    fn alternatives_evaluate_a_conditions_once() {
        // Vanilla `alternatives` tests each child's conditions once, then runs
        // the chosen child. Re-testing the `random_chance` child squares its
        // rate (0.125 → 0.015625).
        let t = table_from(
            r#"{"pools":[{"rolls":1,"entries":[{"type":"minecraft:alternatives","children":[
                {"type":"minecraft:item","name":"minecraft:shears_item",
                 "conditions":[{"condition":"minecraft:match_tool","predicate":{"items":"minecraft:shears"}}]},
                {"type":"minecraft:item","name":"minecraft:seed",
                 "conditions":[{"condition":"minecraft:random_chance","chance":0.125}]}]}]}]}"#,
        );
        let mut rng = Rng::new(123);
        let mut seeds = 0u32;
        for _ in 0..200_000 {
            if t.roll(&LootContext::default(), &mut rng)
                .iter()
                .any(|s| s.item == "minecraft:seed")
            {
                seeds += 1;
            }
        }
        let rate = seeds as f64 / 200_000.0;
        assert!((rate - 0.125).abs() < 0.005, "seed rate was {rate}");
    }

    #[test]
    fn a_failing_entry_is_excluded_before_weighting() {
        // An entry whose condition cannot pass must not consume a slot in the
        // weighted draw, or the roll silently yields nothing.
        let t = table_from(
            r#"{"pools":[{"rolls":1,"entries":[
                {"type":"minecraft:item","name":"minecraft:never","weight":1,
                 "conditions":[{"condition":"minecraft:random_chance","chance":0.0}]},
                {"type":"minecraft:item","name":"minecraft:always","weight":1}]}]}"#,
        );
        let mut rng = Rng::new(1);
        for _ in 0..1000 {
            let out = t.roll(&LootContext::default(), &mut rng);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].item, "minecraft:always");
        }
    }

    #[test]
    fn weighted_pools_and_conditions() {
        // Two pools: always one of a weighted pair, plus a gated rare pool.
        let t = table_from(
            r#"{
              "pools": [
                {"rolls": 1, "entries": [
                  {"type":"minecraft:item","name":"minecraft:common","weight":3},
                  {"type":"minecraft:item","name":"minecraft:rare","weight":1}]},
                {"rolls": 1, "conditions":[
                   {"condition":"minecraft:random_chance","chance":1.0}],
                 "entries":[{"type":"minecraft:item","name":"minecraft:bonus"}]}
              ]
            }"#,
        );
        let ctx = LootContext::default();
        let mut rng = Rng::new(1);
        let out = t.roll(&ctx, &mut rng);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|s| s.item == "minecraft:bonus"));
    }

    #[test]
    fn set_count_defaults_to_replace_and_limit_reads_limit() {
        // Vanilla `set_count` without `add` replaces the count (base 1 → 5),
        // rather than adding to it (which would give 6).
        let t = table_from(
            r#"{"pools":[{"rolls":1,"entries":[
                {"type":"minecraft:item","name":"minecraft:apple",
                 "functions":[{"function":"minecraft:set_count",
                    "count":{"type":"minecraft:uniform","min":5,"max":5}}]}]}]}"#,
        );
        let mut rng = Rng::new(1);
        let out = t.roll(&LootContext::default(), &mut rng);
        assert_eq!(out[0].count, 5, "set_count must replace by default");

        // `limit_count` carries its bounds under `limit`.
        let t = table_from(
            r#"{"pools":[{"rolls":1,"entries":[
                {"type":"minecraft:item","name":"minecraft:apple",
                 "functions":[
                   {"function":"minecraft:set_count","count":9},
                   {"function":"minecraft:limit_count","limit":{"max":4}}]}]}]}"#,
        );
        let mut rng = Rng::new(1);
        assert_eq!(t.roll(&LootContext::default(), &mut rng)[0].count, 4);
    }

    /// The real melon table clamps its slices to 3..=9 with `limit`.
    #[test]
    fn real_melon_limit_count_clamps() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);
        let t = tables.block_table("minecraft:melon").expect("melon table");
        let mut rng = Rng::new(3);
        for _ in 0..20_000 {
            let out = t.roll(&LootContext::default(), &mut rng);
            for s in out {
                assert_eq!(s.item, "minecraft:melon_slice");
                assert!((3..=9).contains(&s.count), "melon slices {}", s.count);
            }
        }
    }

    #[test]
    fn survives_explosion_always_true_without_an_explosion() {
        let t = table_from(
            r#"{"pools":[{"rolls":1,"entries":[{"type":"minecraft:item",
                "name":"minecraft:cobblestone",
                "conditions":[{"condition":"minecraft:survives_explosion"}]}]}]}"#,
        );
        let mut rng = Rng::new(3);
        assert_eq!(t.roll(&LootContext::default(), &mut rng).len(), 1);
    }

    #[test]
    fn hash_positions_differ() {
        assert_ne!(hash_pos((1, 2, 3)), hash_pos((1, 2, 4)));
        assert_ne!(hash_table("minecraft:blocks/stone"), hash_pos((0, 0, 0)));
    }

    #[test]
    fn zombie_looting_raises_the_drop() {
        // The zombie's rotten-flesh pool, trimmed to `set_count` +
        // `enchanted_count_increase`.
        let t = table_from(
            r#"{
              "pools": [{"rolls":1,"entries":[{
                "type":"minecraft:item","name":"minecraft:rotten_flesh",
                "functions":[
                  {"function":"minecraft:set_count","add":false,
                   "count":{"type":"minecraft:uniform","min":0,"max":2}},
                  {"function":"minecraft:enchanted_count_increase",
                   "enchantment":"minecraft:looting",
                   "count":{"type":"minecraft:uniform","min":0,"max":1}}]}]}]
            }"#,
        );
        let mut rng = Rng::new(11);
        let mut sum = 0u64;
        let mut max = 0u8;
        for _ in 0..50_000 {
            let out = t.roll(&LootContext::default(), &mut rng);
            let n = out.first().map_or(0, |s| s.count);
            assert!(n <= 2, "no looting must stay 0..2, got {n}");
            sum += n as u64;
            max = max.max(n);
        }
        assert_eq!(max, 2);
        assert!((sum as f64 / 50_000.0 - 1.0).abs() < 0.03);

        let looted = LootContext {
            enchants: [("minecraft:looting".to_owned(), 3)].into_iter().collect(),
            ..Default::default()
        };
        assert_eq!(looted.enchant_level("minecraft:looting"), 3);
        let mut sum = 0u64;
        let mut max = 0u8;
        for _ in 0..50_000 {
            let out = t.roll(&looted, &mut rng);
            let n = out.first().map_or(0, |s| s.count);
            assert!(n <= 5, "looting III must stay 0..5, got {n}");
            sum += n as u64;
            max = max.max(n);
        }
        assert_eq!(max, 5);
        // base mean 1.0 + (0 or 3) → 2.5.
        assert!((sum as f64 / 50_000.0 - 2.5).abs() < 0.05);
    }

    #[test]
    fn chest_rolls_are_deterministic_per_seed() {
        // A trimmed dungeon-ish table with weights and a set_count.
        let j = r#"{
          "pools": [{"rolls":{"type":"minecraft:uniform","min":1,"max":3},
            "entries":[
              {"type":"minecraft:item","name":"minecraft:bone","weight":5},
              {"type":"minecraft:item","name":"minecraft:coal","weight":3,
               "functions":[{"function":"minecraft:set_count","add":false,
                 "count":{"type":"minecraft:uniform","min":1,"max":4}}]},
              {"type":"minecraft:item","name":"minecraft:diamond","weight":1}]}]
        }"#;
        let table = table_from(j);
        let roll = |seed: u64| {
            let mut rng = Rng::new(
                seed ^ hash_pos((4, 5, 6)) ^ hash_table("minecraft:chests/simple_dungeon"),
            );
            table.roll(&LootContext::default(), &mut rng)
        };
        // Same seed, same result.
        assert_eq!(roll(42), roll(42));
        // Some other world seed differs.
        assert!(roll(42) != roll(43) || roll(43) != roll(44));
        assert!(!roll(42).is_empty());
    }

    /// The pack-backed checks; skipped without `AETHER_VANILLA_PACK`.
    #[test]
    fn real_tables_load_and_match_vanilla() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);

        // Diamond ore: silk touch drops the ore, fortune III averages 2.2.
        let t = tables
            .block_table("minecraft:diamond_ore")
            .expect("diamond_ore table");
        let mut rng = Rng::new(9);
        let silk = LootContext {
            tool: Some("minecraft:diamond_pickaxe".into()),
            ..Default::default()
        }
        .with_enchant("minecraft:silk_touch", 1);
        let out = t.roll(&silk, &mut rng);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].item, "minecraft:diamond_ore");
        let fortune = LootContext::block(Some("minecraft:diamond_pickaxe"))
            .with_enchant("minecraft:fortune", 3);
        let mut total = 0u64;
        for _ in 0..20_000 {
            let out = t.roll(&fortune, &mut rng);
            assert_eq!(out[0].item, "minecraft:diamond");
            total += out[0].count as u64;
        }
        assert!((total as f64 / 20_000.0 - 2.2).abs() < 0.05);

        // Zombie: rotten flesh 0..2 ungated; the rare pool needs a player kill.
        let z = tables
            .entity_table("minecraft:zombie")
            .expect("zombie table");
        let no_kill = LootContext {
            entity_kind: Some("minecraft:zombie".into()),
            ..Default::default()
        };
        for _ in 0..5_000 {
            let out = z.roll(&no_kill, &mut rng);
            for s in &out {
                assert_eq!(s.item, "minecraft:rotten_flesh");
                assert!(s.count <= 2);
            }
        }
        let kill = LootContext {
            entity_kind: Some("minecraft:zombie".into()),
            player_kill: true,
            ..Default::default()
        };
        let mut ever_extra = false;
        for _ in 0..20_000 {
            for s in z.roll(&kill, &mut rng) {
                if s.item != "minecraft:rotten_flesh" {
                    ever_extra = true;
                }
            }
        }
        assert!(ever_extra, "the player-kill pool should fire sometimes");

        // A chest rolls the same once per seed and position.
        let a = tables.roll_seeded(
            "minecraft:chests/simple_dungeon",
            1234,
            (10, -20, 30),
            &LootContext::default(),
        );
        let b = tables.roll_seeded(
            "minecraft:chests/simple_dungeon",
            1234,
            (10, -20, 30),
            &LootContext::default(),
        );
        assert_eq!(a, b);
        assert!(a.is_some_and(|v| !v.is_empty()));
    }

    /// Vanilla's short-grass seed rate is one draw of `random_chance 0.125`,
    /// on the `alternatives` child — not the condition tested twice.
    #[test]
    fn real_short_grass_seed_rate_is_vanilla() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);
        let t = tables
            .block_table("minecraft:short_grass")
            .expect("short_grass table");
        let mut rng = Rng::new(5);
        let mut seeds = 0u32;
        for _ in 0..200_000 {
            if t.roll(&LootContext::default(), &mut rng)
                .iter()
                .any(|s| s.item == "minecraft:wheat_seeds")
            {
                seeds += 1;
            }
        }
        let rate = seeds as f64 / 200_000.0;
        assert!((rate - 0.125).abs() < 0.005, "seed rate was {rate}");
    }

    #[test]
    fn tag_expand_false_emits_every_member() {
        // `expand:false` is a singleton that drops the whole tag at once.
        let table = LootTable {
            functions: Vec::new(),
            pools: vec![Pool {
                rolls: NumberProvider::Constant(1.0),
                bonus_rolls: NumberProvider::Constant(0.0),
                conditions: Vec::new(),
                functions: Vec::new(),
                entries: vec![Entry {
                    weight: 1,
                    conditions: Vec::new(),
                    functions: Vec::new(),
                    kind: EntryKind::Tag {
                        items: vec!["minecraft:a".into(), "minecraft:b".into()],
                        expand: false,
                    },
                }],
            }],
        };
        let mut rng = Rng::new(1);
        let out = table.roll(&LootContext::default(), &mut rng);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|s| s.item == "minecraft:a"));
        assert!(out.iter().any(|s| s.item == "minecraft:b"));
    }

    /// `expand:true` is a composite — one weighted candidate per tag member,
    /// so a roll yields exactly one disc, not the whole tag.
    #[test]
    fn real_creeper_tag_yields_one_disc() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);
        let t = tables
            .entity_table("minecraft:creeper")
            .expect("creeper table");
        let ctx = LootContext {
            entity_kind: Some("minecraft:creeper".into()),
            attacker_kind: Some("minecraft:skeleton".into()),
            ..Default::default()
        };
        let mut rng = Rng::new(7);
        let mut discs = 0u32;
        for _ in 0..20_000 {
            let out = t.roll(&ctx, &mut rng);
            let disc: Vec<_> = out
                .iter()
                .filter(|s| s.item.starts_with("minecraft:music_disc_"))
                .collect();
            assert_eq!(disc.len(), 1, "expected one disc, got {:?}", disc);
            discs += 1;
        }
        assert_eq!(discs, 20_000);
    }

    /// Vanilla `BinomialWithBonusCount` runs `level + extra` rounds, each
    /// adding one; the wheat-seed table is `extra:3, p:0.571` with fortune
    /// III, so 1 + Binomial(6, 0.571) → mean ≈ 4.43, max 7.
    #[test]
    fn real_wheat_seeds_match_binomial_bonus() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);
        let t = tables.block_table("minecraft:wheat").expect("wheat table");
        let ctx = LootContext {
            block: Some(("minecraft:wheat".into(), vec![("age".into(), "7".into())])),
            ..Default::default()
        }
        .with_enchant("minecraft:fortune", 3);
        let mut rng = Rng::new(9);
        let mut sum = 0u64;
        let mut max = 0u8;
        for _ in 0..100_000 {
            let out = t.roll(&ctx, &mut rng);
            let seeds = out
                .iter()
                .find(|s| s.item == "minecraft:wheat_seeds")
                .map_or(0, |s| s.count);
            sum += seeds as u64;
            max = max.max(seeds);
        }
        assert_eq!(max, 7);
        let mean = sum as f64 / 100_000.0;
        assert!((mean - 4.4286).abs() < 0.05, "mean was {mean}");
    }

    /// Sheep wool is dispatched by the `components`/`type_specific`
    /// predicates: colour picks the sub-table, and a sheared sheep drops no
    /// wool. The old code deleted the whole pool and hard-coded white wool.
    #[test]
    fn real_sheep_wool_follows_colour_and_shear() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);
        let t = tables.entity_table("minecraft:sheep").expect("sheep table");
        let roll = |colour: &str, sheared: bool, rng: &mut Rng| {
            let ctx = LootContext {
                entity_kind: Some("minecraft:sheep".into()),
                entity_components: vec![("minecraft:sheep/color".into(), colour.into())],
                entity_type_specific: Some((
                    "minecraft:sheep".into(),
                    vec![("sheared".into(), sheared as i64)],
                )),
                ..Default::default()
            };
            t.roll(&ctx, rng)
        };
        let mut rng = Rng::new(4);
        for _ in 0..500 {
            let out = roll("white", false, &mut rng);
            assert!(
                out.iter().any(|s| s.item == "minecraft:white_wool"),
                "white ewe dropped {out:?}"
            );
            assert!(out.iter().any(|s| s.item == "minecraft:mutton"));
            let out = roll("black", false, &mut rng);
            assert!(out.iter().any(|s| s.item == "minecraft:black_wool"));
            let out = roll("white", true, &mut rng);
            assert!(
                !out.iter().any(|s| s.item.ends_with("_wool")),
                "sheared ewe still dropped wool: {out:?}"
            );
        }
    }

    /// The generator stores no loot table on a chest, so which one it rolls
    /// is inferred from the structure around it — and never for a position the
    /// journal has seen (a player's chest).
    #[test]
    fn chest_table_is_inferred_from_the_structure() {
        use std::collections::HashMap;
        let empty: HashMap<(i32, i32, i32), String> = HashMap::new();
        assert_eq!(
            chest_table_from_blocks(false, (0, 0, 0), |x, y, z| empty.get(&(x, y, z)).cloned()),
            None
        );

        let mut dungeon = HashMap::new();
        dungeon.insert((1, 0, 0), "minecraft:spawner".to_string());
        let get = |m: &HashMap<(i32, i32, i32), String>| {
            let m = m.clone();
            move |x, y, z| m.get(&(x, y, z)).cloned()
        };
        assert_eq!(
            chest_table_from_blocks(false, (0, 0, 0), get(&dungeon)),
            Some("minecraft:chests/simple_dungeon")
        );
        // A touched position is a player's chest, whatever is around it.
        assert_eq!(
            chest_table_from_blocks(true, (0, 0, 0), get(&dungeon)),
            None
        );

        let mut igloo = HashMap::new();
        igloo.insert((2, 1, 0), "minecraft:brewing_stand".to_string());
        igloo.insert((-1, -1, 0), "minecraft:water_cauldron".to_string());
        assert_eq!(
            chest_table_from_blocks(false, (0, 0, 0), get(&igloo)),
            Some("minecraft:chests/igloo_chest")
        );
    }

    /// A generated chest's loot is the same every time for one seed and
    /// position, and lands in distinct in-range slots.
    #[test]
    fn generated_chest_loot_is_deterministic() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);
        let a = generated_chest_loot(
            &tables,
            "minecraft:chests/simple_dungeon",
            99,
            (4, 5, 6),
            27,
        )
        .expect("dungeon table");
        let b = generated_chest_loot(
            &tables,
            "minecraft:chests/simple_dungeon",
            99,
            (4, 5, 6),
            27,
        )
        .expect("dungeon table");
        assert_eq!(a, b);
        assert!(!a.is_empty());
        let mut slots: Vec<usize> = a.iter().map(|(s, _)| *s).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), a.len(), "slots must be distinct");
        assert!(slots.iter().all(|s| *s < 27));
    }

    /// An entry whose condition cannot run is excluded before the weighted
    /// draw, so the pool renormalises to the entries that can. The fishing
    /// junk pool's only such entry is bamboo (`location_check`).
    #[test]
    fn real_fishing_junk_excludes_unrunnable_entries() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);
        let t = tables
            .table("minecraft:gameplay/fishing/junk")
            .expect("junk table");
        let mut rng = Rng::new(21);
        for _ in 0..20_000 {
            let out = t.roll(&LootContext::default(), &mut rng);
            assert_eq!(
                out.len(),
                1,
                "a roll must not silently yield nothing: {out:?}"
            );
            assert_ne!(
                out[0].item, "minecraft:bamboo",
                "bamboo's location_check cannot pass"
            );
        }
    }

    /// The parser records what it skipped, and the real wheat table carries
    /// its root-level `explosion_decay`.
    #[test]
    fn unsupported_constructs_are_recorded() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tables = LootTables::open(&root);
        tables.warm();
        let skipped = tables.unsupported();
        assert!(!skipped.is_empty(), "the pack has unmodelled constructs");
        assert!(
            skipped.iter().any(|s| s.contains("set_potion")),
            "set_potion should be recorded: {skipped:?}"
        );
        assert!(
            skipped.iter().any(|s| s.starts_with("condition:")),
            "an unmodelled condition should be recorded"
        );
        let wheat = tables.block_table("minecraft:wheat").expect("wheat table");
        assert_eq!(wheat.functions.len(), 1, "root explosion_decay");
    }

    /// Every table in the pack must parse; the ones that do not are printed.
    #[test]
    fn every_real_table_parses() {
        let Ok(root) = std::env::var("AETHER_VANILLA_PACK") else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let dir = Path::new(&root).join("data/minecraft/loot_table");
        if !dir.is_dir() {
            eprintln!("skipping: no loot_table directory under {root}");
            return;
        }
        let tables = LootTables::open(&root);
        let mut ids = Vec::new();
        collect(dir.as_path(), "", &mut ids);
        let mut failed = Vec::new();
        for id in &ids {
            if tables.table(id).is_none() {
                failed.push(id.clone());
            }
        }
        assert!(
            failed.is_empty(),
            "{} of {} loot tables failed to parse: {:?}",
            failed.len(),
            ids.len(),
            &failed[..failed.len().min(20)]
        );
    }

    fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                collect(&p, &format!("{prefix}{name}/"), out);
            } else if let Some(stem) = name.strip_suffix(".json") {
                out.push(format!("minecraft:{prefix}{stem}"));
            }
        }
    }
}
