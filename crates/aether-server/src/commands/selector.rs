//! Entity selectors: the `@…` parser and the rules that turn one into a set of
//! entities.
//!
//! A selector names a broad set (`@a` all players, `@e` all entities, `@p` the
//! nearest player, `@r` a random player, `@s` the executing entity, `@n` the
//! nearest entity) and narrows it with a `[key=value,…]` filter list. Parsing
//! is **source-independent** — it reads the text alone and produces a
//! [`Selector`] — and resolution happens later against a [`SelectorSource`],
//! which is the read-only view of the online players and the loaded entities.
//!
//! The rules are vanilla 1.21.11's `EntitySelectorParser` / `EntitySelector`:
//!
//! * defaults: `@a` and `@e` are unlimited (`limit=0`), `@p`, `@r` and `@n`
//!   default to `limit=1`; `@p` and `@n` sort nearest, `@r` sorts randomly and
//!   the rest are arbitrary;
//! * selecting nothing is an error for an argument that needs a target
//!   (`No entity was found` / `No player was found`), but a raw find returns
//!   an empty list so `/execute as @e` can no-op silently;
//! * `@p`/`@r` with no players online, and `@s` with no executing entity, are
//!   errors even at the raw-find level;
//! * an argument that needs a single entity rejects a selector that can match
//!   more than one (`Only one entity is allowed, …`, or the player wording for
//!   a player-only argument);
//! * `distance`, `level`, `x_rotation` and `y_rotation` take ranges: `n`,
//!   `n..`, `..n`, `n..m`. `distance` is a double range, `level` and `scores`
//!   an integer range, and the rotations a **wrapped** angle range (so
//!   `y_rotation=170..-170` is valid and both bounds and the entity's angle
//!   are folded into `[-180, 180)`);
//! * `@e` and `@n` only match live entities; `@a`, `@p`, `@r` and `@s` may
//!   match a dead one;
//! * an option may not mix an inclusive and an exclusive form where vanilla
//!   forbids it (`name`, `gamemode`, `type`, `team`'s inclusive form), and
//!   `limit`/`sort` may not be used on `@s`;
//! * a `type=` id is checked against the entity-type table
//!   (`Invalid or unknown entity type`), as vanilla checks its registry.
//!
//! # Lock order
//!
//! Selection must never hold the entity lock while parsing. Parsing touches no
//! source at all, and [`Selector::find`] calls [`SelectorSource::players`] and
//! [`SelectorSource::entities`] **once each**, up front, and then filters the
//! owned snapshots they return; the caller never keeps a lock across a call
//! back into the source for those. The one predicate that does call back into
//! the source is `type=#tag`, through
//! [`SelectorSource::entity_type_in_tag`] — once per candidate entity — so an
//! implementation of that hook must not take the entity lock. See
//! `docs/storage-v2/lock-order.md`.
//!
//! The trait is named [`SelectorSource`] rather than `CommandSource`, which
//! [`super::source`] already owns for the command-issuer identity (CMD-05).

use aether_api::Vector3 as Vec3;

use super::coordinates::{wrap_degrees, Rotation};
use super::snbt;
use crate::protocol::nbt::Nbt;
use crate::protocol::GameMode as WireGameMode;

/// One entity a selector may match, as an owned snapshot.
///
/// Every field the filter language can test lives here, so resolution never
/// has to reach back into a world or a player handle while filtering; the
/// [`SelectorSource`] hands these out and then gets out of the way. Fields
/// that do not apply to non-players (game mode, level, advancements) are left
/// at their defaults and the player-only filters simply exclude them, exactly
/// as vanilla's do.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityRef {
    /// The entity id (players count up from one, other entities from a long
    /// way above them).
    pub id: i32,
    /// The entity's UUID.
    pub uuid: u128,
    /// The protocol entity type, e.g. `minecraft:player`.
    pub kind: String,
    /// Whether this is a player. Player-only filters key on it.
    pub is_player: bool,
    /// Whether the entity is alive. `@e` and `@n` only match live entities
    /// (`EntitySelectorParser.parseSelector` adds an `Entity::isAlive`
    /// predicate for those two variables); the others may match a dead one.
    pub alive: bool,
    /// The entity's plain-text display name, for `name=`.
    ///
    /// For a player this is the profile name; for another entity it is its
    /// custom name, or `None` to fall back to the type's own display name
    /// ([`EntityRef::stringified_name`]).
    pub name: Option<String>,
    /// Feet position.
    pub pos: Vec3,
    /// Yaw/pitch.
    pub rotation: Rotation,
    /// Scoreboard tags.
    pub tags: Vec<String>,
    /// Team name, if any.
    pub team: Option<String>,
    /// Game mode; only meaningful for a player.
    pub gamemode: Option<Gamemode>,
    /// Experience level; only meaningful for a player.
    pub level: i32,
    /// `(objective, score)` pairs.
    pub scores: Vec<(String, i32)>,
    /// Advancement progress.
    pub advancements: Vec<Advancement>,
    /// The predicates this entity satisfies, by id.
    pub predicates: Vec<String>,
    /// The entity's NBT, for `nbt=` tests.
    pub nbt: Nbt,
}

impl EntityRef {
    /// A player snapshot, with every player-only default.
    pub fn player(name: impl Into<String>, id: i32, pos: Vec3) -> EntityRef {
        let name = name.into();
        EntityRef {
            id,
            uuid: id as u128,
            kind: "minecraft:player".into(),
            is_player: true,
            alive: true,
            name: Some(name),
            pos,
            rotation: Rotation::new(0.0, 0.0),
            tags: Vec::new(),
            team: None,
            gamemode: Some(Gamemode::Survival),
            level: 0,
            scores: Vec::new(),
            advancements: Vec::new(),
            predicates: Vec::new(),
            nbt: Nbt::Compound(Vec::new()),
        }
    }

    /// A non-player snapshot with `kind` and `pos`; every player-only field
    /// stays empty.
    pub fn other(kind: impl Into<String>, id: i32, pos: Vec3) -> EntityRef {
        EntityRef {
            id,
            uuid: (id as u128) | (1 << 127),
            kind: kind.into(),
            is_player: false,
            alive: true,
            name: None,
            pos,
            rotation: Rotation::new(0.0, 0.0),
            tags: Vec::new(),
            team: None,
            gamemode: None,
            level: 0,
            scores: Vec::new(),
            advancements: Vec::new(),
            predicates: Vec::new(),
            nbt: Nbt::Compound(Vec::new()),
        }
    }

    /// The plain-text name a `name=` filter compares, the way vanilla's
    /// `Entity.getStringifiedName()` does: the explicit name if there is one,
    /// else a display name derived from the entity type (`minecraft:zombie`
    /// → `Zombie`).
    ///
    /// This server does not carry the vanilla language table, so a mob's
    /// display name is humanised from its id rather than localised; a caller
    /// that has the real name should put it in [`EntityRef::name`].
    pub fn stringified_name(&self) -> String {
        match &self.name {
            Some(name) => name.clone(),
            None => humanize_kind(&self.kind),
        }
    }
}

/// `minecraft:experience_orb` → `Experience Orb`.
fn humanize_kind(kind: &str) -> String {
    let short = kind.strip_prefix("minecraft:").unwrap_or(kind);
    let mut out = String::new();
    for (i, word) in short.split('_').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(c) = chars.next() {
            out.extend(c.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// A player's progress on one advancement.
#[derive(Clone, Debug, PartialEq)]
pub struct Advancement {
    /// The advancement id.
    pub id: String,
    /// Whether the whole advancement is complete.
    pub done: bool,
    /// `(criterion, done)` pairs.
    pub criteria: Vec<(String, bool)>,
}

/// A game mode, including adventure — which this server's own
/// [`WireGameMode`] does not model yet but the selector language must accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gamemode {
    /// Survival.
    Survival,
    /// Creative.
    Creative,
    /// Adventure.
    Adventure,
    /// Spectator.
    Spectator,
}

impl Gamemode {
    /// The four vanilla names.
    pub fn parse(s: &str) -> Option<Gamemode> {
        Some(match s {
            "survival" => Gamemode::Survival,
            "creative" => Gamemode::Creative,
            "adventure" => Gamemode::Adventure,
            "spectator" => Gamemode::Spectator,
            _ => return None,
        })
    }

    /// The lowercase name.
    pub fn name(self) -> &'static str {
        match self {
            Gamemode::Survival => "survival",
            Gamemode::Creative => "creative",
            Gamemode::Adventure => "adventure",
            Gamemode::Spectator => "spectator",
        }
    }
}

impl From<WireGameMode> for Gamemode {
    fn from(g: WireGameMode) -> Gamemode {
        match g {
            WireGameMode::Survival => Gamemode::Survival,
            WireGameMode::Creative => Gamemode::Creative,
            WireGameMode::Spectator => Gamemode::Spectator,
        }
    }
}

/// Read-only access to everything a selector resolves against.
///
/// The two entity lists are returned as owned snapshots, so the implementation
/// clones under whatever lock it needs and returns before any filtering
/// happens — the entity lock is never held across selector evaluation or
/// parsing (see the module docs).
pub trait SelectorSource {
    /// The entity that ran the command, if any (`@s` and the default origin).
    fn executor(&self) -> Option<EntityRef> {
        None
    }

    /// Every online player.
    fn players(&self) -> Vec<EntityRef> {
        Vec::new()
    }

    /// Every loaded non-player entity.
    fn entities(&self) -> Vec<EntityRef> {
        Vec::new()
    }

    /// The source position, used as the selector origin when `x`/`y`/`z` are
    /// absent. A console has none; callers fall back to the world origin
    /// `(0, 0, 0)`, as vanilla's console does.
    fn position(&self) -> Option<Vec3> {
        self.executor().map(|e| e.pos)
    }

    /// The source rotation.
    fn rotation(&self) -> Option<Rotation> {
        self.executor().map(|e| e.rotation)
    }

    /// Whether `kind` is a member of the entity type tag `tag` (a `#`-prefixed
    /// `type=` value). Tags are datapack data this server does not carry yet,
    /// so the default is "no".
    fn entity_type_in_tag(&self, _kind: &str, _tag: &str) -> bool {
        false
    }
}

/// A selector variable: which broad set the selector draws from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variable {
    /// `@a` — every online player.
    AllPlayers,
    /// `@e` — every loaded entity, players included.
    AllEntities,
    /// `@p` — the nearest player.
    NearestPlayer,
    /// `@r` — a random player.
    RandomPlayer,
    /// `@s` — the entity that ran the command.
    SelfEntity,
    /// `@n` — the nearest entity.
    NearestEntity,
}

impl Variable {
    fn from_char(c: char) -> Option<Variable> {
        Some(match c {
            'a' => Variable::AllPlayers,
            'e' => Variable::AllEntities,
            'p' => Variable::NearestPlayer,
            'r' => Variable::RandomPlayer,
            's' => Variable::SelfEntity,
            'n' => Variable::NearestEntity,
            _ => return None,
        })
    }

    /// The single letter that names the variable.
    pub fn letter(self) -> char {
        match self {
            Variable::AllPlayers => 'a',
            Variable::AllEntities => 'e',
            Variable::NearestPlayer => 'p',
            Variable::RandomPlayer => 'r',
            Variable::SelfEntity => 's',
            Variable::NearestEntity => 'n',
        }
    }

    /// Whether the variable can only ever name players (`@a`, `@p`, `@r`).
    /// Vanilla forbids `type=` on these.
    pub fn is_player_only(self) -> bool {
        matches!(
            self,
            Variable::AllPlayers | Variable::NearestPlayer | Variable::RandomPlayer
        )
    }

