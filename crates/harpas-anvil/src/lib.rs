//! Vanilla's Anvil world format
//!
//! [`Region`] reads and writes region files, the `r.x.z.mca` files of a dimension's `region/`, `entities/` and
//! `poi/` folders, with the external `c.x.z.mcc` files of chunks too large for them. [`Chunk`] turns a chunk's
//! NBT into sections and back. [`read_level`] and [`read_player`] read `level.dat` and player data, which are
//! saved with vanilla's backups. [`check_level`] and [`check_chunk`] refuse what another Minecraft version
//! saved. [`recover`] completes or discards region saves interrupted by a crash. The format knows nothing of the
//! engine: it takes and gives NBT and sections, so the engine, its worker threads and the parity tools share it.
//!
//! Design: `docs/world.md#save-format-anvil`.

mod chunk;
mod file;
mod journal;
mod region;
mod version;
mod world;

pub use chunk::{Chunk, ChunkSection};
pub use file::Source;
pub use journal::{Recovery, recover};
pub use region::{Region, read_chunk, region_of};
pub use version::{
    DATA_VERSION, MINECRAFT_VERSION, SUPPORTED, VersionError, check_chunk, check_level,
};
pub use world::{
    dimension_dir, open_or_create_world_gen_settings, overworld_spawn, read_level, read_player,
    read_world_gen_settings, read_world_seed, write_level, write_player, write_world_gen_settings,
};
