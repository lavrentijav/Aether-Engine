//! The core command registry.
//!
//! Every core command in the catalogue (`docs/design/commands.md` §1–3) that is not implemented
//! yet is listed here with the permission node, op level and task that will implement it. It is
//! the data behind the "not implemented yet (CMD-nn)" reply and `/help`; the real Brigadier tree
//! (CMD-03) and Declare Commands packet (CMD-04) will be built from it.
//!
//! Commands that already exist (the history commands, the economy, the gameplay commands in
//! `game/commands.rs`) are deliberately **not** listed — a command name is answered by exactly one
//! place, and these names must not be shadowed.
//!
//! Plugin commands register here at load (see [`register_plugin`]). The rule is the same one
//! CMD-00N states for native plugins: **the core owns the vanilla names; a plugin may add new
//! names only.** A plugin command is resolved through this registry and permission-checked before
//! the plugin's `on-command` is entered, so it is not a side path around the core's gate.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// A core command that is specified but not implemented yet.
pub struct Entry {
    /// Canonical command name, without the leading slash.
    pub name: &'static str,
    /// Alternative names the command answers to.
    pub aliases: &'static [&'static str],
    /// Permission node checked before the command runs (CMD-01).
    pub node: &'static str,
    /// Vanilla op level required (0 everyone … 4 owner).
    pub op: u8,
    /// The CMD task that implements it.
    pub task: &'static str,
    /// The catalogue group it belongs to, for `/help`.
    pub group: &'static str,
}

const fn e(
    name: &'static str,
    aliases: &'static [&'static str],
    node: &'static str,
    op: u8,
    task: &'static str,
    group: &'static str,
) -> Entry {
    Entry {
        name,
        aliases,
        node,
        op,
        task,
        group,
    }
}

