//! A world that vanilla stopped part way through generating: the chunk workers resume the proto-chunks at its edge
//! and generate past it, and vanilla then opens the result and completes it.
//!
//! The reference server runs as a server here, so its chunk order depends on its threads. The test compares what
//! that order cannot change: which chunks each side resumes, which it leaves as saved, and that vanilla can complete
//! every chunk the workers left.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use harpas_anvil::{dimension_dir, read_chunk, read_world_seed};
use harpas_core::nbt::{Compound, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::{decorated_with_structures, overworld_generator, overworld_starts};
use harpas_worldgen::{StructureTemplates, WorldgenData};
use reference::Reference;

const SEED: i64 = -4_172_144_997_902_289_642;
/// How far around the spawn chunk to look for the chunks vanilla saved.
const SEARCH: i32 = 24;
const TIMEOUT: Duration = Duration::from_secs(300);

/// The reference server, with its console read on another thread.
struct Server {
    child: Child,
    lines: Receiver<String>,
    log: Vec<String>,
}

impl Server {
    fn start(reference: &Reference, dir: &Path) -> Self {
        let mut child = reference
            .server(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start the reference server");
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            lines,
            log: Vec::new(),
        };
        server.wait_for("Done (");
        server
    }

    fn command(&mut self, command: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{command}").unwrap();
        stdin.flush().unwrap();
    }

    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    let found = line.contains(text);
                    self.log.push(line);
                    if found {
                        return;
                    }
                }
                Err(_) => panic!(
                    "the reference server did not log {text:?}:\n{}",
                    self.log.join("\n")
                ),
            }
        }
    }

    /// Stops the server, which saves every chunk, and returns its console.
    fn stop(mut self) -> String {
        self.command("stop");
        let deadline = Instant::now() + TIMEOUT;
        while self.child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "the reference server did not stop"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.log.extend(self.lines.try_iter());
        self.log.join("\n")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn assert_no_chunk_errors(log: &str) {
    for error in [
        "Failed to load chunk",
        "Couldn't load chunk",
        "Recovering chunk",
        "Exception",
    ] {
        assert!(!log.contains(error), "vanilla logged {error:?}:\n{log}");
    }
}

fn status(chunk: &Compound) -> &str {
    match chunk.get("Status") {
        Some(Nbt::String(status)) => status,
        _ => panic!("saved chunk without a status"),
    }
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

fn load_all(world: &mut World, wanted: &[ChunkPos]) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        world.update(wanted.iter().copied()).unwrap();
        let mut loaded = 0;
        for &pos in wanted {
            match world.chunk(pos) {
                Some(ChunkState::Loaded(_)) => loaded += 1,
                Some(ChunkState::Failed(error)) => panic!("chunk {pos:?} failed: {error}"),
                _ => {}
            }
        }
        if loaded == wanted.len() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out loading chunks");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn the_workers_resume_the_edge_of_a_vanilla_world_and_vanilla_completes_theirs() {
    let reference = Reference::required();
    let folder = reference.temp.0.join("server");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("eula.txt"), "eula=true\n").unwrap();
    fs::write(
        folder.join("server.properties"),
        format!("online-mode=false\nserver-ip=127.0.0.1\nserver-port=0\nlevel-seed={SEED}\n"),
    )
    .unwrap();

    // Vanilla prepares the spawn chunks and stops, leaving proto-chunks around them.
    let log = Server::start(&reference, &folder).stop();
    assert_no_chunk_errors(&log);
    let world_dir = folder.join("world");
    assert_eq!(read_world_seed(&world_dir).unwrap().unwrap().0, SEED);
    let level = harpas_anvil::read_level(&world_dir).unwrap().unwrap().0;
    let [x, _, z] = harpas_anvil::overworld_spawn(&level).unwrap();
    let spawn = ChunkPos {
        x: x.div_euclid(16),
        z: z.div_euclid(16),
    };
    let region = dimension_dir(&world_dir, "minecraft:overworld").join("region");
    let vanilla = saved(&region, spawn);
    let count = |wanted: &str| vanilla.values().filter(|c| status(c) == wanted).count();
    for wanted in [
        "minecraft:full",
        "minecraft:initialize_light",
        "minecraft:carvers",
    ] {
        assert!(count(wanted) > 0, "vanilla saved no chunk at {wanted}");
    }

    // The workers load every chunk vanilla carved, and one more around them.
    let carved: Vec<_> = vanilla
        .iter()
        .filter(|(_, chunk)| {
            !matches!(
                status(chunk),
                "minecraft:empty"
                    | "minecraft:structure_starts"
                    | "minecraft:structure_references"
                    | "minecraft:biomes"
                    | "minecraft:noise"
                    | "minecraft:surface"
            )
        })
        .map(|(&pos, _)| pos)
        .collect();
    let min_x = carved.iter().map(|p| p.0).min().unwrap() - 1;
    let max_x = carved.iter().map(|p| p.0).max().unwrap() + 1;
    let min_z = carved.iter().map(|p| p.1).min().unwrap() - 1;
    let max_z = carved.iter().map(|p| p.1).max().unwrap() + 1;
    let wanted: Vec<_> = (min_x..=max_x)
        .flat_map(|x| (min_z..=max_z).map(move |z| ChunkPos { x, z }))
        .collect();
    let data = Arc::new(
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data"),
    );
    let templates = Arc::new(
        StructureTemplates::from_zip_path(&reference.server_jar).expect("load vanilla templates"),
    );
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let generator = overworld_generator(
        SEED,
        Arc::clone(&data),
        Arc::clone(&templates),
        true,
        &biomes,
    )
    .unwrap();
    let starts = overworld_starts(SEED, Arc::clone(&data), Arc::clone(&templates)).unwrap();
    let source =
        decorated_with_structures(generator, starts, SEED, Arc::clone(&data), templates).unwrap();
    let mut world = World::open_with_source(&world_dir, &biomes, source).unwrap();
    load_all(&mut world, &wanted);
    world.flush().unwrap();
    drop(world);

    let after = saved(&region, spawn);
    for (pos, before) in &vanilla {
        let now = &after[pos];
        match status(before) {
            // Final chunks are used as saved.
            "minecraft:light" | "minecraft:spawn" | "minecraft:full" => {
                assert_eq!(now, before, "{pos:?} was final, but the workers changed it");
            }
            // Resumed: decorated, with the neighbors' writes, and lit by vanilla from those blocks.
            "minecraft:initialize_light" | "minecraft:features" | "minecraft:carvers"
                if wanted.contains(&ChunkPos { x: pos.0, z: pos.1 }) =>
            {
                assert_eq!(status(now), "minecraft:features", "{pos:?} was resumed");
            }
            _ => {}
        }
    }
    let resumed = count("minecraft:initialize_light") + count("minecraft:features");
    eprintln!(
        "Resumed {resumed} decorated and {} carved vanilla chunks, and generated {} more",
        count("minecraft:carvers"),
        wanted.len() - carved.len(),
    );

    // Vanilla completes every chunk the workers loaded, which needs their neighbors.
    let mut server = Server::start(&reference, &folder);
    for x in min_x..=max_x {
        server.command(&format!(
            "forceload add {} {} {} {}",
            x * 16,
            min_z * 16,
            x * 16,
            max_z * 16
        ));
    }
    let corners = [
        (min_x, min_z),
        (min_x, max_z),
        (max_x, min_z),
        (max_x, max_z),
    ];
    let deadline = Instant::now() + TIMEOUT;
    let mut ready = 0;
    'loaded: loop {
        let checks: String = corners
            .iter()
            .map(|(x, z)| format!(" if loaded {} 0 {}", x * 16, z * 16))
            .collect();
        ready += 1;
        server.command(&format!("execute{checks} run say ready {ready}"));
        let wait = Instant::now() + Duration::from_secs(1);
        while Instant::now() < wait {
            if let Ok(line) = server.lines.recv_timeout(Duration::from_millis(100)) {
                let found = line.contains("ready ");
                server.log.push(line);
                if found {
                    break 'loaded;
                }
            }
        }
        assert!(Instant::now() < deadline, "vanilla did not load the area");
    }
    let log = server.stop();
    assert_no_chunk_errors(&log);
    let completed = saved(&region, spawn);
    for pos in &wanted {
        assert_eq!(
            status(&completed[&(pos.x, pos.z)]),
            "minecraft:full",
            "{pos:?} after vanilla completed it"
        );
    }
}
