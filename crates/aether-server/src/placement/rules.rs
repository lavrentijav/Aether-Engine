//! Block placement rules: the authoritative replaceable list and the
//! "what can be placed on what" support table, plus its reverse index.
//!
//! `session::place_block` used to write a block into any cell it chose, with
//! no attachment check at all, and "replaceable" was a hand-written `match`
//! holding a *subset* of vanilla. This module replaces both: every placement
//! is checked against the rules below before anything is written.
//!
//! # Where each rule comes from
//!
//! The rules are deliberately split by provenance, and the module says which
//! is which (the generated markdown carries the same split):
//!
//! * **Data-derived** — the replaceable list and the block *sets* a rule
//!   accepts come from the vanilla data pack's block tags
//!   (`#minecraft:replaceable`, `#minecraft:dirt`, `#minecraft:sand`,
//!   `#minecraft:dry_vegetation_may_place_on`, …). They are generated into
//!   [`super::data`] by `tools/gen/placement.js`; a server with no pack reads
//!   that compiled-in copy, and the startup log says which was used.
//! * **Curated** — *which* blocks need support and *what kind* of support
//!   (a block below, a full neighbour face, a wall behind, water around) is
//!   code in vanilla's block classes, not data. The rules here are ported
//!   from the 1.21.11 server's own classes: `VegetationBlock.canSurvive`
//!   (`#minecraft:dirt` or farmland), `LeafLitterBlock.canSurvive` (a sturdy
//!   face below, and — see below — never under water), `BaseTorchBlock`
//!   (a support centre below), `WallTorchBlock`/`LadderBlock` (a sturdy face
//!   behind the block's `facing`), `MultifaceBlock`/`VineBlock` (a full face
//!   on any side), `CactusBlock`, `SugarCaneBlock`, `BambooSaplingBlock`,
//!   `SnowLayerBlock`, and the water plants.
//!
//! Nothing here is invented: where vanilla only checks a block's own state
//! (a double plant's upper half needs its lower half) that is handled by
//! `placement::upper_half_of`, not duplicated. A rule that was uncertain is
//! named in `docs/KNOWN_ISSUES.md`.
//!
//! # The one approximation
//!
//! Vanilla's `isFaceSturdy(FULL)` reads a block's *support* shape, which this
//! engine does not keep per state (`KNOWN_ISSUES` A.6). [`sturdy`] therefore
//! approximates it with the collision shape, excluding the block families
//! that are known to be partial. It errs towards *too few* supports, so the
//! failure mode is a refused placement rather than a floating block.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

use super::data;

// ---------------------------------------------------------------------------
// Tags: the data-derived block sets
// ---------------------------------------------------------------------------

/// A block tag from the pack that a rule can require, or read for the
/// replaceable list. The members are generated into [`super::data`] when no
/// pack is configured, and resolved from the pack when one is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tag {
    Dirt,
    Sand,
    Leaves,
    DryVegetationMayPlaceOn,
    BambooPlantableOn,
    BigDripleafPlaceable,
    SmallDripleafPlaceable,
    AzaleaGrowsOn,
    SnowLayerCanSurviveOn,
    SnowLayerCannotSurviveOn,
    MushroomGrowBlock,
    JungleLogs,
    Ice,
}

impl Tag {
    /// Every tag this module reads, in a stable order.
    pub const ALL: [Tag; 13] = [
        Tag::Dirt,
        Tag::Sand,
        Tag::Leaves,
        Tag::DryVegetationMayPlaceOn,
        Tag::BambooPlantableOn,
        Tag::BigDripleafPlaceable,
        Tag::SmallDripleafPlaceable,
        Tag::AzaleaGrowsOn,
        Tag::SnowLayerCanSurviveOn,
        Tag::SnowLayerCannotSurviveOn,
        Tag::MushroomGrowBlock,
        Tag::JungleLogs,
        Tag::Ice,
    ];

    /// The pack tag id, namespaced.
    pub fn id(self) -> &'static str {
        match self {
            Tag::Dirt => "minecraft:dirt",
            Tag::Sand => "minecraft:sand",
            Tag::Leaves => "minecraft:leaves",
            Tag::DryVegetationMayPlaceOn => "minecraft:dry_vegetation_may_place_on",
            Tag::BambooPlantableOn => "minecraft:bamboo_plantable_on",
            Tag::BigDripleafPlaceable => "minecraft:big_dripleaf_placeable",
            Tag::SmallDripleafPlaceable => "minecraft:small_dripleaf_placeable",
            Tag::AzaleaGrowsOn => "minecraft:azalea_grows_on",
            Tag::SnowLayerCanSurviveOn => "minecraft:snow_layer_can_survive_on",
            Tag::SnowLayerCannotSurviveOn => "minecraft:snow_layer_cannot_survive_on",
            Tag::MushroomGrowBlock => "minecraft:mushroom_grow_block",
            Tag::JungleLogs => "minecraft:jungle_logs",
            Tag::Ice => "minecraft:ice",
        }
    }

    /// The compiled-in members, from the generated [`super::data`] table.
    pub fn builtin(self) -> &'static [&'static str] {
        match self {
            Tag::Dirt => &data::DIRT,
            Tag::Sand => &data::SAND,
            Tag::Leaves => &data::LEAVES,
            Tag::DryVegetationMayPlaceOn => &data::DRY_VEGETATION_MAY_PLACE_ON,
            Tag::BambooPlantableOn => &data::BAMBOO_PLANTABLE_ON,
            Tag::BigDripleafPlaceable => &data::BIG_DRIPLEAF_PLACEABLE,
            Tag::SmallDripleafPlaceable => &data::SMALL_DRIPLEAF_PLACEABLE,
            Tag::AzaleaGrowsOn => &data::AZALEA_GROWS_ON,
            Tag::SnowLayerCanSurviveOn => &data::SNOW_LAYER_CAN_SURVIVE_ON,
            Tag::SnowLayerCannotSurviveOn => &data::SNOW_LAYER_CANNOT_SURVIVE_ON,
            Tag::MushroomGrowBlock => &data::MUSHROOM_GROW_BLOCK,
            Tag::JungleLogs => &data::JUNGLE_LOGS,
            Tag::Ice => &data::ICE,
        }
    }
}

// ---------------------------------------------------------------------------
// The catalog: the replaceable set and the tag sets in force
// ---------------------------------------------------------------------------

/// Which replaceable lists a running server uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatalogSource {
    /// Read from the configured vanilla data pack.
    Pack,
    /// The compiled-in table, for a server with no pack.
    Builtin,
}

impl CatalogSource {
    /// The phrase the startup log and the markdown use.
    pub fn label(self) -> &'static str {
        match self {
            CatalogSource::Pack => "the data pack",
            CatalogSource::Builtin => "the compiled-in fallback",
        }
    }
}

/// The replaceable set and every tag a rule reads, resolved once.
pub struct Catalog {
    replaceable: HashSet<String>,
    tags: HashMap<Tag, HashSet<String>>,
    source: CatalogSource,
}

impl Catalog {
    /// The compiled-in table, from the generated [`super::data`].
    pub fn builtin() -> Self {
        let mut tags = HashMap::new();
        for t in Tag::ALL {
            tags.insert(t, t.builtin().iter().map(|s| s.to_string()).collect());
        }
        Self {
            replaceable: data::REPLACEABLE.iter().map(|s| s.to_string()).collect(),
            tags,
            source: CatalogSource::Builtin,
        }
    }