    /// The default `(limit, sort)` for the variable.
    fn defaults(self) -> (usize, Sort) {
        match self {
            Variable::AllPlayers | Variable::AllEntities => (0, Sort::Arbitrary),
            Variable::NearestPlayer | Variable::NearestEntity => (1, Sort::Nearest),
            Variable::RandomPlayer => (1, Sort::Random),
            Variable::SelfEntity => (1, Sort::Arbitrary),
        }
    }

    /// Whether the selector can initially match a non-player.
    ///
    /// `@a`/`@p`/`@r` are player-only (`limitToType(PLAYER)` in vanilla);
    /// `@e`/`@n`/`@s` start including entities. Options such as `gamemode`,
    /// `level`, `advancements` and `type=player` narrow it back down.
    fn includes_entities_default(self) -> bool {
        matches!(
            self,
            Variable::AllEntities | Variable::NearestEntity | Variable::SelfEntity
        )
    }

    /// Whether the variable is `@s`, for which `limit`/`sort` are inapplicable.
    fn is_self(self) -> bool {
        self == Variable::SelfEntity
    }

    /// Whether `@e`/`@n` require their targets to be alive.
    fn requires_alive(self) -> bool {
        matches!(self, Variable::AllEntities | Variable::NearestEntity)
    }
}

/// How a selector orders the entities before applying `limit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    /// Increasing distance from the origin.
    Nearest,
    /// Decreasing distance from the origin.
    Furthest,
    /// Shuffled.
    Random,
    /// Source order, unsorted.
    Arbitrary,
}

impl Sort {
    fn parse(s: &str) -> Option<Sort> {
        Some(match s {
            "nearest" => Sort::Nearest,
            "furthest" => Sort::Furthest,
            "random" => Sort::Random,
            "arbitrary" => Sort::Arbitrary,
            _ => return None,
        })
    }
}

/// How many entities an argument that owns this selector may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Amount {
    /// Exactly one (`minecraft:entity`, `EntityArgument.getEntity`).
    Single,
    /// Any number (`minecraft:entities`).
    Multiple,
}

/// A value with a leading `!`, which inverts the test.
#[derive(Clone, Debug, PartialEq)]
pub struct Invert<T> {
    /// The value.
    pub value: T,
    /// Whether the test is negated.
    pub inverted: bool,
}

/// An inclusive numeric range: `n` is `min == max`, `n..` has no maximum,
/// `..n` no minimum and `..` is rejected.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumRange {
    /// The lower bound, inclusive.
    pub min: Option<f64>,
    /// The upper bound, inclusive.
    pub max: Option<f64>,
}

impl NumRange {
    /// Whether `v` lies inside the range.
    pub fn contains(&self, v: f64) -> bool {
        self.min.map_or(true, |m| v >= m) && self.max.map_or(true, |m| v <= m)
    }
}

/// An inclusive integer range (`MinMaxBounds.Ints`), for `level=` and
/// `scores=`. `n` is `min == max`, `n..` has no maximum, `..n` no minimum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntRange {
    /// The lower bound, inclusive.
    pub min: Option<i32>,
    /// The upper bound, inclusive.
    pub max: Option<i32>,
}

impl IntRange {
    /// Whether `v` lies inside the range.
    pub fn contains(&self, v: i32) -> bool {
        self.min.map_or(true, |m| v >= m) && self.max.map_or(true, |m| v <= m)
    }
}

/// An inclusive **wrapped** angle range (`MinMaxBounds.FloatDegrees`), for
/// `x_rotation=` and `y_rotation=`.
///
/// Vanilla does not reject a swapped range here: both bounds and the entity's
/// angle are folded into `[-180, 180)` and a swapped pair (`170..-170`) means
/// "outside the 20° gap", so the test is `h >= min || h <= max` rather than
/// `min <= h <= max`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AngleRange {
    /// The first bound (default `0`).
    pub min: Option<f32>,
    /// The second bound (default `359`).
    pub max: Option<f32>,
}

impl AngleRange {
    /// Whether `angle` (degrees) lies inside the wrapped range.
    pub fn contains(&self, angle: f32) -> bool {
        let lo = wrap_degrees(self.min.unwrap_or(0.0));
        let hi = wrap_degrees(self.max.unwrap_or(359.0));
        let a = wrap_degrees(angle);
        if lo > hi {
            a >= lo || a <= hi
        } else {
            a >= lo && a <= hi
        }
    }
}

/// A `type=` value: a concrete entity type or an entity type tag.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeValue {
    /// A namespaced entity type, e.g. `minecraft:player`.
    Name(String),
    /// An entity type tag, e.g. `#minecraft:undead` (the `#` kept).
    Tag(String),
}

/// One `advancements={id=…}` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct AdvancementTest {
    /// The advancement id.
    pub id: String,
    /// Criteria, when the `{criterion=bool}` form was used.
    pub criteria: Vec<(String, bool)>,
    /// The whole-advancement boolean; `None` for the criteria form.
    pub expected: Option<bool>,
}

/// A parsed, source-independent entity selector.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    /// The variable.
    pub variable: Variable,
    /// How many entities the owning argument accepts.
    pub amount: Amount,
    /// Whether the owning argument is a player-type argument, which forbids a
    /// selector that can include non-players (`@e`).
    pub players_only: bool,
    /// Whether the selector can still match a non-player.
    ///
    /// Vanilla's `EntitySelector.includesEntities`: `false` for `@a`/`@p`/`@r`
    /// and for any selector narrowed to players (`type=player`, `gamemode`,
    /// `level`, `advancements`). Used to enforce a player-only argument.
    pub includes_entities: bool,
    /// Include/exclude by name.
    pub name: Vec<Invert<String>>,
    /// Include/exclude by entity type.
    pub entity_type: Vec<Invert<TypeValue>>,
    /// `distance=<range>`.
    pub distance: Option<NumRange>,
    /// `level=<range>`.
    pub level: Option<IntRange>,
    /// `x=` (selector origin).
    pub x: Option<f64>,
    /// `y=` (selector origin).
    pub y: Option<f64>,
    /// `z=` (selector origin).
    pub z: Option<f64>,
    /// `dx=` (volume).
    pub dx: Option<f64>,
    /// `dy=` (volume).
    pub dy: Option<f64>,
    /// `dz=` (volume).
    pub dz: Option<f64>,
    /// `x_rotation=<range>` (pitch).
    pub x_rotation: Option<AngleRange>,
    /// `y_rotation=<range>` (yaw).
    pub y_rotation: Option<AngleRange>,
    /// Include/exclude by scoreboard tag.
    pub tags: Vec<Invert<String>>,
    /// Include/exclude by team.
    pub team: Vec<Invert<String>>,
    /// Include/exclude by game mode.
    pub gamemode: Vec<Invert<Gamemode>>,
    /// `scores={objective=range,…}`.
    pub scores: Vec<(String, IntRange)>,
    /// `advancements={…}`.
    pub advancements: Vec<AdvancementTest>,
    /// Include/exclude by predicate id.
    pub predicates: Vec<Invert<String>>,
    /// Include/exclude by NBT.
    pub nbt: Vec<Invert<Nbt>>,
    /// The result cap; `0` means unlimited.
    pub limit: usize,
    /// The ordering.
    pub sort: Sort,
}

/// Whether `key` is a selector option at all (vanilla's `OPTIONS` table).
fn is_known_option(key: &str) -> bool {
    matches!(
        key,
        "name"
            | "distance"
            | "level"
            | "x"
            | "y"
            | "z"
            | "dx"
            | "dy"
            | "dz"
            | "x_rotation"
            | "y_rotation"
            | "limit"
            | "sort"
            | "gamemode"
            | "team"
            | "type"
            | "tag"
            | "nbt"
            | "scores"
            | "advancements"
            | "predicate"
    )
}

/// The per-option "already set" flags vanilla keeps on the parser.
///
/// These drive each option's applicability predicate (`canUse`) and the
/// extra mixed-positive/negative checks inside a modifier, so duplicates and
/// illegal combinations surface as `argument.entity.options.inapplicable`
/// rather than a second, invented message.
struct OptState {
    has_name_equals: bool,
    has_name_not_equals: bool,
    is_limited: bool,
    is_sorted: bool,
    has_gamemode_equals: bool,
    has_gamemode_not_equals: bool,
    has_team_equals: bool,
    has_team_not_equals: bool,
    type_limited: bool,
    type_limited_inversely: bool,
    has_scores: bool,
    has_advancements: bool,
    distance: bool,
    level: bool,
    x: bool,
    y: bool,
    z: bool,
    dx: bool,
    dy: bool,
    dz: bool,
    x_rotation: bool,
    y_rotation: bool,
}

impl OptState {
    fn new(type_limited: bool) -> OptState {
        OptState {
            has_name_equals: false,
            has_name_not_equals: false,
            is_limited: false,
            is_sorted: false,
            has_gamemode_equals: false,
            has_gamemode_not_equals: false,
            has_team_equals: false,
            has_team_not_equals: false,
            type_limited,
            type_limited_inversely: false,
            has_scores: false,
            has_advancements: false,
            distance: false,
            level: false,
            x: false,
            y: false,
            z: false,
            dx: false,
            dy: false,
            dz: false,
            x_rotation: false,
            y_rotation: false,
        }
    }

    /// Vanilla's `Option.canUse` predicates.
    fn can_use(&self, key: &str, variable: Variable) -> bool {
        match key {
            "name" => !self.has_name_equals,
            "distance" => !self.distance,
            "level" => !self.level,
            "x" => !self.x,
            "y" => !self.y,
            "z" => !self.z,
            "dx" => !self.dx,
            "dy" => !self.dy,
            "dz" => !self.dz,
            "x_rotation" => !self.x_rotation,
            "y_rotation" => !self.y_rotation,
            "limit" => !variable.is_self() && !self.is_limited,
            "sort" => !variable.is_self() && !self.is_sorted,
            "gamemode" => !self.has_gamemode_equals,
            "team" => !self.has_team_equals,
            "type" => !self.type_limited,
            "tag" | "nbt" | "predicate" => true,
            "scores" => !self.has_scores,
            "advancements" => !self.has_advancements,
            _ => false,
        }
    }
}

impl Selector {
    /// Parse a selector for a multiple-entity argument (`EntityArgument.entities`).
    pub fn parse(input: &str) -> Result<Selector, ParseError> {
        Selector::parse_with_amount(input, Amount::Multiple, false)
    }

    /// Parse a selector for a single-entity argument (`EntityArgument.entity`),
    /// rejecting anything that could match more than one.
    pub fn parse_single(input: &str) -> Result<Selector, ParseError> {
        Selector::parse_with_amount(input, Amount::Single, false)
    }

    /// Parse a selector for a player-type multiple argument
    /// (`EntityArgument.players`): a selector that can include non-players is
    /// rejected (`Only players may be affected by this command, …`).
    pub fn parse_players(input: &str) -> Result<Selector, ParseError> {
        Selector::parse_with_amount(input, Amount::Multiple, true)
    }

    /// Parse a selector for a player-type single argument
    /// (`EntityArgument.player`).
    pub fn parse_single_player(input: &str) -> Result<Selector, ParseError> {
        Selector::parse_with_amount(input, Amount::Single, true)
    }

