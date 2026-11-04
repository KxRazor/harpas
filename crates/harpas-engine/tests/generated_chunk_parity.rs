//! The chunk workers generate a real chunk from vanilla's data and save it. Checks Harpas alone, as
//! `chunk_parity` in `harpas-worldgen` compares the terrain itself.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use harpas_core::nbt::Nbt;
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::overworld_generator;
use harpas_worldgen::WorldgenData;
use reference::VanillaData;

#[test]
fn workers_generate_and_persist_a_real_chunk() {
    let Some(vanilla) = VanillaData::find() else {
        eprintln!("Skipping generated chunks: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let folder = std::env::temp_dir().join(format!(
        "harpas-generated-chunk-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&folder).unwrap();
    let data = Arc::new(WorldgenData::from_zip_path(&vanilla.path).unwrap());
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let generator = overworld_generator(42, Arc::clone(&data), &biomes).unwrap();
    let pos = ChunkPos { x: 0, z: 0 };
    let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let expected = loop {
        world.update([pos]).unwrap();
        if let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) {
            break chunk.to_nbt(&biomes);
        }
        assert!(Instant::now() < deadline, "chunk generation timed out");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(
        expected.get("Status"),
        Some(&Nbt::from("minecraft:carvers"))
    );
    world.flush().unwrap();
    drop(world);
    let mut world = World::open(&folder, &biomes).unwrap();
    let actual = loop {
        world.update([pos]).unwrap();
        if let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) {
            break chunk.to_nbt(&biomes);
        }
        assert!(Instant::now() < deadline, "saved chunk did not reload");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(actual, expected);
    drop(world);
    std::fs::remove_dir_all(folder).unwrap();
}