/// Every not-yet-implemented core command, grouped as in the catalogue.
pub const CORE: &[Entry] = &[
    // V-admin: player & access management (CMD-08).
    e("op", &[], "minecraft.command.op", 3, "CMD-08", "admin"),
    e("deop", &[], "minecraft.command.deop", 3, "CMD-08", "admin"),
    e("ban", &[], "minecraft.command.ban", 3, "CMD-08", "admin"),
    e(
        "ban-ip",
        &[],
        "minecraft.command.ban-ip",
        3,
        "CMD-08",
        "admin",
    ),
    e(
        "banlist",
        &[],
        "minecraft.command.banlist",
        3,
        "CMD-08",
        "admin",
    ),
    e(
        "pardon",
        &[],
        "minecraft.command.pardon",
        3,
        "CMD-08",
        "admin",
    ),
    e(
        "pardon-ip",
        &[],
        "minecraft.command.pardon-ip",
        3,
        "CMD-08",
        "admin",
    ),
    e("kick", &[], "minecraft.command.kick", 3, "CMD-08", "admin"),
    e(
        "whitelist",
        &[],
        "minecraft.command.whitelist",
        3,
        "CMD-08",
        "admin",
    ),
    // V-admin: server control & info (CMD-09).
    e("stop", &[], "minecraft.command.stop", 4, "CMD-09", "server"),
    e(
        "save-all",
        &[],
        "minecraft.command.save-all",
        4,
        "CMD-09",
        "server",
    ),
    e(
        "save-on",
        &[],
        "minecraft.command.save-on",
        4,
        "CMD-09",
        "server",
    ),
    e(
        "save-off",
        &[],
        "minecraft.command.save-off",
        4,
        "CMD-09",
        "server",
    ),
    e("list", &[], "minecraft.command.list", 0, "CMD-09", "server"),
    e("seed", &[], "minecraft.command.seed", 2, "CMD-09", "server"),
    e(
        "version",
        &[],
        "minecraft.command.version",
        2,
        "CMD-09",
        "server",
    ),
    e(
        "setidletimeout",
        &[],
        "minecraft.command.setidletimeout",
        3,
        "CMD-09",
        "server",
    ),
    e(
        "transfer",
        &[],
        "minecraft.command.transfer",
        3,
        "CMD-09",
        "server",
    ),
    e(
        "publish",
        &[],
        "minecraft.command.publish",
        4,
        "CMD-09",
        "server",
    ),
    // V-player (CMD-10).
    e(
        "clear",
        &[],
        "minecraft.command.clear",
        2,
        "CMD-10",
        "player",
    ),
    e(
        "effect",
        &[],
        "minecraft.command.effect",
        2,
        "CMD-10",
        "player",
    ),
    e(
        "enchant",
        &[],
        "minecraft.command.enchant",
        2,
        "CMD-10",
        "player",
    ),
    e(
        "experience",
        &["xp"],
        "minecraft.command.experience",
        2,
        "CMD-10",
        "player",
    ),
    e(
        "attribute",
        &[],
        "minecraft.command.attribute",
        2,
        "CMD-10",
        "player",
    ),
    e(
        "damage",
        &[],
        "minecraft.command.damage",
        2,
        "CMD-10",
        "player",
    ),
    // V-player: movement & inventory (CMD-11).
    e(
        "teleport",
        &[],
        "minecraft.command.teleport",
        2,
        "CMD-11",
        "player",
    ),
    e(
        "spreadplayers",
        &[],
        "minecraft.command.spreadplayers",
        2,
        "CMD-11",
        "player",
    ),
    e(
        "spawnpoint",
        &[],
        "minecraft.command.spawnpoint",
        2,
        "CMD-11",
        "player",
    ),
    // `/spectate` is implemented by the native `staff` plugin (SP1), with
    // `aether.staff.spectate`; it is not a pending core command.
    e("ride", &[], "minecraft.command.ride", 2, "CMD-11", "player"),
    e("item", &[], "minecraft.command.item", 2, "CMD-11", "player"),
    // `/rotate` is in the catalogue but is not a vanilla 1.21.11 command; listed so the queue does
    // not silently drop it (see CMD-11).
    e(
        "rotate",
        &[],
        "aether.command.rotate",
        2,
        "CMD-11",
        "player",
    ),
    // V-world: block operations (CMD-12).
    e(
        "setblock",
        &[],
        "minecraft.command.setblock",
        2,
        "CMD-12",
        "world",
    ),
    e("fill", &[], "minecraft.command.fill", 2, "CMD-12", "world"),
    e(
        "clone",
        &[],
        "minecraft.command.clone",
        2,
        "CMD-12",
        "world",
    ),
    e(
        "fillbiome",
        &[],
        "minecraft.command.fillbiome",
        2,
        "CMD-12",
        "world",
    ),
    e(
        "place",
        &[],
        "minecraft.command.place",
        2,
        "CMD-12",
        "world",
    ),
    e(
        "forceload",
        &[],
        "minecraft.command.forceload",
        2,
        "CMD-12",
        "world",
    ),
    // V-world: environment & rules (CMD-13).
    e(
        "locate",
        &[],
        "minecraft.command.locate",
        2,
        "CMD-13",
        "world",
    ),
    e(
        "setworldspawn",
        &[],
        "minecraft.command.setworldspawn",
        2,
        "CMD-13",
        "world",
    ),
    e(
        "worldborder",
        &[],
        "minecraft.command.worldborder",
        2,
        "CMD-13",
        "world",
    ),
    e(
        "weather",
        &[],
        "minecraft.command.weather",
        2,
        "CMD-13",
        "world",
    ),
    e(
        "gamerule",
        &[],
        "minecraft.command.gamerule",
        2,
        "CMD-13",
        "world",
    ),
    e(
        "difficulty",
        &[],
        "minecraft.command.difficulty",
        2,
        "CMD-13",
        "world",
    ),
    e("tick", &[], "minecraft.command.tick", 3, "CMD-13", "world"),
    e(
        "defaultgamemode",
        &[],
        "minecraft.command.defaultgamemode",
        2,
        "CMD-13",
        "world",
    ),
    e(
        "waypoint",
        &[],
        "minecraft.command.waypoint",
        2,
        "CMD-13",
        "world",
    ),
    // V-data (CMD-14 … CMD-22).
    e("data", &[], "minecraft.command.data", 2, "CMD-14", "data"),
    e(
        "scoreboard",
        &[],
        "minecraft.command.scoreboard",
        2,
        "CMD-15",
        "data",
    ),
    e("tag", &[], "minecraft.command.tag", 2, "CMD-16", "data"),
    e("team", &[], "minecraft.command.team", 2, "CMD-16", "data"),
    e(
        "teammsg",
        &["tm"],
        "minecraft.command.teammsg",
        0,
        "CMD-16",
        "data",
    ),
    e("say", &[], "minecraft.command.say", 2, "CMD-17", "data"),
    e("me", &[], "minecraft.command.me", 0, "CMD-17", "data"),
    // `/msg` (`tell`, `w`) is implemented in `game::commands` and lives in the
    // catalogue as an implemented entry, so it is deliberately not listed as a
    // stub here.
    e(
        "tellraw",
        &[],
        "minecraft.command.tellraw",
        2,
        "CMD-17",
        "data",
    ),
    e("title", &[], "minecraft.command.title", 2, "CMD-17", "data"),
    e(
        "bossbar",
        &[],
        "minecraft.command.bossbar",
        2,
        "CMD-17",
        "data",
    ),
    e(
        "dialog",
        &[],
        "minecraft.command.dialog",
        2,
        "CMD-17",
        "data",
    ),
    e(
        "trigger",
        &[],
        "minecraft.command.trigger",
        0,
        "CMD-18",
        "data",
    ),
    e(
        "particle",
        &[],
        "minecraft.command.particle",
        2,
        "CMD-19",
        "data",
    ),
    e(
        "playsound",
        &[],
        "minecraft.command.playsound",
        2,
        "CMD-19",
        "data",
    ),
    e(
        "stopsound",
        &[],
        "minecraft.command.stopsound",
        2,
        "CMD-19",
        "data",
    ),
    e("loot", &[], "minecraft.command.loot", 2, "CMD-20", "data"),
    e(
        "recipe",
        &[],
        "minecraft.command.recipe",
        2,
        "CMD-20",
        "data",
    ),
    e(
        "advancement",
        &[],
        "minecraft.command.advancement",
        2,
        "CMD-20",
        "data",
    ),
    e(
        "random",
        &[],
        "minecraft.command.random",
        2,
        "CMD-20",
        "data",
    ),
    e(
        "schedule",
        &[],
        "minecraft.command.schedule",
        2,
        "CMD-20",
        "data",
    ),
    e(
        "function",
        &[],
        "minecraft.command.function",
        2,
        "CMD-21",
        "data",
    ),
    e(
        "datapack",
        &[],
        "minecraft.command.datapack",
        2,
        "CMD-21",
        "data",
    ),
    e(
        "reload",
        &[],
        "minecraft.command.reload",
        4,
        "CMD-21",
        "data",
    ),
    e(
        "return",
        &[],
        "minecraft.command.return",
        2,
        "CMD-21",
        "data",
    ),
    e("test", &[], "minecraft.command.test", 2, "CMD-21", "data"),
    e(
        "execute",
        &[],
        "minecraft.command.execute",
        2,
        "CMD-22",
        "data",
    ),
    // Server diagnostics & lifecycle (CMD-23).
    e("tps", &[], "aether.command.tps", 3, "CMD-23", "ops"),
    e("mspt", &[], "aether.command.mspt", 3, "CMD-23", "ops"),
    e("gc", &[], "aether.command.gc", 3, "CMD-23", "ops"),
    e(
        "plugins",
        &["pl"],
        "aether.command.plugins",
        3,
        "CMD-23",
        "ops",
    ),
    e("restart", &[], "aether.command.restart", 4, "CMD-23", "ops"),
    e("debug", &[], "minecraft.command.debug", 3, "CMD-23", "ops"),
    e("perf", &[], "minecraft.command.perf", 4, "CMD-23", "ops"),
    e("jfr", &[], "minecraft.command.jfr", 4, "CMD-23", "ops"),
    // `/aether` is implemented in `commands::core_nodes` and lives in the
    // catalogue as an implemented entry; its planned subcommands (reload/backup
    // CMD-24, profile CMD-25, world CMD-26) extend that node rather than the
    // stub list, so no pending `/aether` entry is kept here.
    // History extensions (CMD-27, CMD-28).
    e("co", &[], "aether.command.lookup", 2, "CMD-27", "history"),
    e(
        "restore",
        &[],
        "aether.command.restore",
        3,
        "CMD-28",
        "history",
    ),
    e("purge", &[], "aether.command.purge", 4, "CMD-28", "history"),
];