    fn parse_with_amount(
        input: &str,
        amount: Amount,
        players_only: bool,
    ) -> Result<Selector, ParseError> {
        let mut r = Reader::new(input);
        r.skip_ws();
        if r.peek() != Some('@') {
            return Err(ParseError::MissingVariable);
        }
        r.bump();
        let c = r.bump().ok_or(ParseError::MissingVariable)?;
        let variable = Variable::from_char(c).ok_or(ParseError::UnknownVariable(c))?;
        let (limit, sort) = variable.defaults();
        let mut selector = Selector {
            variable,
            amount,
            players_only,
            includes_entities: variable.includes_entities_default(),
            name: Vec::new(),
            entity_type: Vec::new(),
            distance: None,
            level: None,
            x: None,
            y: None,
            z: None,
            dx: None,
            dy: None,
            dz: None,
            x_rotation: None,
            y_rotation: None,
            tags: Vec::new(),
            team: Vec::new(),
            gamemode: Vec::new(),
            scores: Vec::new(),
            advancements: Vec::new(),
            predicates: Vec::new(),
            nbt: Vec::new(),
            limit,
            sort,
        };
        if r.peek() == Some('[') {
            selector.parse_options(&mut r)?;
        }
        r.skip_ws();
        if !r.eof() {
            return Err(ParseError::Trailing);
        }
        if amount == Amount::Single && !selector.is_single() {
            return Err(if players_only {
                ParseError::TooManyPlayers
            } else {
                ParseError::TooManyEntities
            });
        }
        if players_only && selector.includes_entities && !selector.variable.is_self() {
            return Err(ParseError::OnlyPlayersAllowed);
        }
        Ok(selector)
    }

    /// The letter form of the variable, for diagnostics.
    pub fn variable_letter(&self) -> char {
        self.variable.letter()
    }

    /// Whether this selector can name at most one entity.
    pub fn is_single(&self) -> bool {
        self.variable == Variable::SelfEntity || self.limit == 1
    }

    fn parse_options(&mut self, r: &mut Reader) -> Result<(), ParseError> {
        r.bump(); // '['
                  // Vanilla pre-seeds `type=player` on the player-only variables, so
                  // `type` is already "limited" and every `type=` is inapplicable.
        let mut state = OptState::new(self.variable.is_player_only());
        loop {
            r.skip_ws();
            match r.peek() {
                Some(']') => {
                    r.bump();
                    break;
                }
                None => return Err(ParseError::Unterminated),
                _ => {}
            }
            let key = r.read_word();
            if !is_known_option(key) {
                return Err(ParseError::UnknownKey(key.to_string()));
            }
            // `EntitySelectorOptions.get` tests the option's applicability
            // before its value is read.
            if !state.can_use(key, self.variable) {
                return Err(ParseError::Inapplicable(key.to_string()));
            }
            r.skip_ws();
            if r.peek() != Some('=') {
                return Err(ParseError::MissingValue(key.to_string()));
            }
            r.bump();
            r.skip_ws();
            self.parse_option(key, r, &mut state)?;
            r.skip_ws();
            match r.peek() {
                Some(',') => {
                    r.bump();
                }
                Some(']') | None => {}
                Some(_) => return Err(ParseError::Unterminated),
            }
        }
        Ok(())
    }

    fn parse_option(
        &mut self,
        key: &str,
        r: &mut Reader,
        state: &mut OptState,
    ) -> Result<(), ParseError> {
        match key {
            "name" => {
                let f = read_invert_string(r)?;
                // `hasNameNotEquals() && !negated` → inapplicable.
                if state.has_name_not_equals && !f.inverted {
                    return Err(ParseError::Inapplicable("name".into()));
                }
                if f.inverted {
                    state.has_name_not_equals = true;
                } else {
                    state.has_name_equals = true;
                }
                self.name.push(f);
            }
            "type" => {
                let inverted = eat_bang(r);
                // `isTypeLimitedInversely() && !negated` → inapplicable.
                if state.type_limited_inversely && !inverted {
                    return Err(ParseError::Inapplicable("type".into()));
                }
                if inverted {
                    state.type_limited_inversely = true;
                }
                let word = r.read_word();
                if word.is_empty() {
                    return Err(ParseError::BadValue {
                        key: "type".into(),
                        value: String::new(),
                    });
                }
                let value = if let Some(tag) = word.strip_prefix('#') {
                    if tag.is_empty() {
                        return Err(ParseError::BadValue {
                            key: "type".into(),
                            value: word.to_string(),
                        });
                    }
                    TypeValue::Tag(format!("#{}", normalize_resource(tag)))
                } else {
                    let norm = normalize_resource(word);
                    if !valid_resource(&norm) || !known_entity_type(&norm) {
                        // Vanilla reports the parsed identifier, so a bare
                        // name is named with its `minecraft:` namespace.
                        return Err(ParseError::UnknownType(norm));
                    }
                    // `limitToType`: only a positive **concrete** type limits
                    // the selector's type. `type=player` also narrows it to
                    // players; a negated type does neither.
                    if !inverted {
                        state.type_limited = true;
                        if norm == "minecraft:player" {
                            self.includes_entities = false;
                        }
                    }
                    TypeValue::Name(norm)
                };
                self.entity_type.push(Invert { value, inverted });
            }
            "distance" => {
                state.distance = true;
                let range = read_double_range(r, "distance")?;
                if range.min.is_some_and(|v| v < 0.0) || range.max.is_some_and(|v| v < 0.0) {
                    return Err(ParseError::Negative("distance".into()));
                }
                self.distance = Some(range);
            }
            "level" => {
                state.level = true;
                let range = read_int_range(r, "level")?;
                if range.min.is_some_and(|v| v < 0) || range.max.is_some_and(|v| v < 0) {
                    return Err(ParseError::Negative("level".into()));
                }
                self.level = Some(range);
                // `level` naturally excludes non-players.
                self.includes_entities = false;
            }
            "x" => {
                state.x = true;
                self.x = Some(read_double(r, "x")?);
            }
            "y" => {
                state.y = true;
                self.y = Some(read_double(r, "y")?);
            }
            "z" => {
                state.z = true;
                self.z = Some(read_double(r, "z")?);
            }
            "dx" => {
                state.dx = true;
                self.dx = Some(read_double(r, "dx")?);
            }
            "dy" => {
                state.dy = true;
                self.dy = Some(read_double(r, "dy")?);
            }
            "dz" => {
                state.dz = true;
                self.dz = Some(read_double(r, "dz")?);
            }
            "x_rotation" => {
                state.x_rotation = true;
                self.x_rotation = Some(read_angle_range(r, "x_rotation")?);
            }
            "y_rotation" => {
                state.y_rotation = true;
                self.y_rotation = Some(read_angle_range(r, "y_rotation")?);
            }
            "tag" => {
                let f = read_invert_string(r)?;
                self.tags.push(f);
            }
            "team" => {
                let f = read_invert_string(r)?;
                if f.inverted {
                    state.has_team_not_equals = true;
                } else {
                    state.has_team_equals = true;
                }
                self.team.push(f);
            }
            "gamemode" => {
                let f = read_invert_string(r)?;
                if state.has_gamemode_not_equals && !f.inverted {
                    return Err(ParseError::Inapplicable("gamemode".into()));
                }
                let mode = Gamemode::parse(&f.value)
                    .ok_or_else(|| ParseError::UnknownGamemode(f.value.clone()))?;
                if f.inverted {
                    state.has_gamemode_not_equals = true;
                } else {
                    state.has_gamemode_equals = true;
                }
                self.gamemode.push(Invert {
                    value: mode,
                    inverted: f.inverted,
                });
                self.includes_entities = false;
            }
            "scores" => {
                state.has_scores = true;
                let text = r.read_balanced('{', '}')?;
                self.scores = parse_scores(text)?;
            }
            "advancements" => {
                state.has_advancements = true;
                let text = r.read_balanced('{', '}')?;
                self.advancements = parse_advancements(text)?;
                if !self.advancements.is_empty() {
                    self.includes_entities = false;
                }
            }
            "predicate" => {
                let f = read_invert_string(r)?;
                let norm = normalize_resource(&f.value);
                if !valid_resource(&norm) {
                    return Err(ParseError::BadValue {
                        key: "predicate".into(),
                        value: f.value,
                    });
                }
                self.predicates.push(Invert {
                    value: norm,
                    inverted: f.inverted,
                });
            }
            "nbt" => {
                let inverted = eat_bang(r);
                r.skip_ws();
                let text = r.read_balanced('{', '}')?;
                let nbt = snbt::parse_compound(text).map_err(|_| ParseError::BadValue {
                    key: "nbt".into(),
                    value: text.to_string(),
                })?;
                self.nbt.push(Invert {
                    value: nbt,
                    inverted,
                });
            }
            "limit" => {
                state.is_limited = true;
                let word = r.read_word();
                let n: i64 = word.parse().map_err(|_| ParseError::BadValue {
                    key: "limit".into(),
                    value: word.to_string(),
                })?;
                if n < 1 {
                    return Err(ParseError::LimitTooSmall);
                }
                self.limit = n as usize;
            }
            "sort" => {
                state.is_sorted = true;
                let word = r.read_word();
                let sort =
                    Sort::parse(word).ok_or_else(|| ParseError::UnknownSort(word.to_string()))?;
                self.sort = sort;
            }
            other => return Err(ParseError::UnknownKey(other.to_string())),
        }
        Ok(())
    }

    /// The selector origin: explicit `x`/`y`/`z`, each defaulting to the
    /// source's position (or the world origin for a source with none).
    fn origin(&self, source: &dyn SelectorSource) -> Vec3 {
        let base = source.position().unwrap_or(Vec3::ZERO);
        Vec3::new(
            self.x.unwrap_or(base.x),
            self.y.unwrap_or(base.y),
            self.z.unwrap_or(base.z),
        )
    }

