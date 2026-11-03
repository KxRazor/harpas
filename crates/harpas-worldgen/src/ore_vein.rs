//! `OreVeinifier`: giant copper-granite and iron-tuff ore veins (`vein_toggle`, `vein_ridged`, `vein_gap`)
//!
//! Evaluates vanilla's 3D ore vein density channels and positional RNG (`minecraft:ore`) for solid
//! terrain blocks when `ore_veins_enabled` is true (`docs/world.md#world-generation`).

use harpas_core::block::block_states;
use harpas_core::random::RandomSource;

use crate::aquifer::clamped_map;
use crate::density::PositionalRandom;
use crate::noise_chunk::NoiseChunk;

const VEININESS_THRESHOLD: f32 = 0.4;
const EDGE_ROUNDOFF_BEGIN: i32 = 20;
const MAX_EDGE_ROUNDOFF: f64 = 0.2;
const VEIN_SOLIDNESS: f32 = 0.7;
const MIN_RICHNESS: f32 = 0.1;
const MAX_RICHNESS: f32 = 0.3;
const MAX_RICHNESS_THRESHOLD: f32 = 0.6;
const CHANCE_OF_RAW_ORE_BLOCK: f32 = 0.02;
const SKIP_ORE_IF_GAP_NOISE_IS_BELOW: f32 = -0.3;

/// Ore vein type (`OreVeinifier.VeinType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VeinType {
    /// Ore block state ID (`minecraft:copper_ore` or `minecraft:deepslate_iron_ore`).
    pub ore: u32,
    /// Raw ore block state ID (`minecraft:raw_copper_block` or `minecraft:raw_iron_block`).
    pub raw_ore_block: u32,
    /// Stone filler block state ID (`minecraft:granite` or `minecraft:tuff`).
    pub filler: u32,
    /// Minimum Y coordinate where this vein can generate.
    pub min_y: i32,
    /// Maximum Y coordinate where this vein can generate.
    pub max_y: i32,
}

/// Vanilla's `OreVeinifier` evaluator.
#[derive(Debug, Clone)]
pub struct OreVeinifier {
    positional_random: PositionalRandom,
    copper: VeinType,
    iron: VeinType,
}

impl OreVeinifier {
    /// Creates an `OreVeinifier` from the `"minecraft:ore"` `PositionalRandom` factory, looking up the
    /// default block states in `harpas_core::block::block_states()`.
    pub fn new(positional_random: PositionalRandom) -> Self {
        let states = block_states();
        let copper = VeinType {
            ore: states
                .default_state("minecraft:copper_ore")
                .expect("copper_ore state"),
            raw_ore_block: states
                .default_state("minecraft:raw_copper_block")
                .expect("raw_copper_block state"),
            filler: states
                .default_state("minecraft:granite")
                .expect("granite state"),
            min_y: 0,
            max_y: 50,
        };
        let iron = VeinType {
            ore: states
                .default_state("minecraft:deepslate_iron_ore")
                .expect("deepslate_iron_ore state"),
            raw_ore_block: states
                .default_state("minecraft:raw_iron_block")
                .expect("raw_iron_block state"),
            filler: states.default_state("minecraft:tuff").expect("tuff state"),
            min_y: -60,
            max_y: -8,
        };
        Self {
            positional_random,
            copper,
            iron,
        }
    }

    /// Creates an `OreVeinifier` with explicit `copper` and `iron` [`VeinType`] definitions.
    pub const fn with_types(
        positional_random: PositionalRandom,
        copper: VeinType,
        iron: VeinType,
    ) -> Self {
        Self {
            positional_random,
            copper,
            iron,
        }
    }

    /// Copper vein parameters (`min_y = 0, max_y = 50`).
    pub const fn copper(&self) -> VeinType {
        self.copper
    }

    /// Iron vein parameters (`min_y = -60, max_y = -8`).
    pub const fn iron(&self) -> VeinType {
        self.iron
    }

    /// Evaluates the ore vein block state at the current `noise_chunk` cell position `(block_x, block_y, block_z)`.
    ///
    /// Returns `Some(block_state_id)` when an ore vein filler, ore, or raw ore block replaces the default stone,
    /// or `None` when no ore vein is present.
    pub fn compute(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        block_x: i32,
        block_y: i32,
        block_z: i32,
    ) -> Option<u32> {
        let router = noise_chunk.router();
        let toggle_node = router.vein_toggle;
        let ridged_node = router.vein_ridged;
        let gap_node = router.vein_gap;

        let toggle = noise_chunk.eval_node(toggle_node);
        self.compute_with_samplers(block_x, block_y, block_z, toggle, |is_gap| {
            noise_chunk.eval_node(if is_gap { gap_node } else { ridged_node })
        })
    }

    /// Evaluates the ore vein block state at `(block_x, block_y, block_z)` given `toggle` and a lazy
    /// `eval_channel` closure (`eval_channel(false)` evaluates `vein_ridged`, `eval_channel(true)` evaluates `vein_gap`).
    pub fn compute_with_samplers(
        &self,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        toggle: f64,
        eval_channel: impl FnMut(bool) -> f64,
    ) -> Option<u32> {
        let vein_type = if toggle > 0.0 { self.copper } else { self.iron };
        let abs_toggle = toggle.abs();
        let dist_from_top = vein_type.max_y - block_y;
        let dist_from_bottom = block_y - vein_type.min_y;
        if dist_from_bottom < 0 || dist_from_top < 0 {
            return None;
        }

        let min_edge_dist = dist_from_top.min(dist_from_bottom);
        let edge_roundoff = clamped_map(
            f64::from(min_edge_dist),
            0.0,
            f64::from(EDGE_ROUNDOFF_BEGIN),
            -MAX_EDGE_ROUNDOFF,
            0.0,
        );
        if abs_toggle + edge_roundoff < f64::from(VEININESS_THRESHOLD) {
            return None;
        }

        match &self.positional_random {
            PositionalRandom::Xoroshiro(f) => {
                let mut rng = f.at(block_x, block_y, block_z);
                Self::eval_with_rng(&mut rng, eval_channel, vein_type, abs_toggle)
            }
            PositionalRandom::Legacy(f) => {
                let mut rng = f.at(block_x, block_y, block_z);
                Self::eval_with_rng(&mut rng, eval_channel, vein_type, abs_toggle)
            }
        }
    }

    fn eval_with_rng(
        rng: &mut impl RandomSource,
        mut eval_channel: impl FnMut(bool) -> f64,
        vein_type: VeinType,
        abs_toggle: f64,
    ) -> Option<u32> {
        if rng.next_float() > VEIN_SOLIDNESS {
            return None;
        }
        if eval_channel(false) >= 0.0 {
            return None;
        }
        let richness = clamped_map(
            abs_toggle,
            f64::from(VEININESS_THRESHOLD),
            f64::from(MAX_RICHNESS_THRESHOLD),
            f64::from(MIN_RICHNESS),
            f64::from(MAX_RICHNESS),
        );
        if f64::from(rng.next_float()) < richness
            && eval_channel(true) > f64::from(SKIP_ORE_IF_GAP_NOISE_IS_BELOW)
        {
            Some(if rng.next_float() < CHANCE_OF_RAW_ORE_BLOCK {
                vein_type.raw_ore_block
            } else {
                vein_type.ore
            })
        } else {
            Some(vein_type.filler)
        }
    }
}
