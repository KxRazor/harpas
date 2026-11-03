//! Checks terrain, aquifers, ore veins and selected surface operations against an owner-provided server.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use harpas_worldgen::{GeneratedChunk, NoiseBasedChunkGenerator, OreVeinifier, WorldgenData};

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-worldgen-vanilla-parity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create temporary directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn unpack_reference(jar: &Path, dir: &Path) -> (PathBuf, Vec<PathBuf>) {
    let mut archive =
        zip::ZipArchive::new(File::open(jar).expect("open server jar")).expect("read server jar");
    let mut classpath = vec![dir.to_path_buf()];
    let mut server = None;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read jar entry");
        let name = entry.name().expect("jar entry name");
        let is_server = name.starts_with("META-INF/versions/") && name.ends_with(".jar");
        let is_library = name.starts_with("META-INF/libraries/") && name.ends_with(".jar");
        if !is_server && !is_library {
            continue;
        }
        let destination = dir.join(format!("reference-{index}.jar"));
        std::io::copy(&mut entry, &mut File::create(&destination).unwrap()).unwrap();
        if is_server {
            assert!(
                server.replace(destination.clone()).is_none(),
                "multiple server jars"
            );
        }
        classpath.push(destination);
    }
    (
        server.expect("server jar contains a versioned jar"),
        classpath,
    )
}

fn run(command: &mut Command) {
    let output = command.output().expect("run Java parity probe");
    assert!(
        output.status.success(),
        "Java parity probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn compare_seed(data: &WorldgenData, seed: i64, output: &Path) {
    let generator = NoiseBasedChunkGenerator::overworld(seed, data).expect("compile generator");
    let mut lines = BufReader::new(File::open(output).unwrap()).lines();
    let mut check = |expected: String| {
        let actual = lines.next().expect("missing Java result").unwrap();
        assert_eq!(expected, actual, "worldgen mismatch for seed {seed}");
    };
    let surface = generator.surface_system();
    for x in (-32..=32).step_by(7) {
        for z in (-32..=32).step_by(9) {
            let depth = surface.get_surface_depth(x, z);
            for y in (-64..320).step_by(13) {
                check(format!(
                    "s {x} {y} {z} {depth} {}",
                    surface.get_band(x, y, z)
                ));
            }
        }
    }
    let ore = OreVeinifier::new(generator.graph().ore_veins_random());
    for toggle in [-0.8, -0.6, -0.4, 0.4, 0.6, 0.8] {
        for ridged in [-0.1, 0.1] {
            for gap in [-0.5, 0.0] {
                for x in (-8..=8).step_by(4) {
                    for z in (-8..=8).step_by(4) {
                        for y in (-60..=50).step_by(5) {
                            let state = ore.compute_with_samplers(x, y, z, toggle, |is_gap| {
                                if is_gap { gap } else { ridged }
                            });
                            check(format!(
                                "o {toggle:.1} {ridged:.1} {gap:.1} {x} {y} {z} {}",
                                state.map(i64::from).unwrap_or(-1)
                            ));
                        }
                    }
                }
            }
        }
    }
    let mut chunks = Vec::new();
    for cx in [-1, 0, 1] {
        for cz in [-1, 0, 1] {
            chunks.push((cx, cz));
        }
    }
    if seed == 42 {
        for cx in [-120, -119] {
            for cz in [-120, -119] {
                chunks.push((cx, cz));
            }
        }
        for cx in [360, 361] {
            for cz in [-32, -31] {
                chunks.push((cx, cz));
            }
        }
    }
    for (cx, cz) in chunks {
        let mut noise = generator.create_noise_chunk(cx, cz);
        let mut aquifer = generator.create_aquifer(cx, cz);
        for x in (cx * 16 - 20..=cx * 16 + 20).step_by(8) {
            for z in (cz * 16 - 20..=cz * 16 + 20).step_by(8) {
                check(format!(
                    "p {cx} {cz} {x} {z} {}",
                    noise.preliminary_surface_level(x, z)
                ));
            }
        }
        for x in (cx * 16..cx * 16 + 16).step_by(5) {
            for z in (cz * 16..cz * 16 + 16).step_by(5) {
                for y in (-64..320).step_by(7) {
                    for density in [-1.0, -0.1, 0.0, 0.1] {
                        let state = aquifer.compute_substance(&mut noise, x, y, z, density);
                        check(format!(
                            "a {x} {y} {z} {density:.1} {} {}",
                            state.map(i64::from).unwrap_or(-1),
                            aquifer.should_schedule_fluid_update()
                        ));
                    }
                }
            }
        }
        let mut terrain = GeneratedChunk::new(cx, cz, -64, 384, data.biomes.len());
        let mut noise = generator.create_noise_chunk(cx, cz);
        generator.populate_biomes(&mut terrain, &mut noise);
        generator.fill_from_noise(&mut terrain, &mut noise);
        let mut hash = OFFSET;
        for cell_x in 0..4 {
            for cell_z in 0..4 {
                for cell_y in (0..48).rev() {
                    for iy in (0..8).rev() {
                        let y = -64 + cell_y * 8 + iy;
                        for ix in 0..4 {
                            for iz in 0..4 {
                                let state =
                                    terrain.block_state(cell_x * 4 + ix, y, cell_z * 4 + iz);
                                hash = (hash ^ u64::from(state)).wrapping_mul(PRIME);
                            }
                        }
                    }
                }
            }
        }
        let mut fluid_hash = OFFSET;
        for &(x, y, z) in terrain.fluid_postprocessing() {
            for value in [u64::from(x), u64::from(y as u32), u64::from(z)] {
                fluid_hash = (fluid_hash ^ value).wrapping_mul(PRIME);
            }
        }
        check(format!(
            "t {cx} {cz} {hash:016x} {} {fluid_hash:016x}",
            terrain.fluid_postprocessing().len()
        ));
        generator.build_surface(&mut terrain, &mut noise, data);
        let mut surface_hash = OFFSET;
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..320 {
                    surface_hash = (surface_hash ^ u64::from(terrain.block_state(x, y, z)))
                        .wrapping_mul(PRIME);
                }
            }
        }
        check(format!("u {cx} {cz} {surface_hash:016x}"));
    }
    assert!(lines.next().is_none(), "unexpected extra Java results");
}

