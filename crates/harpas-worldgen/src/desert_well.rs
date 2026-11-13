//! Desert wells: a sandstone basin, pillars and slab roof, with two buried archaeology blocks.

use harpas_core::block::block_states;
use harpas_core::nbt::Compound;
use harpas_core::random::{RandomSource, WorldgenRandom};

use crate::feature::FeatureWorld;

/// Places `minecraft:desert_well`, searching down from one block above the origin for sand. Each column
/// of the foundation needs a non-air block in at least one of the two layers below the surface.
/// Rejected sites consume no random values; accepted sites draw two positions under the water.
pub fn place_desert_well<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    random: &mut WorldgenRandom<R>,
    [x, mut y, z]: [i32; 3],
) -> bool {
    let states = block_states();
    y += 1;
    while states.is_air(world.block_state(x, y, z)) && y > world.min_y() + 2 {
        y -= 1;
    }
    let sand = states.default_state("minecraft:sand").unwrap();
    if world.block_state(x, y, z) != sand {
        return false;
    }
    for dx in -2..=2 {
        for dz in -2..=2 {
            if states.is_air(world.block_state(x + dx, y - 1, z + dz))
                && states.is_air(world.block_state(x + dx, y - 2, z + dz))
            {
                return false;
            }
        }
    }

    let sandstone = states.default_state("minecraft:sandstone").unwrap();
    let slab = states.default_state("minecraft:sandstone_slab").unwrap();
    let water = states.default_state("minecraft:water").unwrap();
    for dy in -2..=0 {
        for dx in -2..=2 {
            for dz in -2..=2 {
                world.set_block_state(x + dx, y + dy, z + dz, sandstone);
            }
        }
    }
    // The water cross also determines the order of the two bounded random draws for archaeology.
    let basin = [[0, 0], [1, 0], [0, 1], [-1, 0], [0, -1]];
    for [dx, dz] in basin {
        world.set_block_state(x + dx, y, z + dz, water);
        world.set_block_state(x + dx, y - 1, z + dz, sand);
    }
    for dx in -2i32..=2 {
        for dz in -2i32..=2 {
            if dx.abs() == 2 || dz.abs() == 2 {
                world.set_block_state(x + dx, y + 1, z + dz, sandstone);
            }
        }
    }
    for [dx, dz] in [[2, 0], [-2, 0], [0, 2], [0, -2]] {
        world.set_block_state(x + dx, y + 1, z + dz, slab);
    }
    for dx in -1..=1 {
        for dz in -1..=1 {
            world.set_block_state(
                x + dx,
                y + 4,
                z + dz,
                if dx == 0 && dz == 0 { sandstone } else { slab },
            );
        }
    }
    for dx in [-1, 1] {
        for dz in [-1, 1] {
            for dy in 1..=3 {
                world.set_block_state(x + dx, y + dy, z + dz, sandstone);
            }
        }
    }
    let suspicious = states.default_state("minecraft:suspicious_sand").unwrap();
    for dy in [-1, -2] {
        let [dx, dz] = basin[random.next_int_bounded(5) as usize];
        let (x, y, z) = (x + dx, y + dy, z + dz);
        if world.set_block_state(x, y, z, suspicious) {
            let mut entity = Compound::new();
            entity.insert("id", "minecraft:brushable_block");
            entity.insert("x", x);
            entity.insert("y", y);
            entity.insert("z", z);
            entity.insert("components", Compound::new());
            entity.insert("LootTable", "minecraft:archaeology/desert_well");
            let seed = ((i64::from(x) & 0x3ff_ffff) << 38)
                | ((i64::from(z) & 0x3ff_ffff) << 12)
                | (i64::from(y) & 0xfff);
            if seed != 0 {
                entity.insert("LootTableSeed", seed);
            }
            world.set_block_entity(x, y, z, entity);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FeatureCompiler, GeneratedChunk, RegionFeatureWorld, WorldgenData, place_feature};
    use harpas_core::nbt::Nbt;
    use harpas_core::random::XoroshiroRandom;

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn world() -> RegionFeatureWorld {
        let chunks =
            (0..=1).flat_map(|x| (-1..=0).map(move |z| GeneratedChunk::new(x, z, 0, 32, 1)));
        let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
        for x in 12..=18 {
            for z in -4..=2 {
                for y in 7..=10 {
                    world.set_block_state(x, y, z, state("sand"));
                }
            }
        }
        world
    }

    #[test]
    fn rejected_sites_leave_blocks_and_random_source_unchanged() {
        for two_air_layers in [false, true] {
            let mut world = world();
            world.set_block_state(13, 9, -3, state("air"));
            if two_air_layers {
                world.set_block_state(13, 8, -3, state("cave_air"));
            }
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert_eq!(
                place_desert_well(&mut world, &mut random, [15, 18, -1]),
                !two_air_layers
            );
            if two_air_layers {
                assert_eq!(world.block_state(15, 10, -1), state("sand"));
                assert_eq!(
                    random.next_int(),
                    WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
                );
            }
        }
    }

    #[test]
    fn compiled_wells_replace_container_data_and_bury_two_archaeology_records() {
        let data = WorldgenData::from_json_entries([(
            "data/minecraft/worldgen/configured_feature/test.json",
            r#"{"type":"minecraft:desert_well","config":{}}"#,
        )])
        .unwrap();
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:test")
            .unwrap()
            .unwrap();
        let mut world = world();
        world.set_block_state(17, 11, -1, state("chest"));
        world.set_block_state(18, 12, -1, state("chest"));
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(
            &mut world,
            &feature,
            &mut random,
            [15, 11, -1]
        ));
        let entities: Vec<_> = (0..=1)
            .flat_map(|x| (-1..=0).map(move |z| (x, z)))
            .flat_map(|(x, z)| world.chunk(x, z).unwrap().block_entities())
            .collect();
        assert_eq!(entities.len(), 3);
        assert_eq!(
            entities
                .iter()
                .filter(|entity| entity.get("id") == Some(&Nbt::from("DUMMY")))
                .count(),
            1
        );
        for (x, y, z) in [(16, 9, -1), (14, 8, -1)] {
            assert_eq!(world.block_state(x, y, z), state("suspicious_sand"));
            let entity = entities
                .iter()
                .find(|entity| {
                    entity.get("x") == Some(&Nbt::Int(x)) && entity.get("y") == Some(&Nbt::Int(y))
                })
                .unwrap();
            assert_eq!(
                entity.get("id"),
                Some(&Nbt::from("minecraft:brushable_block"))
            );
            assert_eq!(
                entity.get("LootTable"),
                Some(&Nbt::from("minecraft:archaeology/desert_well"))
            );
            assert!(matches!(entity.get("LootTableSeed"), Some(Nbt::Long(_))));
            assert_eq!(world.block_state(x, 10, z), state("water"));
        }
    }
}
