//! Vanilla's light engine: sky light and block light.
//!
//! Light is stored as four bits per block, two blocks per byte, in vanilla's section order. A chunk is lit
//! from a volume one chunk wider than itself on every side: light never travels more than fifteen blocks, so
//! the extra chunk is enough for every value that can reach the target.

use harpas_core::block::block_states;

/// Bytes in one section's light layer: 4,096 blocks at four bits each.
pub const LIGHT_BYTES: usize = 2048;

/// One section's sky light and block light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightSection {
    /// The section's height, in sections.
    pub y: i32,
    /// Sky light, packed as vanilla packs it.
    pub sky: Vec<u8>,
    /// Block light, packed as vanilla packs it.
    pub block: Vec<u8>,
}

/// The light of one chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkLight {
    /// The chunk's X coordinate.
    pub x: i32,
    /// The chunk's Z coordinate.
    pub z: i32,
    /// The sections, from the lowest light section to the highest.
    pub sections: Vec<LightSection>,
}

/// Block state access for a volume that covers the target chunk and one chunk on every side.
pub trait LightWorld {
    /// The lowest block Y in the world.
    fn min_y(&self) -> i32;
    /// The number of block Y values in the world.
    fn height(&self) -> i32;
    /// The block state at world coordinates.
    fn block_state(&self, x: i32, y: i32, z: i32) -> u32;
}

/// The width and depth of the lit volume, in blocks: the target chunk plus one on every side.
const VOLUME: i32 = 48;

/// Lights the chunk at `chunk_x, chunk_z`.
///
/// `world` must answer for the one-chunk border around the target. The returned sections include the light-only
/// sections just below and above the world, as vanilla stores them.
pub fn light_chunk(world: &dyn LightWorld, chunk_x: i32, chunk_z: i32) -> ChunkLight {
    let min_y = world.min_y();
    let height = world.height();
    assert!(
        height > 0,
        "a light volume needs at least one block of height"
    );
    let min_x = chunk_x * 16 - 16;
    let min_z = chunk_z * 16 - 16;
    let width = VOLUME as usize;
    let depth = VOLUME as usize;
    let height_usize = height as usize;
    let states = block_states();

    let index = |x: i32, y: i32, z: i32| -> usize {
        ((y - min_y) as usize * depth + (z - min_z) as usize) * width + (x - min_x) as usize
    };

    let mut sky = vec![0_u8; width * depth * height_usize];
    let mut block = vec![0_u8; width * depth * height_usize];
    let mut sky_queue: Vec<Vec<usize>> = vec![Vec::new(); 16];
    let mut block_queue: Vec<Vec<usize>> = vec![Vec::new(); 16];

    // Sky light starts at fifteen above the first block that dampens it in each column.
    for x in 0..VOLUME {
        for z in 0..VOLUME {
            let mut y = min_y + height - 1;
            while y >= min_y {
                let state = world.block_state(min_x + x, y, min_z + z);
                if states.light_dampening(state) > 0 {
                    break;
                }
                let at = index(min_x + x, y, min_z + z);
                sky[at] = 15;
                sky_queue[15].push(at);
                y -= 1;
            }
        }
    }

    // Every emitting block starts its own block-light source.
    for y in min_y..min_y + height {
        for z in 0..VOLUME {
            for x in 0..VOLUME {
                let state = world.block_state(min_x + x, y, min_z + z);
                let emission = states.light_emission(state);
                if emission > 0 {
                    let at = index(min_x + x, y, min_z + z);
                    if emission > block[at] {
                        block[at] = emission;
                        block_queue[usize::from(emission)].push(at);
                    }
                }
            }
        }
    }

    propagate(
        world,
        states,
        min_x,
        min_y,
        min_z,
        width,
        depth,
        height_usize,
        &mut sky,
        &mut sky_queue,
        true,
    );
    propagate(
        world,
        states,
        min_x,
        min_y,
        min_z,
        width,
        depth,
        height_usize,
        &mut block,
        &mut block_queue,
        false,
    );

    let min_section = min_y.div_euclid(16);
    let max_section = (min_y + height).div_euclid(16) - 1;
    let mut sections = Vec::with_capacity((max_section - min_section + 3) as usize);
    for section_y in min_section - 1..=max_section + 1 {
        let mut sky_layer = vec![0_u8; LIGHT_BYTES];
        let mut block_layer = vec![0_u8; LIGHT_BYTES];
        if section_y < min_section {
            // Below the world every light value is zero.
        } else if section_y > max_section {
            // Above the world the sky is open, and there are no blocks to emit light.
            sky_layer.fill(0xff);
        } else {
            for local_y in 0..16 {
                let y = section_y * 16 + local_y;
                for local_z in 0..16 {
                    for local_x in 0..16 {
                        let x = chunk_x * 16 + local_x;
                        let z = chunk_z * 16 + local_z;
                        let at = index(x, y, z);
                        let block_index = (local_y * 256 + local_z * 16 + local_x) as usize;
                        set_nibble(&mut sky_layer, block_index, sky[at]);
                        set_nibble(&mut block_layer, block_index, block[at]);
                    }
                }
            }
        }
        sections.push(LightSection {
            y: section_y,
            sky: sky_layer,
            block: block_layer,
        });
    }

    ChunkLight {
        x: chunk_x,
        z: chunk_z,
        sections,
    }
}

