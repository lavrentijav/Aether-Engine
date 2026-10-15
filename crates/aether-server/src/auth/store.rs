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

/// The lock on `name`, case-insensitively, if any.
pub fn get_by_name(store: &dyn MetaStore, name: &str) -> Option<Lock> {
    let lower = name.to_ascii_lowercase();
    let bytes = store.get(&name_key(&lower))?;
    decode(&lower, &bytes)
}

/// The lock that belongs to `uuid`, whichever name it currently holds.
pub fn get_by_uuid(store: &dyn MetaStore, uuid: u128) -> Option<Lock> {
    let bytes = store.get(&uuid_key(uuid))?;
    let name = String::from_utf8(bytes).ok()?;
    get_by_name(store, &name)
}

/// Write a lock, both directions.
///
/// If the uuid already held a *different* name, that older name's lock is
/// removed: a name change at Mojang moves the lock rather than leaving the
/// abandoned name locked forever.
pub fn put(store: &dyn MetaStore, lock: &Lock) {
    let lower = lock.name.to_ascii_lowercase();
    if let Some(old) = get_by_uuid(store, lock.uuid) {
        if !old.name.eq_ignore_ascii_case(&lower) {
            store.del(&name_key(&old.name));
        }
    }
    store.put(&name_key(&lower), &encode(lock));
    store.put(&uuid_key(lock.uuid), lower.as_bytes());
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
    fn a_corrupt_blob_reads_as_unlocked_not_a_panic() {
        let store = MapStore::default();
        store.put(&name_key("x"), &[0u8; 5]);
        assert_eq!(get_by_name(&store, "x"), None);
    }
}
