//! The one command catalogue.
//!
//! Every command Aether knows about — the ones that run today **and** the ones
//! that are only specified — is listed here exactly once, with its category,
//! permission node, op level and status. This is the single source of truth:
//! the command tree's pending half ([`crate::commands::tree`]) is built from
//! it, the Declare Commands packet and `/help` read it, and the generated docs
//! (`docs/COMMANDS.md`, `docs/COMMANDS.ru.md`) are rendered from it. There is
//! no second list — [`super::registry`] holds only the plugin side.
//!
//! ## One edit, one place
//!
//! * To **add a command**, add one entry to [`CATALOGUE`]. If it is planned
//!   but not written, give it [`Status::Pending`] and the `CMD-nn` task that
//!   will implement it; the tree declares it as a stub that replies
//!   `not implemented yet (CMD-nn)`, so it still completes and colours.
//! * To **flip a command to working**, change its entry's `status` to
//!   [`Status::Implemented`], set `task` to `-`, and wire the executor in the
//!   owning module's `nodes()` ([`crate::game::commands`],
//!   [`crate::economy::commands`] or [`super::core_nodes`]). The catalogue
//!   *lists* commands; it does not run them.
//!
//! A command must never appear twice: a name that is both implemented and
//! listed pending fails [`tests::no_name_is_both_implemented_and_pending`].
//!
//! ## The `gate` field
//!
//! The `node`/`op` are the intended vanilla permission. [`Gate`] records what
//! the tree enforces *today*, so an implemented command gated by the legacy
//! operator check (or not gated at all) is described honestly rather than
//! claiming a node it does not use;
//! [`tests::an_implemented_entry_gate_matches_the_tree`] keeps the two in step.

use std::fmt::Write as _;

/// Whether a command runs today.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// A real executor is wired into the command tree.
    Implemented,
    /// Specified in `docs/design/commands.md`; the tree answers
    /// `not implemented yet (CMD-nn)`.
    Pending,
}

/// A command's category: the small fixed set used for `/help` grouping and the
/// generated docs. Every catalogue entry has exactly one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    /// Player and access management (op, ban, whitelist, kick, help).
    Admin,
    /// Server control and information (stop, save, list, seed, version).
    Server,
    /// Player verbs: inventory, movement, effects, item and entity actions.
    Player,
    /// Blocks, environment and world rules (setblock, fill, weather, time).
    World,
    /// Data, scoreboard, teams and chat (data, scoreboard, say, tellraw).
    Data,
    /// Server diagnostics and lifecycle (tps, gc, plugins, aether, debug).
    Ops,
    /// World history and protection (inspect, lookup, rollback, restore).
    History,
    /// The economy (balance, pay, market, econ).
    Economy,
}

impl Category {
    /// Every category, in the order the docs list them.
    pub const ALL: &'static [Category] = &[
        Category::Admin,
        Category::Server,
        Category::Player,
        Category::World,
        Category::Data,
        Category::Ops,
        Category::History,
        Category::Economy,
    ];

    /// The short key (`admin`) — the `/help` grouping and the docs anchor.
    pub const fn key(self) -> &'static str {
        match self {
            Category::Admin => "admin",
            Category::Server => "server",
            Category::Player => "player",
            Category::World => "world",
            Category::Data => "data",
            Category::Ops => "ops",
            Category::History => "history",
            Category::Economy => "economy",
        }
    }

    /// The category's one-line English description for the docs.
    pub const fn blurb_en(self) -> &'static str {
        match self {
            Category::Admin => "Player and access management.",
            Category::Server => "Server control and information.",
            Category::Player => "Player verbs: inventory, movement, effects and items.",
            Category::World => "Blocks, environment and world rules.",
            Category::Data => "Data, scoreboard, teams and chat.",
            Category::Ops => "Server diagnostics and lifecycle.",
            Category::History => "World history and protection.",
            Category::Economy => "The economy.",
        }
    }

    /// The category's one-line Russian description for the docs.
    pub const fn blurb_ru(self) -> &'static str {
        match self {
            Category::Admin => "Управление игроками и доступом.",
            Category::Server => "Управление сервером и информация о нём.",
            Category::Player => "Игрок: инвентарь, перемещение, эффекты и предметы.",
            Category::World => "Блоки, окружение и правила мира.",
            Category::Data => "Данные, счётная доска, команды и чат.",
            Category::Ops => "Диагностика и жизненный цикл сервера.",
            Category::History => "История мира и защита.",
            Category::Economy => "Экономика.",
        }
    }
}

/// How a command's permission is enforced in the tree **today**.
///
/// The `node`/`op` on a [`Command`] are the intended vanilla permission. For a
/// pending command the stub enforces exactly them (`Gate::Node`); for an
/// implemented command the tree may still use an older check, in which case
/// `node`/`op` are the target and this field says what actually happens. The
/// `an_implemented_entry_gate_matches_the_tree` test keeps the two in step, so
/// the catalogue never claims an enforcement the tree does not perform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gate {
    /// No requirement: anyone may run it. The declared `node`/`op` are the
    /// intended target, not enforced.
    Everyone,
    /// Operators only, through the legacy operator check (`Requirement::Fn`,
    /// `is_operator`); the declared `node`/`op` are the intended target, not
    /// yet enforced as a permission node.
    Operator,
    /// Enforced by the permission node at the op level
    /// (`Requirement::Level { op, node }`).
    Node,
}

