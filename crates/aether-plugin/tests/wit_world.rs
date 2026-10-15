//! The `aether:plugin@0.1.0` world is a **frozen contract**.
//!
//! The component model has no optional exports: adding one export to a world
//! changes its type, so every component already compiled against it stops
//! matching and the host refuses it. That is fine as long as a new role is a
//! new package version, not an addition here. This test pins the package
//! identity and the exact export set so that adding an `economy` world or a
//! `register-enchantment` export to `@0.1.0` fails loudly and forces the
//! author down the versioned path (see `docs/design/plugins.md`, "Layering").

use std::collections::BTreeSet;
use std::path::Path;

use wit_parser::{Resolve, WorldKey};

/// Every export of `world plugin`, in `@0.1.0`. Additions must be a new
/// package version; removals break the contract too.
const FROZEN_EXPORTS: &[&str] = &[
    "init",
    "shutdown",
    "before-block-break",
    "before-block-place",
    "before-use-item",
    "before-attack",
    "before-chat",
    "before-container-click",
    "after-player-join",
    "after-player-leave",
    "after-block-break",
    "after-entity-death",
    "after-damage",
    "after-craft",
    "after-enchant",
    "on-tick",
    "on-command",
];

fn key_name(resolve: &Resolve, key: &WorldKey) -> String {
    match key {
        WorldKey::Name(n) => n.clone(),
        WorldKey::Interface(id) => resolve.interfaces[*id]
            .name
            .clone()
            .unwrap_or_else(|| format!("<interface {id:?}>")),
    }
}

#[test]
fn the_plugin_world_is_frozen_for_0_1_0() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
    let mut resolve = Resolve::default();
    let (pkg, _) = resolve.push_dir(&dir).expect("the WIT package must parse");
    let package = &resolve.packages[pkg];

    assert_eq!(package.name.namespace, "aether");
    assert_eq!(package.name.name, "plugin");
    assert_eq!(
        package
            .name
            .version
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("0.1.0"),
        "the package version is part of the contract a component is built against"
    );

    let worlds: BTreeSet<&str> = package.worlds.keys().map(String::as_str).collect();
    assert_eq!(
        worlds,
        BTreeSet::from(["plugin"]),
        "a second world in `@0.1.0` changes the package; give a new role its own \
         package version (e.g. `aether:plugin@0.2.0`) instead"
    );

    let world = &resolve.worlds[package.worlds["plugin"]];
    let exports: BTreeSet<String> = world
        .exports
        .keys()
        .map(|k| key_name(&resolve, k))
        .collect();
    let expected: BTreeSet<String> = FROZEN_EXPORTS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        exports, expected,
        "the export set of `world plugin` is frozen for 0.1.0: adding or removing \
         one refuses every component already built against it"
    );
}
