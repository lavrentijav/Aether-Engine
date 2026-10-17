//! Protection hooks over the shared region model.
//!
//! The model, the per-chunk spatial index and the snapshot-swapping
//! [`RegionStore`] live in [`aether_region`], a small dependency-free crate
//! that **both** this server and the native `plugins/regions` plugin depend on.
//! That split is what lets the plugin edit the one index the hot path consults
//! without depending on the server (which would be a cycle).
//!
//! This module keeps the server-only half: resolving a player's
//! `aether.region.bypass` permission, building the [`Actor`] and turning a
//! denial into the configured chat line. The index and check themselves are
//! re-exported so existing hook call sites (`crate::region::denial_at`,
//! `crate::region::Action`, …) and the rest of the tree name the same types.
//!
//! ## Snapshot reads, off-tick writes
//!
//! A check calls [`RegionStore::snapshot`] — one brief read lock, just long
//! enough to bump the refcount — and then queries the returned
//! `Arc<RegionIndex>`, which is immutable and therefore lock-free and
//! allocation-free. An update (add / remove / move a region) builds a fresh
//! index **outside** the read lock and swaps the pointer in under a brief write
//! lock, so a reader always sees a whole index and never a half-built one.
//! This mirrors the permission store's snapshot discipline (CMD-01/CMD-02);
//! see [`crate::permissions`].
//!
//! An update takes effect at the *next* snapshot: the `Arc` a caller already
//! holds keeps answering from the old, consistent index until it asks again,
//! which is what makes "create/move/remove takes effect at the next safe point,
//! never mid-operation" true.

pub use aether_region::*;

/// Whether `handle` holds the bypass for `world`.
///
/// Two nodes are accepted: the core [`BYPASS_PERMISSION`] and WorldGuard's
/// per-world `worldguard.region.bypass.<world>`. Resolution goes through the
/// permission store (CMD-01); an undeclared node is operator-only, so an
/// operator bypasses both and a non-operator only when granted one of them.
/// The per-world node is deliberately distinct from the global one, so a
/// grant in `overworld` does not also bypass `the_nether`.
pub fn bypass_in(handle: &crate::players::PlayerHandle, world: &str) -> bool {
    crate::permissions::check_for(&handle.name, handle.ip, BYPASS_PERMISSION, 0)
        || crate::permissions::check_for(
            &handle.name,
            handle.ip,
            &worldguard_bypass_permission(world),
            0,
        )
}

/// Whether `handle` holds the bypass for the engine's world.
///
/// The server currently serves one dimension ([`OVERWORLD`]); the hook call
/// sites that know a world use [`bypass_in`].
pub fn bypass(handle: &crate::players::PlayerHandle) -> bool {
    bypass_in(handle, OVERWORLD)
}

/// The actor a player's action is checked as.
pub fn actor(handle: &crate::players::PlayerHandle) -> Actor<'_> {
    actor_in(handle, OVERWORLD)
}

/// The actor for an action in `world`.
pub fn actor_in<'a>(handle: &'a crate::players::PlayerHandle, world: &str) -> Actor<'a> {
    Actor::with_bypass(&handle.name, bypass_in(handle, world))
}

/// The denial message for `action` at `pos`, or `None` when it is allowed.
///
/// This is the hook helper: it resolves the player's bypass and returns the
/// store's configured line for a [`Verdict::Deny`]. A [`Verdict::Defer`] is
/// already resolved by [`RegionStore::check`].
pub fn denial(
    handle: &crate::players::PlayerHandle,
    pos: BlockPos,
    action: Action,
) -> Option<String> {
    let store = store();
    match store.check(OVERWORLD, pos, action, &actor(handle)) {
        Verdict::Deny => Some(store.deny_message().to_string()),
        _ => None,
    }
}

/// [`denial`] in block coordinates.
pub fn denial_at(
    handle: &crate::players::PlayerHandle,
    x: i32,
    y: i32,
    z: i32,
    action: Action,
) -> Option<String> {
    denial(handle, BlockPos::new(x, y, z), action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(id: u64, min: (i32, i32, i32), max: (i32, i32, i32)) -> Region {
        Region::new(
            RegionId(id),
            "overworld",
            BlockPos::new(min.0, min.1, min.2),
            BlockPos::new(max.0, max.1, max.2),
        )
    }

    #[test]
    fn bypass_allows_an_op_and_denies_a_non_op() {
        let _guard = crate::permissions::test_lock();
        crate::permissions::seed_operators(&["RegionOp".to_string()]);
        let op = crate::players::PlayerHandle::for_test(1, "RegionOp");
        let guest = crate::players::PlayerHandle::for_test(2, "RegionGuest");
        assert!(bypass(&op), "an operator holds the undeclared bypass node");
        assert!(!bypass(&guest), "a non-op does not");

        let _store = install_test_store(RegionStore::new());
        store().upsert(region(1, (0, 0, 0), (15, 255, 15)).with_flag("build", false));
        assert_eq!(
            store().check(
                "overworld",
                BlockPos::new(8, 64, 8),
                Action::Break,
                &actor(&op)
            ),
            Verdict::Allow
        );
        assert_eq!(
            store().check(
                "overworld",
                BlockPos::new(8, 64, 8),
                Action::Break,
                &actor(&guest)
            ),
            Verdict::Deny
        );
    }

    #[test]
    fn the_denial_message_comes_from_the_store() {
        let _store = install_test_store(RegionStore::new());
        store().set_deny_message("This area is protected.");
        let guest = crate::players::PlayerHandle::for_test(3, "RegionGuest");
        store().upsert(region(1, (0, 0, 0), (15, 255, 15)));
        assert_eq!(
            denial_at(&guest, 8, 64, 8, Action::Break).as_deref(),
            Some("This area is protected.")
        );
        // No region covers this cell: the hook allows.
        assert_eq!(denial_at(&guest, 1_000, 64, 1_000, Action::Break), None);
    }
    #[test]
    fn the_bypass_node_is_per_world_and_an_operator_still_bypasses() {
        let _guard = crate::permissions::test_lock();
        let mut store = crate::permissions::Store::new();
        store
            .set_player_node(
                "NetherHolder",
                "worldguard.region.bypass.the_nether",
                None,
                Some(true),
            )
            .unwrap();
        store
            .set_player_node(
                "OverworldHolder",
                "worldguard.region.bypass.overworld",
                None,
                Some(true),
            )
            .unwrap();
        crate::permissions::install_store_for_test(store);

        let nether = crate::players::PlayerHandle::for_test(11, "NetherHolder");
        let overworld = crate::players::PlayerHandle::for_test(12, "OverworldHolder");
        let guest = crate::players::PlayerHandle::for_test(13, "RegionGuest");
        let op = crate::players::PlayerHandle::for_test(14, "RegionOp");
        crate::permissions::seed_operators(&["RegionOp".to_string()]);

        assert!(bypass_in(&nether, "the_nether"));
        assert!(!bypass_in(&nether, OVERWORLD));

        assert!(bypass_in(&overworld, OVERWORLD));
        assert!(!bypass_in(&overworld, "the_nether"));

        assert!(!bypass_in(&guest, OVERWORLD));
        assert!(
            bypass_in(&op, "the_nether"),
            "an operator bypasses each world"
        );
    }
}
