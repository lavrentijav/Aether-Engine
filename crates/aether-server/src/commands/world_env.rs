//! World environment commands: time, weather, world border, world spawn and
//! the default game mode (CMD-13a).
//!
//! The commands follow vanilla 1.21.11's grammar and use its `en_us` strings
//! verbatim:
//!
//! ```text
//! /time add <time>
//! /time set (day|noon|night|midnight|<time>)
//! /time query (daytime|gametime|day)
//! /weather (clear|rain|thunder) [<duration>]
//! /worldborder add <distance> [<time>]
//! /worldborder set <distance> [<time>]
//! /worldborder center <pos>
//! /worldborder damage amount <damagePerBlock>
//! /worldborder damage buffer <distance>
//! /worldborder get
//! /worldborder warning distance <distance>
//! /worldborder warning time <time>
//! /setworldspawn [<pos>] [<angle>]
//! /defaultgamemode <gamemode>
//! ```
//!
//! The old `/time set <day|noon|night|midnight|ticks>`, `/time <ticks>` and
//! bare `/time` (a daytime query) keep working, because players used them
//! before the tree grew the full grammar.
//!
//! ## State
//!
//! Time is the engine's world clock ([`crate::game`]); the weather, world
//! border and `/setworldspawn` override are process-wide runtime state. They
//! survive the game tick and every command, but are deliberately **not**
//! persisted: a restart returns to clear weather, the vanilla default border
//! (59,999,968 blocks wide, centred on 0,0) and the generated spawn. The default
//! game mode is likewise runtime state; it affects players who join afterwards,
//! never someone already online.

use std::sync::{Mutex, OnceLock};

use aether_api::Vector3;

use super::coordinates::{CoordError, CoordinateSource};
use crate::commands::feedback::lang;
use crate::commands::tree::{self, Builder, Ctx, Node, Requirement};
use crate::commands::Reply;
use crate::game;
use crate::players::SharedRegistry;
use crate::protocol::GameMode;
use crate::session::DemoWorld;

/// Vanilla's largest world border diameter.
pub const WORLD_BORDER_MAX: f64 = 59_999_968.0;
/// Vanilla's largest absolute centre coordinate.
pub const WORLD_BORDER_CENTER_MAX: f64 = 29_999_984.0;
/// Vanilla's smallest world border diameter.
pub const WORLD_BORDER_MIN: f64 = 1.0;

fn node_requirement(op: u8, node: &'static str) -> Requirement {
    Requirement::Level { op, node }
}

fn time_node() -> Node {
    let req = node_requirement(2, "minecraft.command.time");

    let mut set = Builder::literal("set");
    for (name, value) in [
        ("day", 1000),
        ("noon", 6000),
        ("night", 13000),
        ("midnight", 18000),
    ] {
        set =
            set.then(Builder::literal(name).executable(move |ctx| exec_time_set_named(ctx, value)));
    }
    set = set.then(Builder::arg("time", tree::ArgKind::Time).executable(exec_time_set_argument));

    let mut query = Builder::literal("query");
    for name in ["daytime", "gametime", "day"] {
        query = query
            .then(Builder::literal(name).executable(move |ctx| exec_time_query_named(ctx, name)));
    }
    query = query.executable(exec_time_query);

    let mut node = Builder::literal("time")
        .requires(req)
        .executable(exec_time_query)
        .then(
            Builder::literal("add")
                .then(Builder::arg("time", tree::ArgKind::Time).executable(exec_time_add)),
        )
        .then(set)
        .then(query);

    // The pre-tree forms: `/time <ticks>` sets, `/time day` sets a marker, and
    // bare `/time` queries daytime (the root is executable above).
    for (name, value) in [
        ("day", 1000),
        ("noon", 6000),
        ("night", 13000),
        ("midnight", 18000),
    ] {
        node = node
            .then(Builder::literal(name).executable(move |ctx| exec_time_set_named(ctx, value)));
    }
    node = node.then(Builder::arg("time", tree::ArgKind::Time).executable(exec_time_set_argument));

    tree::with_usage(
        node.build(),
        "add <time> | set <day|noon|night|midnight|<time>> | query <daytime|gametime|day>",
    )
}

fn weather_node() -> Node {
    let mut node = Builder::literal("weather")
        .requires(node_requirement(2, "minecraft.command.weather"))
        .executable(exec_weather_usage);
    for (name, weather) in [
        ("clear", Weather::Clear),
        ("rain", Weather::Rain),
        ("thunder", Weather::Thunder),
    ] {
        node = node.then(
            Builder::literal(name)
                .executable(move |ctx| exec_weather(ctx, weather))
                .then(
                    Builder::arg("duration", tree::ArgKind::Time)
                        .executable(move |ctx| exec_weather(ctx, weather)),
                ),
        );
    }
    tree::with_usage(node.build(), "<clear|rain|thunder> [<duration>]")
}

