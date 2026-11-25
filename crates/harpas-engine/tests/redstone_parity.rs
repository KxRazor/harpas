//! Compare a lever next to a lamp in a frozen vanilla world, one game tick at a time.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use harpas_anvil::{dimension_dir, read_chunk, write_chunks};
use harpas_core::block::block_states;
use harpas_engine::block::BlockPos;
use harpas_engine::neighbor::{UPDATE_ALL, UPDATE_LIMIT};
use harpas_engine::world::{ChunkState, World};
use reference::{Reference, Server};

const LEVER: BlockPos = BlockPos { x: 7, y: 65, z: 7 };
const LAMP: BlockPos = BlockPos { x: 8, ..LEVER };

fn load(dir: &Path) -> World {
    let mut world = World::open(dir, &["minecraft:plains"]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !matches!(world.chunk(LEVER.chunk()), Some(ChunkState::Loaded(_))) {
        assert!(Instant::now() < deadline, "loading the circuit chunk");
        world.update([LEVER.chunk()]).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    world
}

fn compare(server: &mut Server, world: &World, lit: bool) {
    let expected = if lit { "true" } else { "false" };
    server.command(&format!(
        "execute if block 8 65 7 redstone_lamp[lit={expected}] run say lamp-matches"
    ));
    server.wait_for("lamp-matches");
    assert_eq!(
        world.block_state(LAMP),
        Some(
            block_states()
                .state("minecraft:redstone_lamp", [("lit", expected)])
                .unwrap()
                .0
        )
    );
}

fn step(server: &mut Server, elapsed: u32) {
    server.command("scoreboard players add expected clock 1");
    server.command("tick step 1");
    let marker = format!("lamp-tick-{elapsed}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        server.command("execute store result score actual clock run time query gametime");
        server.command(&format!(
            "execute if score actual clock = expected clock run say {marker}"
        ));
        if server.poll(&marker, Duration::from_millis(100)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "waiting for tick {elapsed}: {}",
            server.tail(12)
        );
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn lever_lamp_delay_survives_reload() {
    let reference = Reference::required();
    let dir = reference.temp.0.join("vanilla");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();
    fs::write(dir.join("server.properties"), "online-mode=false\nserver-ip=127.0.0.1\nserver-port=0\nlevel-type=minecraft:flat\ngenerate-structures=false\nlevel-seed=42\n").unwrap();
    let mut server = Server::start(&reference, &dir);
    server.command("forceload add 7 7");
    server.command("setblock 7 64 7 stone");
    server.command("setblock 8 65 7 redstone_lamp");
    server.command("setblock 7 65 7 lever[face=floor,facing=north,powered=false]");
    server.command("tick freeze");
    server.command("scoreboard objectives add clock dummy");
    server.command("execute store result score expected clock run time query gametime");
    server.command("save-all flush");
    server.wait_for("Saved the game");
    let region = |dir: &Path| dimension_dir(dir, "minecraft:overworld").join("region");
    let chunk = read_chunk(&region(&dir.join("world")), 0, 0)
        .unwrap()
        .unwrap();
    let harpas = reference.temp.0.join("harpas");
    write_chunks(&region(&harpas), 0, 0, &[((0, 0), &chunk)]).unwrap();
    let mut world = load(&harpas);
    compare(&mut server, &world, false);
    for powered in ["true", "false"] {
        server.command(&format!(
            "setblock 7 65 7 lever[face=floor,facing=north,powered={powered}]"
        ));
        let state = block_states()
            .state(
                "minecraft:lever",
                [("face", "floor"), ("facing", "north"), ("powered", powered)],
            )
            .unwrap()
            .0;
        assert!(world.set_block(LEVER, state, UPDATE_ALL, UPDATE_LIMIT));
        compare(&mut server, &world, true);
    }
    for elapsed in 1..=4 {
        step(&mut server, elapsed);
        world.tick_scheduled();
        compare(&mut server, &world, elapsed < 4);
        if elapsed == 1 {
            server.command("save-all flush");
            server.wait_for("Saved the game");
            world.flush().unwrap();
            let ticks = |world: &Path| {
                read_chunk(&region(world), 0, 0)
                    .unwrap()
                    .unwrap()
                    .get("block_ticks")
                    .cloned()
            };
            assert_eq!(ticks(&harpas), ticks(&dir.join("world")));
            drop(world);
            world = load(&harpas);
        }
    }
    server.stop();
}
