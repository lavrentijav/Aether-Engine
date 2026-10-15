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

    // The **imports** of the frozen world are part of what a component is
    // compiled against, too. Adding one is backward compatible (a component
    // requests a subset), but removing or renaming one breaks every component
    // that imports it — so pin the set.
    let imports: BTreeSet<String> = world
        .imports
        .keys()
        .map(|k| key_name(&resolve, k))
        .collect();
    let expected_imports: BTreeSet<String> =
        FROZEN_IMPORTS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        imports, expected_imports,
        "the import set of `world plugin` is part of the contract: removing or \
         renaming an interface refuses components built against it"
    );
}

/// The import interfaces of `world plugin` in `@0.1.0`: the `host` interface
/// plus the shared types the world `use`s (`types` and the individual type
/// names the `use` brings into scope).
const FROZEN_IMPORTS: &[&str] = &[
    "host",
    "types",
    "player",
    "manifest-ack",
    "decision",
    "command-result",
];

/// Every SDK service interface the `aether:sdk/services` world imports. A
/// dependent imports these; the host provides them. Pinned so a rename is
/// caught, and so the set matches what the bridge implements.
const SDK_SERVICE_INTERFACES: &[&str] = &[
    "permissions",
    "commands",
    "messages",
    "players",
    "config",
    "scheduler",
    "cooldowns",
    "confirm",
];

#[test]
fn the_sdk_service_world_imports_every_service_interface() {
    // The SDK package depends on `aether:plugin`, so both directories are
    // parsed into one `Resolve`.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut resolve = Resolve::default();
    resolve
        .push_dir(root.join("wit"))
        .expect("the plugin WIT must parse");
    // The SDK world imports the sidebar package, so it must resolve first.
    resolve
        .push_dir(root.join("wit-sidebar"))
        .expect("the sidebar WIT must parse");
    let (pkg, _) = resolve
        .push_dir(root.join("wit-sdk"))
        .expect("the SDK WIT must parse");
    let package = &resolve.packages[pkg];
    assert_eq!(package.name.namespace, "aether");
    assert_eq!(package.name.name, "sdk");

    let world = &resolve.worlds[package.worlds["services"]];
    let imports: BTreeSet<String> = world
        .imports
        .keys()
        .map(|k| key_name(&resolve, k))
        .collect();
    // The plugin host interfaces plus every SDK service; `aether:plugin/host`
    // and `aether:plugin/types` are imported here so the combined world defines
    // them once.
    for name in SDK_SERVICE_INTERFACES {
        assert!(
            imports.iter().any(|i| i == name),
            "`{name}` is missing from the SDK services world: {imports:?}"
        );
    }
    // The plugin host interfaces (`host`, `types`) are imported here so the
    // combined world defines `aether:plugin/types` exactly once.
    assert!(
        imports.contains("host") && imports.contains("types"),
        "the plugin host interfaces must be imported once here: {imports:?}"
    );
    // The sidebar service is imported too (a separate, independently versioned
    // package), so the host generates one linker covering it.
    assert!(
        imports.contains("sidebar"),
        "the sidebar service must be imported by the combined world: {imports:?}"
    );
}

#[test]
fn the_sidebar_package_pins_its_interface_and_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut resolve = Resolve::default();
    resolve
        .push_dir(root.join("wit"))
        .expect("the plugin WIT must parse");
    let (pkg, _) = resolve
        .push_dir(root.join("wit-sidebar"))
        .expect("the sidebar WIT must parse");
    let package = &resolve.packages[pkg];
    assert_eq!(package.name.namespace, "aether");
    assert_eq!(package.name.name, "sidebar");
    assert_eq!(
        package
            .name
            .version
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("0.1.0"),
        "the sidebar service version is part of what a component is built against"
    );
    let interfaces: BTreeSet<&str> = package.interfaces.keys().map(String::as_str).collect();
    assert_eq!(
        interfaces,
        BTreeSet::from(["sidebar"]),
        "one interface; a second role gets its own package"
    );
}