fn worldborder_node() -> Node {
    let req = node_requirement(2, "minecraft.command.worldborder");
    let time_branch = |literal: &'static str| {
        Builder::literal(literal).then(
            Builder::arg("distance", tree::ArgKind::Double)
                .executable(move |ctx| exec_worldborder_set(ctx, literal == "add"))
                .then(
                    Builder::arg("time", tree::ArgKind::Time)
                        .executable(move |ctx| exec_worldborder_set(ctx, literal == "add")),
                ),
        )
    };

    let node = Builder::literal("worldborder")
        .requires(req)
        .executable(exec_worldborder_usage)
        .then(time_branch("add"))
        .then(time_branch("set"))
        .then(Builder::literal("center").then(
            Builder::arg("pos", tree::ArgKind::Vec2(false)).executable(exec_worldborder_center),
        ))
        .then(
            Builder::literal("damage")
                .then(
                    Builder::literal("amount").then(
                        Builder::arg("damagePerBlock", tree::ArgKind::Float)
                            .executable(exec_worldborder_damage_amount),
                    ),
                )
                .then(
                    Builder::literal("buffer").then(
                        Builder::arg("distance", tree::ArgKind::Float)
                            .executable(exec_worldborder_damage_buffer),
                    ),
                ),
        )
        .then(Builder::literal("get").executable(exec_worldborder_get))
        .then(
            Builder::literal("warning")
                .then(
                    Builder::literal("distance").then(
                        Builder::arg("distance", tree::ArgKind::Integer)
                            .executable(exec_worldborder_warning_distance),
                    ),
                )
                .then(
                    Builder::literal("time").then(
                        Builder::arg("time", tree::ArgKind::Time)
                            .executable(exec_worldborder_warning_time),
                    ),
                ),
        );

    tree::with_usage(
        node.build(),
        "<add|set> <distance> [<time>] | center <pos> | damage <amount|buffer> <value> | get | warning <distance|time> <value>",
    )
}

fn setworldspawn_node() -> Node {
    Builder::literal("setworldspawn")
        .requires(node_requirement(2, "minecraft.command.setworldspawn"))
        .executable(exec_setworldspawn)
        .then(
            Builder::arg("pos", tree::ArgKind::BlockPos)
                .executable(exec_setworldspawn)
                .then(Builder::arg("angle", tree::ArgKind::Angle).executable(exec_setworldspawn)),
        )
        .build()
}

fn defaultgamemode_node() -> Node {
    Builder::literal("defaultgamemode")
        .requires(node_requirement(2, "minecraft.command.defaultgamemode"))
        .executable(exec_defaultgamemode_usage)
        .then(Builder::arg("gamemode", tree::ArgKind::Gamemode).executable(exec_defaultgamemode))
        .build()
}

/// Every CMD-13a command, for the command tree.
pub fn nodes() -> Vec<Node> {
    vec![
        time_node(),
        weather_node(),
        worldborder_node(),
        setworldspawn_node(),
        defaultgamemode_node(),
    ]
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

fn exec_time_add(ctx: &mut Ctx) -> Reply {
    let Some(amount) = int_argument(ctx) else {
        return ctx.fail("/time add <time>");
    };
    if amount < 0 {
        return ctx.fail(lang::format(
            lang::ARG_TIME_TOO_LOW,
            &["0", &amount.to_string()],
        ));
    }
    let after = game::daylight_ticks().saturating_add(amount);
    game::set_time_of_day(after);
    ctx.registry.broadcast(&game::time_event(), ctx.world);
    // Vanilla answers the new *daytime*, modulo 24,000, even though the
    // absolute daylight counter keeps the day.
    Reply::one(lang::format(
        lang::TIME_SET,
        &[&after.rem_euclid(24_000).to_string()],
    ))
}

fn exec_time_set_named(ctx: &mut Ctx, value: i64) -> Reply {
    game::set_time_of_day(value);
    ctx.registry.broadcast(&game::time_event(), ctx.world);
    Reply::one(lang::format(lang::TIME_SET, &[&value.to_string()]))
}

fn exec_time_set_argument(ctx: &mut Ctx) -> Reply {
    let Some(value) = int_argument(ctx) else {
        return ctx.fail("/time set <day|noon|night|midnight|<time>>");
    };
    if value < 0 {
        return ctx.fail(lang::format(
            lang::ARG_TIME_TOO_LOW,
            &["0", &value.to_string()],
        ));
    }
    game::set_time_of_day(value);
    ctx.registry.broadcast(&game::time_event(), ctx.world);
    Reply::one(lang::format(lang::TIME_SET, &[&value.to_string()]))
}

fn exec_time_query(_ctx: &mut Ctx) -> Reply {
    Reply::one(query_time("daytime"))
}

fn exec_time_query_named(_ctx: &mut Ctx, which: &str) -> Reply {
    Reply::one(query_time(which))
}

fn query_time(which: &str) -> String {
    // Vanilla returns `gametime` modulo `2^31 - 1`; `day` is the daylight
    // cycle divided by 24,000, not the age of the world.
    let value = match which {
        "gametime" => (game::now() as i64).rem_euclid(2_147_483_647),
        "day" => game::day(),
        _ => game::time_of_day(),
    };
    lang::format(lang::TIME_QUERY, &[&value.to_string()])
}

// ---------------------------------------------------------------------------
// Weather
// ---------------------------------------------------------------------------

/// The weather the world is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weather {
    /// No rain or thunder.
    Clear,
    /// Rain (or snow in a cold biome).
    Rain,
    /// A thunderstorm, which is also rain.
    Thunder,
}