#[allow(clippy::too_many_arguments)]
fn propagate(
    world: &dyn LightWorld,
    states: &harpas_core::block::BlockStates,
    min_x: i32,
    min_y: i32,
    min_z: i32,
    width: usize,
    depth: usize,
    height: usize,
    light: &mut [u8],
    queue: &mut [Vec<usize>],
    sky: bool,
) {
    const NEIGHBORS: [(i32, i32, i32); 6] = [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];
    let max_y = min_y + height as i32 - 1;
    for level in (1..=15_u8).rev() {
        while let Some(at) = queue[usize::from(level)].pop() {
            if light[at] != level {
                continue;
            }
            let y = at / (width * depth);
            let remainder = at % (width * depth);
            let z = remainder / width;
            let x = remainder % width;
            let world_x = min_x + x as i32;
            let world_y = min_y + y as i32;
            let world_z = min_z + z as i32;
            for (dx, dy, dz) in NEIGHBORS {
                let nx = world_x + dx;
                let ny = world_y + dy;
                let nz = world_z + dz;
                if !(min_y..=max_y).contains(&ny) {
                    continue;
                }
                let local_x = (nx - min_x) as usize;
                let local_z = (nz - min_z) as usize;
                if local_x >= width || local_z >= depth {
                    continue;
                }
                let neighbor = (ny - min_y) as usize * depth * width + local_z * width + local_x;
                let state = world.block_state(nx, ny, nz);
                let dampening = states.light_dampening(state);
                // Direct sky light (fifteen) falls through clear blocks without fading; any dimmer light
                // loses a level per block in every direction, including down.
                let next = if sky && dy < 0 && dampening == 0 && level == 15 {
                    level
                } else {
                    level.saturating_sub(dampening.max(1))
                };
                if next > light[neighbor] {
                    light[neighbor] = next;
                    queue[usize::from(next)].push(neighbor);
                }
            }
        }
    }
}

