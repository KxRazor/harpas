//! Underground monster rooms, with loot chests and an unticked spawner.

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{Compound, List};
use harpas_core::random::{RandomSource, WorldgenRandom};

use crate::feature::FeatureWorld;

fn state(name: &str) -> u32 {
    block_states().default_state(name).unwrap()
}

fn is_block(state: u32, name: &str) -> bool {
    block_states().properties(state).unwrap().0 == name
}

// Direction.Plane.HORIZONTAL: north, east, south, west.
const SIDES: [[i32; 2]; 4] = [[0, -1], [1, 0], [0, 1], [-1, 0]];

fn chest_facing(neighbors: [u32; 4]) -> &'static str {
    if neighbors.iter().any(|&s| is_block(s, "minecraft:chest")) {
        return "north";
    }
    let solid = neighbors.map(|s| block_behavior().is_solid_render(s));
    let mut facing = 0;
    if solid.iter().filter(|&&s| s).count() == 1 {
        facing = (solid.iter().position(|&s| s).unwrap() + 2) % 4;
    } else {
        if solid[facing] {
            facing = (facing + 2) % 4;
        }
        if solid[facing] {
            facing = (facing + 1) % 4;
        }
        if solid[facing] {
            facing = (facing + 2) % 4;
        }
    }
    ["north", "east", "south", "west"][facing]
}

fn entity(id: &str, [x, y, z]: [i32; 3]) -> Compound {
    let mut tag = Compound::new();
    tag.insert("id", id);
    tag.insert("x", x);
    tag.insert("y", y);
    tag.insert("z", z);
    tag.insert("components", Compound::new());
    tag
}