impl Weather {
    /// The `en_us` success message for setting this weather.
    pub fn success(self) -> &'static str {
        match self {
            Weather::Clear => lang::WEATHER_CLEAR,
            Weather::Rain => lang::WEATHER_RAIN,
            Weather::Thunder => lang::WEATHER_THUNDER,
        }
    }

    /// The vanilla command word.
    pub fn as_str(self) -> &'static str {
        match self {
            Weather::Clear => "clear",
            Weather::Rain => "rain",
            Weather::Thunder => "thunder",
        }
    }
}

/// The live weather and the ticks left before it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeatherState {
    /// The current weather.
    pub weather: Weather,
    /// Ticks remaining; `0` means no timed change is pending.
    pub remaining: i64,
}

impl Default for WeatherState {
    fn default() -> WeatherState {
        WeatherState {
            weather: Weather::Clear,
            remaining: 0,
        }
    }
}

static WEATHER: OnceLock<Mutex<WeatherState>> = OnceLock::new();

fn weather_cell() -> &'static Mutex<WeatherState> {
    WEATHER.get_or_init(|| Mutex::new(WeatherState::default()))
}

/// The current weather, for the tick and for diagnostics.
pub fn weather_state() -> WeatherState {
    *weather_cell().lock().unwrap_or_else(|e| e.into_inner())
}

/// Vanilla's omitted-duration ranges, narrowed by uptime so the value is
/// deterministic and still inside the right range.
fn default_weather_duration(weather: Weather) -> i64 {
    // Vanilla's ranges: clear 12,000..=180,000, rain 12,000..=24,000,
    // thunder 3,600..=15,600. Uptime makes the pick deterministic (and still
    // inside the range) instead of drawing from the world RNG, which the tick
    // owns.
    let n = (game::now() % 168_001) as i64;
    match weather {
        Weather::Clear => 12_000 + n,
        Weather::Rain => 12_000 + n % 12_001,
        Weather::Thunder => 3_600 + n % 12_001,
    }
}

/// The duration a weather change gets: omitted means vanilla's default range,
/// explicit is used as-is (the command's `time(1)` argument already bounds it
/// to an `int` and rejects anything below one tick).
fn weather_duration(weather: Weather, duration: Option<i64>) -> i64 {
    duration.unwrap_or_else(|| default_weather_duration(weather))
}

/// Set the weather. An omitted duration uses vanilla's per-type default range.
pub fn set_weather(weather: Weather, duration: Option<i64>) -> WeatherState {
    let state = WeatherState {
        weather,
        remaining: weather_duration(weather, duration),
    };
    *weather_cell().lock().unwrap_or_else(|e| e.into_inner()) = state;
    state
}

fn exec_weather_usage(ctx: &mut Ctx) -> Reply {
    ctx.fail("/weather <clear|rain|thunder> [<duration>]")
}

fn exec_weather(ctx: &mut Ctx, weather: Weather) -> Reply {
    let duration = if !ctx.args.is_empty() {
        let Some(ticks) = int_argument(ctx) else {
            return ctx.fail("/weather <clear|rain|thunder> [<duration>]");
        };
        if ticks < 1 {
            return ctx.fail(lang::format(
                lang::ARG_TIME_TOO_LOW,
                &["1", &ticks.to_string()],
            ));
        }
        Some(ticks)
    } else {
        None
    };
    let state = set_weather(weather, duration);
    broadcast_weather(ctx.registry, ctx.world, state);
    Reply::one(weather.success())
}

/// Tell every client the weather state.
fn broadcast_weather(registry: &SharedRegistry, world: &DemoWorld, state: WeatherState) {
    registry.broadcast(
        &crate::protocol::ServerEvent::Weather {
            rain: state.weather != Weather::Clear,
            thunder: state.weather == Weather::Thunder,
        },
        world,
    );
}

// ---------------------------------------------------------------------------
// World border
// ---------------------------------------------------------------------------

/// The live world border: size, centre, damage and warning settings.
///
/// A `set`/`add` with a time starts a linear interpolation, advanced by
/// [`WorldBorder::tick`] once per game tick; without a time the size changes
/// immediately. The engine keeps the state here rather than in the world
/// store, because CMD-13a's border is runtime-only.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldBorder {
    /// Centre X.
    pub center_x: f64,
    /// Centre Z.
    pub center_z: f64,
    /// Current diameter.
    pub size: f64,
    /// The diameter a running interpolation is heading for.
    pub target_size: f64,
    size_from: f64,
    lerp_ticks: u32,
    lerp_total: u32,
    /// Damage per block past the buffer, every second.
    pub damage_per_block: f64,
    /// Blocks past the border before damage starts.
    pub damage_buffer: f64,
    /// Blocks from the border where the screen tint starts.
    pub warning_distance: i32,
    /// Ticks before a moving border reaches the player. Vanilla keeps this
    /// in ticks and sends it as ticks, even though the command names seconds.
    pub warning_time: i32,
}

