//! Survival rules (`BlockState.canSurvive`) for the blocks that worldgen places or tests with `would_survive`.
//!
//! Each rule reads the blocks around a position, mostly through `supports_*` tags from the data pack:
//! vegetation, saplings, azaleas and dry vegetation need a supporting block below, double plants their own
//! lower half below their upper half, a hanging mangrove propagule leaves above, sugar cane water or frosted
//! ice beside the block below it, cactus no solid block or lava beside it and no liquid above, a lily pad water
//! below and none at its own position, a carpet any block below, leaf litter a full top face below and a spore
//! blossom the center of a bottom face above, out of water. Blocks without a rule of their own always survive.

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};

use crate::data::{BlockStateDef, WorldgenData, WorldgenDataError};
use crate::feature::FeatureWorld;
use crate::placement::{Fluid, StateSet, fluid, fluid_mask, states_where};
use crate::surface::resolve_block_state;

/// A compiled survival rule of one block. Rules that depend on the state, such as a double plant's half, read
/// it when they are tested.
#[derive(Debug, Clone)]
pub enum SurvivalRule {
    /// The block survives anywhere (`BlockBehaviour.canSurvive`).
    Always,
    /// The block below is in a set (`VegetationBlock.mayPlaceOn` and its overrides by tag).
    Below(StateSet),
    /// `DoublePlantBlock`: the lower half needs `support` below, the upper half its own lower half.
    DoublePlant(StateSet),
    /// `MangrovePropaguleBlock`: below it unless it hangs, then above it.
    Mangrove {
        /// `#supports_mangrove_propagule`.
        standing: StateSet,
        /// `#supports_hanging_mangrove_propagule`.
        hanging: StateSet,
    },
    /// `SugarCaneBlock.canSurvive`.
    SugarCane {
        /// Any state of sugar cane.
        cane: StateSet,
        /// `#supports_sugar_cane`.
        support: StateSet,
        /// `#supports_sugar_cane_adjacently`, as blocks.
        adjacent_blocks: StateSet,
        /// `#supports_sugar_cane_adjacently`, as fluids.
        adjacent_fluids: u8,
    },
    /// `CactusBlock.canSurvive`.
    Cactus {
        /// Any state of cactus.
        cactus: StateSet,
        /// `#supports_cactus`.
        support: StateSet,
        /// The `#lava` fluid tag.
        lava: u8,
    },
    /// `LilyPadBlock.mayPlaceOn`: a supporting block or fluid below, and no fluid at its own position.
    LilyPad {
        /// `#supports_lily_pad`, as blocks.
        blocks: StateSet,
        /// `#supports_lily_pad`, as fluids.
        fluids: u8,
    },
    /// `CarpetBlock.canSurvive`: anything but air below.
    NotAirBelow,
    /// `LeafLitterBlock.canSurvive`: a full top face below.
    SturdyBelow,
    /// `SporeBlossomBlock.canSurvive`: the center of a bottom face above, and no water at its position.
    SporeBlossom {
        /// The `#water` fluid tag.
        water: u8,
    },
}

impl SurvivalRule {
    /// Whether `state`, a state of the rule's block, could stay at a position.
    pub fn test(&self, world: &impl FeatureWorld, [x, y, z]: [i32; 3], state: u32) -> bool {
        let in_mask = |mask: u8, fluid: Fluid| mask & (1 << fluid as u8) != 0;
        match self {
            Self::Always => true,
            Self::Below(support) => support.contains(world.block_state(x, y - 1, z)),
            Self::DoublePlant(support) => {
                let below = world.block_state(x, y - 1, z);
                if property(state, "half") == Some("upper") {
                    same_block(below, state) && property(below, "half") == Some("lower")
                } else {
                    support.contains(below)
                }
            }
            Self::Mangrove { standing, hanging } => {
                if property(state, "hanging") == Some("true") {
                    hanging.contains(world.block_state(x, y + 1, z))
                } else {
                    standing.contains(world.block_state(x, y - 1, z))
                }
            }
            Self::SugarCane {
                cane,
                support,
                adjacent_blocks,
                adjacent_fluids,
            } => {
                let below = world.block_state(x, y - 1, z);
                if cane.contains(below) {
                    return true;
                }
                support.contains(below)
                    && Face::HORIZONTAL.iter().any(|face| {
                        let [dx, _, dz] = face.offset();
                        let beside = world.block_state(x + dx, y - 1, z + dz);
                        in_mask(*adjacent_fluids, fluid(beside)) || adjacent_blocks.contains(beside)
                    })
            }
            Self::Cactus {
                cactus,
                support,
                lava,
            } => {
                let blocked = Face::HORIZONTAL.iter().any(|face| {
                    let [dx, _, dz] = face.offset();
                    let beside = world.block_state(x + dx, y, z + dz);
                    block_behavior().is_solid(beside) || in_mask(*lava, fluid(beside))
                });
                if blocked {
                    return false;
                }
                let below = world.block_state(x, y - 1, z);
                (cactus.contains(below) || support.contains(below))
                    && !block_behavior().is_liquid(world.block_state(x, y + 1, z))
            }
            Self::LilyPad { blocks, fluids } => {
                let below = world.block_state(x, y - 1, z);
                (in_mask(*fluids, fluid(below)) || blocks.contains(below))
                    && fluid(world.block_state(x, y, z)) == Fluid::Empty
            }
            Self::NotAirBelow => !block_states().is_air(world.block_state(x, y - 1, z)),
            Self::SturdyBelow => {
                block_behavior().is_face_sturdy(world.block_state(x, y - 1, z), Face::Up)
            }
            Self::SporeBlossom { water } => {
                block_behavior().is_face_center_sturdy(world.block_state(x, y + 1, z), Face::Down)
                    && !in_mask(*water, fluid(world.block_state(x, y, z)))
            }
        }
    }
}