    /// Read `#minecraft:replaceable` and the tag sets from the pack rooted at
    /// `root` (the same `worldgen_data` the generator uses). A tag the pack
    /// does not ship falls back to its compiled-in members, so a custom pack
    /// is never *less* permissive than the built-in table.
    pub fn from_pack(root: &str) -> Result<Self, String> {
        use aether_worldgen::vanilla::density::DataPack;
        use aether_worldgen::vanilla::tags::Tags;

        let pack = DataPack::open(root).map_err(|e| e.to_string())?;
        let tags = Tags::new(&pack);
        let replaceable: HashSet<String> = tag_names(&tags, "minecraft:replaceable")
            .into_iter()
            .collect();
        if replaceable.is_empty() {
            return Err("the pack has no #minecraft:replaceable block tag".into());
        }
        let mut map = HashMap::new();
        for t in Tag::ALL {
            let from_pack = tag_names(&tags, t.id());
            let set = if from_pack.is_empty() {
                t.builtin().iter().map(|s| s.to_string()).collect()
            } else {
                from_pack.into_iter().collect()
            };
            map.insert(t, set);
        }
        Ok(Self {
            replaceable,
            tags: map,
            source: CatalogSource::Pack,
        })
    }

    /// Where this catalog's data came from.
    pub fn source(&self) -> CatalogSource {
        self.source
    }

    /// Whether a block (by full name, e.g. `minecraft:short_grass`) may be
    /// replaced by a placement.
    pub fn is_replaceable_name(&self, name: &str) -> bool {
        self.replaceable.contains(name)
    }

    /// Whether a block (by full name) is a member of `tag`.
    pub fn tag_contains(&self, tag: Tag, name: &str) -> bool {
        self.tags.get(&tag).is_some_and(|s| s.contains(name))
    }

    /// The members of `tag`, sorted, for the markdown.
    pub fn tag_members(&self, tag: Tag) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .tags
            .get(&tag)
            .map(|s| s.iter().map(String::as_str).collect())
            .unwrap_or_default();
        v.sort_unstable();
        v
    }

    /// The replaceable list, sorted, for the markdown.
    pub fn replaceable_sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.replaceable.iter().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

/// The packed block ids of `tag` as full block names. Unknown ids (a pack
/// naming a block this engine does not have) are skipped.
fn tag_names(tags: &aether_worldgen::vanilla::tags::Tags, id: &str) -> Vec<String> {
    tags.block_tag_list(id)
        .into_iter()
        .filter_map(|b| blocks::BLOCKS.get(b as usize).map(|r| r.0.to_string()))
        .collect()
}

static CATALOG: OnceLock<Catalog> = OnceLock::new();

/// Install the catalog for the process. `root` is `[server].worldgen_data`;
/// an empty root, or a pack that cannot be read, uses the compiled-in table.
/// Returns the source used and the reason a pack was rejected, if any.
pub fn install(root: &str) -> (CatalogSource, Option<String>) {
    let (catalog, err) = if root.trim().is_empty() {
        (Catalog::builtin(), None)
    } else {
        match Catalog::from_pack(root) {
            Ok(c) => (c, None),
            Err(e) => (Catalog::builtin(), Some(e)),
        }
    };
    let source = catalog.source();
    let _ = CATALOG.set(catalog);
    (source, err)
}

/// The catalog in force, initialised to the compiled-in table if
/// [`install`] was never called.
pub fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(Catalog::builtin)
}

// ---------------------------------------------------------------------------
// Support rules
// ---------------------------------------------------------------------------

/// What can satisfy a support requirement.
#[derive(Clone, Copy, Debug)]
pub enum Supports {
    /// A full, collidable cube — vanilla's `isFaceSturdy(SupportType.FULL)`.
    FullCube,
    /// Any block that is not air (moss and pale moss carpet sit on anything).
    NotAir,
    /// A pack tag's blocks.
    Tag(Tag),
    /// `#minecraft:dirt` or farmland, plus any extra names.
    DirtFarmland(&'static [&'static str]),
    /// Water, or a block of the ice tag (lily pad).
    WaterOrIce,
    /// A literal set of full block names.
    Named(&'static [&'static str]),
}

impl Supports {
    /// A stable key, for the reverse index and the markdown.
    pub fn key(self) -> String {
        match self {
            Supports::FullCube => "a full block".into(),
            Supports::NotAir => "any non-air block".into(),
            Supports::Tag(t) => format!("#{}", t.id()),
            Supports::DirtFarmland(_) => "#minecraft:dirt or farmland".into(),
            Supports::WaterOrIce => "water or ice".into(),
            Supports::Named(list) => list.join(", "),
        }
    }

    /// Whether this support set's membership is carried by the data pack.
    fn is_data(self) -> bool {
        matches!(
            self,
            Supports::Tag(_) | Supports::DirtFarmland(_) | Supports::WaterOrIce
        )
    }

    /// A human list, for the markdown.
    fn display(self, cat: &Catalog) -> String {
        match self {
            Supports::FullCube => "any full block".into(),
            Supports::NotAir => "any non-air block".into(),
            Supports::Tag(t) => cat.tag_members(t).join(", "),
            Supports::DirtFarmland(extra) => {
                let mut v = cat.tag_members(Tag::Dirt);
                v.push("minecraft:farmland");
                v.extend_from_slice(extra);
                v.join(", ")
            }
            Supports::WaterOrIce => {
                let mut v = vec!["minecraft:water"];
                v.extend(cat.tag_members(Tag::Ice));
                v.join(", ")
            }
            Supports::Named(list) => list.join(", "),
        }
    }
}

/// How a block must be supported when it is placed.
#[derive(Clone, Copy, Debug)]
pub enum Need {
    /// The block below must satisfy `Supports`.
    On(Supports),
    /// The block above must satisfy `Supports`.
    Under(Supports),
    /// The block below is `Supports`, or a block of the same kind (kelp,
    /// big dripleaf, cactus_flower's cactus).
    OnOrNames(Supports, &'static [&'static str]),
    /// The block above is `Supports`, or a block of the same kind (cave
    /// vines, pale hanging moss).
    UnderOrNames(Supports, &'static [&'static str]),
    /// The block behind the block's `facing` must satisfy `Supports`
    /// (wall torches, ladders, wall signs).
    Facing(Supports),
    /// The block *in front of* the block's `facing` must satisfy `Supports`
    /// (cocoa faces the log it grows from).
    FacingAhead(Supports),
    /// On `Supports`, and the cell itself must not hold water (`leaf_litter`).
    OnNotWater(Supports),
    /// A standing torch: a support centre below, or — because this engine
    /// cannot turn a torch item into a wall torch — the wall the placement
    /// clicked, so a torch placed on a wall is not silently lost.
    Torch,
    /// Below a dirt- or sand-tag block, with water (or frosted ice) touching
    /// one of the four sides of the block below (sugar cane).
    SugarCane,
    /// Below sand (or another cactus), no solid or lava neighbour, and no
    /// fluid above.
    Cactus,
    /// Below `Supports`, and the cell itself must be a water source (sea
    /// grass, the lower half of tall sea grass).
    Submerged(Supports),
    /// A tag below, or a water source in the cell with a full block below
    /// (small dripleaf).
    Dripleaf(Tag),
    /// Water or ice below, and the cell itself is not a fluid (lily pad).
    LilyPad,
    /// At least one of the six neighbours offers a full face (glow lichen,
    /// resin clump, vine).
    MultiFace,
    /// Snow layer.
    Snow,
}

/// One rule: a set of blocks and how they must be supported.
pub struct Rule {
    /// A stable key, for diagnostics.
    pub key: &'static str,
    /// The group this rule belongs to, an index into [`GROUPS`].
    pub group: usize,
    /// How the governed blocks are matched.
    pub matcher: Matcher,
    /// The support requirement.
    pub need: Need,
}