impl Default for WorldBorder {
    fn default() -> WorldBorder {
        WorldBorder {
            center_x: 0.0,
            center_z: 0.0,
            size: WORLD_BORDER_MAX,
            target_size: WORLD_BORDER_MAX,
            size_from: WORLD_BORDER_MAX,
            lerp_ticks: 0,
            lerp_total: 0,
            damage_per_block: 0.2,
            damage_buffer: 5.0,
            warning_distance: 5,
            warning_time: 300,
        }
    }
}

impl WorldBorder {
    /// Whether a timed resize is still running.
    pub fn resizing(&self) -> bool {
        self.lerp_ticks > 0
    }

    /// Ticks left in a running resize; `0` when idle.
    pub fn lerp_ticks(&self) -> i64 {
        i64::from(self.lerp_ticks)
    }

    /// The distance outside the border, positive only for a point beyond it.
    ///
    /// Vanilla's `WorldBorder.getDistanceToBorder` uses the larger of the two
    /// axis distances from the centre, because the border is square.
    pub fn outside_distance(&self, x: f64, z: f64) -> f64 {
        let half = self.size / 2.0;
        (x - self.center_x)
            .abs()
            .max((z - self.center_z).abs())
            .max(0.0)
            - half
    }

    /// Advance a running resize by one tick.
    pub fn tick(&mut self) {
        if self.lerp_ticks == 0 {
            return;
        }
        self.lerp_ticks -= 1;
        let elapsed = f64::from(self.lerp_total - self.lerp_ticks);
        let total = f64::from(self.lerp_total);
        self.size = self.size_from + (self.target_size - self.size_from) * (elapsed / total);
        if self.lerp_ticks == 0 {
            self.size = self.target_size;
        }
    }

    /// Set the diameter, over `ticks` (0 = now). The argument is a
    /// `TimeArgumentType`, so it is already in ticks.
    pub fn set_size(&mut self, size: f64, ticks: i64) {
        let ticks = ticks.max(0).min(i64::from(u32::MAX)) as u32;
        if ticks == 0 {
            self.size = size;
            self.target_size = size;
            self.size_from = size;
            self.lerp_ticks = 0;
            self.lerp_total = 0;
        } else {
            self.size_from = self.size;
            self.target_size = size;
            self.lerp_ticks = ticks;
            self.lerp_total = ticks;
        }
    }
}

/// Why a world-border edit was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BorderError {
    /// The requested size is below one block.
    TooSmall,
    /// The requested size is larger than the vanilla maximum.
    TooBig,
    /// The requested centre is outside ±29,999,984.
    TooFar,
    /// The edit would change nothing.
    NoChange,
}

impl BorderError {
    /// The vanilla chat line for this refusal.
    pub fn message(&self) -> String {
        match self {
            BorderError::TooSmall => lang::format(lang::WORLDBORDER_FAILED_SMALL, &[]),
            BorderError::TooBig => lang::format(
                lang::WORLDBORDER_FAILED_BIG,
                &[&java_float(WORLD_BORDER_MAX as f32)],
            ),
            BorderError::TooFar => lang::format(
                lang::WORLDBORDER_FAILED_FAR,
                &[&java_double(WORLD_BORDER_CENTER_MAX)],
            ),
            BorderError::NoChange => lang::WORLDBORDER_SET_NOCHANGE.into(),
        }
    }
}

static BORDER: OnceLock<Mutex<WorldBorder>> = OnceLock::new();

fn border_cell() -> &'static Mutex<WorldBorder> {
    BORDER.get_or_init(|| Mutex::new(WorldBorder::default()))
}