/// A property value of a block state.
fn property(state: u32, key: &str) -> Option<&'static str> {
    let (_, properties) = block_states().properties(state)?;
    properties
        .into_iter()
        .find(|(property, _)| *property == key)
        .map(|(_, value)| value)
}

/// Whether two block states belong to the same block.
fn same_block(a: u32, b: u32) -> bool {
    let name = |state| block_states().properties(state).map(|(name, _)| name);
    name(a).is_some() && name(a) == name(b)
}

/// Compiles the survival rule of a block, such as `minecraft:poppy`; None for blocks whose rule is not ported
/// yet. Plain blocks that never break for want of support, such as melons, get [`SurvivalRule::Always`].
pub fn compile_block_survival(
    name: &str,
    block: &str,
    data: &WorldgenData,
) -> Result<Option<SurvivalRule>, WorldgenDataError> {
    let tag = |tag: &str| -> Result<StateSet, WorldgenDataError> {
        Ok(StateSet::from_membership(&data.block_tag_states(tag)?))
    };
    let fluids = |tag: &str| fluid_mask(name, &[tag.to_string()], data);
    let any_state = || {
        states_where(|id| {
            block_states()
                .properties(id)
                .is_some_and(|(owner, _)| owner == block)
        })
    };
    let short = block.strip_prefix("minecraft:").unwrap_or(block);
    Ok(Some(match short {
        "oak_sapling" | "spruce_sapling" | "birch_sapling" | "jungle_sapling"
        | "acacia_sapling" | "cherry_sapling" | "dark_oak_sapling" | "pale_oak_sapling"
        | "firefly_bush" | "bush" | "short_grass" | "fern" | "pink_petals" | "wildflowers"
        | "sweet_berry_bush" | "allium" | "azure_bluet" | "blue_orchid" | "cornflower"
        | "dandelion" | "lily_of_the_valley" | "orange_tulip" | "oxeye_daisy" | "pink_tulip"
        | "poppy" | "red_tulip" | "white_tulip" | "closed_eyeblossom" | "open_eyeblossom" => {
            SurvivalRule::Below(tag("minecraft:supports_vegetation")?)
        }
        "tall_grass" | "large_fern" | "sunflower" | "lilac" | "rose_bush" | "peony" => {
            SurvivalRule::DoublePlant(tag("minecraft:supports_vegetation")?)
        }
        "azalea" | "flowering_azalea" => SurvivalRule::Below(tag("minecraft:supports_azalea")?),
        "dead_bush" | "short_dry_grass" | "tall_dry_grass" => {
            SurvivalRule::Below(tag("minecraft:supports_dry_vegetation")?)
        }
        "mangrove_propagule" => SurvivalRule::Mangrove {
            standing: tag("minecraft:supports_mangrove_propagule")?,
            hanging: tag("minecraft:supports_hanging_mangrove_propagule")?,
        },
        "sugar_cane" => SurvivalRule::SugarCane {
            cane: any_state(),
            support: tag("minecraft:supports_sugar_cane")?,
            adjacent_blocks: tag("minecraft:supports_sugar_cane_adjacently")?,
            adjacent_fluids: fluids("#minecraft:supports_sugar_cane_adjacently")?,
        },
        "cactus" => SurvivalRule::Cactus {
            cactus: any_state(),
            support: tag("minecraft:supports_cactus")?,
            lava: fluids("#minecraft:lava")?,
        },
        "lily_pad" => SurvivalRule::LilyPad {
            blocks: tag("minecraft:supports_lily_pad")?,
            fluids: fluids("#minecraft:supports_lily_pad")?,
        },
        "moss_carpet" => SurvivalRule::NotAirBelow,
        "leaf_litter" => SurvivalRule::SturdyBelow,
        "spore_blossom" => SurvivalRule::SporeBlossom {
            water: fluids("#minecraft:water")?,
        },
        "melon" | "pumpkin" => SurvivalRule::Always,
        _ => return Ok(None),
    }))
}