/// How a rule's blocks are named.
#[derive(Clone, Copy, Debug)]
pub enum Matcher {
    /// Exact short names (`short_grass`).
    Names(&'static [&'static str]),
    /// Any block whose short name ends with one of these (`wall_torch`,
    /// `_wall_sign`).
    Suffix(&'static [&'static str]),
}

impl Matcher {
    fn matches(&self, short: &str) -> bool {
        match self {
            Matcher::Names(list) => list.contains(&short),
            Matcher::Suffix(list) => list.iter().any(|s| short.ends_with(s)),
        }
    }
}

impl Rule {
    fn matches(&self, short: &str) -> bool {
        self.matcher.matches(short)
    }

    /// Whether the support set comes from the pack (data) or the rule shape
    /// from vanilla's code (curated).
    fn provenance(&self) -> Provenance {
        let data = match self.need {
            Need::On(s)
            | Need::Under(s)
            | Need::Facing(s)
            | Need::FacingAhead(s)
            | Need::OnNotWater(s) => s.is_data(),
            Need::OnOrNames(s, _) | Need::UnderOrNames(s, _) => s.is_data(),
            Need::Submerged(s) => s.is_data(),
            Need::Dripleaf(t) => !t.builtin().is_empty(),
            Need::SugarCane => true,
            Need::Snow => true,
            // The support sets here are curated (full cubes / water / any
            // non-air); the "needs a face" part is code.
            Need::Torch | Need::Cactus | Need::LilyPad | Need::MultiFace => false,
        };
        if data {
            Provenance::Data
        } else {
            Provenance::Curated
        }
    }

    /// The `(role, support)` the rule reads, if it has a concrete one.
    fn support_of(&self) -> Option<(Role, Supports)> {
        match self.need {
            Need::On(s) | Need::OnNotWater(s) => Some((Role::Below, s)),
            Need::Under(s) => Some((Role::Above, s)),
            Need::OnOrNames(s, _) => Some((Role::Below, s)),
            Need::UnderOrNames(s, _) => Some((Role::Above, s)),
            Need::Facing(s) => Some((Role::Behind, s)),
            Need::FacingAhead(s) => Some((Role::Ahead, s)),
            Need::Submerged(s) => Some((Role::Below, s)),
            Need::Dripleaf(t) => Some((Role::Below, Supports::Tag(t))),
            _ => None,
        }
    }
}

/// Where a support block sits relative to the placed block.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Below,
    Above,
    Behind,
    Ahead,
    AnyFace,
}

impl Role {
    fn en(self) -> &'static str {
        match self {
            Role::Below => "below",
            Role::Above => "above",
            Role::Behind => "behind",
            Role::Ahead => "in front",
            Role::AnyFace => "any adjacent",
        }
    }

    fn ru(self) -> &'static str {
        match self {
            Role::Below => "снизу",
            Role::Above => "сверху",
            Role::Behind => "сзади",
            Role::Ahead => "спереди",
            Role::AnyFace => "любая грань",
        }
    }
}

/// Whether a rule's support is carried by the pack or is code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provenance {
    Data,
    Curated,
}

impl Provenance {
    fn label(self) -> &'static str {
        match self {
            Provenance::Data => "data",
            Provenance::Curated => "curated",
        }
    }
}

// ---------------------------------------------------------------------------
// The rule table
// ---------------------------------------------------------------------------

/// A markdown group: a heading and the note under it.
pub struct Group {
    pub title_en: &'static str,
    pub title_ru: &'static str,
    pub note_en: &'static str,
    pub note_ru: &'static str,
}

const G_VEGETATION: usize = 0;
const G_SPECIAL_SOIL: usize = 1;
const G_DRY: usize = 2;
const G_TALL: usize = 3;
const G_CACTUS: usize = 4;
const G_SUGAR_CANE: usize = 5;
const G_BAMBOO: usize = 6;
const G_WATER: usize = 7;
const G_DRIPLEAF: usize = 8;
const G_CEILING: usize = 9;
const G_CARPET: usize = 10;
const G_COCOA: usize = 11;
const G_SNOW: usize = 12;
const G_STANDING: usize = 13;
const G_WALL: usize = 14;
const G_RAILS: usize = 15;
const G_MULTIFACE: usize = 16;

/// The groups, in the order the markdown prints them.
pub static GROUPS: [Group; 17] = [
    Group {
        title_en: "Vegetation on soil",
        title_ru: "Растительность на почве",
        note_en: "Vanilla `VegetationBlock.canSurvive`: the block below must be in \
                  `#minecraft:dirt` or be farmland. A flower, sapling or grass on \
                  sand, stone or air is refused.",
        note_ru: "Ванильный `VegetationBlock.canSurvive`: блок снизу должен входить в \
                  `#minecraft:dirt` или быть грядкой. Цветок, саженец или трава на \
                  песке, камне или в воздухе отклоняются.",
    },
    Group {
        title_en: "Species with their own soil",
        title_ru: "Виды со своей почвой",
        note_en: "Wither rose also grows on netherrack and soul soil; azalea also on \
                  clay. From the same classes' overridden `mayPlaceOn`.",
        note_ru: "Роза иссушения также растёт на незераке и душевной земле; азалия — \
                  на глине. Из переопределённого `mayPlaceOn` тех же классов.",
    },
    Group {
        title_en: "Dry vegetation",
        title_ru: "Сухая растительность",
        note_en: "`#minecraft:dry_vegetation_may_place_on`: sand, terracotta, dirt \
                  or farmland.",
        note_ru: "`#minecraft:dry_vegetation_may_place_on`: песок, терракота, земля \
                  или грядка.",
    },
    Group {
        title_en: "Two-block plants (lower half)",
        title_ru: "Двухблочные растения (нижняя половина)",
        note_en: "The lower half follows the soil rule; the upper half is placed with \
                  it and needs the lower half below (`placement::upper_half_of`).",
        note_ru: "Нижняя половина подчиняется правилу почвы; верхняя ставится вместе \
                  с ней и требует нижнюю половину снизу (`placement::upper_half_of`).",
    },
    Group {
        title_en: "Cactus",
        title_ru: "Кактус",
        note_en: "`CactusBlock.canSurvive`: sand (or another cactus) below, no solid \
                  or lava neighbour, and nothing fluid above.",
        note_ru: "`CactusBlock.canSurvive`: под ним песок (или кактус), рядом нет \
                  твёрдого блока или лавы, сверху нет жидкости.",
    },
    Group {
        title_en: "Sugar cane",
        title_ru: "Сахарный тростник",
        note_en: "`SugarCaneBlock.canSurvive`: dirt or sand below (or another cane), \
                  with water or frosted ice touching a side of that block.",
        note_ru: "`SugarCaneBlock.canSurvive`: снизу земля или песок (или тростник), \
                  и вода либо замороженный лёд касается бока этого блока.",
    },
    Group {
        title_en: "Bamboo",
        title_ru: "Бамбук",
        note_en: "`#minecraft:bamboo_plantable_on`: sand, dirt, gravel, bamboo.",
        note_ru: "`#minecraft:bamboo_plantable_on`: песок, земля, гравий, бамбук.",
    },
    Group {
        title_en: "Water plants and corals",
        title_ru: "Водные растения и кораллы",
        note_en: "Sea grass, kelp and corals stand on a full block; tall sea grass \
                  and small dripleaf's submerged form also require a water source in \
                  the cell; lily pads float on water or ice.",
        note_ru: "Морская трава, ламинария и кораллы стоят на полном блоке; высокую \
                  морскую траву и подводную форму малого диплефа требует также источник \
                  воды в клетке; кувшинка плавает на воде или льду.",
    },
    Group {
        title_en: "Small dripleaf",
        title_ru: "Малый диплеф",
        note_en: "`SmallDripleafBlock.canSurvive`: `#minecraft:small_dripleaf_placeable` \
                  (clay or moss) below, or a water source in the cell over a full block.",
        note_ru: "`SmallDripleafBlock.canSurvive`: снизу \
                  `#minecraft:small_dripleaf_placeable` (глина или мох), либо источник \
                  воды в клетке над полным блоком.",
    },
    Group {
        title_en: "Ceiling plants",
        title_ru: "Потолочные растения",
        note_en: "Hanging roots, spore blossoms and cave vines hang from a full block \
                  (or from another block of the same kind) above them.",
        note_ru: "Висячие корни, спороцвет и пещерные лозы свисают с полного блока \
                  (или с такого же блока) сверху.",
    },
    Group {
        title_en: "Carpets, moss and leaf litter",
        title_ru: "Ковры, мох и листва",
        note_en: "Moss and pale moss carpet sit on any non-air block. `leaf_litter` \
                  is the reported bug: it must sit on a full block **and** must never \
                  be placed into water — two independent checks.",
        note_ru: "Моховый и бледно-моховый ковёр кладутся на любой не-воздух. \
                  `leaf_litter` — это сообщённый баг: он должен лежать на полном блоке \
                  **и** никогда не ставиться в воду — две независимые проверки.",
    },
    Group {
        title_en: "Cocoa",
        title_ru: "Какао",
        note_en: "`CocoaBlock.canSurvive`: the block in the direction the pod faces is \
                  a jungle log.",
        note_ru: "`CocoaBlock.canSurvive`: блок в направлении, куда смотрит стручок — \
                  бревно тропического дерева.",
    },
    Group {
        title_en: "Snow layer",
        title_ru: "Слой снега",
        note_en: "`SnowLayerBlock.canSurvive`: refused on ice/packed ice/barrier, always \
                  allowed on honey block/soul sand/mud, otherwise a full block below.",
        note_ru: "`SnowLayerBlock.canSurvive`: нельзя на льду/плотном льду/барьере, \
                  всегда можно на медовом блоке/душевном песке/грязи, иначе — полный \
                  блок снизу.",
    },
    Group {
        title_en: "Blocks standing on a support",
        title_ru: "Блоки, стоящие на опоре",
        note_en: "A standing torch uses a support centre below (`BaseTorchBlock`); a \
                  lantern hangs or stands by its `hanging` property; a rail lies on a \
                  full block.",
        note_ru: "Стоящий факел опирается на центр опоры снизу (`BaseTorchBlock`); \
                  фонарь висит или стоит по свойству `hanging`; рельсы лежат на полном \
                  блоке.",
    },
    Group {
        title_en: "Wall-mounted blocks",
        title_ru: "Настенные блоки",
        note_en: "`WallTorchBlock`/`LadderBlock`: the block behind the block's `facing` \
                  must offer a full face.",
        note_ru: "`WallTorchBlock`/`LadderBlock`: блок позади `facing` должен давать \
                  полную грань.",
    },
    Group {
        title_en: "Rails",
        title_ru: "Рельсы",
        note_en: "Every rail needs a full block below.",
        note_ru: "Любым рельсам нужен полный блок снизу.",
    },
    Group {
        title_en: "Blocks on any face",
        title_ru: "Блоки на любой грани",
        note_en: "`MultifaceBlock`/`VineBlock`: glow lichen, resin clump and vines \
                  attach to a full face of any neighbour.",
        note_ru: "`MultifaceBlock`/`VineBlock`: светящийся лишайник, смоляной комок и \
                  лозы крепятся к полной грани любого соседа.",
    },
];

