//! Compares trees, fallen trees and huge mushrooms with vanilla's own configured features: many trees placed one after another in the same
//! chunks, each with a decoration random source seeded from the chunk and its index, so that later trees grow
//! through, are cut short by or change the leaves of earlier ones. Each tree's result and the random values it
//! consumed are compared, then every block that changed in the 3x3 chunks and the block entities the trees
//! created there.
mod reference;

use std::collections::BTreeSet;
use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::Nbt;
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    FeatureCompiler, FeatureWorld, NoiseBasedChunkGenerator, RegionFeatureWorld, WorldgenData,
    place_feature,
};
use reference::Reference;

const SEED: i64 = -4_172_144_997_902_289_642;
/// The configured features compared, cycled through in each chunk: huge brown and red mushrooms around the first
/// oak, at positions where they find ground and room for their caps, then trees without decorators, then trees with
/// bee nests every time, vines and cocoa (twice, as cocoa grows on a fifth of jungle trees), leaf litter, and
/// fallen trees with vines on their stumps and mushrooms on their logs, then acacias, jungle bushes and mega
/// jungle trees, mega pines and mega spruces with podzol around them, dark oaks and cherries with hanging
/// leaves, mangroves with their roots and hanging propagules, pale oaks with pale moss and creaking hearts, and
/// azaleas with their bending trunks (twice, so that the 33 features cycle through every position of the
/// pattern).
const FEATURES: [&str; 33] = [
    "minecraft:huge_brown_mushroom",
    "minecraft:oak",
    "minecraft:huge_red_mushroom",
    "minecraft:fancy_oak",
    "minecraft:birch",
    "minecraft:spruce",
    "minecraft:pine",
    "minecraft:fancy_oak_bees",
    "minecraft:super_birch_bees",
    "minecraft:jungle_tree",
    "minecraft:swamp_oak",
    "minecraft:jungle_tree",
    "minecraft:oak_leaf_litter",
    "minecraft:birch_bees_0002_leaf_litter",
    "minecraft:fallen_oak_tree",
    "minecraft:fallen_birch_tree",
    "minecraft:fallen_spruce_tree",
    "minecraft:fallen_jungle_tree",
    "minecraft:fallen_super_birch_tree",
    "minecraft:acacia",
    "minecraft:jungle_bush",
    "minecraft:mega_jungle_tree",
    "minecraft:mega_pine",
    "minecraft:mega_spruce",
    "minecraft:dark_oak",
    "minecraft:cherry",
    "minecraft:cherry_bees_005",
    "minecraft:mangrove",
    "minecraft:tall_mangrove",
    "minecraft:pale_oak",
    "minecraft:pale_oak_creaking",
    "minecraft:azalea_tree",
    "minecraft:azalea_tree",
];
const TREES_PER_CHUNK: usize = 198;
/// A plains, a swamp beside water, snowy plains, a birch forest, a grove on snow, a steep mixed chunk, a
/// mangrove swamp, where mangrove roots grow through mud, a jungle and a dark forest.
const CHUNKS: [(i32, i32); 9] = [
    (-42, 6),
    (-36, -36),
    (-198, -198),
    (-108, -108),
    (-528, -480),
    (600, 312),
    (126, 72),
    (84, 42),
    (-102, -114),
];

/// The blocks the decorators place, each of which vanilla's trees must place for the comparison to cover it.
const DECORATIONS: [&str; 15] = [
    "minecraft:vine",
    "minecraft:cocoa",
    "minecraft:bee_nest",
    "minecraft:leaf_litter",
    "minecraft:red_mushroom",
    "minecraft:podzol",
    "minecraft:brown_mushroom_block",
    "minecraft:red_mushroom_block",
    "minecraft:mushroom_stem",
    "minecraft:mangrove_roots",
    "minecraft:mangrove_propagule",
    "minecraft:pale_moss_block",
    "minecraft:pale_moss_carpet",
    "minecraft:pale_hanging_moss",
    "minecraft:creaking_heart",
];

type Blocks = BTreeSet<(i32, i32, i32, u32)>;

