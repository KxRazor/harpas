//! Vanilla's Anvil world format
//!
//! [`Region`] reads and writes region files, the `r.x.z.mca` files of a world's `region/`, `entities/` and `poi/`
//! folders, with the external `c.x.z.mcc` files of chunks too large for them. The format knows nothing of the
//! engine: it takes and gives NBT, so the engine, its worker threads and the parity tools share it.
//!
//! Design: `docs/world.md#save-format-anvil`.

mod region;

pub use region::{Region, region_of};