/// Every placement rule. Blocks with no rule are plain blocks and may be
/// placed anywhere (subject to replaceability and not burying a player).
pub static RULES: &[Rule] = &[
    Rule {
        key: "vegetation",
        group: G_VEGETATION,
        matcher: Matcher::Names(&[
            "short_grass",
            "fern",
            "bush",
            "firefly_bush",
            "sweet_berry_bush",
            "pink_petals",
            "wildflowers",
            "dandelion",
            "poppy",
            "blue_orchid",
            "allium",
            "azure_bluet",
            "red_tulip",
            "orange_tulip",
            "white_tulip",
            "pink_tulip",
            "oxeye_daisy",
            "cornflower",
            "lily_of_the_valley",
            "torchflower",
            "open_eyeblossom",
            "closed_eyeblossom",
            "oak_sapling",
            "spruce_sapling",
            "birch_sapling",
            "jungle_sapling",
            "acacia_sapling",
            "cherry_sapling",
            "dark_oak_sapling",
            "pale_oak_sapling",
        ]),
        need: Need::On(Supports::DirtFarmland(&[])),
    },
    Rule {
        key: "wither_rose",
        group: G_SPECIAL_SOIL,
        matcher: Matcher::Names(&["wither_rose"]),
        need: Need::On(Supports::DirtFarmland(&[
            "minecraft:netherrack",
            "minecraft:soul_sand",
            "minecraft:soul_soil",
        ])),
    },
    Rule {
        key: "azalea",
        group: G_SPECIAL_SOIL,
        matcher: Matcher::Names(&["azalea", "flowering_azalea"]),
        need: Need::On(Supports::DirtFarmland(&["minecraft:clay"])),
    },
    Rule {
        key: "dry_vegetation",
        group: G_DRY,
        matcher: Matcher::Names(&["dead_bush", "short_dry_grass", "tall_dry_grass"]),
        need: Need::On(Supports::Tag(Tag::DryVegetationMayPlaceOn)),
    },
    Rule {
        key: "tall_plants",
        group: G_TALL,
        matcher: Matcher::Names(&[
            "tall_grass",
            "large_fern",
            "sunflower",
            "lilac",
            "rose_bush",
            "peony",
            "pitcher_plant",
        ]),
        need: Need::On(Supports::DirtFarmland(&[])),
    },
    Rule {
        key: "cactus",
        group: G_CACTUS,
        matcher: Matcher::Names(&["cactus"]),
        need: Need::Cactus,
    },
    Rule {
        key: "sugar_cane",
        group: G_SUGAR_CANE,
        matcher: Matcher::Names(&["sugar_cane"]),
        need: Need::SugarCane,
    },
    Rule {
        key: "bamboo",
        group: G_BAMBOO,
        matcher: Matcher::Names(&["bamboo", "bamboo_sapling"]),
        need: Need::On(Supports::Tag(Tag::BambooPlantableOn)),
    },
    Rule {
        key: "seagrass",
        group: G_WATER,
        matcher: Matcher::Names(&["seagrass"]),
        need: Need::On(Supports::FullCube),
    },
    Rule {
        key: "tall_seagrass",
        group: G_WATER,
        matcher: Matcher::Names(&["tall_seagrass"]),
        need: Need::Submerged(Supports::FullCube),
    },
    Rule {
        key: "kelp",
        group: G_WATER,
        matcher: Matcher::Names(&["kelp", "kelp_plant"]),
        need: Need::OnOrNames(
            Supports::FullCube,
            &["minecraft:kelp", "minecraft:kelp_plant"],
        ),
    },
    Rule {
        key: "lily_pad",
        group: G_WATER,
        matcher: Matcher::Names(&["lily_pad"]),
        need: Need::LilyPad,
    },
    Rule {
        key: "sea_pickle",
        group: G_WATER,
        matcher: Matcher::Names(&["sea_pickle"]),
        need: Need::On(Supports::NotAir),
    },
    Rule {
        key: "corals",
        group: G_WATER,
        matcher: Matcher::Names(&[
            "tube_coral",
            "brain_coral",
            "bubble_coral",
            "fire_coral",
            "horn_coral",
            "tube_coral_fan",
            "brain_coral_fan",
            "bubble_coral_fan",
            "fire_coral_fan",
            "horn_coral_fan",
            "dead_tube_coral",
            "dead_brain_coral",
            "dead_bubble_coral",
            "dead_fire_coral",
            "dead_horn_coral",
        ]),
        need: Need::On(Supports::FullCube),
    },
    Rule {
        key: "big_dripleaf",
        group: G_WATER,
        matcher: Matcher::Names(&["big_dripleaf", "big_dripleaf_stem"]),
        need: Need::OnOrNames(
            Supports::Tag(Tag::BigDripleafPlaceable),
            &["minecraft:big_dripleaf", "minecraft:big_dripleaf_stem"],
        ),
    },
    Rule {
        key: "small_dripleaf",
        group: G_DRIPLEAF,
        matcher: Matcher::Names(&["small_dripleaf"]),
        need: Need::Dripleaf(Tag::SmallDripleafPlaceable),
    },
    Rule {
        key: "hanging_roots",
        group: G_CEILING,
        matcher: Matcher::Names(&["hanging_roots", "spore_blossom"]),
        need: Need::Under(Supports::FullCube),
    },
    Rule {
        key: "cave_vines",
        group: G_CEILING,
        matcher: Matcher::Names(&["cave_vines", "cave_vines_plant"]),
        need: Need::UnderOrNames(
            Supports::FullCube,
            &["minecraft:cave_vines", "minecraft:cave_vines_plant"],
        ),
    },
    Rule {
        key: "pale_hanging_moss",
        group: G_CEILING,
        matcher: Matcher::Names(&["pale_hanging_moss"]),
        need: Need::UnderOrNames(Supports::FullCube, &["minecraft:pale_hanging_moss"]),
    },
    Rule {
        key: "moss_carpet",
        group: G_CARPET,
        matcher: Matcher::Names(&["moss_carpet", "pale_moss_carpet"]),
        need: Need::On(Supports::NotAir),
    },
    Rule {
        key: "leaf_litter",
        group: G_CARPET,
        matcher: Matcher::Names(&["leaf_litter"]),
        need: Need::OnNotWater(Supports::FullCube),
    },
    Rule {
        key: "cocoa",
        group: G_COCOA,
        matcher: Matcher::Names(&["cocoa"]),
        need: Need::FacingAhead(Supports::Tag(Tag::JungleLogs)),
    },
    Rule {
        key: "snow",
        group: G_SNOW,
        matcher: Matcher::Names(&["snow"]),
        need: Need::Snow,
    },
    Rule {
        key: "standing_torch",
        group: G_STANDING,
        matcher: Matcher::Names(&["torch", "soul_torch", "redstone_torch", "copper_torch"]),
        need: Need::Torch,
    },
    Rule {
        key: "lantern",
        group: G_STANDING,
        matcher: Matcher::Names(&["lantern", "soul_lantern"]),
        // This engine places the standing form (its `hanging` property is not
        // chosen at placement yet), so a full block below is required.
        need: Need::On(Supports::FullCube),
    },
    Rule {
        key: "cactus_flower",
        group: G_STANDING,
        matcher: Matcher::Names(&["cactus_flower"]),
        need: Need::OnOrNames(
            Supports::FullCube,
            &["minecraft:cactus", "minecraft:farmland"],
        ),
    },
    Rule {
        key: "wall_torches",
        group: G_WALL,
        matcher: Matcher::Suffix(&["wall_torch"]),
        need: Need::Facing(Supports::FullCube),
    },
    Rule {
        key: "ladder",
        group: G_WALL,
        matcher: Matcher::Names(&["ladder"]),
        need: Need::Facing(Supports::FullCube),
    },
    Rule {
        key: "wall_signs",
        group: G_WALL,
        matcher: Matcher::Suffix(&["_wall_sign", "_wall_banner"]),
        need: Need::Facing(Supports::FullCube),
    },
    Rule {
        key: "rails",
        group: G_RAILS,
        matcher: Matcher::Names(&["rail", "powered_rail", "detector_rail", "activator_rail"]),
        need: Need::On(Supports::FullCube),
    },
    Rule {
        key: "multiface",
        group: G_MULTIFACE,
        matcher: Matcher::Names(&["vine", "glow_lichen", "resin_clump"]),
        need: Need::MultiFace,
    },
];

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// The full name (`minecraft:short_grass`) of a block state.
fn full_name(state: BlockStateId) -> &'static str {
    blocks::block_of_state(state).map(|(_, n)| n).unwrap_or("")
}