/// Compiles the survival rule of a block state from worldgen JSON, with the state it applies to.
pub fn compile_survival(
    name: &str,
    state: &BlockStateDef,
    data: &WorldgenData,
) -> Result<Option<(SurvivalRule, u32)>, WorldgenDataError> {
    let id = resolve_block_state(state, name)?;
    Ok(compile_block_survival(name, &state.name, data)?.map(|rule| (rule, id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::RegionFeatureWorld;
    use crate::generator::GeneratedChunk;

    fn data() -> WorldgenData {
        let tag = |values: &str| format!(r#"{{"values":[{values}]}}"#);
        WorldgenData::from_json_entries([
            (
                "data/minecraft/tags/block/supports_vegetation.json",
                tag(r#""minecraft:dirt","minecraft:grass_block""#),
            ),
            (
                "data/minecraft/tags/block/supports_sugar_cane.json",
                tag(r#""minecraft:sand","minecraft:dirt""#),
            ),
            (
                "data/minecraft/tags/block/supports_sugar_cane_adjacently.json",
                tag(r#""minecraft:frosted_ice""#),
            ),
            (
                "data/minecraft/tags/block/supports_cactus.json",
                tag(r#""minecraft:sand""#),
            ),
            (
                "data/minecraft/tags/block/supports_mangrove_propagule.json",
                tag(r#""minecraft:mud""#),
            ),
            (
                "data/minecraft/tags/block/supports_hanging_mangrove_propagule.json",
                tag(r#""minecraft:mangrove_leaves""#),
            ),
            (
                "data/minecraft/tags/block/supports_lily_pad.json",
                tag(r#""minecraft:ice""#),
            ),
            (
                "data/minecraft/tags/fluid/supports_lily_pad.json",
                tag(r#""minecraft:water""#),
            ),
            (
                "data/minecraft/tags/fluid/water.json",
                tag(r#""minecraft:water","minecraft:flowing_water""#),
            ),
            (
                "data/minecraft/tags/fluid/lava.json",
                tag(r#""minecraft:lava","minecraft:flowing_lava""#),
            ),
            (
                "data/minecraft/tags/fluid/supports_sugar_cane_adjacently.json",
                tag(r##""#minecraft:water""##),
            ),
        ])
        .unwrap()
    }

    fn rule(name: &str, properties: &[(&str, &str)]) -> (SurvivalRule, u32) {
        let mut state = BlockStateDef::simple(name);
        for (key, value) in properties {
            state
                .properties
                .insert((*key).to_string(), (*value).to_string());
        }
        compile_survival("test", &state, &data()).unwrap().unwrap()
    }

    /// Tests a rule with its own state.
    fn survives(
        rule: &(SurvivalRule, u32),
        world: &RegionFeatureWorld,
        position: [i32; 3],
    ) -> bool {
        rule.0.test(world, position, rule.1)
    }

    fn world(blocks: &[([i32; 3], &str)]) -> RegionFeatureWorld {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        for &([x, y, z], name) in blocks {
            let state = block_states().default_state(name).unwrap();
            chunk.set_block_state(x as usize, y, z as usize, state);
        }
        RegionFeatureWorld::new([chunk], 0).unwrap()
    }

    #[test]
    fn saplings_need_supporting_ground() {
        let sapling = rule("minecraft:oak_sapling", &[("stage", "0")]);
        assert!(survives(
            &sapling,
            &world(&[([5, 0, 5], "minecraft:dirt")]),
            [5, 1, 5]
        ));
        assert!(!survives(
            &sapling,
            &world(&[([5, 0, 5], "minecraft:stone")]),
            [5, 1, 5]
        ));
        assert!(
            compile_survival("test", &BlockStateDef::simple("minecraft:torch"), &data())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn hanging_propagules_hang_from_leaves() {
        let hanging = rule("minecraft:mangrove_propagule", &[("hanging", "true")]);
        let standing = rule("minecraft:mangrove_propagule", &[("hanging", "false")]);
        let leaves = world(&[([5, 2, 5], "minecraft:mangrove_leaves")]);
        assert!(survives(&hanging, &leaves, [5, 1, 5]));
        assert!(!survives(&standing, &leaves, [5, 1, 5]));
        assert!(survives(
            &standing,
            &world(&[([5, 0, 5], "minecraft:mud")]),
            [5, 1, 5]
        ));
    }

    #[test]
    fn sugar_cane_needs_water_or_frosted_ice_beside_its_ground() {
        let cane = rule("minecraft:sugar_cane", &[("age", "0")]);
        let ground = ([5, 0, 5], "minecraft:sand");
        assert!(!survives(&cane, &world(&[ground]), [5, 1, 5]));
        assert!(survives(
            &cane,
            &world(&[ground, ([6, 0, 5], "minecraft:water")]),
            [5, 1, 5]
        ));
        assert!(survives(
            &cane,
            &world(&[ground, ([5, 0, 4], "minecraft:frosted_ice")]),
            [5, 1, 5]
        ));
        // Water above the ground's level does not count, but cane on cane always survives.
        assert!(!survives(
            &cane,
            &world(&[ground, ([6, 1, 5], "minecraft:water")]),
            [5, 1, 5]
        ));
        assert!(survives(
            &cane,
            &world(&[([5, 0, 5], "minecraft:sugar_cane")]),
            [5, 1, 5]
        ));
    }

    #[test]
    fn cactus_needs_room_and_sand() {
        let cactus = rule("minecraft:cactus", &[("age", "0")]);
        let sand = ([5, 0, 5], "minecraft:sand");
        assert!(survives(&cactus, &world(&[sand]), [5, 1, 5]));
        assert!(!survives(
            &cactus,
            &world(&[sand, ([4, 1, 5], "minecraft:stone")]),
            [5, 1, 5]
        ));
        assert!(!survives(
            &cactus,
            &world(&[sand, ([5, 1, 6], "minecraft:lava")]),
            [5, 1, 5]
        ));
        assert!(!survives(
            &cactus,
            &world(&[sand, ([5, 2, 5], "minecraft:water")]),
            [5, 1, 5]
        ));
        // A non-solid neighbor, such as a torch, leaves room.
        assert!(survives(
            &cactus,
            &world(&[sand, ([4, 1, 5], "minecraft:torch")]),
            [5, 1, 5]
        ));
    }

    #[test]
    fn double_plants_lily_pads_and_carpets() {
        let data = data();
        let rule = |block: &str| {
            compile_block_survival("test", block, &data)
                .unwrap()
                .unwrap()
        };
        let state = |name: &str, properties: &[(&str, &str)]| {
            block_states()
                .state(name, properties.iter().copied())
                .unwrap()
                .0
        };
        let grass = rule("minecraft:tall_grass");
        let lower = state("minecraft:tall_grass", &[("half", "lower")]);
        let upper = state("minecraft:tall_grass", &[("half", "upper")]);
        let mut placed = world(&[([5, 0, 5], "minecraft:dirt")]);
        assert!(grass.test(&placed, [5, 1, 5], lower));
        assert!(!grass.test(&placed, [5, 1, 5], upper));
        placed.set_block_state(5, 1, 5, lower);
        assert!(grass.test(&placed, [5, 2, 5], upper));

        let lily = rule("minecraft:lily_pad");
        let pad = state("minecraft:lily_pad", &[]);
        assert!(lily.test(&world(&[([5, 0, 5], "minecraft:water")]), [5, 1, 5], pad));
        let flooded = world(&[
            ([5, 0, 5], "minecraft:water"),
            ([5, 1, 5], "minecraft:water"),
        ]);
        assert!(!lily.test(&flooded, [5, 1, 5], pad));

        let carpet = rule("minecraft:moss_carpet");
        let moss = state("minecraft:moss_carpet", &[]);
        assert!(carpet.test(&world(&[([5, 0, 5], "minecraft:stone")]), [5, 1, 5], moss));
        assert!(!carpet.test(&world(&[]), [5, 1, 5], moss));
        assert!(matches!(rule("minecraft:melon"), SurvivalRule::Always));
    }
}