impl Gate {
    /// The short key shown in the generated docs.
    pub const fn key(self) -> &'static str {
        match self {
            Gate::Everyone => "everyone",
            Gate::Operator => "operator",
            Gate::Node => "node",
        }
    }
}

/// One command in the catalogue, working or not.
pub struct Command {
    /// Canonical command name, without the leading slash.
    pub name: &'static str,
    /// Alternative names the command answers to.
    pub aliases: &'static [&'static str],
    /// Permission node (see [`Gate`]: enforced when `gate` is
    /// [`Gate::Node`], else the intended target).
    pub node: &'static str,
    /// Vanilla op level required (0 everyone … 4 owner).
    pub op: u8,
    /// The `CMD-nn` task that implements it, or `-` when it is implemented.
    pub task: &'static str,
    /// The category it belongs to.
    pub category: Category,
    /// Implemented or pending.
    pub status: Status,
    /// How the permission is enforced in the tree today.
    pub gate: Gate,
    /// A one-line usage/description, without a leading slash.
    pub usage: &'static str,
}

/// An implemented command anyone may run (the tree has no requirement for it).
const fn imp(
    name: &'static str,
    aliases: &'static [&'static str],
    node: &'static str,
    op: u8,
    category: Category,
    usage: &'static str,
) -> Command {
    Command {
        name,
        aliases,
        node,
        op,
        task: "-",
        category,
        status: Status::Implemented,
        gate: Gate::Everyone,
        usage,
    }
}

/// An implemented command gated by its permission node/op
/// ([`Gate::Node`]).
const fn imp_node(
    name: &'static str,
    aliases: &'static [&'static str],
    node: &'static str,
    op: u8,
    category: Category,
    usage: &'static str,
) -> Command {
    Command {
        name,
        aliases,
        node,
        op,
        task: "-",
        category,
        status: Status::Implemented,
        gate: Gate::Node,
        usage,
    }
}

/// An implemented command held by the legacy operator check
/// ([`Gate::Operator`]) until its node/op is wired (CMD-05/CMD-01).
const fn imp_op(
    name: &'static str,
    aliases: &'static [&'static str],
    node: &'static str,
    op: u8,
    category: Category,
    usage: &'static str,
) -> Command {
    Command {
        name,
        aliases,
        node,
        op,
        task: "-",
        category,
        status: Status::Implemented,
        gate: Gate::Operator,
        usage,
    }
}

const fn pend(
    name: &'static str,
    aliases: &'static [&'static str],
    node: &'static str,
    op: u8,
    task: &'static str,
    category: Category,
    usage: &'static str,
) -> Command {
    Command {
        name,
        aliases,
        node,
        op,
        task,
        category,
        status: Status::Pending,
        // A pending command is a stub gated by its node/op.
        gate: Gate::Node,
        usage,
    }
}

