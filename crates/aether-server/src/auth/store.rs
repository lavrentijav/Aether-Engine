//! The premium-name lock store.
//!
//! In hybrid mode a name that has ever completed online auth is *locked* to
//! the Mojang uuid that authenticated it. The lock lives in the world's KV
//! store, under [`aether_api::keys::AUTH_LOCK`], in two directions:
//!
//! * `AUTH` + `n` + lowercased name → [`Lock`] (uuid, first/last seen);
//! * `AUTH` + `u` + uuid bytes → the lowercased name, so a locked player
//!   arriving under a new name can move the lock rather than be refused.
//!
//! The store is deliberately dumb: it does not know about time, policy or
//! HTTP. Everything above it ([`super::policy`]) decides *when* a lock is
//! written.

use aether_api::keys::AUTH_LOCK;

/// A lock: the uuid a name belongs to, and when we saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lock {
    /// The Mojang uuid that authenticated this name.
    pub uuid: u128,
    /// The player name, lowercased. Matches are case-insensitive, and the
    /// canonical spelling is the lowercase one.
    pub name: String,
    /// Unix seconds when the name first completed online auth.
    pub first_seen: i64,
    /// Unix seconds of the most recent online auth.
    pub last_seen: i64,
}

/// The subset of the world store the lock table needs.
///
/// A trait rather than the concrete `World` so the store can be tested
/// against an in-memory map without a disk.
pub trait MetaStore: Send + Sync {
    /// Read a blob, `None` when absent.
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    /// Write a blob.
    fn put(&self, key: &[u8], value: &[u8]);
    /// Remove a blob.
    fn del(&self, key: &[u8]);
}

/// The name-index key for `name_lower`.
fn name_key(name_lower: &str) -> Vec<u8> {
    let mut k = AUTH_LOCK.to_vec();
    k.push(b'n');
    k.extend_from_slice(name_lower.as_bytes());
    k
}

/// The uuid-index key for `uuid`.
fn uuid_key(uuid: u128) -> Vec<u8> {
    let mut k = AUTH_LOCK.to_vec();
    k.push(b'u');
    k.extend_from_slice(&uuid.to_be_bytes());
    k
}

/// Encode the lock blob: uuid (16) | first_seen (i64) | last_seen (i64).
fn encode(lock: &Lock) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(&lock.uuid.to_be_bytes());
    out.extend_from_slice(&lock.first_seen.to_be_bytes());
    out.extend_from_slice(&lock.last_seen.to_be_bytes());
    out
}

/// Decode a lock blob. `name` is carried by the key, not the value.
fn decode(name: &str, bytes: &[u8]) -> Option<Lock> {
    if bytes.len() != 32 {
        return None;
    }
    let uuid = u128::from_be_bytes(bytes[0..16].try_into().ok()?);
    let first_seen = i64::from_be_bytes(bytes[16..24].try_into().ok()?);
    let last_seen = i64::from_be_bytes(bytes[24..32].try_into().ok()?);
    Some(Lock {
        uuid,
        name: name.to_string(),
        first_seen,
        last_seen,
    })
}

/// The result of a locked-name check.
///
/// A corrupt blob is its own case, not "missing": a security boundary must
/// fail **closed**, so the caller refuses rather than treating an unreadable
/// lock as an unlocked name.
#[derive(Debug)]
pub enum Lookup {
    /// No lock for this name.
    Missing,
    /// The name is locked.
    Locked(Lock),
    /// A lock key exists but does not decode. Refuse.
    Corrupt,
}

/// Whether `name` is locked, case-insensitively — failing closed on a corrupt
/// blob.
pub fn check(store: &dyn MetaStore, name: &str) -> Lookup {
    let lower = name.to_ascii_lowercase();
    let Some(bytes) = store.get(&name_key(&lower)) else {
        return Lookup::Missing;
    };
    match decode(&lower, &bytes) {
        Some(lock) => Lookup::Locked(lock),
        None => Lookup::Corrupt,
    }
}

/// The lock on `name`, case-insensitively, if any. A corrupt blob reads as
/// `None` here; the locked-name check that must fail closed uses [`check`].
pub fn get_by_name(store: &dyn MetaStore, name: &str) -> Option<Lock> {
    match check(store, name) {
        Lookup::Locked(lock) => Some(lock),
        _ => None,
    }
}

/// The lock that belongs to `uuid`, whichever name it currently holds.
pub fn get_by_uuid(store: &dyn MetaStore, uuid: u128) -> Option<Lock> {
    let bytes = store.get(&uuid_key(uuid))?;
    let name = String::from_utf8(bytes).ok()?;
    get_by_name(store, &name)
}