#[test]
fn terrain_aquifers_veins_and_surface_samples_match_vanilla() {
    let Some(reference) = std::env::var_os("HARPAS_VANILLA_SERVER_DIR") else {
        eprintln!("Skipping vanilla worldgen parity: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let reference = PathBuf::from(reference);
    let eula = fs::read_to_string(reference.join("eula.txt")).expect("reference EULA");
    assert!(
        eula.lines().any(|line| line.trim() == "eula=true"),
        "the reference server's EULA must already be accepted"
    );
    let temp = TempDir::new();
    let (server_jar, paths) = unpack_reference(&reference.join("server.jar"), &temp.0);
    let data = WorldgenData::from_zip_path(&server_jar).expect("load vanilla worldgen data");
    let classpath = std::env::join_paths(paths).expect("Java classpath");
    let source = temp.0.join("WorldgenReferenceProbe.java");
    fs::write(&source, include_str!("WorldgenReferenceProbe.java")).unwrap();
    run(Command::new("javac")
        .current_dir(&temp.0)
        .arg("-cp")
        .arg(&classpath)
        .arg(&source));
    for seed in [123_456_789, 42, -1] {
        let output = temp.0.join("results.txt");
        run(Command::new("java")
            .current_dir(&temp.0)
            .arg("-cp")
            .arg(&classpath)
            .arg("WorldgenReferenceProbe")
            .arg(&output)
            .arg(seed.to_string()));
        compare_seed(&data, seed, &output);
    }
}
