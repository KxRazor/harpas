//! The plants of oceans, rivers and swamps: `minecraft:seagrass` (`SeagrassFeature`), `minecraft:kelp`
//! (`KelpFeature`) and `minecraft:sea_pickle` (`SeaPickleFeature`).
//!
//! Each places on the ocean floor (`OCEAN_FLOOR`) of its column, in water, where its block survives. Seagrass and
//! sea pickles first move up to seven blocks away on each horizontal axis; seagrass is tall by `probability`, and
//! then needs water above it for its upper half too. Kelp grows a column of kelp plants with a kelp at its top, one
//! to eleven blocks high, as far as the water and its survival allow; a column cut short ends in a kelp one block
//! lower, unless kelp is already below it. Kelp and sea pickles draw their age and their count of pickles.

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::data::{IntProviderDef, WorldgenDataError, parse_int_provider};
use crate::feature::FeatureWorld;
use crate::generator::HeightmapType;
use crate::placement::sample_int_provider;
use crate::shape_update::with_property;
use crate::survival::{SurvivalRule, compile_block_survival};

/// A compiled ocean plant feature.
#[derive(Debug)]
pub enum CompiledOceanPlant {
    /// `minecraft:seagrass`, tall by `probability`.
    Seagrass {
        /// The chance of tall seagrass.
        probability: f32,
        /// The states placed.
        blocks: OceanBlocks,
    },
    /// `minecraft:kelp`.
    Kelp(OceanBlocks),
    /// `minecraft:sea_pickle`, with the number of attempts.
    SeaPickle {
        /// The number of attempts.
        count: IntProviderDef,
        /// The states placed.
        blocks: OceanBlocks,
    },
}

/// The states ocean plants place and their survival rules.
#[derive(Debug)]
pub struct OceanBlocks {
    seagrass: (u32, SurvivalRule),
    tall_seagrass: (u32, SurvivalRule),
    kelp: (u32, SurvivalRule),
    kelp_plant: (u32, SurvivalRule),
    sea_pickle: (u32, SurvivalRule),
}

impl OceanBlocks {
    fn new(data: &crate::data::WorldgenData) -> Result<Self, WorldgenDataError> {
        let block = |name: &str| -> Result<(u32, SurvivalRule), WorldgenDataError> {
            let state = block_states().default_state(name).ok_or_else(|| {
                WorldgenDataError::InvalidDefinition {
                    context: name.to_string(),
                    message: "unknown block".to_string(),
                }
            })?;
            let rule = compile_block_survival(name, name, data)?.unwrap_or(SurvivalRule::Always);
            Ok((state, rule))
        };
        Ok(Self {
            seagrass: block("minecraft:seagrass")?,
            tall_seagrass: block("minecraft:tall_seagrass")?,
            kelp: block("minecraft:kelp")?,
            kelp_plant: block("minecraft:kelp_plant")?,
            sea_pickle: block("minecraft:sea_pickle")?,
        })
    }
}

/// Places an ocean plant feature.
pub fn place_ocean_plant<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    plant: &CompiledOceanPlant,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    match plant {
        CompiledOceanPlant::Seagrass {
            probability,
            blocks,
        } => place_seagrass(world, *probability, blocks, random, origin),
        CompiledOceanPlant::Kelp(blocks) => place_kelp(world, blocks, random, origin),
        CompiledOceanPlant::SeaPickle { count, blocks } => {
            place_sea_pickles(world, count, blocks, random, origin)
        }
    }
}

/// Whether the block is water, a source or flowing (`BlockState.is(Blocks.WATER)`).
fn is_water<W: FeatureWorld>(world: &W, [x, y, z]: [i32; 3]) -> bool {
    block_states()
        .properties(world.block_state(x, y, z))
        .is_some_and(|(name, _)| name == "minecraft:water")
}

fn survives<W: FeatureWorld>(world: &W, (state, rule): &(u32, SurvivalRule), at: [i32; 3]) -> bool {
    rule.test(world, at, *state)
}

fn set<W: FeatureWorld>(world: &mut W, [x, y, z]: [i32; 3], state: u32) {
    world.set_block_state(x, y, z, state);
}