/// Every command Aether knows, implemented and pending, grouped as the docs
/// list them (per category, in the order [`Category::ALL`] gives).
pub const CATALOGUE: &[Command] = &[
    // -- admin -------------------------------------------------------------
    imp(
        "help",
        &["?"],
        "minecraft.command.help",
        0,
        Category::Admin,
        "/help [command]",
    ),
    imp_node(
        "op",
        &[],
        "minecraft.command.op",
        3,
        Category::Admin,
        "/op <targets>",
    ),
    imp_node(
        "deop",
        &[],
        "minecraft.command.deop",
        3,
        Category::Admin,
        "/deop <targets>",
    ),
    imp_node(
        "ban",
        &[],
        "minecraft.command.ban",
        3,
        Category::Admin,
        "/ban <targets> [reason]",
    ),
    imp_node(
        "ban-ip",
        &[],
        "minecraft.command.ban-ip",
        3,
        Category::Admin,
        "/ban-ip <targets> [reason]",
    ),
    imp_node(
        "banlist",
        &[],
        "minecraft.command.banlist",
        3,
        Category::Admin,
        "/banlist [ips|players]",
    ),
    imp_node(
        "pardon",
        &[],
        "minecraft.command.pardon",
        3,
        Category::Admin,
        "/pardon <targets>",
    ),
    imp_node(
        "pardon-ip",
        &[],
        "minecraft.command.pardon-ip",
        3,
        Category::Admin,
        "/pardon-ip <targets>",
    ),
    imp_node(
        "kick",
        &[],
        "minecraft.command.kick",
        3,
        Category::Admin,
        "/kick <targets> [reason]",
    ),
    imp_node(
        "whitelist",
        &[],
        "minecraft.command.whitelist",
        3,
        Category::Admin,
        "/whitelist <on|off|list|add|remove|reload>",
    ),
    // -- server ------------------------------------------------------------
    // `/stop` and `/save-all` were first wired for the console and RCON
    // lifecycle (CMD-07); CMD-09 completes the group with the information and
    // transfer verbs. `/publish` is deliberately absent: 1.21.11's dedicated
    // server does not register the singleplayer integrated-server command, so
    // the tree must not either.
    imp_node(
        "stop",
        &[],
        "minecraft.command.stop",
        4,
        Category::Server,
        "/stop",
    ),
    imp_node(
        "save-all",
        &[],
        "minecraft.command.save-all",
        4,
        Category::Server,
        "/save-all [flush]",
    ),
    imp_node(
        "save-on",
        &[],
        "minecraft.command.save-on",
        4,
        Category::Server,
        "/save-on",
    ),
    imp_node(
        "save-off",
        &[],
        "minecraft.command.save-off",
        4,
        Category::Server,
        "/save-off",
    ),
    // Op level 0 is "everyone"; the tree has no requirement for it.
    imp(
        "list",
        &[],
        "minecraft.command.list",
        0,
        Category::Server,
        "/list [uuids]",
    ),
    imp_node(
        "seed",
        &[],
        "minecraft.command.seed",
        2,
        Category::Server,
        "/seed",
    ),
    imp_node(
        "version",
        &[],
        "minecraft.command.version",
        2,
        Category::Server,
        "/version",
    ),
    imp_node(
        "setidletimeout",
        &[],
        "minecraft.command.setidletimeout",
        3,
        Category::Server,
        "/setidletimeout <minutes>",
    ),
    imp_node(
        "transfer",
        &[],
        "minecraft.command.transfer",
        3,
        Category::Server,
        "/transfer <hostname> [port] [players]",
    ),
    // -- player ------------------------------------------------------------
    imp_op(
        "gamemode",
        &["gm"],
        "minecraft.command.gamemode",
        2,
        Category::Player,
        "/gamemode <mode> [player]",
    ),
    imp_op(
        "give",
        &[],
        "minecraft.command.give",
        2,
        Category::Player,
        "/give <item> [count]",
    ),
    imp_op(
        "summon",
        &[],
        "minecraft.command.summon",
        2,
        Category::Player,
        "/summon <entity> [pos]",
    ),
    imp_op(
        "killall",
        &[],
        "aether.command.killall",
        2,
        Category::Player,
        "/killall",
    ),
    imp(
        "kill",
        &[],
        "minecraft.command.kill",
        2,
        Category::Player,
        "/kill [targets]",
    ),
    imp(
        "spawn",
        &[],
        "aether.command.spawn",
        0,
        Category::Player,
        "/spawn",
    ),
    imp_op(
        "teleport",
        &["tp"],
        "minecraft.command.teleport",
        2,
        Category::Player,
        "/teleport <target>",
    ),
    imp(
        "block",
        &[],
        "aether.command.block",
        0,
        Category::Player,
        "/block <x> <y> <z>",
    ),
    imp_op(
        "food",
        &[],
        "aether.command.food",
        2,
        Category::Player,
        "/food [level]",
    ),
    imp_op(
        "heal",
        &[],
        "aether.command.heal",
        2,
        Category::Player,
        "/heal",
    ),
    imp(
        "inv",
        &["inventory"],
        "aether.command.inventory",
        0,
        Category::Player,
        "/inv",
    ),
    imp_node(
        "clear",
        &[],
        "minecraft.command.clear",
        2,
        Category::Player,
        "/clear [targets] [item] [max-count]",
    ),
    imp_node(
        "effect",
        &[],
        "minecraft.command.effect",
        2,
        Category::Player,
        "/effect <give|clear> <targets> <effect> [seconds|infinite] [amplifier] [hideParticles]",
    ),
    imp_node(
        "enchant",
        &[],
        "minecraft.command.enchant",
        2,
        Category::Player,
        "/enchant <targets> <enchantment> [level]",
    ),
    pend(
        "experience",
        &["xp"],
        "minecraft.command.experience",
        2,
        "CMD-10",
        Category::Player,
        "/experience <targets> <amount> [points,levels]",
    ),
    pend(
        "attribute",
        &[],
        "minecraft.command.attribute",
        2,
        "CMD-10",
        Category::Player,
        "/attribute <target> <attribute> <get,base> [value]",
    ),
    pend(
        "damage",
        &[],
        "minecraft.command.damage",
        2,
        "CMD-10",
        Category::Player,
        "/damage <target> <amount> [type]",
    ),
    pend(
        "spreadplayers",
        &[],
        "minecraft.command.spreadplayers",
        2,
        "CMD-11",
        Category::Player,
        "/spreadplayers <center> <spread> <max-range> [teams]",
    ),
    pend(
        "spawnpoint",
        &[],
        "minecraft.command.spawnpoint",
        2,
        "CMD-11",
        Category::Player,
        "/spawnpoint [target] [pos]",
    ),
    pend(
        "ride",
        &[],
        "minecraft.command.ride",
        2,
        "CMD-11",
        Category::Player,
        "/ride <target> <vehicle>",
    ),
    pend(
        "item",
        &[],
        "minecraft.command.item",
        2,
        "CMD-11",
        Category::Player,
        "/item <target> <slot> <replace,modify> [...]",
    ),
    pend(
        "rotate",
        &[],
        "minecraft.command.rotate",
        2,
        "CMD-11",
        Category::Player,
        "/rotate <target> <yaw> <pitch>",
    ),
    // -- world -------------------------------------------------------------
    imp_node(
        "time",
        &[],
        "minecraft.command.time",
        2,
        Category::World,
        "/time <add,set,query> [...]",
    ),
    imp_node(
        "setblock",
        &[],
        "minecraft.command.setblock",
        2,
        Category::World,
        "/setblock <pos> <block> [destroy|keep|replace|strict]",
    ),
    imp_node(
        "fill",
        &[],
        "minecraft.command.fill",
        2,
        Category::World,
        "/fill <from> <to> <block> [destroy|hollow|keep|outline|replace|strict] | <from> <to> <block> replace <filter> [destroy|hollow|keep|outline|strict]",
    ),
    imp_node(
        "clone",
        &[],
        "minecraft.command.clone",
        2,
        Category::World,
        "/clone [from <dimension>] <begin> <end> [to <dimension>] <destination> [strict] [replace|masked|filtered <filter>] [force|move|normal]",
    ),
    pend(
        "fillbiome",
        &[],
        "minecraft.command.fillbiome",
        2,
        "CMD-12",
        Category::World,
        "/fillbiome <from> <to> <biome>",
    ),
    pend(
        "place",
        &[],
        "minecraft.command.place",
        2,
        "CMD-12",
        Category::World,
        "/place <feature,template> <pos> [...]",
    ),
    pend(
        "forceload",
        &[],
        "minecraft.command.forceload",
        2,
        "CMD-12",
        Category::World,
        "/forceload <add,remove,query> <from> [to]",
    ),
    pend(
        "locate",
        &[],
        "minecraft.command.locate",
        2,
        "CMD-13",
        Category::World,
        "/locate <structure,biome,poi> [...]",
    ),
    imp_node(
        "setworldspawn",
        &[],
        "minecraft.command.setworldspawn",
        2,
        Category::World,
        "/setworldspawn [<pos>] [<angle>]",
    ),
    imp_node(
        "worldborder",
        &[],
        "minecraft.command.worldborder",
        2,
        Category::World,
        "/worldborder <add,set,center,damage,get,warning> [...]",
    ),
    imp_node(
        "weather",
        &[],
        "minecraft.command.weather",
        2,
        Category::World,
        "/weather <clear,rain,thunder> [<duration>]",
    ),
    pend(
        "gamerule",
        &[],
        "minecraft.command.gamerule",
        2,
        "CMD-13",
        Category::World,
        "/gamerule <rule> [value]",
    ),
    pend(
        "difficulty",
        &[],
        "minecraft.command.difficulty",
        2,
        "CMD-13",
        Category::World,
        "/difficulty <peaceful,easy,normal,hard>",
    ),
    pend(
        "tick",
        &[],
        "minecraft.command.tick",
        3,
        "CMD-13",
        Category::World,
        "/tick <rate,freeze,step,sprint,unfreeze,query> [...]",
    ),
    imp_node(
        "defaultgamemode",
        &[],
        "minecraft.command.defaultgamemode",
        2,
        Category::World,
        "/defaultgamemode <gamemode>",
    ),
    pend(
        "waypoint",
        &[],
        "minecraft.command.waypoint",
        2,
        "CMD-13",
        Category::World,
        "/waypoint <list,modify>",
    ),
    // -- data --------------------------------------------------------------
    imp(
        "msg",
        &["tell", "w"],
        "minecraft.command.msg",
        0,
        Category::Data,
        "/msg <targets> <message>",
    ),
    pend(
        "data",
        &[],
        "minecraft.command.data",
        2,
        "CMD-14",
        Category::Data,
        "/data <get,merge,modify,remove> <target> <path> [...]",
    ),
    pend(
        "scoreboard",
        &[],
        "minecraft.command.scoreboard",
        2,
        "CMD-15",
        Category::Data,
        "/scoreboard <objectives,players> [...]",
    ),
    imp_node(
        "tag",
        &[],
        "minecraft.command.tag",
        2,
        Category::Data,
        "/tag <targets> <add|remove> <name> | <targets> list",
    ),
    imp_node(
        "team",
        &[],
        "minecraft.command.team",
        2,
        Category::Data,
        "/team <list,add,remove,empty,join,leave,modify> [...]",
    ),
    imp(
        "teammsg",
        &["tm"],
        "minecraft.command.teammsg",
        0,
        Category::Data,
        "/teammsg <message>",
    ),
    imp_node(
        "say",
        &[],
        "minecraft.command.say",
        2,
        Category::Data,
        "/say <message>",
    ),
    imp(
        "me",
        &[],
        "minecraft.command.me",
        0,
        Category::Data,
        "/me <action>",
    ),
    imp_node(
        "tellraw",
        &[],
        "minecraft.command.tellraw",
        2,
        Category::Data,
        "/tellraw <targets> <message>",
    ),
    pend(
        "title",
        &[],
        "minecraft.command.title",
        2,
        "CMD-17",
        Category::Data,
        "/title <targets> <title,subtitle,actionbar,times,reset> [...]",
    ),
    pend(
        "bossbar",
        &[],
        "minecraft.command.bossbar",
        2,
        "CMD-17",
        Category::Data,
        "/bossbar <add,get,list,remove,set> [...]",
    ),
    pend(
        "dialog",
        &[],
        "minecraft.command.dialog",
        2,
        "CMD-17",
        Category::Data,
        "/dialog show <targets> <dialog>",
    ),
    pend(
        "trigger",
        &[],
        "minecraft.command.trigger",
        0,
        "CMD-18",
        Category::Data,
        "/trigger <objective> [add,set] [value]",
    ),
    imp_node(
        "particle",
        &[],
        "minecraft.command.particle",
        2,
        Category::Data,
        "/particle <name> [<pos>] [<delta>] [<speed>] [<count>] [force|normal] [<viewers>]",
    ),
    imp_node(
        "playsound",
        &[],
        "minecraft.command.playsound",
        2,
        Category::Data,
        "/playsound <sound> [<source>] [<targets>] [<pos>] [<volume>] [<pitch>] [<minVolume>]",
    ),
    imp_node(
        "stopsound",
        &[],
        "minecraft.command.stopsound",
        2,
        Category::Data,
        "/stopsound <targets> [<source>] [<sound>]",
    ),
    pend(
        "loot",
        &[],
        "minecraft.command.loot",
        2,
        "CMD-20",
        Category::Data,
        "/loot <give,insert,spawn,replace> [...]",
    ),
    pend(
        "recipe",
        &[],
        "minecraft.command.recipe",
        2,
        "CMD-20",
        Category::Data,
        "/recipe <give,take> <targets> <recipe>",
    ),
    pend(
        "advancement",
        &[],
        "minecraft.command.advancement",
        2,
        "CMD-20",
        Category::Data,
        "/advancement <grant,revoke> <targets> <advancement>",
    ),
    pend(
        "random",
        &[],
        "minecraft.command.random",
        2,
        "CMD-20",
        Category::Data,
        "/random <value,roll> [...]",
    ),
    pend(
        "schedule",
        &[],
        "minecraft.command.schedule",
        2,
        "CMD-20",
        Category::Data,
        "/schedule <function> <clear,replace> <time>",
    ),
    pend(
        "function",
        &[],
        "minecraft.command.function",
        2,
        "CMD-21",
        Category::Data,
        "/function <name> [arguments]",
    ),
    pend(
        "datapack",
        &[],
        "minecraft.command.datapack",
        2,
        "CMD-21",
        Category::Data,
        "/datapack <enable,disable,list> [name]",
    ),
    pend(
        "reload",
        &[],
        "minecraft.command.reload",
        4,
        "CMD-21",
        Category::Data,
        "/reload",
    ),
    pend(
        "return",
        &[],
        "minecraft.command.return",
        2,
        "CMD-21",
        Category::Data,
        "/return <value>",
    ),
    pend(
        "test",
        &[],
        "minecraft.command.test",
        2,
        "CMD-21",
        Category::Data,
        "/test <run,create,clear,list> [...]",
    ),
    pend(
        "execute",
        &[],
        "minecraft.command.execute",
        2,
        "CMD-22",
        Category::Data,
        "/execute <...> run <command>",
    ),
    // -- ops ---------------------------------------------------------------
    imp(
        "cache",
        &[],
        "aether.command.cache",
        3,
        Category::Ops,
        "/cache",
    ),
    imp_op(
        "aether",
        &[],
        "aether.command.aether",
        4,
        Category::Ops,
        "/aether <subcommand>",
    ),
    imp_op(
        "visibility",
        &[],
        "aether.command.visibility",
        3,
        Category::Ops,
        "/visibility [on,off,status]",
    ),
    pend(
        "tps",
        &[],
        "aether.command.tps",
        3,
        "CMD-23",
        Category::Ops,
        "/tps",
    ),
    pend(
        "mspt",
        &[],
        "aether.command.mspt",
        3,
        "CMD-23",
        Category::Ops,
        "/mspt",
    ),
    pend(
        "gc",
        &[],
        "aether.command.gc",
        3,
        "CMD-23",
        Category::Ops,
        "/gc",
    ),
    pend(
        "plugins",
        &["pl"],
        "aether.command.plugins",
        3,
        "CMD-23",
        Category::Ops,
        "/plugins",
    ),
    pend(
        "restart",
        &[],
        "aether.command.restart",
        4,
        "CMD-23",
        Category::Ops,
        "/restart",
    ),
    pend(
        "debug",
        &[],
        "minecraft.command.debug",
        3,
        "CMD-23",
        Category::Ops,
        "/debug <start,stop,function> [...]",
    ),
    pend(
        "perf",
        &[],
        "minecraft.command.perf",
        4,
        "CMD-23",
        Category::Ops,
        "/perf <start,stop>",
    ),
    pend(
        "jfr",
        &[],
        "minecraft.command.jfr",
        4,
        "CMD-23",
        Category::Ops,
        "/jfr <start,stop,dump> [...]",
    ),
    // -- history -----------------------------------------------------------
    imp(
        "inspect",
        &["i"],
        "aether.command.inspect",
        2,
        Category::History,
        "/inspect",
    ),
    imp(
        "lookup",
        &["l"],
        "aether.command.lookup",
        2,
        Category::History,
        "/lookup <filters>",
    ),
    imp(
        "rollback",
        &["rb"],
        "aether.command.rollback",
        3,
        Category::History,
        "/rollback <filters>",
    ),
    imp(
        "page",
        &["pg"],
        "aether.command.page",
        2,
        Category::History,
        "/page next,prev,number",
    ),
    imp(
        "stash",
        &[],
        "aether.command.stash",
        2,
        Category::History,
        "/stash",
    ),
    imp(
        "audit",
        &[],
        "aether.command.audit",
        2,
        Category::History,
        "/audit",
    ),
    pend(
        "co",
        &[],
        "aether.command.lookup",
        2,
        "CMD-27",
        Category::History,
        "/co <l,u:<player> t:<time> r:<radius> a:<action>>",
    ),
    pend(
        "restore",
        &[],
        "aether.command.restore",
        3,
        "CMD-28",
        Category::History,
        "/restore <filters>",
    ),
    pend(
        "purge",
        &[],
        "aether.command.purge",
        4,
        "CMD-28",
        Category::History,
        "/purge <filters>",
    ),
    // -- economy -----------------------------------------------------------
    imp(
        "balance",
        &["bal", "money"],
        "aether.command.balance",
        0,
        Category::Economy,
        "/balance [player]",
    ),
    imp(
        "pay",
        &[],
        "aether.command.pay",
        0,
        Category::Economy,
        "/pay <player> <amount>",
    ),
    imp(
        "sell",
        &[],
        "aether.command.sell",
        0,
        Category::Economy,
        "/sell <count> <price> [duration]",
    ),
    imp(
        "market",
        &[],
        "aether.command.market",
        0,
        Category::Economy,
        "/market [filters]",
    ),
    imp(
        "buy",
        &[],
        "aether.command.buy",
        0,
        Category::Economy,
        "/buy <id> [count]",
    ),
    imp(
        "listings",
        &[],
        "aether.command.listings",
        0,
        Category::Economy,
        "/listings",
    ),
    imp(
        "unlist",
        &[],
        "aether.command.unlist",
        0,
        Category::Economy,
        "/unlist <id>",
    ),
    imp_op(
        "econ",
        &[],
        "aether.command.econ",
        3,
        Category::Economy,
        "/econ give <player> <amount> [reason]",
    ),
];