/// A snapshot of the live border.
pub fn world_border() -> WorldBorder {
    border_cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Reset the live border to its vanilla defaults. Tests use it so they do not
/// depend on each other's order.
#[cfg(test)]
pub(crate) fn reset_world_border() {
    *border_cell().lock().unwrap_or_else(|e| e.into_inner()) = WorldBorder::default();
}

/// Advance the live border one tick (the game tick calls this).
pub fn tick_border() {
    border_cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .tick();
}

fn exec_worldborder_set(ctx: &mut Ctx, add: bool) -> Reply {
    let Some(distance) = float_argument(ctx) else {
        return ctx.fail(if add {
            "/worldborder add <distance> [<time>]"
        } else {
            "/worldborder set <distance> [<time>]"
        });
    };
    let requested_ticks = if ctx.args.len() > 1 {
        int_argument(ctx).unwrap_or(0)
    } else {
        0
    };
    if requested_ticks < 0 {
        return ctx.fail(lang::format(
            lang::ARG_TIME_TOO_LOW,
            &["0", &requested_ticks.to_string()],
        ));
    }
    // The argument itself is bounded like vanilla's `doubleArg(-max, max)`: an
    // out-of-range distance is an argument error, not a border-size error.
    if distance < -WORLD_BORDER_MAX {
        return ctx.fail(lang::format(
            lang::ARG_DOUBLE_LOW,
            &[&java_double(-WORLD_BORDER_MAX), &java_double(distance)],
        ));
    }
    if distance > WORLD_BORDER_MAX {
        return ctx.fail(lang::format(
            lang::ARG_DOUBLE_BIG,
            &[&java_double(WORLD_BORDER_MAX), &java_double(distance)],
        ));
    }
    let (message, event) = {
        let mut border = border_cell().lock().unwrap_or_else(|e| e.into_inner());
        let current = border.size;
        let target = if add { current + distance } else { distance };
        if target == current {
            return ctx.fail(BorderError::NoChange.message());
        }
        if target < WORLD_BORDER_MIN {
            return ctx.fail(BorderError::TooSmall.message());
        }
        if target > WORLD_BORDER_MAX {
            return ctx.fail(BorderError::TooBig.message());
        }
        // Vanilla's `add` extends the resize already in flight, so the new
        // lerp lasts `getLerpTime() + time`; `set` replaces it.
        let ticks = if add && requested_ticks > 0 {
            border.lerp_ticks().saturating_add(requested_ticks)
        } else {
            requested_ticks
        }
        // The border stores its lerp in a `u32`; clamp here too so the packet
        // cannot carry a speed the client would run forever.
        .min(i64::from(u32::MAX));
        let seconds = ticks as f64 / 20.0;
        let message = if ticks == 0 {
            lang::format(lang::WORLDBORDER_SET_IMMEDIATE, &[&fmt_one(target)])
        } else if target > current {
            lang::format(
                lang::WORLDBORDER_SET_GROW,
                &[&fmt_one(target), &fmt_two(seconds)],
            )
        } else {
            lang::format(
                lang::WORLDBORDER_SET_SHRINK,
                &[&fmt_one(target), &fmt_two(seconds)],
            )
        };
        border.set_size(target, ticks);
        // An immediate resize is vanilla's Set Border Size (it leaves the
        // warning settings and any lerp alone); a timed one needs the full
        // Initialize payload.
        let event = if ticks == 0 {
            crate::protocol::ServerEvent::WorldBorderSize { diameter: target }
        } else {
            crate::protocol::ServerEvent::WorldBorder {
                x: border.center_x,
                z: border.center_z,
                old_diameter: current,
                new_diameter: target,
                speed: ticks,
                warning_blocks: border.warning_distance,
                warning_time: border.warning_time,
            }
        };
        (message, event)
    };
    ctx.registry.broadcast(&event, ctx.world);
    Reply::one(message)
}

fn exec_worldborder_usage(ctx: &mut Ctx) -> Reply {
    ctx.fail("/worldborder <add|set|center|damage|get|warning> ...")
}

fn exec_worldborder_get(_ctx: &mut Ctx) -> Reply {
    let border = world_border();
    Reply::one(lang::format(
        lang::WORLDBORDER_GET,
        &[&fmt_zero(border.size)],
    ))
}

fn exec_worldborder_center(ctx: &mut Ctx) -> Reply {
    let Some(coords) = vec2_argument(ctx) else {
        return ctx.fail("/worldborder center <pos>");
    };
    let (x, z) = match ctx.player() {
        Some(player) => match coords.resolve(player as &dyn CoordinateSource) {
            Ok(pair) => pair,
            Err(e) => return ctx.fail(e.message()),
        },
        None => match coords.resolve(&NoCoordinates) {
            Ok(pair) => pair,
            Err(e) => return ctx.fail(e.message()),
        },
    };
    if x.abs() > WORLD_BORDER_CENTER_MAX || z.abs() > WORLD_BORDER_CENTER_MAX {
        return ctx.fail(BorderError::TooFar.message());
    }
    let event = {
        let mut border = border_cell().lock().unwrap_or_else(|e| e.into_inner());
        if border.center_x == x && border.center_z == z {
            return ctx.fail(lang::WORLDBORDER_CENTER_FAILED);
        }
        border.center_x = x;
        border.center_z = z;
        crate::protocol::ServerEvent::WorldBorderCenter { x, z }
    };
    ctx.registry.broadcast(&event, ctx.world);
    Reply::one(lang::format(
        lang::WORLDBORDER_CENTER_SUCCESS,
        &[&fmt_two(x), &fmt_two(z)],
    ))
}

fn exec_worldborder_damage_amount(ctx: &mut Ctx) -> Reply {
    let Some(value) = float_argument(ctx) else {
        return ctx.fail("/worldborder damage amount <damagePerBlock>");
    };
    if value < 0.0 {
        return ctx.fail(lang::format(
            lang::ARG_FLOAT_LOW,
            &["0.0", &fmt_float(value)],
        ));
    }
    {
        let mut border = border_cell().lock().unwrap_or_else(|e| e.into_inner());
        if border.damage_per_block == value {
            return ctx.fail(lang::WORLDBORDER_DAMAGE_AMOUNT_FAILED);
        }
        border.damage_per_block = value;
    }
    Reply::one(lang::format(
        lang::WORLDBORDER_DAMAGE_AMOUNT_SUCCESS,
        &[&fmt_two(value)],
    ))
}

fn exec_worldborder_damage_buffer(ctx: &mut Ctx) -> Reply {
    let Some(value) = float_argument(ctx) else {
        return ctx.fail("/worldborder damage buffer <distance>");
    };
    if value < 0.0 {
        return ctx.fail(lang::format(
            lang::ARG_FLOAT_LOW,
            &["0.0", &fmt_float(value)],
        ));
    }
    {
        let mut border = border_cell().lock().unwrap_or_else(|e| e.into_inner());
        if border.damage_buffer == value {
            return ctx.fail(lang::WORLDBORDER_DAMAGE_BUFFER_FAILED);
        }
        border.damage_buffer = value;
    }
    Reply::one(lang::format(
        lang::WORLDBORDER_DAMAGE_BUFFER_SUCCESS,
        &[&fmt_two(value)],
    ))
}

fn exec_worldborder_warning_distance(ctx: &mut Ctx) -> Reply {
    let Some(value) = int_argument(ctx) else {
        return ctx.fail("/worldborder warning distance <distance>");
    };
    if value < 0 {
        return ctx.fail(lang::format(
            lang::ARG_INTEGER_LOW,
            &["0", &value.to_string()],
        ));
    }
    let value = value as i32;
    let event = {
        let mut border = border_cell().lock().unwrap_or_else(|e| e.into_inner());
        if border.warning_distance == value {
            return ctx.fail(lang::WORLDBORDER_WARNING_DISTANCE_FAILED);
        }
        border.warning_distance = value;
        crate::protocol::ServerEvent::WorldBorderWarningDistance { blocks: value }
    };
    ctx.registry.broadcast(&event, ctx.world);
    Reply::one(lang::format(
        lang::WORLDBORDER_WARNING_DISTANCE_SUCCESS,
        &[&value.to_string()],
    ))
}

fn exec_worldborder_warning_time(ctx: &mut Ctx) -> Reply {
    let Some(ticks) = int_argument(ctx) else {
        return ctx.fail("/worldborder warning time <time>");
    };
    if ticks < 0 {
        return ctx.fail(lang::format(
            lang::ARG_TIME_TOO_LOW,
            &["0", &ticks.to_string()],
        ));
    }
    if ticks > i64::from(i32::MAX) {
        return ctx.fail(format!(
            "Integer must not be more than {}: found {ticks}",
            i32::MAX
        ));
    }
    let ticks = ticks as i32;
    let event = {
        let mut border = border_cell().lock().unwrap_or_else(|e| e.into_inner());
        if border.warning_time == ticks {
            return ctx.fail(lang::WORLDBORDER_WARNING_TIME_FAILED);
        }
        border.warning_time = ticks;
        crate::protocol::ServerEvent::WorldBorderWarningTime { ticks }
    };
    ctx.registry.broadcast(&event, ctx.world);
    Reply::one(lang::format(
        lang::WORLDBORDER_WARNING_TIME_SUCCESS,
        &[&fmt_two(f64::from(ticks) / 20.0)],
    ))
}

fn exec_setworldspawn(ctx: &mut Ctx) -> Reply {
    let (x, y, z, yaw) = if ctx.args.is_empty() {
        let Some(player) = ctx.player() else {
            return ctx.fail(CoordError::NoPosition.message());
        };
        let pos = player.pos();
        let yaw = angle_argument(ctx, Some(player as &dyn CoordinateSource))
            .unwrap_or(None)
            .unwrap_or(0.0);
        (
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
            yaw,
        )
    } else {
        let Some(block) = block_pos_argument(ctx) else {
            return ctx.fail("/setworldspawn [<pos>] [<angle>]");
        };
        let (x, y, z) = match ctx.player() {
            Some(player) => match block.resolve_block(player as &dyn CoordinateSource) {
                Ok(p) => (p.x, p.y, p.z),
                Err(e) => return ctx.fail(e.message()),
            },
            None => {
                if block.is_local() || block.is_relative() {
                    return ctx.fail(CoordError::NoPosition.message());
                }
                match block.resolve_block(&NoCoordinates) {
                    Ok(p) => (p.x, p.y, p.z),
                    Err(e) => return ctx.fail(e.message()),
                }
            }
        };
        let yaw = match angle_argument(ctx, ctx.player().map(|p| p as &dyn CoordinateSource)) {
            Ok(angle) => angle.unwrap_or(0.0),
            Err(e) => return ctx.fail(e.message()),
        };
        (x, y, z, yaw)
    };
    // Vanilla's `setworldspawn` position bounds: x/z in [-30,000,000,
    // 30,000,000), y in [-20,000,000, 20,000,000).
    if !(-30_000_000..30_000_000).contains(&x)
        || !(-30_000_000..30_000_000).contains(&z)
        || !(-20_000_000..20_000_000).contains(&y)
    {
        return ctx.fail(lang::POS_OUT_OF_BOUNDS);
    }
    game::set_world_spawn(x, y, z, yaw);
    ctx.registry.broadcast(
        &crate::protocol::ServerEvent::SpawnPosition { x, y, z, yaw },
        ctx.world,
    );
    Reply::one(lang::format(
        lang::SETWORLDSPAWN_SUCCESS,
        &[
            &x.to_string(),
            &y.to_string(),
            &z.to_string(),
            &fmt_angle(yaw),
        ],
    ))
}

fn exec_defaultgamemode_usage(ctx: &mut Ctx) -> Reply {
    ctx.fail("/defaultgamemode <gamemode>")
}

fn exec_defaultgamemode(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_argument(ctx) else {
        return ctx.fail("/defaultgamemode <gamemode>");
    };
    let Some(mode) = parse_game_mode(name) else {
        return ctx.fail(format!("Unknown game mode '{name}'"));
    };
    game::set_default_game_mode(mode);
    Reply::one(lang::format(
        lang::DEFAULTGAMEMODE_SUCCESS,
        &[game_mode_name(mode)],
    ))
}

fn parse_game_mode(name: &str) -> Option<GameMode> {
    match name {
        "survival" => Some(GameMode::Survival),
        "creative" => Some(GameMode::Creative),
        "adventure" => Some(GameMode::Adventure),
        "spectator" => Some(GameMode::Spectator),
        _ => None,
    }
}

fn game_mode_name(mode: GameMode) -> &'static str {
    match mode {
        GameMode::Survival => lang::GAMEMODE_SURVIVAL,
        GameMode::Creative => lang::GAMEMODE_CREATIVE,
        GameMode::Adventure => lang::GAMEMODE_ADVENTURE,
        GameMode::Spectator => lang::GAMEMODE_SPECTATOR,
    }
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn int_argument(ctx: &Ctx) -> Option<i64> {
    ctx.args.iter().rev().find_map(|arg| match &arg.value {
        tree::Value::Int(v) => Some(i64::from(*v)),
        tree::Value::Long(v) => Some(*v),
        _ => None,
    })
}

fn float_argument(ctx: &Ctx) -> Option<f64> {
    ctx.args.iter().rev().find_map(|arg| match &arg.value {
        tree::Value::Float(v) => Some(f64::from(*v)),
        tree::Value::Double(v) => Some(*v),
        _ => None,
    })
}

fn text_argument<'a>(ctx: &'a Ctx<'_, '_>) -> Option<&'a str> {
    ctx.args.iter().rev().find_map(|arg| match &arg.value {
        tree::Value::Text(v) => Some(v.as_str()),
        _ => None,
    })
}