/// The ocean floor of a column up to seven blocks away on each axis.
fn spread_floor<W: FeatureWorld, R: RandomSource>(
    world: &W,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> [i32; 3] {
    let dx = random.next_int_bounded(8) - random.next_int_bounded(8);
    let dz = random.next_int_bounded(8) - random.next_int_bounded(8);
    let (x, z) = (origin[0] + dx, origin[2] + dz);
    [x, world.heightmap(HeightmapType::OceanFloor, x, z), z]
}

/// `SeagrassFeature.place`.
fn place_seagrass<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    probability: f32,
    blocks: &OceanBlocks,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let position = spread_floor(world, random, origin);
    if !is_water(world, position) {
        return false;
    }
    let tall = random.next_double() < f64::from(probability);
    let plant = if tall {
        &blocks.tall_seagrass
    } else {
        &blocks.seagrass
    };
    if !survives(world, plant, position) {
        return false;
    }
    if tall {
        let above = [position[0], position[1] + 1, position[2]];
        if is_water(world, above) {
            set(world, position, plant.0);
            set(world, above, with_property(plant.0, "half", "upper"));
        }
    } else {
        set(world, position, plant.0);
    }
    true
}

/// `KelpFeature.place`.
fn place_kelp<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    blocks: &OceanBlocks,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let (x, z) = (origin[0], origin[2]);
    let mut position = [x, world.heightmap(HeightmapType::OceanFloor, x, z), z];
    if !is_water(world, position) {
        return false;
    }
    let mut placed = 0;
    let height = 1 + random.next_int_bounded(10);
    for i in 0..=height {
        let above = [position[0], position[1] + 1, position[2]];
        if is_water(world, position)
            && is_water(world, above)
            && survives(world, &blocks.kelp_plant, position)
        {
            if i == height {
                let age = (random.next_int_bounded(4) + 20).to_string();
                set(world, position, with_property(blocks.kelp.0, "age", &age));
                placed += 1;
            } else {
                set(world, position, blocks.kelp_plant.0);
            }
        } else if i > 0 {
            let below = [position[0], position[1] - 1, position[2]];
            let under = world.block_state(below[0], below[1] - 1, below[2]);
            let under_is_kelp = block_states()
                .properties(under)
                .is_some_and(|(name, _)| name == "minecraft:kelp");
            if survives(world, &blocks.kelp, below) && !under_is_kelp {
                let age = (random.next_int_bounded(4) + 20).to_string();
                set(world, below, with_property(blocks.kelp.0, "age", &age));
                placed += 1;
            }
            break;
        }
        // A first block that cannot hold kelp moves the column up instead.
        position = above;
    }
    placed > 0
}

/// `SeaPickleFeature.place`.
fn place_sea_pickles<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    count: &IntProviderDef,
    blocks: &OceanBlocks,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let mut placed = 0;
    let count = sample_int_provider(count, random).unwrap_or(0);
    for _ in 0..count {
        let position = spread_floor(world, random, origin);
        let pickles = (random.next_int_bounded(4) + 1).to_string();
        let state = with_property(blocks.sea_pickle.0, "pickles", &pickles);
        if is_water(world, position) && blocks.sea_pickle.1.test(world, position, state) {
            set(world, position, state);
            placed += 1;
        }
    }
    placed > 0
}

impl crate::configured::FeatureCompiler<'_> {
    /// Compiles `minecraft:seagrass`, `minecraft:kelp` or `minecraft:sea_pickle`; None for parts not ported yet.
    pub(crate) fn ocean_plant(
        &mut self,
        name: &str,
        kind: &str,
        config: &Value,
    ) -> Result<Option<CompiledOceanPlant>, WorldgenDataError> {
        let invalid = |message: &str| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message: message.to_string(),
        };
        let blocks = OceanBlocks::new(self.data())?;
        Ok(Some(match kind {
            "minecraft:seagrass" => CompiledOceanPlant::Seagrass {
                probability: config
                    .get("probability")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| invalid("`probability` must be a number"))?
                    as f32,
                blocks,
            },
            "minecraft:kelp" => CompiledOceanPlant::Kelp(blocks),
            "minecraft:sea_pickle" => {
                let count = parse_int_provider(
                    name,
                    config
                        .get("count")
                        .ok_or_else(|| invalid("missing field `count`"))?,
                )?;
                if matches!(count, IntProviderDef::Other(_)) {
                    return Ok(None);
                }
                CompiledOceanPlant::SeaPickle { count, blocks }
            }
            _ => return Ok(None),
        }))
    }
}
