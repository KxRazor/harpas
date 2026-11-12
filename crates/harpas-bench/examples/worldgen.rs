//! World generation benchmark: the time to generate Overworld chunks up to the carvers, on one thread.
//!
//! It needs the vanilla data pack that the server extracts from Mojang's jar on its first start
//! (`docs/world.md#vanilla-game-data`):
//!
//! ```sh
//! cargo run --release -p harpas-bench --example worldgen -- <server>/.harpas/vanilla-data/26.1.zip
//! ```
//!
//! The chunks are a square around the origin of a fixed seed, generated without structures so that each chunk
//! costs about the same and none reads its neighbors.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use harpas_engine::world::ChunkPos;
use harpas_worldgen::{StructureTemplates, WorldgenData};

const SEED: i64 = 20_261_112;
/// The chunks are a square of this many on each side.
const SIDE: i32 = 12;
const RUNS: usize = 3;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(pack) = std::env::args().nth(1) else {
        return Err("usage: worldgen <vanilla data pack>".into());
    };
    let data = Arc::new(WorldgenData::from_zip_path(std::path::Path::new(&pack))?);
    let templates = Arc::new(StructureTemplates::from_zip_path(std::path::Path::new(
        &pack,
    ))?);
    let generator = harpas_engine::worldgen::overworld_generator(
        SEED,
        data,
        templates,
        false,
        harpas_java::biome_names(),
    )?;
    let chunks = (SIDE * SIDE) as f64;
    let mut times = Vec::new();
    for run in 0..=RUNS {
        let start = Instant::now();
        for x in -SIDE / 2..SIDE / 2 {
            for z in -SIDE / 2..SIDE / 2 {
                black_box(generator(ChunkPos { x, z }, &|_| Ok(None))?);
            }
        }
        // The first run warms the caches.
        if run > 0 {
            times.push(start.elapsed().as_secs_f64() * 1000.0 / chunks);
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{} chunks, {RUNS} runs: median {:.2} ms per chunk (fastest {:.2}, slowest {:.2})",
        SIDE * SIDE,
        times[RUNS / 2],
        times[0],
        times[RUNS - 1]
    );
    Ok(())
}
