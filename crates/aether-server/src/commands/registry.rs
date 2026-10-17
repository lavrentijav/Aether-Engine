//! The core command registry: the plugin-command side and the one ownership
//! check.
//!
//! The full list of core commands — implemented and pending, with their
//! categories, permission nodes and status — lives in [`super::catalogue`].
//! That is the single source of truth the command tree (CMD-03), the Declare
//! Commands packet (CMD-04), `/help` and the generated `docs/COMMANDS.md` are
//! built from, so adding or flipping a command is one edit in one list. This
//! module deliberately keeps **no** copy of that list.
//!
//! What remains here is not a command definition:
//!
//! * [`core_owns`] — the ownership rule a plugin registration faces: the core
//!   owns every catalogue name (pending or working), so a plugin may add new
//!   names only.
//! * the plugin-command table ([`register_plugin`] and friends), which records
//!   the commands a plugin declared at load and the permission each is checked
//!   against before the plugin's `on-command` is entered.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Whether the **core** owns `name`, implemented or merely specified.
///
/// The catalogue (`super::catalogue`) is the single list of core names, so it
/// is the only thing checked. A plugin may register none of them.
pub fn core_owns(name: &str) -> bool {
    super::catalogue::owns(name)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_may_not_take_a_core_name() {
        // Implemented core names...
        assert!(register_plugin("give", "p", "p.give", 0).is_err());
        assert!(register_plugin("gamemode", "p", "p.gm", 0).is_err());
        // ...and the server-control commands (`/stop` is CMD-07/09).
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
