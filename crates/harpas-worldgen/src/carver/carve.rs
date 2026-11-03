//! Applying carver ellipsoids to a generated chunk.

use harpas_core::block::block_states;

use super::{CarverConfig, CarvingEllipsoid, CarvingMask};
use crate::generator::GeneratedChunk;

/// The aquifer and surface-rule operations needed while carving.
pub trait CarvingEnvironment {
    /// Returns the aquifer's material at density zero, or `None` when it prevents carving.
    fn compute_substance(&mut self, x: i32, y: i32, z: i32) -> Option<u32>;
    /// Whether the most recent aquifer computation requests fluid postprocessing.
    fn should_schedule_fluid_update(&self) -> bool;
    /// Whether a block state contains fluid.
    fn has_fluid(&self, state: u32) -> bool;
    /// Evaluates the top-material rule for dirt exposed below a carved surface block.
    fn top_material(
        &mut self,
        chunk: &GeneratedChunk,
        x: i32,
        y: i32,
        z: i32,
        above_has_fluid: bool,
    ) -> Option<u32>;
}

/// Carves one ellipsoid using a cave or canyon skip check and the supplied aquifer and surface rules.
/// Returns whether at least one block was replaced.
pub fn carve_ellipsoid(
    chunk: &mut GeneratedChunk,
    mask: &mut CarvingMask,
    config: &CarverConfig,
    ellipsoid: CarvingEllipsoid,
    environment: &mut impl CarvingEnvironment,
    mut should_skip: impl FnMut([f64; 3], i32) -> bool,
) -> bool {
    let states = block_states();
    let lava = states.default_state("minecraft:lava").expect("lava state");
    let dirt = states.default_state("minecraft:dirt").expect("dirt state");
    let chunk_x = chunk.chunk_x();
    let chunk_z = chunk.chunk_z();
    let min_y = chunk.min_y();
    let height = chunk.height();
    let lava_level = config.lava_level.resolve(min_y, height);
    let mut column = None;
    let mut found_surface = false;
    let mut changed = false;
    ellipsoid.for_each_candidate(chunk_x, chunk_z, min_y, height, |x, y, z, normalized| {
        if column != Some((x, z)) {
            column = Some((x, z));
            found_surface = false;
        }
        if should_skip(normalized, y) || !mask.insert(x, y, z) {
            return;
        }
        let previous = chunk.block_state(x as usize, y, z as usize);
        if states
            .properties(previous)
            .is_some_and(|(name, _)| matches!(name, "minecraft:grass_block" | "minecraft:mycelium"))
        {
            found_surface = true;
        }
        if !config
            .replaceable
            .get(previous as usize)
            .copied()
            .unwrap_or(false)
        {
            return;
        }
        let bx = chunk_x.wrapping_mul(16).wrapping_add(i32::from(x));
        let bz = chunk_z.wrapping_mul(16).wrapping_add(i32::from(z));
        let result = if y <= lava_level {
            Some(lava)
        } else {
            environment.compute_substance(bx, y, bz)
        };
        let Some(state) = result else { return };
        chunk.set_block_state(x as usize, y, z as usize, state);
        changed = true;
        let fluid = environment.has_fluid(state);
        if fluid && environment.should_schedule_fluid_update() {
            chunk.mark_fluid_postprocessing(x, y, z);
        }
        if found_surface
            && chunk.block_state(x as usize, y - 1, z as usize) == dirt
            && let Some(top) = environment.top_material(chunk, bx, y - 1, bz, fluid)
        {
            chunk.set_block_state(x as usize, y - 1, z as usize, top);
        }
    });
    changed
}