/// Find the registry entry for `name`, canonical or alias.
pub fn find(name: &str) -> Option<&'static Entry> {
    CORE.iter()
        .find(|e| e.name == name || e.aliases.contains(&name))
}

/// Whether the **core** owns `name`, implemented or merely specified.
///
/// The catalogue (`super::catalogue`) is the full list of core names, so it is
/// checked first; [`RESERVED_COMMANDS`](super::RESERVED_COMMANDS) is kept as a
/// belt-and-braces floor for the names the tree owns but the catalogue might
/// momentarily miss. A plugin may register none of them.
pub fn core_owns(name: &str) -> bool {
    find(name).is_some() || super::catalogue::owns(name) || super::RESERVED_COMMANDS.contains(&name)
}

/// A command a plugin registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// The plugin that answers it.
    pub plugin: String,
    /// Permission node checked before the command runs.
    pub node: String,
    /// Vanilla op level required (0 everyone … 4 owner).
    pub op: u8,
}

fn plugin_commands() -> &'static Mutex<BTreeMap<String, PluginEntry>> {
    static TABLE: OnceLock<Mutex<BTreeMap<String, PluginEntry>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Register a plugin's command.
///
/// Refused, with a reason, when the name is a core name (the ownership rule) or is already
/// registered by another plugin; the caller logs and drops the declaration. Registration is
/// idempotent per plugin so a reload does not fail on its own entries.
pub fn register_plugin(name: &str, plugin: &str, node: &str, op: u8) -> Result<(), String> {
    if name.is_empty() {
        return Err("a command name is empty".into());
    }
    if core_owns(name) {
        return Err(format!(
            "`/{name}` is a core command; plugins may add new names only"
        ));
    }
    let mut table = plugin_commands().lock().unwrap();
    if let Some(existing) = table.get(name) {
        if existing.plugin == plugin {
            return Ok(());
        }
        return Err(format!(
            "`/{name}` is already registered by plugin `{}`",
            existing.plugin
        ));
    }
    // Declare the node so a permission check resolves it explicitly rather
    // than falling to the fail-closed default. A public command allows
    // everyone; a gated one stays operator-only, with the op level enforced
    // separately by the check. The declaration is non-destructive, so an
    // operator's own setting (CMD-02) is kept; and a malformed node is a
    // registration error, not a command that registers and then silently
    // denies every check, so the reason reaches the operator's log.
    crate::permissions::declare_command_node(node, op)?;
    table.insert(
        name.to_string(),
        PluginEntry {
            plugin: plugin.to_string(),
            node: node.to_string(),
            op: op.min(4),
        },
    );
    Ok(())
}