/// One property of a state, by name.
fn prop_of(state: BlockStateId, key: &str) -> Option<&'static str> {
    let (id, _) = blocks::block_of_state(state)?;
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    props::values_of(id, state.raw() - lo)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Whether the state holds water: the water block, a bubble column, or a
/// waterlogged block. This is the "not under water" half of the leaf-litter
/// rule.
fn holds_water(state: BlockStateId) -> bool {
    match full_name(state) {
        "minecraft:water" | "minecraft:bubble_column" => true,
        _ => prop_of(state, "waterlogged") == Some("true"),
    }
}

fn is_lava(state: BlockStateId) -> bool {
    full_name(state) == "minecraft:lava"
}

fn is_fluid(state: BlockStateId) -> bool {
    holds_water(state) || is_lava(state)
}

/// Whether the state is "legacy solid", the predicate cactus uses for its
/// neighbours.
fn is_solid(state: BlockStateId) -> bool {
    blocks::props_of_state(state).is_some_and(|p| p.solid)
}

/// A block family whose shape is known to be partial. Not a substitute for a
/// real per-state support shape (see the module doc) — a coarse filter that
/// errs towards refusing.
fn is_partial(name: &str) -> bool {
    const SUFFIXES: [&str; 13] = [
        "_stairs",
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
    if SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    matches!(
        name,
        "minecraft:snow"
            | "minecraft:iron_bars"
            | "minecraft:chain"
            | "minecraft:cactus"
            | "minecraft:bamboo"
            | "minecraft:farmland"
            | "minecraft:dirt_path"
            | "minecraft:pointed_dripstone"
    ) || name.contains("dripleaf")
}

/// Vanilla's `isFaceSturdy(FULL)`, approximated by the collision shape.
pub fn sturdy(state: BlockStateId) -> bool {
    let name = full_name(state);
    if name.is_empty() || name == "minecraft:air" {
        return false;
    }
    if name.ends_with("_slab") {
        return prop_of(state, "type") == Some("double");
    }
    if is_partial(name) {
        return false;
    }
    blocks::props_of_state(state).is_some_and(|p| p.collision)
}

/// The unit offset of a clicked face, matching the wire values (0 down, 1 up,
/// 2 north, 3 south, 4 west, 5 east).
fn face_offset(face: u8) -> (i32, i32, i32) {
    match face {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        _ => (1, 0, 0),
    }
}

/// The unit offset of a horizontal direction name.
fn dir_offset(dir: &str) -> (i32, i32, i32) {
    match dir {
        "north" => (0, 0, -1),
        "south" => (0, 0, 1),
        "west" => (-1, 0, 0),
        _ => (1, 0, 0),
    }
}

/// Whether `state` (the supporting block) satisfies `support`.
fn satisfies(cat: &Catalog, support: Supports, state: BlockStateId) -> bool {
    let name = full_name(state);
    match support {
        Supports::FullCube => sturdy(state),
        Supports::NotAir => name != "minecraft:air",
        Supports::Tag(t) => cat.tag_contains(t, name),
        Supports::DirtFarmland(extra) => {
            cat.tag_contains(Tag::Dirt, name)
                || name == "minecraft:farmland"
                || extra.contains(&name)
        }
        Supports::WaterOrIce => holds_water(state) || cat.tag_contains(Tag::Ice, name),
        Supports::Named(list) => list.contains(&name),
    }
}

/// Everything the check needs that is not the world itself.
#[derive(Clone, Copy)]
struct Scene {
    /// The clicked face: 0 down, 1 up, 2 north, 3 south, 4 west, 5 east.
    face: u8,
}

fn evaluate(
    cat: &Catalog,
    need: Need,
    state: BlockStateId,
    (x, y, z): (i32, i32, i32),
    get: &dyn Fn(i32, i32, i32) -> BlockStateId,
    scene: Scene,
) -> bool {
    let below = get(x, y - 1, z);
    let above = get(x, y + 1, z);
    match need {
        Need::On(s) => satisfies(cat, s, below),
        Need::Under(s) => satisfies(cat, s, above),
        Need::OnOrNames(s, names) => satisfies(cat, s, below) || names.contains(&full_name(below)),
        Need::UnderOrNames(s, names) => {
            satisfies(cat, s, above) || names.contains(&full_name(above))
        }
        Need::Facing(s) => match prop_of(state, "facing") {
            Some(dir) => {
                // The block is attached to the cell *behind* the direction it
                // faces: a wall torch facing east is held by the block to its
                // west (`WallTorchBlock.canSurvive`, `LadderBlock.canSurvive`).
                let (dx, dy, dz) = dir_offset(dir);
                satisfies(cat, s, get(x - dx, y - dy, z - dz))
            }
            None => true,
        },
        Need::FacingAhead(s) => match prop_of(state, "facing") {
            Some(dir) => {
                // Cocoa faces the log it grows from (`CocoaBlock.canSurvive`).
                let (dx, dy, dz) = dir_offset(dir);
                satisfies(cat, s, get(x + dx, y + dy, z + dz))
            }
            None => true,
        },
        Need::OnNotWater(s) => satisfies(cat, s, below) && !holds_water(get(x, y, z)),
        Need::Torch => {
            if sturdy(below) {
                return true;
            }
            // Until a torch item can become a wall torch, accept the wall the
            // placement clicked so the block is not lost (see the module doc).
            if scene.face >= 2 {
                let (dx, dy, dz) = face_offset(scene.face);
                return sturdy(get(x - dx, y - dy, z - dz));
            }
            false
        }
        Need::SugarCane => {
            if full_name(below) == "minecraft:sugar_cane" {
                return true;
            }
            if !(cat.tag_contains(Tag::Dirt, full_name(below))
                || cat.tag_contains(Tag::Sand, full_name(below)))
            {
                return false;
            }
            const SIDES: [(i32, i32, i32); 4] = [(0, 0, -1), (0, 0, 1), (-1, 0, 0), (1, 0, 0)];
            SIDES.iter().any(|(dx, dy, dz)| {
                let n = get(x + dx, y - 1 + dy, z + dz);
                holds_water(n) || full_name(n) == "minecraft:frosted_ice"
            })
        }
        Need::Cactus => {
            if full_name(below) != "minecraft:cactus"
                && !cat.tag_contains(Tag::Sand, full_name(below))
            {
                return false;
            }
            const SIDES: [(i32, i32, i32); 4] = [(0, 0, -1), (0, 0, 1), (-1, 0, 0), (1, 0, 0)];
            if SIDES.iter().any(|(dx, _, dz)| {
                let n = get(x + dx, y, z + dz);
                is_solid(n) || is_lava(n)
            }) {
                return false;
            }
            !is_fluid(above)
        }
        Need::Submerged(s) => satisfies(cat, s, below) && holds_water(get(x, y, z)),
        Need::Dripleaf(t) => {
            if cat.tag_contains(t, full_name(below)) {
                return true;
            }
            holds_water(get(x, y, z)) && sturdy(below)
        }
        Need::LilyPad => {
            (holds_water(below) || cat.tag_contains(Tag::Ice, full_name(below)))
                && !is_fluid(get(x, y, z))
        }
        Need::MultiFace => {
            const SIDES: [(i32, i32, i32); 6] = [
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, -1),
                (0, 0, 1),
                (-1, 0, 0),
                (1, 0, 0),
            ];
            SIDES
                .iter()
                .any(|(dx, dy, dz)| sturdy(get(x + dx, y + dy, z + dz)))
        }
        Need::Snow => {
            if cat.tag_contains(Tag::SnowLayerCannotSurviveOn, full_name(below)) {
                false
            } else if cat.tag_contains(Tag::SnowLayerCanSurviveOn, full_name(below)) {
                true
            } else {
                sturdy(below)
                    || (full_name(below) == "minecraft:snow"
                        && prop_of(below, "layers") == Some("8"))
            }
        }
    }
}

