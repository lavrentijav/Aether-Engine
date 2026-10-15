//! Declarative menus: `menus/*.toml` files loaded at `init` (and `/menus
//! reload`). The full DeluxeMenus-style schema lands in MN1c; this module is
//! the seam.
//!
//! (MN1b — the loader is implemented in the next step.)

use aether_plugin::menus::MenuDefinition;
use aether_plugin::NativeContext;

/// Load every declarative menu the operator configured for the plugin.
pub fn load(_ctx: &NativeContext) -> Vec<MenuDefinition> {
    Vec::new()
}
