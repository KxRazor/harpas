//! Vanilla's Anvil world format
//!
//! [`Region`] reads and writes region files, the `r.x.z.mca` files of a dimension's `region/`, `entities/` and
//! `poi/` folders, with the external `c.x.z.mcc` files of chunks too large for them. [`Chunk`] turns a chunk's
//! NBT into sections and back. [`read_level`] and [`read_player`] read `level.dat` and player data, which are
//! saved with vanilla's backups. The format knows nothing of the engine: it takes and gives NBT and sections, so
//! the engine, its worker threads and the parity tools share it.
//!
//! Design: `docs/world.md#save-format-anvil`.

mod chunk;
mod file;
mod region;
mod world;

pub use chunk::{Chunk, ChunkSection};
pub use file::Source;
pub use region::{Region, region_of};
pub use world::{dimension_dir, read_level, read_player, write_level, write_player};