/// Whether `name` (a full block name) may be placed at `pos` given the world
/// `get`, using the catalog in force. A block with no rule is allowed.
pub fn allowed(
    name: &str,
    state: BlockStateId,
    pos: (i32, i32, i32),
    get: &dyn Fn(i32, i32, i32) -> BlockStateId,
) -> bool {
    allowed_with(catalog(), name, state, pos, get, Scene { face: 1 })
}

/// As [`allowed`], but with the clicked face, for the placement path.
pub fn allowed_facing(
    name: &str,
    state: BlockStateId,
    pos: (i32, i32, i32),
    get: &dyn Fn(i32, i32, i32) -> BlockStateId,
    face: u8,
) -> bool {
    allowed_with(catalog(), name, state, pos, get, Scene { face })
}

/// [`allowed`] against an explicit catalog, for tests and the pack installer.
fn allowed_with(
    cat: &Catalog,
    name: &str,
    state: BlockStateId,
    pos: (i32, i32, i32),
    get: &dyn Fn(i32, i32, i32) -> BlockStateId,
    scene: Scene,
) -> bool {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    RULES
        .iter()
        .filter(|r| r.matches(short))
        .all(|r| evaluate(cat, r.need, state, pos, get, scene))
}

/// Whether a placement into the cell holding `state` replaces it: the
/// authoritative `#minecraft:replaceable` tag.
pub fn is_replaceable(state: BlockStateId) -> bool {
    catalog().is_replaceable_name(full_name(state))
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

/// The two languages the generated docs are kept in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
}