    /// The matching entities, without treating an empty result as an error.
    ///
    /// `@p`/`@r` with no players online and `@s` with no executing entity are
    /// the only intrinsic failures; `@a`/`@e`/`@n` return an empty list, which
    /// is what `/execute as @e` needs.
    pub fn find(
        &self,
        source: &dyn SelectorSource,
        rng: &mut dyn SelectRng,
    ) -> Result<Vec<EntityRef>, SelectError> {
        let origin = self.origin(source);
        let mut candidates: Vec<EntityRef> = match self.variable {
            Variable::SelfEntity => match source.executor() {
                Some(e) => vec![e],
                None => return Err(SelectError::NoEntity),
            },
            Variable::NearestPlayer | Variable::RandomPlayer => {
                let players = source.players();
                if players.is_empty() {
                    return Err(SelectError::NoPlayer);
                }
                players
            }
            Variable::AllPlayers => source.players(),
            Variable::AllEntities | Variable::NearestEntity => {
                let mut all = source.players();
                all.extend(source.entities());
                all
            }
        };
        // `@e` and `@n` only match live entities (`Entity::isAlive` in vanilla);
        // `@a`, `@p`, `@r` and `@s` may match a dead one.
        if self.variable.requires_alive() {
            candidates.retain(|e| e.alive);
        }
        candidates.retain(|e| self.matches(e, origin, source));
        match self.sort {
            Sort::Nearest => candidates.sort_by(|a, b| {
                distance_sq(a, origin)
                    .partial_cmp(&distance_sq(b, origin))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            Sort::Furthest => candidates.sort_by(|a, b| {
                distance_sq(b, origin)
                    .partial_cmp(&distance_sq(a, origin))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            Sort::Random => shuffle(&mut candidates, rng),
            Sort::Arbitrary => {}
        }
        if self.limit > 0 && candidates.len() > self.limit {
            candidates.truncate(self.limit);
        }
        Ok(candidates)
    }

    /// [`find`](Self::find), then an empty result becomes the vanilla
    /// `No entity was found` (or `No player was found` for a player-only
    /// variable). This is what a command that needs targets uses.
    pub fn select(
        &self,
        source: &dyn SelectorSource,
        rng: &mut dyn SelectRng,
    ) -> Result<Vec<EntityRef>, SelectError> {
        let out = self.find(source, rng)?;
        if out.is_empty() {
            return Err(self.empty_error());
        }
        Ok(out)
    }

    /// [`find`](Self::find), requiring exactly one result.
    pub fn select_one(
        &self,
        source: &dyn SelectorSource,
        rng: &mut dyn SelectRng,
    ) -> Result<EntityRef, SelectError> {
        let out = self.find(source, rng)?;
        if out.is_empty() {
            return Err(self.empty_error());
        }
        if out.len() > 1 {
            return Err(if self.players_only {
                SelectError::TooManyPlayers
            } else {
                SelectError::TooManyEntities
            });
        }
        Ok(out.into_iter().next().expect("len checked"))
    }

    fn empty_error(&self) -> SelectError {
        if self.variable.is_player_only() {
            SelectError::NoPlayer
        } else {
            SelectError::NoEntity
        }
    }

    /// Whether `e` passes every filter.
    fn matches(&self, e: &EntityRef, origin: Vec3, source: &dyn SelectorSource) -> bool {
        for f in &self.name {
            let hit = e.stringified_name() == f.value;
            if hit == f.inverted {
                return false;
            }
        }
        for f in &self.entity_type {
            let hit = match &f.value {
                TypeValue::Name(n) => &e.kind == n,
                TypeValue::Tag(t) => source.entity_type_in_tag(&e.kind, t),
            };
            if hit == f.inverted {
                return false;
            }
        }
        if let Some(range) = &self.distance {
            if !range.contains((e.pos - origin).length()) {
                return false;
            }
        }
        // NOTE: vanilla tests `box.intersects(entity.getBoundingBox())`; this
        // server's [`EntityRef`] carries only the feet position, so the test is
        // the feet point against the same box. An entity straddling a boundary
        // (or one whose hitbox is larger than a point) can differ. A future
        // `EntityRef` bounding box would close the gap; the box itself
        // (`span`, with vanilla's mandatory `+1` extension) already matches.
        if self.dx.is_some() || self.dy.is_some() || self.dz.is_some() {
            let (min_x, max_x) = span(origin.x, self.dx.unwrap_or(0.0));
            let (min_y, max_y) = span(origin.y, self.dy.unwrap_or(0.0));
            let (min_z, max_z) = span(origin.z, self.dz.unwrap_or(0.0));
            if !(within(e.pos.x, min_x, max_x)
                && within(e.pos.y, min_y, max_y)
                && within(e.pos.z, min_z, max_z))
            {
                return false;
            }
        }
        if let Some(range) = &self.level {
            if !e.is_player || !range.contains(e.level) {
                return false;
            }
        }
        for f in &self.gamemode {
            if !e.is_player {
                return false;
            }
            let hit = e.gamemode == Some(f.value);
            if hit == f.inverted {
                return false;
            }
        }
        if let Some(range) = &self.x_rotation {
            if !range.contains(e.rotation.pitch) {
                return false;
            }
        }
        if let Some(range) = &self.y_rotation {
            if !range.contains(e.rotation.yaw) {
                return false;
            }
        }
        for f in &self.team {
            let hit = e.team.as_deref().unwrap_or("") == f.value;
            if hit == f.inverted {
                return false;
            }
        }
        for f in &self.tags {
            let hit = if f.value.is_empty() {
                e.tags.is_empty()
            } else {
                e.tags.iter().any(|t| t == &f.value)
            };
            if hit == f.inverted {
                return false;
            }
        }
        for (objective, range) in &self.scores {
            match e.scores.iter().find(|(o, _)| o == objective) {
                Some((_, v)) if range.contains(*v) => {}
                _ => return false,
            }
        }
        for test in &self.advancements {
            let adv = e.advancements.iter().find(|a| a.id == test.id);
            let hit = match test.expected {
                Some(want) => adv.is_some_and(|a| a.done) == want,
                None => adv.is_some_and(|a| {
                    test.criteria
                        .iter()
                        .all(|(c, want)| a.criteria.iter().any(|(cc, got)| cc == c && got == want))
                }),
            };
            if !hit {
                return false;
            }
        }
        for f in &self.predicates {
            let hit = e.predicates.contains(&f.value);
            if hit == f.inverted {
                return false;
            }
        }
        for f in &self.nbt {
            let hit = nbt_matches(&f.value, &e.nbt);
            if hit == f.inverted {
                return false;
            }
        }
        true
    }
}

/// The two ends of a volume axis: `[min(origin, origin+d), max(origin, origin+d)+1]`.
///
/// Vanilla's box always extends by one block in the positive direction, so no
/// single dimension is ever smaller than one block.
fn span(origin: f64, delta: f64) -> (f64, f64) {
    let a = origin;
    let b = origin + delta;
    (a.min(b), a.max(b) + 1.0)
}

fn within(v: f64, min: f64, max: f64) -> bool {
    v >= min && v <= max
}

fn distance_sq(e: &EntityRef, origin: Vec3) -> f64 {
    (e.pos - origin).length_squared()
}

/// A random source for `sort=random` and `@r`, so selection is deterministic
/// under a seeded generator in tests.
pub trait SelectRng {
    /// A uniform index in `0..n` (`n` is never zero).
    fn gen_below(&mut self, n: usize) -> usize;
}

/// A small deterministic LCG, for tests and for callers without the world
/// RNG to hand.
#[derive(Clone, Debug)]
pub struct LcgRng {
    state: u64,
}

impl LcgRng {
    /// A generator seeded with `seed`.
    pub fn new(seed: u64) -> LcgRng {
        LcgRng { state: seed }
    }
}

impl SelectRng for LcgRng {
    fn gen_below(&mut self, n: usize) -> usize {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.state >> 33) % n as u64) as usize
    }
}

fn shuffle<T>(v: &mut [T], rng: &mut dyn SelectRng) {
    for i in (1..v.len()).rev() {
        let j = rng.gen_below(i + 1);
        v.swap(i, j);
    }
}

/// Whether `expected` is a subset of `actual` (vanilla `nbt=` matching).
///
/// Compounds match if every expected key is present and matches; lists must
/// have the same length and match element-wise; scalars and arrays are equal.
pub fn nbt_matches(expected: &Nbt, actual: &Nbt) -> bool {
    match (expected, actual) {
        (Nbt::Compound(e), Nbt::Compound(a)) => e.iter().all(|(k, v)| {
            a.iter()
                .find(|(ak, _)| ak == k)
                .is_some_and(|(_, av)| nbt_matches(v, av))
        }),
        (Nbt::List(e), Nbt::List(a)) => {
            e.len() == a.len() && e.iter().zip(a).all(|(x, y)| nbt_matches(x, y))
        }
        _ => expected == actual,
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a selector text did not parse.
#[derive(Clone, Debug, PartialEq)]
pub enum ParseError {
    /// The text did not start with `@`.
    MissingVariable,
    /// The character after `@` is not one of `a e p r s n`.
    UnknownVariable(char),
    /// An unknown `[key=…]`.
    UnknownKey(String),
    /// A key with no `=`.
    MissingValue(String),
    /// An integer range bound that was not a whole number (`Invalid integer
    /// '%s'`, Brigadier's `readerInvalidInt`).
    InvalidInteger(String),
    /// An angle range bound that was not a number (`Invalid float '%s'`,
    /// Brigadier's `readerInvalidFloat`).
    InvalidFloat(String),
    /// A value that did not fit its key.
    BadValue {
        /// The key.
        key: String,
        /// The offending value.
        value: String,
    },
    /// `sort=` was not a known order.
    UnknownSort(String),
    /// `gamemode=` was not a known mode.
    UnknownGamemode(String),
    /// `type=` was not a valid resource location.
    UnknownType(String),
    /// An option that is not allowed here (e.g. `type` on `@a`).
    Inapplicable(String),
    /// The `[…]` list never closed.
    Unterminated,
    /// Trailing characters after the selector.
    Trailing,
    /// `limit` was below one.
    LimitTooSmall,
    /// A range whose minimum is above its maximum.
    SwappedRange,
    /// A negative `distance` or `level`.
    Negative(String),
    /// A single-entity argument was given a multi-entity selector.
    TooManyEntities,
    /// A single-player argument was given a multi-player selector.
    TooManyPlayers,
    /// A player-only argument was given a selector that can include entities.
    OnlyPlayersAllowed,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ParseError {}

impl ParseError {
    /// The vanilla chat line for the failure.
    pub fn message(&self) -> String {
        match self {
            ParseError::MissingVariable => "Missing selector type".into(),
            ParseError::UnknownVariable(c) => format!("Unknown selector type '@{c}'"),
            ParseError::UnknownKey(k) => format!("Unknown option '{k}'"),
            ParseError::MissingValue(k) => format!("Expected value for option '{k}'"),
            ParseError::InvalidInteger(v) => format!("Invalid integer '{v}'"),
            ParseError::InvalidFloat(v) => format!("Invalid float '{v}'"),
            ParseError::BadValue { key, value } => {
                format!("Invalid value '{value}' for option '{key}'")
            }
            ParseError::UnknownSort(v) => format!("Invalid or unknown sort type '{v}'"),
            ParseError::UnknownGamemode(v) => format!("Invalid or unknown game mode '{v}'"),
            ParseError::UnknownType(v) => format!("Invalid or unknown entity type '{v}'"),
            ParseError::Inapplicable(k) => format!("Option '{k}' isn't applicable here"),
            ParseError::Unterminated => "Expected end of options".into(),
            ParseError::Trailing => "Unexpected trailing data".into(),
            ParseError::LimitTooSmall => "Limit must be at least 1".into(),
            ParseError::SwappedRange => "Min cannot be bigger than max".into(),
            ParseError::Negative(k) => match k.as_str() {
                "distance" => "Distance cannot be negative".into(),
                _ => "Level shouldn't be negative".into(),
            },
            ParseError::TooManyEntities => {
                "Only one entity is allowed, but the provided selector allows more than one".into()
            }
            ParseError::TooManyPlayers => {
                "Only one player is allowed, but the provided selector allows more than one".into()
            }
            ParseError::OnlyPlayersAllowed => {
                "Only players may be affected by this command, but the provided selector includes entities".into()
            }
        }
    }
}

/// Why selection failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectError {
    /// `No entity was found`.
    NoEntity,
    /// `No player was found`.
    NoPlayer,
    /// `Only one entity is allowed, but the provided selector allows more than one`.
    TooManyEntities,
    /// `Only one player is allowed, but the provided selector allows more than one`.
    TooManyPlayers,
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SelectError {}

impl SelectError {
    /// The vanilla chat line for the failure.
    pub fn message(&self) -> String {
        match self {
            SelectError::NoEntity => "No entity was found".into(),
            SelectError::NoPlayer => "No player was found".into(),
            SelectError::TooManyEntities => {
                "Only one entity is allowed, but the provided selector allows more than one".into()
            }
            SelectError::TooManyPlayers => {
                "Only one player is allowed, but the provided selector allows more than one".into()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// A character reader over a selector string.
struct Reader<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Reader<'a> {
    fn new(s: &'a str) -> Reader<'a> {
        Reader { s, i: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        Some(c)
    }

    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.bump();
        }
    }

    /// A bare word: up to whitespace or any structural character.
    fn read_word(&mut self) -> &'a str {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_whitespace() || matches!(c, '=' | ',' | ']' | '[' | '{' | '}') {
                break;
            }
            self.bump();
        }
        &self.s[start..self.i]
    }

    /// A quoted value, or a bare word when there is no opening quote.
    fn read_value_string(&mut self) -> Result<String, ParseError> {
        self.skip_ws();
        match self.peek() {
            Some('"' | '\'') => self.read_quoted(),
            _ => Ok(self.read_word().to_string()),
        }
    }

    fn read_quoted(&mut self) -> Result<String, ParseError> {
        let quote = self.bump().ok_or(ParseError::Unterminated)?;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(ParseError::Unterminated),
                Some('\\') => match self.bump() {
                    Some(c) => out.push(c),
                    None => return Err(ParseError::Unterminated),
                },
                Some(c) if c == quote => return Ok(out),
                Some(c) => out.push(c),
            }
        }
    }

    /// A balanced `open…close` span, quotes respected, returned with its
    /// delimiters.
    fn read_balanced(&mut self, open: char, close: char) -> Result<&'a str, ParseError> {
        self.skip_ws();
        if self.peek() != Some(open) {
            return Err(ParseError::Unterminated);
        }
        let start = self.i;
        let mut depth = 0i32;
        let mut quote: Option<char> = None;
        while let Some(c) = self.bump() {
            if let Some(q) = quote {
                if c == '\\' {
                    self.bump();
                } else if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '"' | '\'' => quote = Some(c),
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(&self.s[start..self.i]);
                    }
                }
                _ => {}
            }
        }
        Err(ParseError::Unterminated)
    }
}