fn block_pos_argument(ctx: &Ctx) -> Option<super::coordinates::Coordinates> {
    ctx.args.iter().find_map(|arg| match &arg.value {
        tree::Value::Coordinates(c) => Some(*c),
        _ => None,
    })
}

fn vec2_argument(ctx: &Ctx) -> Option<super::coordinates::Coordinates2> {
    ctx.args.iter().find_map(|arg| match &arg.value {
        tree::Value::Vec2(c) => Some(*c),
        _ => None,
    })
}

fn angle_argument(
    ctx: &Ctx,
    source: Option<&dyn CoordinateSource>,
) -> Result<Option<f32>, CoordError> {
    let Some(coord) = ctx.args.iter().rev().find_map(|arg| match &arg.value {
        tree::Value::Angle(c) => Some(*c),
        _ => None,
    }) else {
        return Ok(None);
    };
    let resolved = match source {
        Some(source) => super::coordinates::resolve_angle(coord, source)?,
        None => super::coordinates::resolve_angle(coord, &NoCoordinates)?,
    };
    Ok(Some(resolved))
}

/// A source with no position or rotation, for absolute-only coordinates from
/// the console or RCON.
struct NoCoordinates;

impl CoordinateSource for NoCoordinates {
    fn position(&self) -> Option<Vector3> {
        None
    }

