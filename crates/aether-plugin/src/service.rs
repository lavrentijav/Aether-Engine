//! Cross-plugin services: a small registry a plugin publishes into and others
//! resolve by name and version.
//!
//! The native SDK plugin (`plugins/sdk`) is the first provider: it registers
//! its helpers under `sdk.<area>@0.1`, and dependents — native or WASM —
//! resolve them before they run. A WASM dependent reaches a service through a
//! host import that forwards here; a native one gets the value directly.
//!
//! The registry is deliberately untyped at rest (`Arc<dyn Any>`): a service can
//! be any Rust value, and the typed accessors in [`crate::sdk`] downcast to the
//! service trait the caller expects. A wrong version or a missing provider is
//! `None`, never a panic — that is what lets an optional service be probed.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};

/// A service identity: a name and a version, e.g. `sdk.permissions@0.1`.
///
/// The version is part of the key, so an incompatible new service does not
/// silently replace the old one; a host may hold several at once.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceRef {
    /// Service name, dotted for grouping: `sdk.permissions`, `economy`.
    pub name: String,
    /// Service version, e.g. `0.1`.
    pub version: String,
}

impl ServiceRef {
    /// Build a reference from a name and version.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> ServiceRef {
        ServiceRef {
            name: name.into(),
            version: version.into(),
        }
    }

    /// Parse the `name@version` spelling a manifest uses.
    pub fn parse(text: &str) -> Result<ServiceRef, String> {
        let (name, version) = text
            .split_once('@')
            .ok_or_else(|| format!("service `{text}` must be spelled `name@version`"))?;
        if name.is_empty() {
            return Err(format!("service `{text}` has an empty name"));
        }
        if version.is_empty() {
            return Err(format!("service `{text}` has an empty version"));
        }
        Ok(ServiceRef::new(name, version))
    }
}

impl fmt::Display for ServiceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// The services currently provided, keyed by name and version.
///
/// Interior mutability (an `RwLock`) so it can be shared as `Arc<ServiceRegistry>`
/// by the host, the WASM import bridge and every native [`crate::NativeContext`]
/// at once. Registration happens in a provider's `init`; reads happen whenever a
/// dependent calls a service. The lock is never held across a service call:
/// [`Self::get`] clones the `Arc` and releases it first, so a service method may
/// itself resolve another service without deadlocking.
#[derive(Default)]
pub struct ServiceRegistry {
    map: RwLock<BTreeMap<ServiceRef, Arc<dyn Any + Send + Sync>>>,
}

impl ServiceRegistry {
    /// An empty registry.
    pub fn new() -> ServiceRegistry {
        ServiceRegistry::default()
    }

    /// Publish `service` under `name`/`version`. Two providers of the same
    /// identity is a configuration error, so the second is refused.
    pub fn register<T: Send + Sync + 'static>(
        &self,
        name: &str,
        version: &str,
        service: Arc<T>,
    ) -> Result<(), String> {
        let key = ServiceRef::new(name, version);
        let mut map = self.map.write().unwrap();
        if map.contains_key(&key) {
            return Err(format!("service `{key}` is already registered"));
        }
        map.insert(key, service);
        Ok(())
    }

    /// Whether `service` is registered.
    pub fn contains(&self, service: &ServiceRef) -> bool {
        self.map.read().unwrap().contains_key(service)
    }

    /// Resolve `name`/`version` to the concrete type it was registered as.
    ///
    /// `None` covers both "no provider" and "provider of another type"; the
    /// typed helpers in [`crate::sdk`] are the intended callers.
    pub fn get<T: Send + Sync + 'static>(&self, name: &str, version: &str) -> Option<Arc<T>> {
        let key = ServiceRef::new(name, version);
        let value = self.map.read().unwrap().get(&key)?.clone();
        value.downcast::<T>().ok()
    }

    /// Every registered service, for diagnostics.
    pub fn refs(&self) -> Vec<ServiceRef> {
        self.map.read().unwrap().keys().cloned().collect()
    }

    /// How many services are registered.
    pub fn len(&self) -> usize {
        self.map.read().unwrap().len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.map.read().unwrap().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_parses_only_from_name_at_version() {
        assert_eq!(
            ServiceRef::parse("sdk.permissions@0.1").unwrap(),
            ServiceRef::new("sdk.permissions", "0.1")
        );
        assert!(ServiceRef::parse("sdk").is_err());
        assert!(ServiceRef::parse("@0.1").is_err());
        assert!(ServiceRef::parse("sdk@").is_err());
    }

    #[test]
    fn a_registered_service_is_resolved_by_type_and_version() {
        let reg = ServiceRegistry::new();
        reg.register("thing", "0.1", Arc::new(41u32)).unwrap();
        assert_eq!(*reg.get::<u32>("thing", "0.1").unwrap(), 41);
        // Wrong version, wrong type and missing name are all `None`.
        assert!(reg.get::<u32>("thing", "0.2").is_none());
        assert!(reg.get::<u64>("thing", "0.1").is_none());
        assert!(reg.get::<u32>("other", "0.1").is_none());
    }

    #[test]
    fn two_providers_of_one_identity_are_refused() {
        let reg = ServiceRegistry::new();
        reg.register("thing", "0.1", Arc::new(1u8)).unwrap();
        let err = reg.register("thing", "0.1", Arc::new(2u8)).unwrap_err();
        assert!(err.contains("already registered"), "{err}");
        // The first provider is untouched.
        assert_eq!(*reg.get::<u8>("thing", "0.1").unwrap(), 1);
    }
}