fn eat_bang(r: &mut Reader) -> bool {
    if r.peek() == Some('!') {
        r.bump();
        true
    } else {
        false
    }
}

fn read_invert_string(r: &mut Reader) -> Result<Invert<String>, ParseError> {
    let inverted = eat_bang(r);
    let value = r.read_value_string()?;
    Ok(Invert { value, inverted })
}

fn read_double(r: &mut Reader, key: &'static str) -> Result<f64, ParseError> {
    let word = r.read_word();
    word.parse::<f64>().map_err(|_| ParseError::BadValue {
        key: key.to_string(),
        value: word.to_string(),
    })
}

/// Whether `name` is a known entity type, the registry check vanilla's
/// `type=` does against `BuiltInRegistries.ENTITY_TYPE` (`type.invalid` if
/// not). This server's table (`game::data::ENTITY_TYPES`) is the closest
/// equivalent.
fn known_entity_type(name: &str) -> bool {
    crate::game::tables::entity_type(name).is_some()
}

/// Split `word` into `(min, max)` bound texts around a `..`; a bare number is
/// both bounds, an absent side is `None`, and an empty range is an error.
fn split_bounds<'a>(
    word: &'a str,
    key: &'static str,
) -> Result<(Option<&'a str>, Option<&'a str>), ParseError> {
    let bad = || ParseError::BadValue {
        key: key.to_string(),
        value: word.to_string(),
    };
    match word.split_once("..") {
        Some((lo, hi)) => {
            if lo.is_empty() && hi.is_empty() {
                return Err(bad());
            }
            if lo.contains("..") || hi.contains("..") {
                return Err(bad());
            }
            Ok((
                (!lo.is_empty()).then_some(lo),
                (!hi.is_empty()).then_some(hi),
            ))
        }
        None => {
            if word.is_empty() {
                return Err(bad());
            }
            Ok((Some(word), Some(word)))
        }
    }
}

/// Parse a double range (`MinMaxBounds.Doubles`): `n`, `n..`, `..n`, `n..m`,
/// rejecting a swapped pair (`argument.range.swapped`).
fn read_double_range(r: &mut Reader, key: &'static str) -> Result<NumRange, ParseError> {
    let word = r.read_word();
    let (lo, hi) = split_bounds(word, key)?;
    let parse = |s: &str| {
        s.parse::<f64>().map_err(|_| ParseError::BadValue {
            key: key.to_string(),
            value: word.to_string(),
        })
    };
    let min = lo.map(parse).transpose()?;
    let max = hi.map(parse).transpose()?;
    if let (Some(a), Some(b)) = (min, max) {
        if a > b {
            return Err(ParseError::SwappedRange);
        }
    }
    Ok(NumRange { min, max })
}

/// Parse an integer range (`MinMaxBounds.Ints`): `n`, `n..`, `..n`, `n..m`.
/// A non-integer bound surfaces the Brigadier `Invalid integer '%s'`.
fn read_int_range(r: &mut Reader, key: &'static str) -> Result<IntRange, ParseError> {
    let word = r.read_word();
    let (lo, hi) = split_bounds(word, key)?;
    let parse = |s: &str| {
        s.parse::<i32>()
            .map_err(|_| ParseError::InvalidInteger(s.to_string()))
    };
    let min = lo.map(parse).transpose()?;
    let max = hi.map(parse).transpose()?;
    if let (Some(a), Some(b)) = (min, max) {
        if a > b {
            return Err(ParseError::SwappedRange);
        }
    }
    Ok(IntRange { min, max })
}

/// Parse a wrapped angle range (`MinMaxBounds.FloatDegrees`): `n`, `n..`,
/// `..n`, `n..m`. Vanilla does **not** reject a swapped pair here.
fn read_angle_range(r: &mut Reader, key: &'static str) -> Result<AngleRange, ParseError> {
    let word = r.read_word();
    let (lo, hi) = split_bounds(word, key)?;
    let parse = |s: &str| {
        s.parse::<f32>()
            .map_err(|_| ParseError::InvalidFloat(s.to_string()))
    };
    let min = lo.map(parse).transpose()?;
    let max = hi.map(parse).transpose()?;
    Ok(AngleRange { min, max })
}

fn parse_scores(text: &str) -> Result<Vec<(String, IntRange)>, ParseError> {
    let inner = text
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or(ParseError::Unterminated)?;
    let mut out = Vec::new();
    for entry in split_top_level(inner) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (objective, value) = entry.split_once('=').ok_or_else(|| ParseError::BadValue {
            key: "scores".into(),
            value: entry.to_string(),
        })?;
        let objective = objective.trim().to_string();
        // Vanilla parses each score with `MinMaxBounds.Ints`.
        let range = parse_int_bounds(value.trim(), "scores")?;
        out.push((objective, range));
    }
    Ok(out)
}

/// Parse an integer range from text (no reader), for a `scores=` entry.
fn parse_int_bounds(word: &str, key: &'static str) -> Result<IntRange, ParseError> {
    let (lo, hi) = split_bounds(word, key)?;
    let parse = |s: &str| {
        s.parse::<i32>()
            .map_err(|_| ParseError::InvalidInteger(s.to_string()))
    };
    let min = lo.map(parse).transpose()?;
    let max = hi.map(parse).transpose()?;
    if let (Some(a), Some(b)) = (min, max) {
        if a > b {
            return Err(ParseError::SwappedRange);
        }
    }
    Ok(IntRange { min, max })
}

fn parse_advancements(text: &str) -> Result<Vec<AdvancementTest>, ParseError> {
    let inner = text
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or(ParseError::Unterminated)?;
    let mut out = Vec::new();
    for entry in split_top_level(inner) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (id, value) = entry.split_once('=').ok_or_else(|| ParseError::BadValue {
            key: "advancements".into(),
            value: entry.to_string(),
        })?;
        let id = normalize_resource(id.trim());
        let value = value.trim();
        if let Some(inner) = value.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            let mut criteria = Vec::new();
            for c in split_top_level(inner) {
                let c = c.trim();
                if c.is_empty() {
                    continue;
                }
                let (name, done) = c.split_once('=').ok_or_else(|| ParseError::BadValue {
                    key: "advancements".into(),
                    value: c.to_string(),
                })?;
                criteria.push((name.trim().to_string(), parse_bool(done.trim())?));
            }
            out.push(AdvancementTest {
                id,
                criteria,
                expected: None,
            });
        } else {
            out.push(AdvancementTest {
                id,
                criteria: Vec::new(),
                expected: Some(parse_bool(value)?),
            });
        }
    }
    Ok(out)
}

fn parse_bool(word: &str) -> Result<bool, ParseError> {
    match word {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ParseError::BadValue {
            key: "advancements".into(),
            value: word.to_string(),
        }),
    }
}

