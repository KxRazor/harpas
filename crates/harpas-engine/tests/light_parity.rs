//! Compares the light engine with a vanilla server's saved chunks.
//!
//! The server generates and lights a small area; the test reads its blocks, computes light for the same blocks
//! with Harpas, and compares every saved light array. Vanilla omits a sky-light layer when every value is
//! fifteen and a block-light layer when every value is zero, so a missing layer is accepted only when
//! Harpas's layer is uniform.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use harpas_anvil::{Chunk, read_chunk};
use harpas_core::nbt::Nbt;
use harpas_worldgen::{LIGHT_BYTES, LightWorld, light_chunk};

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-light-parity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_log(server: &mut Server, log: &Path, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let contents = fs::read_to_string(log).unwrap_or_default();
        if contents.contains(text) {
            return;
        }
        if let Some(status) = server.0.try_wait().unwrap() {
            panic!("the server exited with {status} before {text:?}:\n{contents}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {text:?}:\n{contents}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

fn command(server: &mut Server, command: &str) {
    server
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{command}\n").as_bytes())
        .unwrap();
}

fn main_server(folder: &Path, reference: &Path) -> (Server, PathBuf) {
    fs::write(folder.join("eula.txt"), "eula=true\n").unwrap();
    fs::write(
        folder.join("server.properties"),
        "level-seed=42\n\
         online-mode=false\n\
         generate-structures=false\n\
         spawn-protection=0\n\
         view-distance=4\n\
         simulation-distance=4\n\
         sync-chunk-writes=true\n\
         max-tick-time=-1\n\
         enable-command-block=false\n\
         motd=light parity\n",
    )
    .unwrap();
    let log = folder.join("vanilla.log");
    let output = fs::File::create(&log).unwrap();
    let server = Server(
        Command::new("java")
            .args(["-Xms256M", "-Xmx1G", "-jar"])
            .arg(reference.join("server.jar"))
            .arg("nogui")
            .current_dir(folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap(),
    );
    (server, log)
}

struct World {
    chunks: HashMap<(i32, i32), Chunk>,
}

impl LightWorld for World {
    fn min_y(&self) -> i32 {
        -64
    }

    fn height(&self) -> i32 {
        384
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> u32 {
        let chunk = self.chunks.get(&(x.div_euclid(16), z.div_euclid(16)));
        chunk
            .and_then(|chunk| {
                chunk
                    .sections
                    .iter()
                    .find(|section| i32::from(section.y) == y.div_euclid(16))
            })
            .and_then(|section| section.section.as_ref())
            .map(|section| {
                section.block_state((x & 15) as usize, (y & 15) as usize, (z & 15) as usize)
            })
            .unwrap_or(0)
    }
}

fn nibble(layer: &[u8], index: usize) -> u8 {
    let byte = layer[index / 2];
    if index.is_multiple_of(2) {
        byte & 0x0f
    } else {
        byte >> 4
    }
}

fn saved_layer(section: &harpas_anvil::ChunkSection, name: &str) -> Option<Vec<u8>> {
    match section.rest.get(name) {
        Some(Nbt::ByteArray(bytes)) => Some(bytes.iter().map(|&value| value as u8).collect()),
        _ => None,
    }
}

fn compare(target_x: i32, target_z: i32, world: &World) -> Vec<String> {
    let computed = light_chunk(world, target_x, target_z);
    let saved = &world.chunks[&(target_x, target_z)];
    let mut problems = Vec::new();
    for section in &computed.sections {
        let Some(saved_section) = saved
            .sections
            .iter()
            .find(|saved| i32::from(saved.y) == section.y)
        else {
            continue;
        };
        for (name, computed_layer) in [("SkyLight", &section.sky), ("BlockLight", &section.block)] {
            match saved_layer(saved_section, name) {
                Some(saved_layer) if saved_layer.len() == LIGHT_BYTES => {
                    for index in 0..4096 {
                        let expected = nibble(&saved_layer, index);
                        let actual = nibble(computed_layer, index);
                        if expected != actual {
                            problems.push(format!(
                                "chunk {target_x},{target_z} {name} section {} index {index}: vanilla {expected}, harpas {actual}",
                                section.y
                            ));
                            if problems.len() >= 40 {
                                return problems;
                            }
                        }
                    }
                }
                Some(saved_layer) => problems.push(format!(
                    "chunk {target_x},{target_z} {name} section {}: {} bytes, expected {LIGHT_BYTES}",
                    section.y,
                    saved_layer.len()
                )),
                None => {
                    let uniform = computed_layer.iter().all(|byte| *byte == 0 || *byte == 0xff);
                    if !uniform {
                        problems.push(format!(
                            "chunk {target_x},{target_z} {name} section {}: vanilla omits a non-uniform layer",
                            section.y
                        ));
                    }
                }
            }
        }
    }
    problems
}

#[test]
fn light_matches_vanillas_saved_chunks() {
    let Some(reference) = std::env::var_os("HARPAS_VANILLA_SERVER_DIR").map(PathBuf::from) else {
        eprintln!("Skipping light parity: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let folder = Folder::new();
    let (mut server, log) = main_server(&folder.0, &reference);
    wait_for_log(&mut server, &log, "Done (");
    command(&mut server, "forceload add -48 -48 63 63");
    wait_for_log(&mut server, &log, "Marked 49 chunks");
    command(&mut server, "save-all flush");
    wait_for_log(&mut server, &log, "Saved the game");
    let contents = fs::read_to_string(&log).unwrap();
    assert!(!contents.contains("Failed to load chunk"), "{contents}");
    command(&mut server, "stop");
    let status = server.0.wait().unwrap();
    assert!(status.success(), "the vanilla server exited with {status}");

    let region = folder.0.join("world/dimensions/minecraft/overworld/region");
    let mut chunks = HashMap::new();
    for x in -3..=3 {
        for z in -3..=3 {
            let nbt = read_chunk(&region, x, z).unwrap().unwrap();
            let (chunk, _warnings) = Chunk::read(nbt, &["minecraft:plains"]).unwrap();
            chunks.insert((x, z), chunk);
        }
    }
    let world = World { chunks };
    let mut problems = Vec::new();
    for (x, z) in [(-1, -1), (0, 0), (1, 1)] {
        problems.extend(compare(x, z, &world));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