    fn rotation(&self) -> Option<super::coordinates::Rotation> {
        None
    }
}

/// Vanilla's `%.1f`: the `set`/grow/shrink message.
fn fmt_one(value: f64) -> String {
    format!("{value:.1}")
}

/// Vanilla's `%.2f`: the centre, damage and timed-border messages.
fn fmt_two(value: f64) -> String {
    format!("{value:.2}")
}

/// Vanilla's `%.0f`, rounded half-up. Rust rounds half-to-even, so `x.5` is
/// nudged before formatting to match Java.
fn fmt_zero(value: f64) -> String {
    format!("{}", (value + 0.5).floor())
}

/// A Java `Float.toString` argument: `-1.0`, not `-1`.
fn fmt_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// Java's `Double.toString`, which Brigadier's numeric errors use: plain
/// decimal in `[1e-3, 1e7)`, scientific (`5.9999968E7`, `6.0E7`) outside it.
fn java_double(value: f64) -> String {
    java_scientific(value, &format!("{value:e}"), fmt_float)
}

/// Java's `Float.toString`, used where vanilla passes a `float` (the
/// world-border size ceiling): `5.999997E7`.
fn java_float(value: f32) -> String {
    let shortest = format!("{value:e}");
    java_scientific(f64::from(value), &shortest, fmt_float)
}

/// Format a value in Java's scientific style: a mantissa that always carries
/// a fractional digit (`6` becomes `6.0`) and an unsigned exponent (`E7`,
/// `E-4`).
fn java_scientific(value: f64, shortest: &str, plain: fn(f64) -> String) -> String {
    let abs = value.abs();
    if abs == 0.0 || (1e-3..1e7).contains(&abs) {
        return plain(value);
    }
    let (mantissa, exponent) = shortest.split_once('e').unwrap_or((shortest, "0"));
    let mantissa = if mantissa.contains('.') {
        mantissa.to_string()
    } else {
        format!("{mantissa}.0")
    };
    format!("{mantissa}E{exponent}")
}