/// Render `docs/BLOCK_PLACEMENT.md` (or `.ru.md`) from the rule table. The
/// checked-in file is byte-compared against this by a test, so it cannot
/// drift.
pub fn render_markdown(lang: Lang, cat: &Catalog) -> String {
    let mut out = String::new();
    let (h1, intro, legend, repl, rules_h, rev_h, gen_note) = match lang {
        Lang::En => (
            "# Block placement rules",
            "What may be placed where, the replaceable list, and the reverse \
             \"what supports what\" index. Generated from `crates/aether-server/src/placement`; \
             **do not edit by hand**.",
            "Legend: source **data** = the block set comes from the vanilla data pack; \
             **curated** = the rule shape is ported from vanilla's block classes.",
            "## Replaceable blocks",
            "## Rules by group",
            "## Reverse index — what supports what",
            "Regenerate with `cargo run -p aether-server --example gen_placement_md`.",
        ),
        Lang::Ru => (
            "# Правила установки блоков",
            "Что куда можно ставить, список заменяемых блоков и обратный индекс \
             «что поддерживает что». Сгенерировано из `crates/aether-server/src/placement`; \
             **не редактировать вручную**.",
            "Легенда: источник **data** — набор блоков взят из ванильного пакета данных; \
             **curated** — форма правила перенесена из классов блоков ванилы.",
            "## Заменяемые блоки",
            "## Правила по группам",
            "## Обратный индекс — что поддерживает что",
            "Перегенерировать: `cargo run -p aether-server --example gen_placement_md`.",
        ),
    };
    out.push_str(h1);
    out.push_str("\n\n");
    out.push_str(intro);
    out.push_str("\n\n");
    out.push_str(legend);
    out.push_str("\n\n");
    out.push_str(&format!(
        "{gen_note}\n\nReplaceable list source: **{}** ({} blocks).\n\n",
        cat.source().label(),
        cat.replaceable_sorted().len()
    ));

    out.push_str(repl);
    out.push_str("\n\n");
    for name in cat.replaceable_sorted() {
        out.push_str(&format!("- `{name}`\n"));
    }

    out.push('\n');
    out.push_str(rules_h);
    out.push_str("\n\n");
    for (i, group) in GROUPS.iter().enumerate() {
        out.push_str(&format!("### {}\n\n", title(group, lang)));
        out.push_str(note(group, lang));
        out.push_str("\n\n");
        out.push_str(match lang {
            Lang::En => {
                "| Block(s) | Constraint | Valid support | Source |\n\
                         |---|---|---|---|\n"
            }
            Lang::Ru => {
                "| Блок(и) | Ограничение | Допустимая опора | Источник |\n\
                         |---|---|---|---|\n"
            }
        });
        for rule in RULES.iter().filter(|r| r.group == i) {
            let blocks = matcher_display(&rule.matcher);
            let constraint = constraint_text(rule.need, lang);
            let support = rule
                .support_of()
                .map(|(_, s)| s.display(cat))
                .unwrap_or_else(|| special_support(rule.need, lang).to_string());
            out.push_str(&format!(
                "| {blocks} | {constraint} | {support} | {} |\n",
                rule.provenance().label()
            ));
        }
        out.push('\n');
    }

    out.push_str(rev_h);
    out.push_str("\n\n");
    let mut reverse: BTreeMap<(Role, String), BTreeSet<String>> = BTreeMap::new();
    for rule in RULES {
        let (role, support) = match rule.support_of() {
            Some((role, s)) => (role, s.key()),
            None => {
                let role = match rule.need {
                    Need::MultiFace => Role::AnyFace,
                    _ => Role::Below,
                };
                (role, special_support(rule.need, lang).to_string())
            }
        };
        let names = matcher_names(&rule.matcher);
        reverse.entry((role, support)).or_default().extend(names);
    }
    for ((role, support), blocks) in reverse {
        let role_label = match lang {
            Lang::En => role.en(),
            Lang::Ru => role.ru(),
        };
        out.push_str(&format!(
            "- **{} {}** — {}\n",
            role_label,
            support,
            blocks.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    out.push('\n');
    out
}

fn title(group: &Group, lang: Lang) -> &'static str {
    match lang {
        Lang::En => group.title_en,
        Lang::Ru => group.title_ru,
    }
}

fn note(group: &Group, lang: Lang) -> &'static str {
    match lang {
        Lang::En => group.note_en,
        Lang::Ru => group.note_ru,
    }
}

fn matcher_display(m: &Matcher) -> String {
    match m {
        Matcher::Names(list) => list
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", "),
        Matcher::Suffix(list) => list
            .iter()
            .map(|s| format!("any `*{s}`"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn matcher_names(m: &Matcher) -> Vec<String> {
    match m {
        Matcher::Names(list) => list.iter().map(|n| (*n).to_string()).collect(),
        Matcher::Suffix(list) => list.iter().map(|s| format!("*{s}")).collect(),
    }
}

fn constraint_text(need: Need, lang: Lang) -> &'static str {
    match (lang, need) {
        (Lang::En, Need::On(_)) => "on the block below",
        (Lang::En, Need::Under(_)) => "under the block above",
        (Lang::En, Need::OnOrNames(..)) => "on the block below, or the same block",
        (Lang::En, Need::UnderOrNames(..)) => "under the block above, or the same block",
        (Lang::En, Need::Facing(_)) => "against the block behind its facing",
        (Lang::En, Need::FacingAhead(_)) => "against the block it faces",
        (Lang::En, Need::OnNotWater(_)) => "on the block below, and not in water",
        (Lang::En, Need::Torch) => "on a support below, or the wall clicked",
        (Lang::En, Need::SugarCane) => "on dirt/sand with water beside",
        (Lang::En, Need::Cactus) => "on sand, clear sides, no fluid above",
        (Lang::En, Need::Submerged(_)) => "on the block below, in a water source",
        (Lang::En, Need::Dripleaf(_)) => "on the tag below, or in a water source",
        (Lang::En, Need::LilyPad) => "on water or ice, not in a fluid",
        (Lang::En, Need::MultiFace) => "a full face on any neighbour",
        (Lang::En, Need::Snow) => "as the snow-layer rule allows",
        (Lang::Ru, Need::On(_)) => "на блок снизу",
        (Lang::Ru, Need::Under(_)) => "под блоком сверху",
        (Lang::Ru, Need::OnOrNames(..)) => "на блок снизу или такой же блок",
        (Lang::Ru, Need::UnderOrNames(..)) => "под блоком сверху или таким же блоком",
        (Lang::Ru, Need::Facing(_)) => "к блоку позади facing",
        (Lang::Ru, Need::FacingAhead(_)) => "к блоку, на который смотрит",
        (Lang::Ru, Need::OnNotWater(_)) => "на блок снизу и не в воде",
        (Lang::Ru, Need::Torch) => "на опору снизу или на стену, куда кликнули",
        (Lang::Ru, Need::SugarCane) => "на землю/песок с водой рядом",
        (Lang::Ru, Need::Cactus) => "на песок, свободные бока, без жидкости сверху",
        (Lang::Ru, Need::Submerged(_)) => "на блок снизу, в источнике воды",
        (Lang::Ru, Need::Dripleaf(_)) => "на тег снизу или в источнике воды",
        (Lang::Ru, Need::LilyPad) => "на воду или лёд, не в жидкости",
        (Lang::Ru, Need::MultiFace) => "полная грань любого соседа",
        (Lang::Ru, Need::Snow) => "как допускает правило слоя снега",
    }
}

/// The support description for a need whose support has no single set.
fn special_support(need: Need, lang: Lang) -> &'static str {
    match (lang, need) {
        (Lang::En, Need::Torch) => "a full block or the clicked wall",
        (Lang::En, Need::SugarCane) => "#minecraft:dirt or #minecraft:sand, with water beside",
        (Lang::En, Need::Cactus) => "#minecraft:sand, or another cactus",
        (Lang::En, Need::LilyPad) => "water or #minecraft:ice",
        (Lang::En, Need::MultiFace) => "full face",
        (Lang::En, Need::Snow) => "a full block, honey, soul sand or mud",
        (Lang::Ru, Need::Torch) => "полный блок или стена, куда кликнули",
        (Lang::Ru, Need::SugarCane) => "#minecraft:dirt или #minecraft:sand, вода рядом",
        (Lang::Ru, Need::Cactus) => "#minecraft:sand или другой кактус",
        (Lang::Ru, Need::LilyPad) => "вода или #minecraft:ice",
        (Lang::Ru, Need::MultiFace) => "полная грань",
        (Lang::Ru, Need::Snow) => "полный блок, мёд, душевный песок или грязь",
        _ => "—",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A tiny world: anything not set is air.
    struct W(HashMap<(i32, i32, i32), BlockStateId>);
    impl W {
        fn new(entries: &[((i32, i32, i32), &str)]) -> Self {
            let mut m = HashMap::new();
            for (p, name) in entries {
                m.insert(*p, blocks::default_state(name).unwrap());
            }
            W(m)
        }
        fn get(&self) -> impl Fn(i32, i32, i32) -> BlockStateId + '_ {
            move |x, y, z| {
                self.0
                    .get(&(x, y, z))
                    .copied()
                    .unwrap_or(blocks::default_state("minecraft:air").unwrap())
            }
        }
    }

    fn state(name: &str) -> BlockStateId {
        blocks::default_state(name).unwrap()
    }

    fn allowed_in(world: &W, name: &str, pos: (i32, i32, i32)) -> bool {
        allowed(name, state(name), pos, &world.get())
    }

    // --- The replaceable list ------------------------------------------------

    #[test]
    fn the_required_replaceables_are_present() {
        // Every name the queue called out as missing from the old `match`.
        for want in [
            "leaf_litter",
            "soul_fire",
            "glow_lichen",
            "resin_clump",
            "light",
            "structure_void",
            "bubble_column",
            "warped_roots",
            "nether_sprouts",
            "crimson_roots",
            "hanging_roots",
        ] {
            assert!(
                catalog().is_replaceable_name(&format!("minecraft:{want}")),
                "{want} must be replaceable"
            );
        }
        assert_eq!(
            data::REPLACEABLE.len(),
            29,
            "the 1.21.11 #minecraft:replaceable tag has 29 entries"
        );
    }

    #[test]
    fn every_generated_replaceable_name_is_replaceable() {
        // Driven off the generated list, so a new entry is covered
        // automatically.
        for name in data::REPLACEABLE {
            assert!(
                is_replaceable(blocks::default_state(name).unwrap()),
                "{name} must be replaceable"
            );
        }
    }

    #[test]
    fn a_full_block_is_not_replaceable() {
        for name in [
            "minecraft:stone",
            "minecraft:oak_planks",
            "minecraft:oak_log",
            "minecraft:dirt",
        ] {
            assert!(
                !is_replaceable(blocks::default_state(name).unwrap()),
                "{name} must not be replaceable (build against it, not into it)"
            );
        }
    }

    // --- Plants and soil (the reported grass-on-sand bug) -------------------

    #[test]
    fn a_plant_places_on_valid_soil_and_is_refused_on_invalid_soil() {
        let on_dirt = W::new(&[((0, 0, 0), "minecraft:dirt")]);
        assert!(allowed_in(&on_dirt, "minecraft:poppy", (0, 1, 0)));
        assert!(allowed_in(&on_dirt, "minecraft:oak_sapling", (0, 1, 0)));
        assert!(allowed_in(&on_dirt, "minecraft:short_grass", (0, 1, 0)));

        // Stone, sand and air are all refused.
        let on_stone = W::new(&[((0, 0, 0), "minecraft:stone")]);
        assert!(!allowed_in(&on_stone, "minecraft:poppy", (0, 1, 0)));
        let on_sand = W::new(&[((0, 0, 0), "minecraft:sand")]);
        assert!(!allowed_in(&on_sand, "minecraft:poppy", (0, 1, 0)));
        let floating = W::new(&[]);
        assert!(!allowed_in(&floating, "minecraft:poppy", (0, 1, 0)));
        // Farmland, by contrast, is valid.
        let on_farmland = W::new(&[((0, 0, 0), "minecraft:farmland")]);
        assert!(allowed_in(&on_farmland, "minecraft:short_grass", (0, 1, 0)));
    }

    #[test]
    fn grass_on_sand_is_refused() {
        // The owner's bug, verbatim: grass on sand must not place.
        let on_sand = W::new(&[((0, 0, 0), "minecraft:sand")]);
        assert!(!allowed_in(&on_sand, "minecraft:short_grass", (0, 1, 0)));
        let on_dirt = W::new(&[((0, 0, 0), "minecraft:dirt")]);
        assert!(allowed_in(&on_dirt, "minecraft:short_grass", (0, 1, 0)));
    }

    // --- leaf_litter: two independent triggers --------------------------------

    #[test]
    fn leaf_litter_on_a_wall_is_refused_and_on_the_floor_places() {
        // Trigger 1: the block below must be a full block.
        let floating = W::new(&[]);
        assert!(
            !allowed_in(&floating, "minecraft:leaf_litter", (0, 1, 0)),
            "leaf litter with nothing below is on a wall / in the air"
        );
        let on_floor = W::new(&[((0, 0, 0), "minecraft:stone")]);
        assert!(allowed_in(&on_floor, "minecraft:leaf_litter", (0, 1, 0)));
    }

    #[test]
    fn leaf_litter_on_the_floor_under_water_is_refused() {
        // Trigger 2, independent of trigger 1: the cell itself must not hold
        // water, even with a full block below it.
        let water = W::new(&[
            ((0, 0, 0), "minecraft:stone"),
            ((0, 1, 0), "minecraft:water"),
        ]);
        assert!(
            !allowed_in(&water, "minecraft:leaf_litter", (0, 1, 0)),
            "leaf litter must not be placed under water"
        );
    }

    // --- Torches and ladders --------------------------------------------------

    #[test]
    fn a_torch_needs_a_support_below() {
        let on_solid = W::new(&[((0, 0, 0), "minecraft:stone")]);
        assert!(allowed_in(&on_solid, "minecraft:torch", (0, 1, 0)));
        let floating = W::new(&[]);
        assert!(!allowed_in(&floating, "minecraft:torch", (0, 1, 0)));
    }

    #[test]
    fn a_ladder_needs_a_wall_behind_its_facing() {
        // Facing north, so the attached block is to the south (z + 1).
        let block = blocks::block_id_of("minecraft:ladder").unwrap();
        let down = BlockStateId(props::state_with(block, &[("facing", "north")]).unwrap());

        let with_wall = W::new(&[((0, 1, 1), "minecraft:stone")]);
        assert!(allowed_with(
            catalog(),
            "minecraft:ladder",
            down,
            (0, 1, 0),
            &with_wall.get(),
            Scene { face: 1 }
        ));
        let without = W::new(&[]);
        assert!(!allowed_with(
            catalog(),
            "minecraft:ladder",
            down,
            (0, 1, 0),
            &without.get(),
            Scene { face: 1 }
        ));
    }

    #[test]
    fn a_wall_torch_needs_the_wall_behind_it() {
        let block = blocks::block_id_of("minecraft:wall_torch").unwrap();
        let north = BlockStateId(props::state_with(block, &[("facing", "north")]).unwrap());
        let with_wall = W::new(&[((0, 1, 1), "minecraft:stone")]);
        assert!(allowed_with(
            catalog(),
            "minecraft:wall_torch",
            north,
            (0, 1, 0),
            &with_wall.get(),
            Scene { face: 1 }
        ));
        assert!(!allowed_in(&W::new(&[]), "minecraft:wall_torch", (0, 1, 0)));
    }

    // --- The markdown cannot drift -------------------------------------------

    #[test]
    fn the_markdown_matches_the_checked_in_docs() {
        let cat = catalog();
        assert_eq!(
            render_markdown(Lang::En, cat),
            include_str!("../../../../docs/BLOCK_PLACEMENT.md"),
            "docs/BLOCK_PLACEMENT.md drifted; regenerate with \
             `cargo run -p aether-server --example gen_placement_md`"
        );
        assert_eq!(
            render_markdown(Lang::Ru, cat),
            include_str!("../../../../docs/BLOCK_PLACEMENT.ru.md"),
            "docs/BLOCK_PLACEMENT.ru.md drifted; regenerate with \
             `cargo run -p aether-server --example gen_placement_md`"
        );
    }

    #[test]
    fn the_markdown_names_every_group_and_rule() {
        let md = render_markdown(Lang::En, catalog());
        for group in &GROUPS {
            assert!(
                md.contains(group.title_en),
                "missing group `{}`",
                group.title_en
            );
        }
        assert!(md.contains("`leaf_litter`"));
        assert!(md.contains("do not edit by hand"));
    }
}