/// `TreeFeatureProbe.plantGround`: on the top grass block of each column, by a pattern of its coordinates, tall
/// grass, short grass, a poppy, or snow on the grass block itself, for the trees to update.
fn plant_ground(world: &mut RegionFeatureWorld, cx: i32, cz: i32) {
    let states = block_states();
    let state = |name: &str, properties: &[(&str, &str)]| {
        states
            .state(name, properties.iter().copied())
            .expect("known block")
            .0
    };
    let grass_states: Vec<u32> = (0..states.len())
        .filter(|&id| {
            states
                .properties(id)
                .is_some_and(|(name, _)| name == "minecraft:grass_block")
        })
        .collect();
    for z in (cz - 1) * 16..(cz + 2) * 16 {
        for x in (cx - 1) * 16..(cx + 2) * 16 {
            let mut top = 319;
            while top > -64 && states.is_air(world.block_state(x, top, z)) {
                top -= 1;
            }
            if !grass_states.contains(&world.block_state(x, top, z)) {
                continue;
            }
            match (x * 3 + z * 7).rem_euclid(6) {
                0 => {
                    world.set_block_state(
                        x,
                        top + 1,
                        z,
                        state("minecraft:tall_grass", &[("half", "lower")]),
                    );
                    world.set_block_state(
                        x,
                        top + 2,
                        z,
                        state("minecraft:tall_grass", &[("half", "upper")]),
                    );
                }
                1 => {
                    world.set_block_state(x, top + 1, z, state("minecraft:short_grass", &[]));
                }
                2 => {
                    world.set_block_state(
                        x,
                        top,
                        z,
                        state("minecraft:grass_block", &[("snowy", "true")]),
                    );
                }
                3 => {
                    world.set_block_state(x, top + 1, z, state("minecraft:poppy", &[]));
                }
                _ => {}
            }
        }
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn trees_match_vanilla() {
    let reference = Reference::required();
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    reference.compile(&[
        (
            "WorldgenReferenceProbe.java",
            include_str!("WorldgenReferenceProbe.java"),
        ),
        (
            "ChunkParityProbe.java",
            include_str!("ChunkParityProbe.java"),
        ),
        ("PlacementProbe.java", include_str!("PlacementProbe.java")),
        (
            "TreeFeatureProbe.java",
            include_str!("TreeFeatureProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("tree-features.txt");
    let mut args = vec![
        output.display().to_string(),
        reference.server_jar.display().to_string(),
        SEED.to_string(),
        TREES_PER_CHUNK.to_string(),
        FEATURES.join(","),
    ];
    args.extend(CHUNKS.iter().map(|(x, z)| format!("{x},{z}")));
    reference.java("TreeFeatureProbe", args);
    let text = fs::read_to_string(&output).expect("read the vanilla tree output");

    let mut compiler = FeatureCompiler::new(&data);
    let features: Vec<_> = FEATURES
        .iter()
        .map(|name| {
            compiler
                .configured(name)
                .expect("compile the tree")
                .unwrap_or_else(|| panic!("{name} is not ported"))
        })
        .collect();
    let generator = NoiseBasedChunkGenerator::overworld(SEED, &data).expect("compile generator");

    let mut lines = text.lines().peekable();
    let mut placed_trees = 0;
    let mut compared_blocks = 0;
    let mut compared_entities = 0;
    let mut placed_by_feature = [0; FEATURES.len()];
    let mut decorations = [0; DECORATIONS.len()];
    for &(cx, cz) in &CHUNKS {
        assert_eq!(lines.next(), Some(format!("chunk {cx} {cz}").as_str()));
        let chunks: Vec<_> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (cx + dx, cz + dz)))
            .map(|(x, z)| generator.generate_chunk_carved(x, z, &data).unwrap())
            .collect();
        let mut world = RegionFeatureWorld::with_stored_biomes(chunks, SEED).expect("region");
        plant_ground(&mut world, cx, cz);
        world.start_region(SEED, cx, cz);
        let before = RegionFeatureWorld::with_stored_biomes(
            (-1..=1)
                .flat_map(|dx| (-1..=1).map(move |dz| (cx + dx, cz + dz)))
                .map(|(x, z)| world.chunk(x, z).expect("region chunk").clone()),
            SEED,
        )
        .expect("region");
        while let Some(line) = lines.next_if(|line| line.starts_with("tree ")) {
            let fields: Vec<&str> = line.split(' ').collect();
            let [_, index, name, x, y, z, placed, next] = fields[..] else {
                panic!("unexpected tree line {line}");
            };
            let index: usize = index.parse().unwrap();
            assert_eq!(name, FEATURES[index % FEATURES.len()]);
            let origin = [x.parse().unwrap(), y.parse().unwrap(), z.parse().unwrap()];
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
            let decoration = random.set_decoration_seed(SEED, cx * 16, cz * 16);
            random.set_feature_seed(decoration, index as i32, 9);
            let feature = &features[index % FEATURES.len()];
            let result = place_feature(&mut world, feature, &mut random, origin);
            assert_eq!(
                (result, random.next_int()),
                (placed.parse().unwrap(), next.parse().unwrap()),
                "chunk ({cx}, {cz}) tree {index} ({name} at {origin:?}): result and next random value",
            );
            placed_trees += usize::from(result);
            placed_by_feature[index % FEATURES.len()] += usize::from(result);
        }
        let mut expected = Blocks::new();
        while let Some(line) =
            lines.next_if(|line| !line.starts_with("chunk") && !line.starts_with("entity "))
        {
            let mut fields = line.split(' ').map(|field| field.parse::<i64>().unwrap());
            let mut field = || fields.next().expect("block field");
            expected.insert((
                field() as i32,
                field() as i32,
                field() as i32,
                field() as u32,
            ));
        }
        let mut actual = Blocks::new();
        for y in -64..320 {
            for z in (cz - 1) * 16..(cz + 2) * 16 {
                for x in (cx - 1) * 16..(cx + 2) * 16 {
                    let state = world.block_state(x, y, z);
                    if state != before.block_state(x, y, z) {
                        actual.insert((x, y, z, state));
                    }
                }
            }
        }
        let missing: Vec<_> = expected.difference(&actual).take(20).collect();
        let extra: Vec<_> = actual.difference(&expected).take(20).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "chunk ({cx}, {cz}): {} blocks from vanilla, {} from Harpas\nmissing {missing:?}\nextra {extra:?}",
            expected.len(),
            actual.len(),
        );
        compared_blocks += expected.len();
        for &(_, _, _, state) in &expected {
            let name = block_states().properties(state).map(|(name, _)| name);
            if let Some(count) = DECORATIONS
                .iter()
                .position(|&decoration| Some(decoration) == name)
            {
                decorations[count] += 1;
            }
        }

        let mut expected_entities = Vec::new();
        while let Some(line) = lines.next_if(|line| line.starts_with("entity ")) {
            let bytes = hex(&line["entity ".len()..]);
            expected_entities.push(Nbt::read_network(&mut bytes.as_slice()).expect("vanilla NBT"));
        }
        let mut actual_entities: Vec<Nbt> = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (cx + dx, cz + dz)))
            .flat_map(|(x, z)| world.chunk(x, z).expect("region chunk").block_entities())
            .map(|entity| Nbt::Compound(entity.clone()))
            .collect();
        let key = |entity: &Nbt| format!("{entity:?}");
        expected_entities.sort_by_key(key);
        actual_entities.sort_by_key(key);
        assert_eq!(
            actual_entities, expected_entities,
            "chunk ({cx}, {cz}): block entities"
        );
        compared_entities += expected_entities.len();
    }
    assert!(lines.next().is_none(), "unread vanilla output");
    for name in FEATURES {
        let placed: usize = FEATURES
            .iter()
            .zip(placed_by_feature)
            .filter(|(feature, _)| **feature == name)
            .map(|(_, placed)| placed)
            .sum();
        assert!(placed > 0, "vanilla placed no {name}");
    }
    assert!(compared_entities > 0, "no bee nest was placed");
    for (name, count) in DECORATIONS.iter().zip(decorations) {
        assert!(count > 0, "vanilla placed no {name}");
    }
    eprintln!(
        "Compared {placed_trees} placed trees, {compared_blocks} blocks and {compared_entities} block entities \
         around {} chunks",
        CHUNKS.len()
    );
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("a hex byte"))
        .collect()
}
