//! A world saved by vanilla, opened and saved again by the server, then opened by vanilla: every chunk the server
//! rewrites must hold exactly what vanilla saved, and vanilla must read them back without trouble.
//!
//! Vanilla generates the spawn area of a world, with blocks whose block entities hold data placed by commands,
//! and stops. The server then loads every chunk vanilla saved, at every status, marks each one changed so that it
//! writes the whole chunk again, and saves. The chunks are compared as NBT, whatever the order of their keys.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use harpas_anvil::{dimension_dir, read_chunk};
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_worldgen::WorldgenData;
use reference::{Reference, Server, assert_no_chunk_errors};

const SEED: i64 = 42;
/// How far around the spawn chunk to look for the chunks vanilla saved.
const SEARCH: i32 = 24;
const TIMEOUT: Duration = Duration::from_secs(300);

/// Blocks with block entities placed above the spawn, relative to it, with the data vanilla stores for them.
const BLOCKS: [(i32, &str); 5] = [
    (
        0,
        r#"minecraft:chest[facing=east]{Items:[{Slot:0b,id:"minecraft:diamond",count:3},{Slot:13b,id:"minecraft:oak_log",count:64}]}"#,
    ),
    (
        2,
        r#"minecraft:furnace{Items:[{Slot:0b,id:"minecraft:iron_ore",count:5}],cooking_time_spent:20s}"#,
    ),
    (
        4,
        r#"minecraft:spawner{SpawnData:{entity:{id:"minecraft:zombie"}},Delay:40s}"#,
    ),
    (
        6,
        r#"minecraft:white_banner{patterns:[{pattern:"minecraft:creeper",color:"green"}]}"#,
    ),
    (8, r#"minecraft:barrel[open=true]{CustomName:"Supplies"}"#),
];

/// Writes the server's files for a world in `dir`.
fn prepare(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();
    fs::write(
        dir.join("server.properties"),
        format!("online-mode=false\nserver-ip=127.0.0.1\nserver-port=0\nlevel-seed={SEED}\n"),
    )
    .unwrap();
}

/// Every chunk saved around the spawn chunk, by position.
fn saved(region: &Path, spawn: ChunkPos) -> BTreeMap<(i32, i32), Compound> {
    let mut chunks = BTreeMap::new();
    for x in spawn.x - SEARCH..=spawn.x + SEARCH {
        for z in spawn.z - SEARCH..=spawn.z + SEARCH {
            if let Some(chunk) = read_chunk(region, x, z).unwrap() {
                chunks.insert((x, z), chunk);
            }
        }
    }
    assert!(
        chunks
            .keys()
            .all(|&(x, z)| (x - spawn.x).abs() < SEARCH && (z - spawn.z).abs() < SEARCH),
        "vanilla saved chunks up to the edge of the search"
    );
    chunks
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The first difference between two NBT values, as a path, ignoring the order of compound keys.
fn difference(path: &str, vanilla: &Nbt, harpas: &Nbt) -> Option<String> {
    match (vanilla, harpas) {
        (Nbt::Compound(a), Nbt::Compound(b)) => compound_difference(path, a, b),
        (Nbt::List(List::Compound(a)), Nbt::List(List::Compound(b))) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .find_map(|(i, (a, b))| compound_difference(&format!("{path}[{i}]"), a, b)),
        _ if vanilla == harpas => None,
        _ => Some(format!("{path}: vanilla {vanilla:?}, Harpas {harpas:?}")),
    }
}

fn compound_difference(path: &str, vanilla: &Compound, harpas: &Compound) -> Option<String> {
    let mut keys: Vec<&str> = vanilla.iter().map(|(key, _)| key).collect();
    keys.extend(harpas.iter().map(|(key, _)| key));
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter().find_map(|key| {
        let path = format!("{path}.{key}");
        match (vanilla.get(key), harpas.get(key)) {
            (Some(a), Some(b)) => difference(&path, a, b),
            (Some(_), None) => Some(format!("{path}: missing in Harpas")),
            (None, Some(b)) => Some(format!("{path}: only in Harpas, {b:?}")),
            (None, None) => None,
        }
    })
}

fn status(chunk: &Compound) -> String {
    match chunk.get("Status") {
        Some(Nbt::String(status)) => status.clone(),
        _ => panic!("saved chunk without a status"),
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn a_vanilla_world_saved_again_by_the_server_holds_what_vanilla_saved() {
    let reference = Reference::required();
    let vanilla_dir = reference.temp.0.join("vanilla");
    prepare(&vanilla_dir);

    // Vanilla prepares the spawn chunks, then places blocks with data above the spawn and stops.
    assert_no_chunk_errors(&Server::start(&reference, &vanilla_dir).stop());
    let world = vanilla_dir.join("world");
    let level = harpas_anvil::read_level(&world).unwrap().unwrap().0;
    let [sx, _, sz] = harpas_anvil::overworld_spawn(&level).unwrap();
    let mut server = Server::start(&reference, &vanilla_dir);
    // A square of full chunks around the spawn, kept loaded so that commands reach them.
    let (x0, z0, x1, z1) = (sx - 48, sz - 48, sx + 48, sz + 48);
    server.command(&format!("forceload add {x0} {z0} {x1} {z1}"));
    let deadline = Instant::now() + TIMEOUT;
    loop {
        server.command(&format!(
            "execute if loaded {x0} 0 {z0} if loaded {x1} 0 {z1} if loaded {x0} 0 {z1} if loaded {x1} 0 {z0} \
             run say loaded"
        ));
        if server.poll("loaded", Duration::from_secs(1)) {
            break;
        }
        assert!(Instant::now() < deadline, "vanilla did not load the spawn");
    }
    for (dx, block) in BLOCKS {
        server.command(&format!("setblock {} 200 {sz} {block}", sx + dx));
        assert!(
            server.poll("Changed the block", Duration::from_secs(20)),
            "vanilla did not place {block}:\n{}",
            server.tail(5)
        );
    }
    server.command("save-all flush");
    server.wait_for("Saved the game");
    assert_no_chunk_errors(&server.stop());

    let spawn = ChunkPos {
        x: sx.div_euclid(16),
        z: sz.div_euclid(16),
    };
    let region = |world: &Path| dimension_dir(world, "minecraft:overworld").join("region");
    let vanilla = saved(&region(&world), spawn);
    let statuses: BTreeMap<String, usize> =
        vanilla.values().fold(BTreeMap::new(), |mut counts, chunk| {
            *counts.entry(status(chunk)).or_default() += 1;
            counts
        });
    for wanted in ["minecraft:full", "minecraft:carvers"] {
        assert!(
            statuses.contains_key(wanted),
            "vanilla saved no chunk at {wanted}: {statuses:?}"
        );
    }

    // The server loads every chunk, marks it changed and saves the world.
    let harpas_dir = reference.temp.0.join("harpas");
    prepare(&harpas_dir);
    let harpas_world = harpas_dir.join("world");
    copy_dir(&world, &harpas_world);
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let wanted: Vec<_> = vanilla.keys().map(|&(x, z)| ChunkPos { x, z }).collect();
    let mut opened = World::open(&harpas_world, &biomes).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !wanted.iter().all(|&pos| match opened.chunk(pos) {
        Some(ChunkState::Loaded(_)) => true,
        Some(ChunkState::Failed(error)) => panic!("chunk {pos:?} failed: {error}"),
        _ => false,
    }) {
        opened.update(wanted.iter().copied()).unwrap();
        assert!(Instant::now() < deadline, "timed out loading chunks");
        std::thread::sleep(Duration::from_millis(5));
    }
    for &pos in &wanted {
        opened.chunk_mut(pos).expect("a loaded chunk");
    }
    opened.flush().unwrap();
    drop(opened);

    // The server wrote the regions again rather than leaving vanilla's files.
    let rewritten = fs::read_dir(region(&world))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| {
            fs::read(region(&world).join(name)).unwrap()
                != fs::read(region(&harpas_world).join(name)).unwrap()
        })
        .count();
    assert!(
        rewritten > 0,
        "the server left every region file as vanilla wrote it"
    );
    let harpas = saved(&region(&harpas_world), spawn);
    let chest_items = harpas.values().any(|chunk| {
        matches!(chunk.get("block_entities"), Some(Nbt::List(List::Compound(entities)))
            if entities.iter().any(|e| e.get("id") == Some(&Nbt::from("minecraft:chest")) && e.get("Items").is_some()))
    });
    assert!(
        chest_items,
        "the comparison includes the chest and its items"
    );
    assert_eq!(
        harpas.keys().collect::<Vec<_>>(),
        vanilla.keys().collect::<Vec<_>>(),
        "the server saved the same chunks"
    );
    let mut failures = Vec::new();
    for (pos, chunk) in &vanilla {
        if let Some(difference) = compound_difference(&format!("{pos:?}"), chunk, &harpas[pos]) {
            failures.push(format!("{} {difference}", status(chunk)));
        }
    }
    let shown: Vec<_> = failures
        .iter()
        .take(20)
        .map(|f| f.chars().take(400).collect::<String>())
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} chunks differ ({statuses:?}):\n{}",
        failures.len(),
        vanilla.len(),
        shown.join("\n")
    );

    // Vanilla opens the server's world and saves it again without trouble.
    let log = Server::start(&reference, &harpas_dir).stop();
    assert_no_chunk_errors(&log);
    eprintln!(
        "{} chunks in {rewritten} rewritten regions round-tripped: {statuses:?}",
        vanilla.len()
    );
}