/// The catalogue entry for `name`, canonical or alias.
pub fn find(name: &str) -> Option<&'static Command> {
    CATALOGUE
        .iter()
        .find(|c| c.name == name || c.aliases.contains(&name))
}

/// Whether the catalogue owns `name`, implemented or pending.
pub fn owns(name: &str) -> bool {
    find(name).is_some()
}

/// The documentation language.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    /// English (`docs/COMMANDS.md`).
    En,
    /// Russian (`docs/COMMANDS.ru.md`).
    Ru,
}

/// Render the whole catalogue as the generated markdown catalogue.
///
/// [`tests::the_generated_docs_are_in_sync`] byte-compares the result with the
/// checked-in `docs/COMMANDS.md` and `docs/COMMANDS.ru.md`, so the two cannot
/// drift. Regenerate with
/// `cargo-locked run -p aether-server --example gen_commands_md`.
pub fn render_markdown(lang: Lang) -> String {
    let ru = lang == Lang::Ru;
    let t = |en: &'static str, ru_s: &'static str| if ru { ru_s } else { en };
    let total = CATALOGUE.len();
    let working = CATALOGUE
        .iter()
        .filter(|c| c.status == Status::Implemented)
        .count();
    let pending = total - working;

    let mut s = String::new();
    s.push_str(t(
        "<!-- GENERATED FILE — do not edit by hand.\n     \
         Source:     crates/aether-server/src/commands/catalogue.rs\n     \
         Regenerate: cargo-locked run -p aether-server --example gen_commands_md -->\n\n",
        "<!-- СГЕНЕРИРОВАННЫЙ ФАЙЛ — не редактируйте вручную.\n     \
         Источник:    crates/aether-server/src/commands/catalogue.rs\n     \
         Пересобрать: cargo-locked run -p aether-server --example gen_commands_md -->\n\n",
    ));
    writeln!(
        s,
        "# {}",
        t(
            "Aether Engine — Command Catalogue",
            "Aether Engine — Каталог команд"
        )
    )
    .unwrap();
    writeln!(s).unwrap();
    if ru {
        writeln!(s, "> **English version:** [COMMANDS.md](COMMANDS.md)").unwrap();
    } else {
        writeln!(s, "> **Русская версия:** [COMMANDS.ru.md](COMMANDS.ru.md)").unwrap();
    }
    writeln!(s).unwrap();
    writeln!(
        s,
        "{}",
        t(
            "Every command Aether knows, grouped by category: the ones that run today and the \
             ones that are only specified. It is generated from one list in the server \
             (`crates/aether-server/src/commands/catalogue.rs`), so nothing is listed twice and a \
             planned command cannot quietly disappear.",
            "Каждая известная Aether команда, сгруппированная по категориям: и те, что работают \
             сейчас, и те, что пока лишь запланированы. Файл сгенерирован из одного списка в \
             сервере (`crates/aether-server/src/commands/catalogue.rs`), поэтому ни одна команда \
             не указана дважды и запланированная не может незаметно исчезнуть.",
        )
    )
    .unwrap();
    writeln!(s).unwrap();
    writeln!(
        s,
        "{}",
        t(
            "*This file is generated — do not edit it by hand. Regenerate with \
             `cargo-locked run -p aether-server --example gen_commands_md`.*",
            "*Файл сгенерирован — не редактируйте его вручную. Пересобрать: \
             `cargo-locked run -p aether-server --example gen_commands_md`.*",
        )
    )
    .unwrap();
    writeln!(s).unwrap();
    writeln!(
        s,
        "{}",
        t(
            "**Legend:** ✅ working · 🚧 pending (`CMD-nn` is the task that will implement it). \
             **Node** is the permission node and **Op** the vanilla operator level (0 everyone … \
             4 owner); for a pending command the stub enforces exactly them. **Gate** is what the \
             server checks *today*: `node` (enforced node/op), `operator` (the legacy \
             operator-only check — Node/Op are the intended target, not yet enforced), or \
             `everyone` (no requirement; Node/Op are the intended target).",
            "**Легенда:** ✅ работает · 🚧 ожидает (`CMD-nn` — задача, которая её реализует). \
             **Узел** — правомочие, а **Op** — ванильный уровень оператора (0 — всем … 4 — \
             владельцу); для ожидающей команды заглушка требует ровно их. **Gate** — что сервер \
             проверяет *сегодня*: `node` (узел/op применяются), `operator` (прежняя проверка \
             «только оператор» — узел/op это цель, пока не применяются) или `everyone` \
             (требований нет; узел/op — цель).",
        )
    )
    .unwrap();
    writeln!(s).unwrap();
    if ru {
        writeln!(
            s,
            "**Итого:** {total} команд — {working} работают, {pending} ожидают."
        )
        .unwrap();
    } else {
        writeln!(
            s,
            "**Totals:** {total} commands — {working} working, {pending} pending."
        )
        .unwrap();
    }
    writeln!(s).unwrap();

    // Table of contents, one line per category with its count.
    writeln!(s, "## {}", t("Contents", "Содержание")).unwrap();
    writeln!(s).unwrap();
    for cat in Category::ALL {
        let n = CATALOGUE.iter().filter(|c| c.category == *cat).count();
        writeln!(s, "- [{0}](#{0}) — {1} ({n})", cat.key(), cat.blurb(lang)).unwrap();
    }
    writeln!(s).unwrap();

    for cat in Category::ALL {
        writeln!(s, "## {}", cat.key()).unwrap();
        writeln!(s).unwrap();
        writeln!(s, "{}", cat.blurb(lang)).unwrap();
        writeln!(s).unwrap();
        writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} |",
            t("Command", "Команда"),
            t("Aliases", "Синонимы"),
            t("Status", "Статус"),
            t("Gate", "Gate"),
            t("Node", "Узел"),
            t("Op", "Op"),
            t("Usage", "Использование"),
        )
        .unwrap();
        writeln!(s, "| --- | --- | --- | --- | --- | --- | --- |").unwrap();
        for c in CATALOGUE.iter().filter(|c| c.category == *cat) {
            let aliases = if c.aliases.is_empty() {
                "–".to_string()
            } else {
                c.aliases
                    .iter()
                    .map(|a| format!("`{a}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let status = match (c.status, ru) {
                (Status::Implemented, false) => "✅ working".to_string(),
                (Status::Implemented, true) => "✅ работает".to_string(),
                (Status::Pending, false) => format!("🚧 pending {}", c.task),
                (Status::Pending, true) => format!("🚧 ожидает {}", c.task),
            };
            writeln!(
                s,
                "| `/{name}` | {aliases} | {status} | `{gate}` | `{node}` | {op} | `{usage}` |",
                name = c.name,
                gate = c.gate.key(),
                node = c.node,
                op = c.op,
                usage = c.usage.replace('|', "\\|"),
            )
            .unwrap();
        }
        writeln!(s).unwrap();
    }

    s
}

