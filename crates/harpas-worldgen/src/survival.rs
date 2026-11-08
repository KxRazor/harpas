//! Survival rules (`BlockState.canSurvive`) for the blocks that worldgen tests with `would_survive`.
//!
//! Each rule reads the blocks around a position through tags from the data pack: vegetation and saplings need
//! a supporting block below, a hanging mangrove propagule one above, sugar cane water or frosted ice beside
//! the block below it, and cactus no solid block or lava beside it and no liquid above.

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};

use crate::data::{BlockStateDef, WorldgenData, WorldgenDataError};
use crate::feature::FeatureWorld;
use crate::placement::{StateSet, fluid, fluid_mask, states_where};
use crate::surface::resolve_block_state;

/// A compiled survival rule.
#[derive(Debug, Clone)]
pub enum SurvivalRule {
    /// The block below is in a set (`VegetationBlock`, saplings, a standing mangrove propagule).
    Below(StateSet),
    /// The block above is in a set (a hanging mangrove propagule).
    Above(StateSet),
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
}

impl SurvivalRule {
    /// Whether the block could stay at a position.
    pub fn test(&self, world: &impl FeatureWorld, [x, y, z]: [i32; 3]) -> bool {
        match self {
            Self::Below(support) => support.contains(world.block_state(x, y - 1, z)),
            Self::Above(support) => support.contains(world.block_state(x, y + 1, z)),
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
                        adjacent_fluids & (1 << fluid(beside) as u8) != 0
                            || adjacent_blocks.contains(beside)
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
                    block_behavior().is_solid(beside) || lava & (1 << fluid(beside) as u8) != 0
                });
                if blocked {
                    return false;
                }
                let below = world.block_state(x, y - 1, z);
                (cactus.contains(below) || support.contains(below))
                    && !block_behavior().is_liquid(world.block_state(x, y + 1, z))
            }
        }
    }
}

/// Compiles the survival rule of a block state; None for blocks whose rule is not ported yet.
pub fn compile_survival(
    name: &str,
    state: &BlockStateDef,
    data: &WorldgenData,
) -> Result<Option<SurvivalRule>, WorldgenDataError> {
    let tag = |tag: &str| -> Result<StateSet, WorldgenDataError> {
        Ok(StateSet::from_membership(&data.block_tag_states(tag)?))
    };
    let block = |block: &str| {
        states_where(|id| {
            block_states()
                .properties(id)
                .is_some_and(|(owner, _)| owner == block)
        })
    };
    let block_name = state.name.as_str();
    let short = block_name.strip_prefix("minecraft:").unwrap_or(block_name);
    Ok(Some(match short {
        "oak_sapling" | "spruce_sapling" | "birch_sapling" | "jungle_sapling"
        | "acacia_sapling" | "cherry_sapling" | "dark_oak_sapling" | "pale_oak_sapling"
        | "firefly_bush" => SurvivalRule::Below(tag("minecraft:supports_vegetation")?),
        "mangrove_propagule" => {
            let id = resolve_block_state(state, name)?;
            let hanging = block_states()
                .properties(id)
                .is_some_and(|(_, properties)| properties.contains(&("hanging", "true")));
            if hanging {
                SurvivalRule::Above(tag("minecraft:supports_hanging_mangrove_propagule")?)
            } else {
                SurvivalRule::Below(tag("minecraft:supports_mangrove_propagule")?)
            }
        }
        "sugar_cane" => SurvivalRule::SugarCane {
            cane: block(block_name),
            support: tag("minecraft:supports_sugar_cane")?,
            adjacent_blocks: tag("minecraft:supports_sugar_cane_adjacently")?,
            adjacent_fluids: fluid_mask(
                name,
                &["#minecraft:supports_sugar_cane_adjacently".to_string()],
                data,
            )?,
        },
        "cactus" => SurvivalRule::Cactus {
            cactus: block(block_name),
            support: tag("minecraft:supports_cactus")?,
            lava: fluid_mask(name, &["#minecraft:lava".to_string()], data)?,
        },
        _ => return Ok(None),
    }))
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

    fn rule(name: &str, properties: &[(&str, &str)]) -> SurvivalRule {
        let mut state = BlockStateDef::simple(name);
        for (key, value) in properties {
            state
                .properties
                .insert((*key).to_string(), (*value).to_string());
        }
        compile_survival("test", &state, &data()).unwrap().unwrap()
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
        assert!(sapling.test(&world(&[([5, 0, 5], "minecraft:dirt")]), [5, 1, 5]));
        assert!(!sapling.test(&world(&[([5, 0, 5], "minecraft:stone")]), [5, 1, 5]));
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
        assert!(hanging.test(&leaves, [5, 1, 5]));
        assert!(!standing.test(&leaves, [5, 1, 5]));
        assert!(standing.test(&world(&[([5, 0, 5], "minecraft:mud")]), [5, 1, 5]));
    }

    #[test]
    fn sugar_cane_needs_water_or_frosted_ice_beside_its_ground() {
        let cane = rule("minecraft:sugar_cane", &[("age", "0")]);
        let ground = ([5, 0, 5], "minecraft:sand");
        assert!(!cane.test(&world(&[ground]), [5, 1, 5]));
        assert!(cane.test(&world(&[ground, ([6, 0, 5], "minecraft:water")]), [5, 1, 5]));
        assert!(cane.test(
            &world(&[ground, ([5, 0, 4], "minecraft:frosted_ice")]),
            [5, 1, 5]
        ));
        // Water above the ground's level does not count, but cane on cane always survives.
        assert!(!cane.test(&world(&[ground, ([6, 1, 5], "minecraft:water")]), [5, 1, 5]));
        assert!(cane.test(&world(&[([5, 0, 5], "minecraft:sugar_cane")]), [5, 1, 5]));
    }

    #[test]
    fn cactus_needs_room_and_sand() {
        let cactus = rule("minecraft:cactus", &[("age", "0")]);
        let sand = ([5, 0, 5], "minecraft:sand");
        assert!(cactus.test(&world(&[sand]), [5, 1, 5]));
        assert!(!cactus.test(&world(&[sand, ([4, 1, 5], "minecraft:stone")]), [5, 1, 5]));
        assert!(!cactus.test(&world(&[sand, ([5, 1, 6], "minecraft:lava")]), [5, 1, 5]));
        assert!(!cactus.test(&world(&[sand, ([5, 2, 5], "minecraft:water")]), [5, 1, 5]));
        // A non-solid neighbor, such as a torch, leaves room.
        assert!(cactus.test(&world(&[sand, ([4, 1, 5], "minecraft:torch")]), [5, 1, 5]));
    }
}