/// Write a lock, both directions.
///
/// The two invariants:
///
/// * a name changing hands (a different uuid now owns it) clears the *old*
///   owner's reverse index, or a stale `u/<old>` keeps resolving the name
///   through the name index to the new owner;
/// * a uuid renaming at Mojang releases its *own* old name — but only if that
///   name is still that uuid's. Resolving the old name through the name index
///   without checking the uuid is exactly the bug that let one player delete
///   another's lock.
///
/// The new name is written before the old is released, so a crash mid-write
/// leaves a name locked rather than unlocked.
pub fn put(store: &dyn MetaStore, lock: &Lock) {
    let lower = lock.name.to_ascii_lowercase();

    // Capture the uuid (if any) that currently owns this name, so its reverse
    // index can be cleared after the forward index is rewritten.
    let displaced = get_by_name(store, &lower).map(|l| l.uuid);
    // The name (if any) this uuid used to hold, and whether it is still its
    // own (a stale `u/<uuid>` can point at a name now owned by someone else).
    let old_name = get_by_uuid(store, lock.uuid)
        .filter(|old| old.uuid == lock.uuid && !old.name.eq_ignore_ascii_case(&lower))
        .map(|old| old.name);

    store.put(&name_key(&lower), &encode(lock));
    store.put(&uuid_key(lock.uuid), lower.as_bytes());

    if let Some(prev) = displaced {
        if prev != lock.uuid {
            store.del(&uuid_key(prev));
        }
    }
    if let Some(name) = old_name {
        store.del(&name_key(&name));
    }
}

/// Write a deliberately unreadable lock blob for `name`. Tests only.
#[cfg(test)]
pub(crate) fn set_corrupt(store: &dyn MetaStore, name: &str) {
    let lower = name.to_ascii_lowercase();
    store.put(&name_key(&lower), &[0u8; 5]);
}

#[cfg(test)]
pub(crate) mod test_store {
    use super::MetaStore;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// An in-memory [`MetaStore`] for tests.
    #[derive(Default)]
    pub struct MapStore(pub Mutex<HashMap<Vec<u8>, Vec<u8>>>);

    impl MetaStore for MapStore {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn put(&self, key: &[u8], value: &[u8]) {
            self.0.lock().unwrap().insert(key.to_vec(), value.to_vec());
        }
        fn del(&self, key: &[u8]) {
            self.0.lock().unwrap().remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_store::MapStore;
    use super::*;

    fn lock(uuid: u128, name: &str, first: i64, last: i64) -> Lock {
        Lock {
            uuid,
            name: name.to_ascii_lowercase(),
            first_seen: first,
            last_seen: last,
        }
    }

    #[test]
    fn a_lock_round_trips_through_both_indexes() {
        let store = MapStore::default();
        let l = lock(0x1234, "Notch", 100, 200);
        put(&store, &l);
        assert_eq!(get_by_name(&store, "notch"), Some(l.clone()));
        assert_eq!(get_by_name(&store, "NOTCH"), Some(l.clone()));
        assert_eq!(get_by_uuid(&store, 0x1234), Some(l));
    }

    #[test]
    fn an_unlocked_name_is_none() {
        let store = MapStore::default();
        assert_eq!(get_by_name(&store, "someone"), None);
        assert_eq!(get_by_uuid(&store, 42), None);
    }

    #[test]
    fn a_name_change_moves_the_lock_and_releases_the_old_name() {
        let store = MapStore::default();
        put(&store, &lock(7, "OldName", 1, 2));
        assert!(get_by_name(&store, "oldname").is_some());

        // Same uuid, new name: the lock follows the uuid.
        put(&store, &lock(7, "NewName", 1, 3));
        assert_eq!(get_by_name(&store, "oldname"), None);
        assert_eq!(get_by_name(&store, "newname").unwrap().uuid, 7);
        assert_eq!(get_by_uuid(&store, 7).unwrap().name, "newname");
    }

    #[test]
    fn a_corrupt_blob_is_reported_as_corrupt_not_missing() {
        // A security check must fail closed: a key that exists but does not
        // decode is Corrupt, never Missing, so the caller refuses.
        let store = MapStore::default();
        store.put(&name_key("x"), &[0u8; 5]);
        assert!(matches!(check(&store, "x"), Lookup::Corrupt));
        assert_eq!(
            get_by_name(&store, "x"),
            None,
            "non-fatal readers read None"
        );
    }

    #[test]
    fn a_handover_then_the_old_owners_rename_does_not_delete_the_new_lock() {
        // Regression for the review's finding 2: a stale uuid->name index let
        // the old owner of a name delete the new owner's lock.
        let store = MapStore::default();
        // 1. Alice (uuid A) locks `alice`.
        put(&store, &lock(0xAA, "alice", 1, 1));
        assert_eq!(get_by_name(&store, "alice").unwrap().uuid, 0xAA);

        // 2. Bob (uuid B) becomes `alice` at Mojang and logs in online. The
        //    name is handed over; A's reverse index must be cleared.
        put(&store, &lock(0xBB, "alice", 2, 2));
        assert_eq!(get_by_name(&store, "alice").unwrap().uuid, 0xBB);
        assert_eq!(get_by_uuid(&store, 0xAA), None, "A's stale index is gone");

        // 3. Alice (uuid A) logs in under a new name. This must NOT delete
        //    Bob's `alice` lock.
        put(&store, &lock(0xAA, "bob", 1, 3));
        assert_eq!(
            get_by_name(&store, "alice").map(|l| l.uuid),
            Some(0xBB),
            "Bob's lock survives Alice's rename"
        );
        assert_eq!(get_by_name(&store, "bob").unwrap().uuid, 0xAA);
    }

    #[test]
    fn a_handover_clears_the_displaced_owners_reverse_index() {
        let store = MapStore::default();
        put(&store, &lock(1, "name", 1, 1));
        put(&store, &lock(2, "name", 2, 2));
        assert_eq!(get_by_uuid(&store, 1), None);
        assert_eq!(get_by_uuid(&store, 2).unwrap().name, "name");
    }
}