impl Category {
    /// The localized one-line description.
    fn blurb(self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.blurb_en(),
            Lang::Ru => self.blurb_ru(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tree::{self, Requirement};

    /// Every name and alias in the catalogue, in order.
    fn catalogue_names() -> Vec<&'static str> {
        CATALOGUE
            .iter()
            .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
            .collect()
    }

    #[test]
    fn no_name_is_declared_twice() {
        let mut seen: Vec<&str> = Vec::new();
        for name in catalogue_names() {
            assert!(!name.is_empty(), "an empty command name");
            assert!(!name.contains('/'), "{name} must not contain a slash");
            assert!(!seen.contains(&name), "{name} is declared twice");
            seen.push(name);
        }
    }

    #[test]
    fn every_entry_is_well_formed() {
        for c in CATALOGUE {
            assert!(
                c.node.starts_with("minecraft.command.") || c.node.starts_with("aether.command."),
                "{}: unexpected permission node {}",
                c.name,
                c.node
            );
            assert!(c.op <= 4, "{}: op level {} out of range", c.name, c.op);
            assert!(!c.usage.is_empty(), "{}: no usage", c.name);
            match c.status {
                Status::Implemented => assert_eq!(
                    c.task, "-",
                    "{}: an implemented command must have task `-`",
                    c.name
                ),
                Status::Pending => assert!(
                    c.task.starts_with("CMD-"),
                    "{}: a pending command needs a CMD-nn task, got {}",
                    c.name,
                    c.task
                ),
            }
        }
    }

    #[test]
    fn no_name_is_both_implemented_and_pending() {
        for c in CATALOGUE {
            let names = std::iter::once(c.name).chain(c.aliases.iter().copied());
            for name in names {
                for other in CATALOGUE {
                    if other.name == c.name {
                        continue;
                    }
                    if other.aliases.contains(&name) || other.name == name {
                        panic!(
                            "`{name}` is listed under both `{}` and `{}`",
                            c.name, other.name
                        );
                    }
                }
            }
        }
    }

    /// The tree node for `name`, or a panic naming the missing command.
    fn command_node(name: &str) -> &'static tree::Node {
        tree::tree()
            .commands()
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("`{name}` has no tree node"))
    }

    #[test]
    fn an_implemented_entry_gate_matches_the_tree() {
        // The drift guard: the catalogue's `gate` must describe what the tree
        // actually does, so an implemented command never claims a node/op it
        // does not enforce. A gate that changes without the catalogue (or the
        // reverse) fails here.
        for c in CATALOGUE.iter().filter(|c| c.status == Status::Implemented) {
            let req = &command_node(c.name).requirement;
            match (c.gate, req) {
                (Gate::Everyone, Requirement::None) => {}
                (Gate::Operator, Requirement::Fn(_)) => {}
                (Gate::Node, Requirement::Level { op, node }) => {
                    assert_eq!(op, &c.op, "{}: op level", c.name);
                    assert_eq!(node, &c.node, "{}: permission node", c.name);
                }
                _ => panic!(
                    "`{}` claims gate {:?} but the tree enforces {:?}",
                    c.name,
                    c.gate,
                    requirement_kind(req)
                ),
            }
        }
    }

    #[test]
    fn a_pending_entry_gate_matches_the_tree() {
        // A pending command is a stub: the tree enforces exactly its node/op,
        // and answers `not implemented yet (CMD-nn)`.
        for c in CATALOGUE.iter().filter(|c| c.status == Status::Pending) {
            let req = &command_node(c.name).requirement;
            match req {
                Requirement::Level { op, node } => {
                    assert_eq!(op, &c.op, "{}: op level", c.name);
                    assert_eq!(node, &c.node, "{}: permission node", c.name);
                }
                _ => panic!("pending `{}` is not a node/op stub", c.name),
            }
        }
    }

    /// A readable label for a requirement, for the panic above.
    fn requirement_kind(req: &Requirement) -> &'static str {
        match req {
            Requirement::None => "no requirement",
            Requirement::Fn(_) => "an operator check",
            Requirement::Level { .. } => "a node/op level",
        }
    }

    #[test]
    fn every_implemented_entry_has_a_real_executor() {
        fn has_executor(node: &tree::Node) -> bool {
            (node.executable && node.executor.is_some()) || node.children.iter().any(has_executor)
        }
        let tree = tree::tree();
        for c in CATALOGUE.iter().filter(|c| c.status == Status::Implemented) {
            let node = tree
                .commands()
                .iter()
                .find(|n| n.name == c.name)
                .unwrap_or_else(|| panic!("implemented `{}` has no tree node", c.name));
            // A command may be non-executable at its root (vanilla's `/tag`,
            // `/team` and `/teammsg` are), as long as some node below it runs.
            assert!(
                node.executor.is_some() || has_executor(node),
                "`{}` has no executor",
                c.name
            );
        }
    }

    #[test]
    fn every_pending_entry_is_not_implemented() {
        let implemented: Vec<&str> = CATALOGUE
            .iter()
            .filter(|c| c.status == Status::Implemented)
            .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
            .collect();
        for c in CATALOGUE.iter().filter(|c| c.status == Status::Pending) {
            for name in std::iter::once(c.name).chain(c.aliases.iter().copied()) {
                assert!(
                    !implemented.contains(&name),
                    "pending `{name}` collides with an implemented command"
                );
            }
        }
    }

    #[test]
    fn the_tree_declares_exactly_the_catalogue() {
        // Every catalogue name and alias is declared exactly once in the wire
        // tree, and the tree declares nothing the catalogue does not list.
        let mut tree_names: Vec<&str> = tree::tree()
            .commands()
            .iter()
            .flat_map(|n| std::iter::once(n.name).chain(n.aliases.iter().copied()))
            .collect();
        tree_names.sort_unstable();
        let mut cat_names = catalogue_names();
        cat_names.sort_unstable();
        assert_eq!(tree_names, cat_names, "the tree and the catalogue disagree");
    }

    #[test]
    fn every_category_is_used() {
        for cat in Category::ALL {
            assert!(
                CATALOGUE.iter().any(|c| c.category == *cat),
                "category `{}` has no commands",
                cat.key()
            );
        }
    }

    #[test]
    fn the_generated_docs_are_in_sync() {
        let en = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/COMMANDS.md"
        ));
        let ru = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/COMMANDS.ru.md"
        ));
        assert_eq!(
            render_markdown(Lang::En),
            en,
            "docs/COMMANDS.md is stale — regenerate with \
             `cargo-locked run -p aether-server --example gen_commands_md`"
        );
        assert_eq!(
            render_markdown(Lang::Ru),
            ru,
            "docs/COMMANDS.ru.md is stale — regenerate with \
             `cargo-locked run -p aether-server --example gen_commands_md`"
        );
    }

    #[test]
    fn lookup_answers_canonical_names_and_aliases() {
        assert_eq!(find("gamemode").map(|c| c.name), Some("gamemode"));
        assert_eq!(find("gm").map(|c| c.name), Some("gamemode"));
        assert_eq!(find("xp").map(|c| c.task), Some("CMD-10"));
        assert_eq!(find("tell").map(|c| c.status), Some(Status::Implemented));
        assert!(find("definitely-not-a-command").is_none());
    }
}
