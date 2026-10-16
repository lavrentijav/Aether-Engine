//! The voice chat plugin (VC1): a native server side for **Simple Voice Chat**
//! (henkelmax) and **Plasmo Voice**.
//!
//! This is a native plugin (tier N, per `docs/design/plugins.md`): it is linked
//! into the server and owns its own UDP servers on threads off the tick. The
//! protocol modules are pure — bytes in, a packet out — and are pinned against
//! the mods' own source, so a captured packet can be replayed in a test.
//!
//! Build order (all one branch `oc/VC1`): VC1a Simple Voice Chat, VC1b Plasmo
//! Voice, VC1c the bridge, the admin commands and the recording.

pub mod bytes;
pub mod config;
pub mod crypto;
pub mod plasmo;
pub mod svc;