/// Places `minecraft:monster_room`. Solid floors and ceilings and one to five two-block-high openings
/// are required. Dimensions, floor moss, chest attempts, loot seeds and spawner species share the feature's
/// decoration random source. Existing chests and spawners inside the room survive excavation.
pub fn place_monster_room<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
) -> bool {
    let rx = random.next_int_bounded(2) + 2;
    let rz = random.next_int_bounded(2) + 2;
    let (sx, sz) = (rx + 1, rz + 1);
    let solid = |s| block_behavior().is_solid(s);
    let air = |s| block_states().is_air(s);
    let mut openings = 0;
    for dx in -sx..=sx {
        for dy in -1..=4 {
            for dz in -sz..=sz {
                let current = world.block_state(x + dx, y + dy, z + dz);
                if (dy == -1 || dy == 4) && !solid(current) {
                    return false;
                }
                if (dx.abs() == sx || dz.abs() == sz)
                    && dy == 0
                    && air(current)
                    && air(world.block_state(x + dx, y + 1, z + dz))
                {
                    openings += 1;
                }
            }
        }
    }
    if !(1..=5).contains(&openings) {
        return false;
    }

    let cave_air = state("minecraft:cave_air");
    let cobble = state("minecraft:cobblestone");
    let moss = state("minecraft:mossy_cobblestone");
    for dx in -sx..=sx {
        for dy in (-1..=3).rev() {
            for dz in -sz..=sz {
                let (px, py, pz) = (x + dx, y + dy, z + dz);
                let current = world.block_state(px, py, pz);
                if dx.abs() != sx && dy != -1 && dz.abs() != sz {
                    if !is_block(current, "minecraft:chest")
                        && !is_block(current, "minecraft:spawner")
                    {
                        world.set_block_state(px, py, pz, cave_air);
                    }
                } else if py >= world.min_y() && !solid(world.block_state(px, py - 1, pz)) {
                    world.set_block_state(px, py, pz, cave_air);
                } else if solid(current) && !is_block(current, "minecraft:chest") {
                    let block = if dy == -1 && random.next_int_bounded(4) != 0 {
                        moss
                    } else {
                        cobble
                    };
                    world.set_block_state(px, py, pz, block);
                }
            }
        }
    }
    for _ in 0..2 {
        for _ in 0..3 {
            let px = x + random.next_int_bounded(rx * 2 + 1) - rx;
            let pz = z + random.next_int_bounded(rz * 2 + 1) - rz;
            if !air(world.block_state(px, y, pz)) {
                continue;
            }
            let neighbors = SIDES.map(|[dx, dz]| world.block_state(px + dx, y, pz + dz));
            if neighbors.iter().filter(|&&s| solid(s)).count() != 1 {
                continue;
            }
            let chest = block_states()
                .state("minecraft:chest", [("facing", chest_facing(neighbors))])
                .unwrap()
                .0;
            if world.set_block_state(px, y, pz, chest) {
                let mut tag = entity("minecraft:chest", [px, y, pz]);
                tag.insert("LootTable", "minecraft:chests/simple_dungeon");
                let seed = random.next_long();
                if seed != 0 {
                    tag.insert("LootTableSeed", seed);
                }
                world.set_block_entity(px, y, pz, tag);
            }
            break;
        }
    }
    if world.set_block_state(x, y, z, state("minecraft:spawner")) {
        let mob = [
            "minecraft:skeleton",
            "minecraft:zombie",
            "minecraft:zombie",
            "minecraft:spider",
        ][random.next_int_bounded(4) as usize];
        let mut tag = entity("minecraft:mob_spawner", [x, y, z]);
        for (key, value) in [
            ("Delay", 20_i16),
            ("MaxNearbyEntities", 6),
            ("MaxSpawnDelay", 800),
            ("MinSpawnDelay", 200),
            ("RequiredPlayerRange", 16),
            ("SpawnCount", 4),
            ("SpawnRange", 4),
        ] {
            tag.insert(key, value);
        }
        let mut mob_tag = Compound::new();
        mob_tag.insert("id", mob);
        let mut spawn = Compound::new();
        spawn.insert("entity", mob_tag);
        tag.insert("SpawnData", spawn);
        tag.insert("SpawnPotentials", List::Empty);
        world.set_block_entity(x, y, z, tag);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FeatureCompiler, GeneratedChunk, RegionFeatureWorld, WorldgenData, place_feature};
    use harpas_core::nbt::Nbt;
    use harpas_core::random::XoroshiroRandom;

    fn world() -> RegionFeatureWorld {
        let mut world = RegionFeatureWorld::new(
            (0..=1).flat_map(|cx| (-1..=0).map(move |cz| GeneratedChunk::new(cx, cz, -64, 384, 1))),
            0,
        )
        .unwrap();
        for x in 9..=21 {
            for z in -7..=5 {
                for y in 62..=68 {
                    if z != -1 || !(64..=65).contains(&y) {
                        world.set_block_state(x, y, z, state("minecraft:stone"));
                    }
                }
            }
        }
        world
    }

    #[test]
    fn invalid_caves_draw_only_the_two_dimensions_and_leave_no_writes() {
        for ceiling in [false, true] {
            let mut world = world();
            world.set_block_state(
                15,
                if ceiling { 68 } else { 63 },
                -1,
                state("minecraft:air"),
            );
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            let mut expected = WorldgenRandom::new(XoroshiroRandom::new(42));
            expected.next_int_bounded(2);
            expected.next_int_bounded(2);
            assert!(!place_monster_room(&mut world, &mut random, [15, 64, -1]));
            assert_eq!(random.next_int(), expected.next_int());
            assert_eq!(world.block_state(14, 64, 0), state("minecraft:stone"));
            assert_eq!(world.block_state(14, 63, 0), state("minecraft:stone"));
            assert_eq!(world.chunk(0, 0).unwrap().block_entities().count(), 0);
        }
    }

    #[test]
    fn compiled_room_crosses_chunk_edges_and_saves_chest_loot_and_spawner_species() {
        let data = WorldgenData::from_json_entries([(
            "data/minecraft/worldgen/configured_feature/room.json",
            r#"{"type":"minecraft:monster_room","config":{}}"#,
        )])
        .unwrap();
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:room")
            .unwrap()
            .unwrap();
        let mut world = world();
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(
            &mut world,
            &feature,
            &mut random,
            [15, 64, -1]
        ));
        assert_eq!(world.block_state(16, 66, 0), state("minecraft:cave_air"));
        assert_eq!(world.block_state(15, 68, -1), state("minecraft:stone"));
        let spawner = world
            .chunk(0, -1)
            .unwrap()
            .block_entities()
            .find(|e| e.get("id") == Some(&Nbt::from("minecraft:mob_spawner")))
            .unwrap();
        let Some(Nbt::Compound(spawn)) = spawner.get("SpawnData") else {
            panic!("missing spawn data")
        };
        let Some(Nbt::Compound(mob)) = spawn.get("entity") else {
            panic!("missing entity")
        };
        assert_eq!(mob.get("id"), Some(&Nbt::from("minecraft:zombie")));
        assert_eq!(spawner.get("Delay"), Some(&Nbt::Short(20)));
        for (cx, cz) in [(0, 0), (1, -1)] {
            let chest = world
                .chunk(cx, cz)
                .unwrap()
                .block_entities()
                .find(|e| e.get("id") == Some(&Nbt::from("minecraft:chest")))
                .unwrap();
            assert_eq!(
                chest.get("LootTable"),
                Some(&Nbt::from("minecraft:chests/simple_dungeon"))
            );
            assert!(matches!(chest.get("LootTableSeed"), Some(Nbt::Long(_))));
        }
    }

    #[test]
    fn excavation_preserves_existing_containers_and_spawners() {
        let mut world = world();
        for (x, block) in [(14, "minecraft:chest"), (16, "minecraft:spawner")] {
            world.set_block_state(x, 65, 0, state(block));
            let mut tag = entity("test:existing", [x, 65, 0]);
            tag.insert("marker", 123);
            world.set_block_entity(x, 65, 0, tag);
        }
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_monster_room(&mut world, &mut random, [15, 64, -1]));
        for (cx, block) in [(0, "minecraft:chest"), (1, "minecraft:spawner")] {
            let x = if cx == 0 { 14 } else { 16 };
            assert_eq!(world.block_state(x, 65, 0), state(block));
            assert!(
                world
                    .chunk(cx, 0)
                    .unwrap()
                    .block_entities()
                    .any(|e| e.get("marker") == Some(&Nbt::Int(123)))
            );
        }
    }
}