/// The plugin command registered under `name`, if any.
pub fn plugin_command(name: &str) -> Option<PluginEntry> {
    plugin_commands().lock().unwrap().get(name).cloned()
}

/// Every plugin command name, for a diagnostic listing.
pub fn plugin_command_names() -> Vec<String> {
    plugin_commands().lock().unwrap().keys().cloned().collect()
}

/// The `/help` listing: one line per group, compact enough to fit in chat.
pub fn help_lines() -> Vec<String> {
    let mut groups: Vec<&'static str> = Vec::new();
    for e in CORE {
        if !groups.contains(&e.group) {
            groups.push(e.group);
        }
    }
    let mut lines =
        vec!["Commands not implemented yet — /help <command> for the node and task:".to_string()];
    for g in groups {
        let names: Vec<&str> = CORE
            .iter()
            .filter(|e| e.group == g)
            .map(|e| e.name)
            .collect();
        lines.push(format!("  {g}: {}", names.join(" ")));
    }
    lines.push("Implemented: /help /inspect /lookup /rollback /page /stash /audit /cache".into());
    lines.push(
        "  /gamemode /give /summon /tp /time /heal /food /kill /killall /spawn /inventory".into(),
    );
    lines.push("  /balance /pay /sell /market /buy /listings /unlist /econ".into());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_aliases_are_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for e in CORE {
            for name in std::iter::once(e.name).chain(e.aliases.iter().copied()) {
                assert!(!name.is_empty(), "an empty command name");
                assert!(!name.contains('/'), "{name} must not start with a slash");
                assert!(!seen.contains(&name), "{name} is registered twice");
                seen.push(name);
            }
        }
    }

    #[test]
    fn every_entry_is_well_formed() {
        for e in CORE {
            assert!(
                e.node.starts_with("minecraft.command.") || e.node.starts_with("aether.command."),
                "{}: unexpected permission node {}",
                e.name,
                e.node
            );
            assert!(e.op <= 4, "{}: op level {} out of range", e.name, e.op);
            assert!(
                e.task.starts_with("CMD-"),
                "{}: bad task id {}",
                e.name,
                e.task
            );
            assert!(!e.group.is_empty(), "{}: no group", e.name);
        }
    }

    #[test]
    fn existing_commands_are_not_stubbed() {
        // A stub that shadows a working command would break it.
        let existing = [
            "inspect",
            "i",
            "lookup",
            "l",
            "rollback",
            "rb",
            "page",
            "pg",
            "inv",
            "inventory",
            "stash",
            "audit",
            "cache",
            "gamemode",
            "gm",
            "time",
            "give",
            "summon",
            "killall",
            "kill",
            "spawn",
            "tp",
            "block",
            "food",
            "heal",
            "balance",
            "bal",
            "money",
            "pay",
            "sell",
            "market",
            "buy",
            "listings",
            "unlist",
            "econ",
            "visibility",
        ];
        for name in existing {
            assert!(
                find(name).is_none(),
                "{name} exists but is also in the stub registry"
            );
        }
    }

    #[test]
    fn lookup_answers_canonical_names_and_aliases() {
        assert_eq!(find("experience").map(|e| e.task), Some("CMD-10"));
        assert_eq!(find("xp").map(|e| e.task), Some("CMD-10"));
        assert_eq!(find("tm").map(|e| e.task), Some("CMD-16"));
        assert_eq!(find("teammsg").map(|e| e.task), Some("CMD-16"));
        assert_eq!(find("pl").map(|e| e.task), Some("CMD-23"));
        assert!(find("definitely-not-a-command").is_none());
    }

    #[test]
    fn help_lists_every_group() {
        let lines = help_lines();
        for e in CORE {
            assert!(
                lines.iter().any(|l| l.contains(e.group)),
                "group {} is missing from /help",
                e.group
            );
        }
    }

    #[test]
    fn a_plugin_may_not_take_a_core_name() {
        // Implemented core names...
        assert!(register_plugin("give", "p", "p.give", 0).is_err());
        assert!(register_plugin("gamemode", "p", "p.gm", 0).is_err());
        // ...and specified-but-unimplemented ones (`/stop` is a CMD-09 stub).
        assert!(register_plugin("stop", "p", "p.stop", 4).is_err());
        assert!(core_owns("stop") && core_owns("give") && !core_owns("myplugin_cmd"));
    }

    #[test]
    fn a_plugin_command_is_registered_with_its_permission() {
        // A unique name so this cannot collide with another test or a real plugin.
        let name = "registry_test_op_cmd";
        assert!(register_plugin(name, "regtest", "regtest.opcmd", 3).is_ok());
        let entry = plugin_command(name).expect("registered");
        assert_eq!(entry.plugin, "regtest");
        assert_eq!(entry.node, "regtest.opcmd");
        assert_eq!(entry.op, 3);
        // Re-registering the same plugin's own command is fine; another's is refused.
        assert!(register_plugin(name, "regtest", "regtest.opcmd", 3).is_ok());
        assert!(register_plugin(name, "other", "other.x", 0).is_err());
    }
}