fn set_nibble(layer: &mut [u8], index: usize, value: u8) {
    debug_assert!(index < 4096);
    let byte = index / 2;
    let shift = if index.is_multiple_of(2) { 0 } else { 4 };
    layer[byte] = (layer[byte] & !(0x0f << shift)) | ((value & 0x0f) << shift);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use harpas_core::block::block_states;

    struct TestWorld {
        blocks: HashMap<(i32, i32, i32), u32>,
    }

    impl TestWorld {
        fn flat() -> Self {
            let stone = block_states().default_state("minecraft:stone").unwrap();
            let mut blocks = HashMap::new();
            for x in -16..32 {
                for z in -16..32 {
                    blocks.insert((x, 0, z), stone);
                }
            }
            Self { blocks }
        }

        fn set(&mut self, x: i32, y: i32, z: i32, name: &str) {
            self.blocks
                .insert((x, y, z), block_states().default_state(name).unwrap());
        }
    }

    impl LightWorld for TestWorld {
        fn min_y(&self) -> i32 {
            0
        }

        fn height(&self) -> i32 {
            16
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> u32 {
            self.blocks.get(&(x, y, z)).copied().unwrap_or(0)
        }
    }

    fn nibble(layer: &[u8], index: usize) -> u8 {
        let byte = layer[index / 2];
        if index.is_multiple_of(2) {
            byte & 0x0f
        } else {
            byte >> 4
        }
    }

    fn section(light: &ChunkLight, y: i32) -> &LightSection {
        light
            .sections
            .iter()
            .find(|section| section.y == y)
            .unwrap()
    }

    fn index(x: usize, y: usize, z: usize) -> usize {
        y * 256 + z * 16 + x
    }

    #[test]
    fn open_sky_is_full_and_the_ground_stops_it() {
        let light = light_chunk(&TestWorld::flat(), 0, 0);
        assert_eq!(
            light
                .sections
                .iter()
                .map(|section| section.y)
                .collect::<Vec<_>>(),
            vec![-1, 0, 1]
        );
        let ground = section(&light, 0);
        assert_eq!(nibble(&ground.sky, index(8, 0, 8)), 0);
        assert_eq!(nibble(&ground.sky, index(8, 1, 8)), 15);
        assert_eq!(nibble(&ground.sky, index(8, 15, 8)), 15);
        assert!(ground.block.iter().all(|byte| *byte == 0));
        let above = section(&light, 1);
        assert!(above.sky.iter().all(|byte| *byte == 0xff));
        assert!(above.block.iter().all(|byte| *byte == 0));
        let below = section(&light, -1);
        assert!(below.sky.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn block_light_falls_off_by_one_per_block() {
        let mut world = TestWorld::flat();
        world.set(8, 1, 8, "minecraft:glowstone");
        let light = light_chunk(&world, 0, 0);
        let section = section(&light, 0);
        assert_eq!(nibble(&section.block, index(8, 1, 8)), 15);
        assert_eq!(nibble(&section.block, index(9, 1, 8)), 14);
        assert_eq!(nibble(&section.block, index(10, 1, 8)), 13);
        assert_eq!(nibble(&section.block, index(8, 2, 8)), 14);
        assert_eq!(nibble(&section.block, index(8, 1, 9)), 14);
        // Stone at the floor is opaque.
        assert_eq!(nibble(&section.block, index(8, 0, 8)), 0);
    }

    #[test]
    fn water_dims_sky_light_by_one_per_block() {
        let mut world = TestWorld::flat();
        for y in 1..=3 {
            for x in -16..32 {
                for z in -16..32 {
                    world.set(x, y, z, "minecraft:water");
                }
            }
        }
        let light = light_chunk(&world, 0, 0);
        let section = section(&light, 0);
        assert_eq!(nibble(&section.sky, index(8, 3, 8)), 14);
        assert_eq!(nibble(&section.sky, index(8, 2, 8)), 13);
        assert_eq!(nibble(&section.sky, index(8, 1, 8)), 12);
        assert_eq!(nibble(&section.sky, index(8, 0, 8)), 0);
    }

    #[test]
    fn light_crosses_a_chunk_border() {
        let mut world = TestWorld::flat();
        world.set(-1, 1, 8, "minecraft:torch");
        let light = light_chunk(&world, 0, 0);
        let section = section(&light, 0);
        assert_eq!(nibble(&section.block, index(0, 1, 8)), 13);
        assert_eq!(nibble(&section.block, index(7, 1, 8)), 6);
        assert_eq!(nibble(&section.block, index(8, 1, 8)), 5);
    }

    #[test]
    fn an_empty_chunk_has_full_sky_light_everywhere() {
        let world = TestWorld {
            blocks: HashMap::new(),
        };
        let light = light_chunk(&world, 0, 0);
        let section = section(&light, 0);
        assert!(section.sky.iter().all(|byte| *byte == 0xff));
        assert!(section.block.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn nibbles_are_low_first() {
        let mut layer = vec![0_u8; LIGHT_BYTES];
        set_nibble(&mut layer, 0, 3);
        set_nibble(&mut layer, 1, 12);
        assert_eq!(layer[0], 0xc3);
        assert_eq!(nibble(&layer, 0), 3);
        assert_eq!(nibble(&layer, 1), 12);
    }
}