/// A Java `Float.toString`-style angle: `0.0`, not `0`.
fn fmt_angle(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

// ---------------------------------------------------------------------------
// The game tick
// ---------------------------------------------------------------------------

/// Advance the runtime environment and hurt players outside the border.
///
/// Called once per tick from [`crate::game::tick`]; the damage pass runs every
/// twenty ticks, which is vanilla's once-a-second schedule.
pub fn tick_world(registry: &SharedRegistry, world: &DemoWorld) {
    tick_border();
    let cleared = {
        let mut weather = weather_cell().lock().unwrap_or_else(|e| e.into_inner());
        let mut cleared = false;
        if weather.remaining > 0 {
            weather.remaining -= 1;
            if weather.remaining == 0 && weather.weather != Weather::Clear {
                weather.weather = Weather::Clear;
                cleared = true;
            }
        }
        cleared
    };
    if cleared {
        broadcast_weather(registry, world, weather_state());
    }
    if game::now() % 20 != 0 {
        return;
    }
    let border = world_border();
    if border.damage_per_block <= 0.0 {
        return;
    }
    for player in registry.snapshot() {
        let pos = player.pos();
        let outside = border.outside_distance(pos.x, pos.z) - border.damage_buffer;
        if outside > 0.0 {
            let amount = (border.damage_per_block * outside) as f32;
            crate::game::combat::hurt_player(
                &player,
                amount,
                "minecraft:outside_border",
                None,
                registry,
                world,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_border_grows_and_shrinks_over_time_in_ticks() {
        let mut border = WorldBorder::default();
        border.set_size(100.0, 0);
        assert_eq!(border.size, 100.0);
        // 200 ticks = 10 seconds, as the TimeArgumentType says.
        border.set_size(200.0, 200);
        assert!(border.resizing());
        for _ in 0..100 {
            border.tick();
        }
        assert!((border.size - 150.0).abs() < 1e-9, "{}", border.size);
        for _ in 0..100 {
            border.tick();
        }
        assert_eq!(border.size, 200.0);
        assert!(!border.resizing());

        // One second is 20 ticks, not one tick.
        border.set_size(50.0, 20);
        for _ in 0..20 {
            border.tick();
        }
        assert_eq!(border.size, 50.0);
    }

    #[test]
    fn outside_damage_is_measured_from_the_nearer_square_edge() {
        let mut border = WorldBorder::default();
        border.set_size(100.0, 0);
        // Inside, exactly on the edge, and outside.
        assert!(border.outside_distance(0.0, 0.0) < 0.0);
        assert_eq!(border.outside_distance(50.0, 0.0), 0.0);
        assert_eq!(border.outside_distance(55.0, 0.0), 5.0);
        // The larger axis wins: a corner is further out than either side.
        assert_eq!(border.outside_distance(60.0, 70.0), 20.0);
    }

    #[test]
    fn the_border_defaults_are_vanillas() {
        let border = WorldBorder::default();
        assert_eq!(border.size, 59_999_968.0);
        assert_eq!(border.center_x, 0.0);
        assert_eq!(border.center_z, 0.0);
        assert_eq!(border.damage_per_block, 0.2);
        assert_eq!(border.damage_buffer, 5.0);
        assert_eq!(border.warning_distance, 5);
        assert_eq!(border.warning_time, 300);
    }

    #[test]
    fn omitted_weather_durations_are_in_vanillas_ranges() {
        for (weather, low, high) in [
            (Weather::Clear, 12_000, 180_000),
            (Weather::Rain, 12_000, 24_000),
            (Weather::Thunder, 3_600, 15_600),
        ] {
            let d = default_weather_duration(weather);
            assert!((low..=high).contains(&d), "{weather:?}: {d}");
        }
    }

    #[test]
    fn an_explicit_weather_duration_is_used_as_given() {
        // Vanilla has no upper clamp beyond the `int` argument itself.
        assert_eq!(weather_duration(Weather::Rain, Some(2_000_000)), 2_000_000);
        assert_eq!(weather_duration(Weather::Thunder, Some(200)), 200);
    }

    #[test]
    fn border_numbers_use_vanillas_decimal_places() {
        assert_eq!(fmt_one(100.0), "100.0");
        assert_eq!(fmt_two(10.0), "10.00");
        assert_eq!(fmt_two(0.5), "0.50");
        assert_eq!(fmt_zero(99.6), "100");
        assert_eq!(fmt_zero(100.5), "101", "Java rounds half up");
        assert_eq!(fmt_float(-1.0), "-1.0");
    }

    #[test]
    fn argument_errors_use_java_double_to_string() {
        assert_eq!(java_double(59_999_968.0), "5.9999968E7");
        assert_eq!(java_double(29_999_984.0), "2.9999984E7");
        assert_eq!(java_double(-59_999_968.0), "-5.9999968E7");
        assert_eq!(java_double(0.5), "0.5");
        assert_eq!(java_double(1.0), "1.0");
        assert_eq!(java_double(0.0001), "1.0E-4");
        assert_eq!(java_double(6.0e7), "6.0E7");
        assert_eq!(java_float(59_999_968.0), "5.999997E7");
        assert_eq!(java_float(60_000_000.0), "6.0E7");
    }
}
