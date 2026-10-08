//! A from-scratch port of the classic Minecraft Beta/1.8-era terrain
//! algorithm's *shape* — real gradient noise, a genuine 3D density field
//! (so overhangs and caves fall out for free), per-column control noise —
//! without chasing bit-exact parity with the Java original. See
//! [`terrain`] for the honest list of what's preserved vs simplified.

pub mod octaves;
pub mod perlin;
pub mod random;
pub mod terrain;

pub use terrain::{BetaTerrain, ColumnControl, WORLD_HEIGHT};