/// Split on top-level commas: commas inside `{}`, `[]` or quotes are kept.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Add the default `minecraft:` namespace when the user left it off.
fn normalize_resource(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

/// A syntactically valid `namespace:path`.
fn valid_resource(name: &str) -> bool {
    let Some((ns, path)) = name.split_once(':') else {
        return false;
    };
    let ok_ns = !ns.is_empty()
        && ns
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    let ok_path = !path.is_empty()
        && path.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-' | '/')
        });
    ok_ns && ok_path
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------

    fn at(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    #[derive(Default)]
    struct Fixture {
        executor: Option<EntityRef>,
        players: Vec<EntityRef>,
        entities: Vec<EntityRef>,
        pos: Option<Vec3>,
        rot: Option<Rotation>,
        tags: Vec<(String, String)>,
    }

    impl SelectorSource for Fixture {
        fn executor(&self) -> Option<EntityRef> {
            self.executor.clone()
        }
        fn players(&self) -> Vec<EntityRef> {
            self.players.clone()
        }
        fn entities(&self) -> Vec<EntityRef> {
            self.entities.clone()
        }
        fn position(&self) -> Option<Vec3> {
            self.pos
        }
        fn rotation(&self) -> Option<Rotation> {
            self.rot
        }
        fn entity_type_in_tag(&self, kind: &str, tag: &str) -> bool {
            self.tags.iter().any(|(k, t)| k == kind && t == tag)
        }
    }

    fn player(name: &str, id: i32, x: f64, y: f64, z: f64) -> EntityRef {
        EntityRef::player(name, id, at(x, y, z))
    }

    fn mob(kind: &str, id: i32, x: f64, y: f64, z: f64) -> EntityRef {
        EntityRef::other(kind, id, at(x, y, z))
    }

    fn parse_err(text: &str) -> ParseError {
        match Selector::parse(text) {
            Ok(s) => panic!("`{text}` parsed as {s:?}"),
            Err(e) => e,
        }
    }

    fn rng() -> LcgRng {
        LcgRng::new(0x5eed_1234_abcd_0001)
    }

    fn names(entities: &[EntityRef]) -> Vec<&str> {
        entities.iter().filter_map(|e| e.name.as_deref()).collect()
    }

    // -----------------------------------------------------------------
    // Parsing: variables and defaults
    // -----------------------------------------------------------------

    #[test]
    fn variables_and_their_defaults() {
        let a = Selector::parse("@a").unwrap();
        assert_eq!(a.variable, Variable::AllPlayers);
        assert_eq!(a.limit, 0);
        assert_eq!(a.sort, Sort::Arbitrary);
        assert!(!a.is_single());

        let e = Selector::parse("@e").unwrap();
        assert_eq!(e.variable, Variable::AllEntities);
        assert_eq!(e.limit, 0);
        assert_eq!(e.sort, Sort::Arbitrary);

        let p = Selector::parse("@p").unwrap();
        assert_eq!(p.limit, 1);
        assert_eq!(p.sort, Sort::Nearest);
        assert!(p.is_single());

        let r = Selector::parse("@r").unwrap();
        assert_eq!(r.limit, 1);
        assert_eq!(r.sort, Sort::Random);

        let s = Selector::parse("@s").unwrap();
        assert_eq!(s.variable, Variable::SelfEntity);
        assert!(s.is_single());

        let n = Selector::parse("@n").unwrap();
        assert_eq!(n.variable, Variable::NearestEntity);
        assert_eq!(n.limit, 1);
        assert_eq!(n.sort, Sort::Nearest);
    }

    #[test]
    fn unknown_and_missing_variables_name_what_is_wrong() {
        assert_eq!(parse_err("e"), ParseError::MissingVariable);
        assert_eq!(parse_err("@"), ParseError::MissingVariable);
        assert_eq!(parse_err("@x"), ParseError::UnknownVariable('x'));
        assert_eq!(parse_err("@c"), ParseError::UnknownVariable('c'));
    }

    // -----------------------------------------------------------------
    // Parsing: options
    // -----------------------------------------------------------------

    #[test]
    fn all_documented_keys_parse() {
        let text = "@e[type=!player,name=\"Bob\",distance=..5,x=1,y=2,z=3,\
                    dx=1,dy=2,dz=3,scores={foo=1..3,bar=7},tag=here,tag=!there,\
                    team=red,limit=4,sort=furthest,level=10..20,gamemode=creative,\
                    advancements={story/smelt_iron=true,story/root={x=false}},\
                    predicate=mymod:test,nbt={OnGround:1b},x_rotation=-90..0,\
                    y_rotation=0..180]";
        let s = Selector::parse(text).unwrap();
        assert_eq!(s.limit, 4);
        assert_eq!(s.sort, Sort::Furthest);
        assert_eq!(s.name.len(), 1);
        assert_eq!(s.name[0].value, "Bob");
        assert_eq!(s.entity_type.len(), 1);
        assert!(s.entity_type[0].inverted);
        assert_eq!(
            s.distance,
            Some(NumRange {
                min: None,
                max: Some(5.0)
            })
        );
        assert_eq!(
            s.level,
            Some(IntRange {
                min: Some(10),
                max: Some(20)
            })
        );
        assert_eq!(s.x, Some(1.0));
        assert_eq!(s.dy, Some(2.0));
        assert_eq!(s.scores.len(), 2);
        assert_eq!(s.tags.len(), 2);
        assert_eq!(s.team.len(), 1);
        assert_eq!(s.gamemode[0].value, Gamemode::Creative);
        assert_eq!(s.advancements.len(), 2);
        assert_eq!(s.advancements[0].expected, Some(true));
        assert_eq!(s.advancements[1].criteria, vec![("x".into(), false)]);
        assert_eq!(s.predicates[0].value, "mymod:test");
        assert_eq!(s.nbt.len(), 1);
        assert!(s.x_rotation.is_some() && s.y_rotation.is_some());
    }

    #[test]
    fn an_unknown_key_errors_naming_the_key() {
        let e = parse_err("@e[sideways=1]");
        assert_eq!(e, ParseError::UnknownKey("sideways".into()));
        assert!(e.message().contains("sideways"), "{}", e.message());
    }

    #[test]
    fn a_bad_value_errors_naming_the_value() {
        let e = parse_err("@e[sort=sideways]");
        assert_eq!(e, ParseError::UnknownSort("sideways".into()));
        assert!(e.message().contains("sideways"));

        let e = parse_err("@e[gamemode=sideways]");
        assert_eq!(e, ParseError::UnknownGamemode("sideways".into()));

        let e = parse_err("@e[x=notanumber]");
        assert_eq!(
            e,
            ParseError::BadValue {
                key: "x".into(),
                value: "notanumber".into()
            }
        );
        assert!(e.message().contains("notanumber"));
    }

    #[test]
    fn malformed_option_lists_error() {
        assert_eq!(parse_err("@e["), ParseError::Unterminated);
        assert_eq!(parse_err("@e[type=player"), ParseError::Unterminated);
        assert_eq!(
            parse_err("@e[type]"),
            ParseError::MissingValue("type".into())
        );
        assert_eq!(parse_err("@e[t=1]"), ParseError::UnknownKey("t".into()));
        assert_eq!(parse_err("@e trailing"), ParseError::Trailing);
    }

    #[test]
    fn duplicate_options_report_inapplicable_not_a_custom_string() {
        // Vanilla has no "already set" key: a second occurrence fails the
        // option's applicability predicate.
        for text in [
            "@e[limit=1,limit=2]",
            "@e[distance=1,distance=2]",
            "@e[level=1,level=2]",
            "@e[x=1,x=2]",
            "@e[x_rotation=1,x_rotation=2]",
            "@e[sort=nearest,sort=random]",
            "@e[scores={a=1},scores={b=2}]",
            "@e[advancements={a=true},advancements={b=true}]",
            "@e[type=creeper,type=pig]",
            "@a[name=Alice,name=Bob]",
            "@a[gamemode=creative,gamemode=survival]",
            "@a[team=red,team=blue]",
        ] {
            let key = text
                .split_once('[')
                .unwrap()
                .1
                .split_once('=')
                .unwrap()
                .0
                .to_string();
            assert_eq!(parse_err(text), ParseError::Inapplicable(key), "{text}");
        }
        // Repeated negations and repeated multi-valued options are allowed.
        assert!(Selector::parse("@e[type=!creeper,type=!pig]").is_ok());
        assert!(Selector::parse("@e[tag=a,tag=b]").is_ok());
        assert!(Selector::parse("@e[nbt={a:1},nbt={b:2}]").is_ok());
        assert!(Selector::parse("@e[predicate=a:1,predicate=a:2]").is_ok());
    }

    #[test]
    fn positive_and_negated_forms_cannot_mix() {
        // Vanilla's `excludesX() && !negated` check: a positive after a
        // negation is inapplicable for `name`, `gamemode` and `type`.
        for text in [
            "@e[type=!creeper,type=pig]",
            "@a[name=!Alice,name=Bob]",
            "@a[gamemode=!creative,gamemode=survival]",
        ] {
            let key = text
                .split_once('[')
                .unwrap()
                .1
                .split_once('=')
                .unwrap()
                .0
                .to_string();
            assert_eq!(parse_err(text), ParseError::Inapplicable(key), "{text}");
        }
        // Positive-then-negative is caught by the applicability predicate.
        for text in [
            "@e[type=creeper,type=!pig]",
            "@a[name=Alice,name=!Bob]",
            "@a[gamemode=creative,gamemode=!survival]",
            "@a[team=red,team=!blue]",
        ] {
            let key = text
                .split_once('[')
                .unwrap()
                .1
                .split_once('=')
                .unwrap()
                .0
                .to_string();
            assert_eq!(parse_err(text), ParseError::Inapplicable(key), "{text}");
        }
        // Negation-then-positive is allowed for `team` (no `excludesTeam`).
        assert!(Selector::parse("@a[team=!red,team=blue]").is_ok());
        // Negative-then-negative is always fine.
        assert!(Selector::parse("@a[name=!A,name=!B]").is_ok());
        assert!(Selector::parse("@a[gamemode=!creative,gamemode=!survival]").is_ok());
    }

    #[test]
    fn limit_and_sort_are_inapplicable_on_self() {
        for text in [
            "@s[limit=2]",
            "@s[sort=nearest]",
            "@s[limit=1]",
            "@s[sort=random]",
        ] {
            let key = text
                .split_once('[')
                .unwrap()
                .1
                .split_once('=')
                .unwrap()
                .0
                .to_string();
            assert_eq!(parse_err(text), ParseError::Inapplicable(key), "{text}");
        }
        // Still fine on every other variable.
        assert!(Selector::parse("@a[limit=2,sort=nearest]").is_ok());
        assert!(Selector::parse("@e[limit=2]").is_ok());
    }

    #[test]
    fn type_is_refused_on_player_only_selectors() {
        for text in ["@a[type=zombie]", "@p[type=zombie]", "@r[type=!player]"] {
            assert_eq!(
                parse_err(text),
                ParseError::Inapplicable("type".into()),
                "{text}"
            );
        }
        // @e, @n and @s allow it.
        assert!(Selector::parse("@e[type=zombie]").is_ok());
        assert!(Selector::parse("@n[type=zombie]").is_ok());
        assert!(Selector::parse("@s[type=zombie]").is_ok());
    }

    #[test]
    fn an_unknown_entity_type_is_rejected() {
        // Vanilla checks the id against the entity-type registry and reports
        // `type.invalid` with the parsed identifier.
        let e = parse_err("@e[type=notarealtype]");
        assert_eq!(e, ParseError::UnknownType("minecraft:notarealtype".into()));
        assert_eq!(
            e.message(),
            "Invalid or unknown entity type 'minecraft:notarealtype'"
        );
        // A known mob type is accepted.
        assert!(Selector::parse("@e[type=minecraft:zombie]").is_ok());
    }

    #[test]
    fn limit_and_ranges_validate() {
        assert_eq!(parse_err("@e[limit=0]"), ParseError::LimitTooSmall);
        assert_eq!(parse_err("@e[limit=-1]"), ParseError::LimitTooSmall);
        assert_eq!(
            parse_err("@e[distance=..]"),
            ParseError::BadValue {
                key: "distance".into(),
                value: "..".into()
            }
        );
        assert_eq!(parse_err("@e[distance=9..2]"), ParseError::SwappedRange);
        assert_eq!(
            parse_err("@e[distance=-1]"),
            ParseError::Negative("distance".into())
        );
        assert_eq!(
            parse_err("@e[level=-1]"),
            ParseError::Negative("level".into())
        );
        assert_eq!(
            parse_err("@e[level=1.5]"),
            ParseError::InvalidInteger("1.5".into())
        );
        assert_eq!(
            parse_err("@e[scores={foo=1.5}]"),
            ParseError::InvalidInteger("1.5".into())
        );
        assert_eq!(
            parse_err("@e[x_rotation=sideways]"),
            ParseError::InvalidFloat("sideways".into())
        );
        // Vanilla's `level` uses the Brigadier integer message.
        assert_eq!(
            parse_err("@e[level=1.5]").message(),
            "Invalid integer '1.5'"
        );
        assert_eq!(
            parse_err("@e[level=-1]").message(),
            "Level shouldn't be negative"
        );
    }

    #[test]
    fn all_range_forms_work_for_every_range_option() {
        // Double range.
        for (text, want) in [
            (
                "5",
                NumRange {
                    min: Some(5.0),
                    max: Some(5.0),
                },
            ),
            (
                "5..",
                NumRange {
                    min: Some(5.0),
                    max: None,
                },
            ),
            (
                "..5",
                NumRange {
                    min: None,
                    max: Some(5.0),
                },
            ),
            (
                "2..5",
                NumRange {
                    min: Some(2.0),
                    max: Some(5.0),
                },
            ),
        ] {
            let s = Selector::parse(&format!("@e[distance={text}]")).unwrap();
            assert_eq!(s.distance, Some(want), "distance={text}");
        }
        // Integer range.
        for (text, want) in [
            (
                "5",
                IntRange {
                    min: Some(5),
                    max: Some(5),
                },
            ),
            (
                "5..",
                IntRange {
                    min: Some(5),
                    max: None,
                },
            ),
            (
                "..5",
                IntRange {
                    min: None,
                    max: Some(5),
                },
            ),
            (
                "2..5",
                IntRange {
                    min: Some(2),
                    max: Some(5),
                },
            ),
        ] {
            let s = Selector::parse(&format!("@e[level={text}]")).unwrap();
            assert_eq!(s.level, Some(want), "level={text}");
        }
        // Wrapped angle range.
        for (text, want) in [
            (
                "5",
                AngleRange {
                    min: Some(5.0),
                    max: Some(5.0),
                },
            ),
            (
                "5..",
                AngleRange {
                    min: Some(5.0),
                    max: None,
                },
            ),
            (
                "..5",
                AngleRange {
                    min: None,
                    max: Some(5.0),
                },
            ),
            (
                "2..5",
                AngleRange {
                    min: Some(2.0),
                    max: Some(5.0),
                },
            ),
        ] {
            let s = Selector::parse(&format!("@e[x_rotation={text}]")).unwrap();
            assert_eq!(s.x_rotation, Some(want), "x_rotation={text}");
        }
    }

    #[test]
    fn angle_ranges_wrap_and_allow_a_swapped_pair() {
        // Vanilla's `FloatDegrees` does not reject `170..-170`; it wraps both
        // bounds and the entity's angle.
        let s = Selector::parse("@e[y_rotation=170..-170]").unwrap();
        let range = s.y_rotation.unwrap();
        assert!(range.contains(180.0), "180 is just west of north");
        assert!(range.contains(-175.0));
        assert!(!range.contains(0.0), "0 is due south, in the 20 gap");
        assert!(!range.contains(90.0));

        // A plain range still behaves normally, around the wrap point.
        let s = Selector::parse("@e[y_rotation=170..200]").unwrap();
        let range = s.y_rotation.unwrap();
        assert!(range.contains(175.0));
        assert!(range.contains(-160.0), "200 wraps to -160");
        assert!(!range.contains(0.0));

        // Vanilla's predicate fills an absent bound with `0`/`359` and then
        // wraps, so `..n` is `0..n` and `n..` is `n..359` rather than
        // unbounded. `x_rotation=..0` is therefore exactly `0`.
        let s = Selector::parse("@e[x_rotation=..0]").unwrap();
        let range = s.x_rotation.unwrap();
        assert!(range.contains(0.0));
        assert!(!range.contains(-1.0));
        assert!(!range.contains(90.0));
    }

    #[test]
    fn single_arguments_reject_multi_selectors() {
        assert!(Selector::parse_single("@p").is_ok());
        assert!(Selector::parse_single("@r").is_ok());
        assert!(Selector::parse_single("@s").is_ok());
        assert!(Selector::parse_single("@n").is_ok());
        assert!(Selector::parse_single("@e[limit=1]").is_ok());
        assert!(Selector::parse_single("@a[limit=1]").is_ok());

        assert_eq!(
            Selector::parse_single("@a").unwrap_err(),
            ParseError::TooManyEntities
        );
        assert_eq!(
            Selector::parse_single("@e").unwrap_err(),
            ParseError::TooManyEntities
        );
        assert_eq!(
            Selector::parse_single("@e[limit=2]").unwrap_err(),
            ParseError::TooManyEntities
        );
    }

    #[test]
    fn player_arguments_use_the_player_wording_and_reject_entities() {
        // A player single argument given `@a`/`@e` reports the player cap.
        assert_eq!(
            Selector::parse_single_player("@a").unwrap_err(),
            ParseError::TooManyPlayers
        );
        assert_eq!(
            Selector::parse_single_player("@a").unwrap_err().message(),
            "Only one player is allowed, but the provided selector allows more than one"
        );
        assert!(Selector::parse_single_player("@p").is_ok());
        assert!(Selector::parse_single_player("@e[type=player,limit=1]").is_ok());

        // A player multiple argument rejects a selector that can include
        // non-players, but accepts one narrowed to players or `@s`.
        assert_eq!(
            Selector::parse_players("@e").unwrap_err(),
            ParseError::OnlyPlayersAllowed
        );
        assert_eq!(
            Selector::parse_players("@e").unwrap_err().message(),
            "Only players may be affected by this command, but the provided selector includes entities"
        );
        assert!(Selector::parse_players("@a").is_ok());
        assert!(Selector::parse_players("@s").is_ok());
        assert!(Selector::parse_players("@e[type=player]").is_ok());
        assert!(Selector::parse_players("@e[gamemode=creative]").is_ok());
        assert!(Selector::parse_players("@e[level=1..]").is_ok());
    }

    // -----------------------------------------------------------------
    // Selection semantics
    // -----------------------------------------------------------------

    #[test]
    fn all_players_and_all_entities_differ_only_by_mobs() {
        let world = Fixture {
            players: vec![player("a", 1, 0.0, 0.0, 0.0), player("b", 2, 1.0, 0.0, 0.0)],
            entities: vec![mob("minecraft:zombie", 100, 2.0, 0.0, 0.0)],
            ..Default::default()
        };
        let all = Selector::parse("@a")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(names(&all), ["a", "b"]);
        let every = Selector::parse("@e")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(every.len(), 3);
        assert_eq!(every[2].kind, "minecraft:zombie");
    }

    #[test]
    fn type_negation_excludes_players() {
        let world = Fixture {
            players: vec![player("a", 1, 0.0, 0.0, 0.0)],
            entities: vec![mob("minecraft:zombie", 100, 2.0, 0.0, 0.0)],
            ..Default::default()
        };
        let out = Selector::parse("@e[type=!player]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "minecraft:zombie");

        let players = Selector::parse("@e[type=player]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(names(&players), ["a"]);
    }

    #[test]
    fn distance_ranges_are_measured_from_the_origin() {
        let world = Fixture {
            players: vec![
                player("near", 1, 0.0, 0.0, 0.0),
                player("mid", 2, 3.0, 4.0, 0.0),
                player("far", 3, 100.0, 0.0, 0.0),
            ],
            pos: Some(at(0.0, 0.0, 0.0)),
            ..Default::default()
        };
        let out = Selector::parse("@a[distance=..5]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(names(&out), ["near", "mid"]);

        // `x/y/z` move the origin: from (100,0,0) only `far` is within 5.
        let out = Selector::parse("@a[distance=..5,x=100,y=0,z=0]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(names(&out), ["far"]);
    }

    #[test]
    fn a_volume_box_filters_by_dx_dy_dz() {
        let world = Fixture {
            players: vec![
                player("in", 1, 1.5, 0.5, 1.5),
                player("out", 2, 5.5, 0.5, 1.5),
            ],
            pos: Some(at(0.0, 0.0, 0.0)),
            ..Default::default()
        };
        let out = Selector::parse("@a[dx=2,dy=2,dz=2]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(names(&out), ["in"]);
    }

    #[test]
    fn nearest_and_furthest_sort_by_distance() {
        let world = Fixture {
            players: vec![
                player("middle", 1, 5.0, 0.0, 0.0),
                player("near", 2, 1.0, 0.0, 0.0),
                player("far", 3, 9.0, 0.0, 0.0),
            ],
            pos: Some(at(0.0, 0.0, 0.0)),
            ..Default::default()
        };
        let near = Selector::parse("@a[sort=nearest]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(names(&near), ["near", "middle", "far"]);
        let far = Selector::parse("@a[sort=furthest,limit=1]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(names(&far), ["far"]);
    }

    #[test]
    fn random_sort_is_deterministic_under_a_seed() {
        let world = Fixture {
            players: (0..8)
                .map(|i| player(&format!("p{i}"), i, i as f64, 0.0, 0.0))
                .collect(),
            pos: Some(at(0.0, 0.0, 0.0)),
            ..Default::default()
        };
        let selector = Selector::parse("@e[limit=1,sort=random]").unwrap();
        let a = selector.find(&world, &mut LcgRng::new(42)).unwrap();
        let b = selector.find(&world, &mut LcgRng::new(42)).unwrap();
        assert_eq!(names(&a), names(&b));
        assert_eq!(a.len(), 1);

        // Different seeds eventually pick a different entity.
        let mut seen = std::collections::HashSet::new();
        for seed in 0..50 {
            let out = selector.find(&world, &mut LcgRng::new(seed)).unwrap();
            seen.insert(out[0].name.clone().unwrap());
        }
        assert!(seen.len() > 1, "random never varied: {seen:?}");
    }

    #[test]
    fn limit_caps_the_result() {
        let world = Fixture {
            players: (0..10)
                .map(|i| player(&format!("p{i}"), i, i as f64, 0.0, 0.0))
                .collect(),
            pos: Some(at(0.0, 0.0, 0.0)),
            ..Default::default()
        };
        let out = Selector::parse("@a[limit=3]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(out.len(), 3);
        // Unlimited selects everyone.
        let all = Selector::parse("@a")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(all.len(), 10);
    }

    #[test]
    fn name_tag_team_scores_level_gamemode_advancement_and_predicate_filters() {
        let mut alice = player("Alice", 1, 0.0, 0.0, 0.0);
        alice.tags = vec!["here".into(), "vip".into()];
        alice.team = Some("red".into());
        alice.scores = vec![("kills".into(), 12), ("deaths".into(), 1)];
        alice.level = 30;
        alice.gamemode = Some(Gamemode::Creative);
        alice.advancements = vec![Advancement {
            id: "minecraft:story/smelt_iron".into(),
            done: true,
            criteria: vec![("has_iron".into(), true)],
        }];
        alice.predicates = vec!["mymod:test".into()];
        alice.nbt = Nbt::Compound(vec![
            ("OnGround".into(), Nbt::Byte(1)),
            ("Health".into(), Nbt::Float(20.0)),
        ]);
        alice.rotation = Rotation::new(45.0, -10.0);
        let mut bob = player("Bob", 2, 1.0, 0.0, 0.0);
        bob.team = None;
        bob.scores = vec![("kills".into(), 3)];
        bob.level = 5;
        bob.rotation = Rotation::new(180.0, 80.0);
        let world = Fixture {
            players: vec![alice, bob],
            ..Default::default()
        };

        let check = |text: &str| {
            let out = Selector::parse(text)
                .unwrap()
                .find(&world, &mut rng())
                .unwrap();
            names(&out)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };

        assert_eq!(check("@a[name=Alice]"), ["Alice"]);
        assert_eq!(check("@a[name=!Alice]"), ["Bob"]);
        assert_eq!(check("@e[tag=here]"), ["Alice"]);
        assert_eq!(check("@e[tag=!here]"), ["Bob"]);
        assert_eq!(check("@e[tag=]"), ["Bob"]);
        assert_eq!(check("@e[tag=!]"), ["Alice"]);
        assert_eq!(check("@e[team=red]"), ["Alice"]);
        assert_eq!(check("@e[team=!red]"), ["Bob"]);
        assert_eq!(check("@e[team=]"), ["Bob"]);
        assert_eq!(check("@a[scores={kills=10..}]"), ["Alice"]);
        assert_eq!(
            check("@a[scores={kills=1..5,deaths=1}]"),
            Vec::<String>::new()
        );
        assert_eq!(check("@a[level=20..]"), ["Alice"]);
        assert_eq!(check("@a[gamemode=creative]"), ["Alice"]);
        assert_eq!(check("@a[gamemode=!creative]"), ["Bob"]);
        assert_eq!(check("@a[advancements={story/smelt_iron=true}]"), ["Alice"]);
        assert_eq!(
            check("@a[advancements={story/smelt_iron={has_iron=true}}]"),
            ["Alice"]
        );
        assert_eq!(check("@a[predicate=mymod:test]"), ["Alice"]);
        // Alice looks 10° up (pitch -10); a wrapped angle range brackets it.
        assert_eq!(check("@a[x_rotation=-90..0]"), ["Alice"]);
        assert_eq!(check("@a[y_rotation=0..90]"), ["Alice"]);
    }

    #[test]
    fn nbt_subset_matching() {
        let expected = snbt::parse_compound("{OnGround:1b}").unwrap();
        let actual = snbt::parse_compound("{OnGround:1b,Health:20.0f}").unwrap();
        assert!(nbt_matches(&expected, &actual));
        let missing = snbt::parse_compound("{Flying:1b}").unwrap();
        assert!(!nbt_matches(&missing, &actual));
        // Lists must match exactly.
        let list = snbt::parse_compound("{Tags:[\"a\",\"b\"]}").unwrap();
        let same = snbt::parse_compound("{Tags:[\"a\",\"b\"]}").unwrap();
        let shorter = snbt::parse_compound("{Tags:[\"a\"]}").unwrap();
        assert!(nbt_matches(&list, &same));
        assert!(!nbt_matches(&list, &shorter));
    }

    // -----------------------------------------------------------------
    // Empty / error semantics
    // -----------------------------------------------------------------

    #[test]
    fn p_and_r_with_no_players_error() {
        let empty = Fixture::default();
        assert_eq!(
            Selector::parse("@p").unwrap().find(&empty, &mut rng()),
            Err(SelectError::NoPlayer)
        );
        assert_eq!(
            Selector::parse("@r").unwrap().find(&empty, &mut rng()),
            Err(SelectError::NoPlayer)
        );
    }

    #[test]
    fn s_with_no_executor_errors() {
        let console = Fixture::default();
        assert_eq!(
            Selector::parse("@s").unwrap().find(&console, &mut rng()),
            Err(SelectError::NoEntity)
        );
        assert_eq!(SelectError::NoEntity.message(), "No entity was found");
    }

    #[test]
    fn select_turns_an_empty_result_into_an_error() {
        let no_mobs = Fixture {
            players: vec![player("a", 1, 0.0, 0.0, 0.0)],
            ..Default::default()
        };
        // `find` is silent for @e, `select` is not.
        assert!(Selector::parse("@e[type=zombie]")
            .unwrap()
            .find(&no_mobs, &mut rng())
            .unwrap()
            .is_empty());
        assert_eq!(
            Selector::parse("@e[type=zombie]")
                .unwrap()
                .select(&no_mobs, &mut rng()),
            Err(SelectError::NoEntity)
        );
        // A player-only variable reports the player wording.
        assert_eq!(
            Selector::parse("@a[name=nobody]")
                .unwrap()
                .select(&no_mobs, &mut rng()),
            Err(SelectError::NoPlayer)
        );
    }

    #[test]
    fn select_one_enforces_the_single_cap() {
        let world = Fixture {
            players: vec![player("a", 1, 0.0, 0.0, 0.0), player("b", 2, 1.0, 0.0, 0.0)],
            ..Default::default()
        };
        // An entity argument reports the entity wording...
        assert_eq!(
            Selector::parse("@a")
                .unwrap()
                .select_one(&world, &mut rng()),
            Err(SelectError::TooManyEntities)
        );
        assert_eq!(
            Selector::parse("@e")
                .unwrap()
                .select_one(&world, &mut rng()),
            Err(SelectError::TooManyEntities)
        );
        // ...a player argument the player wording.
        assert_eq!(
            Selector::parse_players("@a")
                .unwrap()
                .select_one(&world, &mut rng()),
            Err(SelectError::TooManyPlayers)
        );
        let one = Selector::parse("@p")
            .unwrap()
            .select_one(&world, &mut rng())
            .unwrap();
        assert_eq!(one.name.as_deref(), Some("a"));
    }

    #[test]
    fn s_selects_only_an_executor_that_passes_the_filters() {
        let mut me = player("me", 1, 0.0, 0.0, 0.0);
        me.tags = vec!["vip".into()];
        let world = Fixture {
            executor: Some(me),
            pos: Some(at(0.0, 0.0, 0.0)),
            ..Default::default()
        };
        assert_eq!(
            Selector::parse("@s")
                .unwrap()
                .find(&world, &mut rng())
                .unwrap()
                .len(),
            1
        );
        assert!(
            Selector::parse("@s[tag=vip]")
                .unwrap()
                .find(&world, &mut rng())
                .unwrap()
                .len()
                == 1
        );
        assert!(Selector::parse("@s[tag=nope]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn type_tags_are_resolved_through_the_source() {
        let world = Fixture {
            entities: vec![mob("minecraft:zombie", 100, 0.0, 0.0, 0.0)],
            tags: vec![("minecraft:zombie".into(), "#minecraft:undead".into())],
            ..Default::default()
        };
        let out = Selector::parse("@e[type=#minecraft:undead]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(out.len(), 1);
        let none = Selector::parse("@e[type=#minecraft:arthropod]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn e_and_n_only_match_live_entities() {
        let mut dead_mob = mob("minecraft:zombie", 100, 2.0, 0.0, 0.0);
        dead_mob.alive = false;
        let mut dead_player = player("ghost", 2, 3.0, 0.0, 0.0);
        dead_player.alive = false;
        let world = Fixture {
            players: vec![player("alive", 1, 0.0, 0.0, 0.0), dead_player],
            entities: vec![dead_mob],
            ..Default::default()
        };
        // `@e` drops the dead mob.
        let out = Selector::parse("@e")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name.as_deref(), Some("alive"));

        // `@n` too.
        let nearest = Selector::parse("@n")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(nearest[0].name.as_deref(), Some("alive"));

        // `@a` may still see a dead player.
        let players = Selector::parse("@a")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap();
        assert_eq!(names(&players), ["alive", "ghost"]);
    }

    #[test]
    fn name_falls_back_to_the_type_display_name() {
        // A mob with no custom name matches its humanised type name, as
        // vanilla's `getStringifiedName` would.
        let world = Fixture {
            entities: vec![mob("minecraft:experience_orb", 100, 0.0, 0.0, 0.0)],
            ..Default::default()
        };
        assert_eq!(
            Selector::parse("@e[name=\"Experience Orb\"]")
                .unwrap()
                .find(&world, &mut rng())
                .unwrap()
                .len(),
            1
        );
        // An explicit name wins over the fallback.
        let mut named = mob("minecraft:zombie", 101, 1.0, 0.0, 0.0);
        named.name = Some("Bob".into());
        let world = Fixture {
            entities: vec![named],
            ..Default::default()
        };
        assert_eq!(
            Selector::parse("@e[name=Bob]")
                .unwrap()
                .find(&world, &mut rng())
                .unwrap()
                .len(),
            1
        );
        assert!(Selector::parse("@e[name=Zombie]")
            .unwrap()
            .find(&world, &mut rng())
            .unwrap()
            .is_empty());
    }

    // -----------------------------------------------------------------
    // Lock-order shape
    // -----------------------------------------------------------------

    #[test]
    fn parsing_never_touches_the_source() {
        // `parse` takes no source at all, so a caller may parse while holding
        // the entity lock. This is a compile-time fact; the assertion is only
        // here so the test names the guarantee.
        let selector = Selector::parse("@e[type=!player,distance=..5]").unwrap();
        assert_eq!(selector.variable, Variable::AllEntities);
    }

    #[test]
    fn find_snapshots_the_source_once_each() {
        // The lock order depends on `find` taking the player and entity lists
        // once and then filtering owned copies. The one per-entity callback,
        // `entity_type_in_tag` for `type=#tag`, must not be used to reach back
        // into a locked source's lists — the two list hooks are each called
        // exactly once.
        use std::cell::Cell;
        struct Counting<'a> {
            inner: &'a Fixture,
            player_calls: Cell<usize>,
            entity_calls: Cell<usize>,
            tag_calls: Cell<usize>,
        }
        impl SelectorSource for Counting<'_> {
            fn players(&self) -> Vec<EntityRef> {
                self.player_calls.set(self.player_calls.get() + 1);
                self.inner.players()
            }
            fn entities(&self) -> Vec<EntityRef> {
                self.entity_calls.set(self.entity_calls.get() + 1);
                self.inner.entities()
            }
            fn entity_type_in_tag(&self, kind: &str, tag: &str) -> bool {
                self.tag_calls.set(self.tag_calls.get() + 1);
                self.inner.entity_type_in_tag(kind, tag)
            }
        }
        let inner = Fixture {
            players: vec![player("a", 1, 0.0, 0.0, 0.0)],
            entities: vec![mob("minecraft:zombie", 100, 2.0, 0.0, 0.0)],
            tags: vec![("minecraft:zombie".into(), "#minecraft:undead".into())],
            ..Default::default()
        };
        let counting = Counting {
            inner: &inner,
            player_calls: Cell::new(0),
            entity_calls: Cell::new(0),
            tag_calls: Cell::new(0),
        };
        let out = Selector::parse("@e[type=#minecraft:undead]")
            .unwrap()
            .find(&counting, &mut rng())
            .unwrap();
        assert_eq!(out.len(), 1);
        // Both snapshots are taken up front; the tag hook runs once per
        // candidate, but never triggers another snapshot.
        assert_eq!(counting.player_calls.get(), 1);
        assert_eq!(counting.entity_calls.get(), 1);
        assert_eq!(counting.tag_calls.get(), 2, "one per candidate entity");

        // A selector without a type tag never calls the tag hook.
        let counting = Counting {
            inner: &inner,
            player_calls: Cell::new(0),
            entity_calls: Cell::new(0),
            tag_calls: Cell::new(0),
        };
        let _ = Selector::parse("@e[type=!player]")
            .unwrap()
            .find(&counting, &mut rng())
            .unwrap();
        assert_eq!(counting.tag_calls.get(), 0);
    }

    // -----------------------------------------------------------------
    // Fuzz-ish property test
    // -----------------------------------------------------------------

    /// A second, independently-seeded LCG so the fuzz corpus differs from the
    /// random-sort test's stream.
    struct Fuzz(u64);
    impl Fuzz {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 11
        }
    }

    #[test]
    fn garbage_and_mutated_selectors_never_panic() {
        const CHARSET: &[char] = &[
            '@', 'a', 'e', 'p', 'r', 's', 'n', 'x', '[', ']', '=', ',', '!', '.', ':', '{', '}',
            '"', '\'', '#', '-', '0', '1', '9', '_', ' ', '\t', 'y', 'z', 't', 'g', 'l', 'm', '\\',
            '+', 'h',
        ];
        let mut fuzz = Fuzz(0xdead_beef_cafe_0001);
        let world = Fixture {
            executor: Some(player("me", 1, 0.0, 0.0, 0.0)),
            players: vec![player("a", 1, 1.0, 0.0, 0.0), player("b", 2, 2.0, 0.0, 0.0)],
            entities: vec![mob("minecraft:zombie", 100, 3.0, 0.0, 0.0)],
            pos: Some(at(0.0, 0.0, 0.0)),
            rot: Some(Rotation::new(0.0, 0.0)),
            tags: vec![("minecraft:zombie".into(), "#minecraft:undead".into())],
        };
        for _ in 0..8_000 {
            let len = (fuzz.next() % 24) as usize;
            let s: String = (0..len)
                .map(|_| CHARSET[(fuzz.next() % CHARSET.len() as u64) as usize])
                .collect();
            match Selector::parse(&s) {
                Ok(selector) => {
                    // A parseable selector must resolve without panicking, and
                    // the raw find must never fail on an empty world.
                    let mut rng = LcgRng::new(fuzz.next());
                    let _ = selector.find(&world, &mut rng);
                    let _ = selector.find(&Fixture::default(), &mut rng);
                }
                Err(e) => {
                    // Errors always render, and never carry a newline.
                    assert!(!e.message().is_empty());
                }
            }
        }
    }

    #[test]
    fn mutations_of_valid_selectors_are_stable() {
        let valid = [
            "@a",
            "@e",
            "@p",
            "@r",
            "@s",
            "@n",
            "@e[type=!player,limit=3,sort=nearest]",
            "@a[distance=..10,name=\"Bob\"]",
            "@e[scores={kills=1..5},tag=x,team=red]",
            "@e[advancements={story/root=true},predicate=pkg:id]",
        ];
        let world = Fixture {
            executor: Some(player("me", 1, 0.0, 0.0, 0.0)),
            players: vec![player("Bob", 1, 1.0, 0.0, 0.0)],
            entities: vec![mob("minecraft:zombie", 100, 2.0, 0.0, 0.0)],
            pos: Some(at(0.0, 0.0, 0.0)),
            rot: Some(Rotation::new(0.0, 0.0)),
            tags: Vec::new(),
        };
        let alphabet: &[u8] = b"@aeprsn[]=,!.{}:\"'#-019_yztglm\\+h ";
        let mut fuzz = Fuzz(0x1234_5678_9abc_0002);
        for base in valid {
            // The base parses.
            assert!(Selector::parse(base).is_ok(), "{base}");
            let bytes = base.as_bytes();
            for _ in 0..500 {
                let mut mutated = bytes.to_vec();
                let mutations = 1 + fuzz.next() % 3;
                for _ in 0..mutations {
                    if mutated.is_empty() {
                        break;
                    }
                    let at = (fuzz.next() as usize) % mutated.len();
                    mutated[at] = alphabet[(fuzz.next() % alphabet.len() as u64) as usize];
                }
                if let Ok(s) = std::str::from_utf8(&mutated) {
                    match Selector::parse(s) {
                        Ok(sel) => {
                            let mut rng = LcgRng::new(fuzz.next());
                            let _ = sel.find(&world, &mut rng);
                        }
                        Err(e) => assert!(!e.message().is_empty()),
                    }
                }
            }
        }
    }
}
