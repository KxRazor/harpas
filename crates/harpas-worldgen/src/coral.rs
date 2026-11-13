//! Warm ocean reefs: `coral_tree`, `coral_claw` and `coral_mushroom`. Each draws one coral block from
//! `#coral_blocks`, then grows in water, decorating its blocks with random `#corals`, sea pickles and
//! outward-facing `#wall_corals`. Tag encounter order and failed growth attempts affect the random stream.

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom};

use crate::configured::FeatureCompiler;
use crate::data::WorldgenDataError;
use crate::feature::FeatureWorld;
use crate::placement::StateSet;
use crate::shape_update::{Direction, with_property};
use crate::tree_decorator::HORIZONTAL;

/// A compiled reef shape and the ordered block tags it draws from.
#[derive(Debug)]
pub struct CompiledCoral {
    shape: Shape,
    blocks: Vec<u32>,
    corals: Vec<u32>,
    wall_corals: Vec<u32>,
    replaceable: StateSet,
}

#[derive(Debug)]
enum Shape {
    Tree,
    Claw,
    Mushroom,
}

impl FeatureCompiler<'_> {
    pub(crate) fn coral(&self, kind: &str) -> Result<CompiledCoral, WorldgenDataError> {
        let data = self.data();
        Ok(CompiledCoral {
            shape: match kind {
                "minecraft:coral_tree" => Shape::Tree,
                "minecraft:coral_claw" => Shape::Claw,
                "minecraft:coral_mushroom" => Shape::Mushroom,
                _ => unreachable!("coral feature type"),
            },
            blocks: data.block_tag_blocks("minecraft:coral_blocks")?,
            corals: data.block_tag_blocks("minecraft:corals")?,
            wall_corals: data.block_tag_blocks("minecraft:wall_corals")?,
            replaceable: StateSet::from_membership(&data.block_tag_states("minecraft:corals")?),
        })
    }
}

/// Places a reef (`CoralFeature.place`). An empty `#coral_blocks` tag fails without drawing randomness.
/// Tree and mushroom shapes can report success even when no block could be placed, as vanilla does.
pub fn place_coral<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    coral: &CompiledCoral,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let Some(state) = pick(&coral.blocks, random) else {
        return false;
    };
    match coral.shape {
        Shape::Tree => {
            let mut at = origin;
            let height = random.next_int_bounded(3) + 1;
            for _ in 0..height {
                if !coral.block(world, random, at, state) {
                    return true;
                }
                at = Direction::Up.step(at);
            }
            let top = at;
            let count = random.next_int_bounded(3) as usize + 2;
            let mut directions = HORIZONTAL;
            shuffle(&mut directions, random);
            for direction in &directions[..count] {
                at = direction.step(top);
                let length = random.next_int_bounded(5) + 2;
                let mut straight = 0;
                for i in 0..length {
                    if !coral.block(world, random, at, state) {
                        break;
                    }
                    straight += 1;
                    at = Direction::Up.step(at);
                    if i == 0 || (straight >= 2 && random.next_float() < 0.25) {
                        at = direction.step(at);
                        straight = 0;
                    }
                }
            }
        }
        Shape::Claw => {
            if !coral.block(world, random, origin, state) {
                return false;
            }
            let heading = random.next_int_bounded(4) as usize;
            let forward = HORIZONTAL[heading];
            let count = random.next_int_bounded(2) as usize + 2;
            let mut directions = [
                forward,
                HORIZONTAL[(heading + 1) % 4],
                HORIZONTAL[(heading + 3) % 4],
            ];
            shuffle(&mut directions, random);
            for &direction in &directions[..count] {
                let mut at = direction.step(origin);
                let length = random.next_int_bounded(2) + 1;
                let (step, extension) = if direction == forward {
                    (forward, random.next_int_bounded(3) + 2)
                } else {
                    at = Direction::Up.step(at);
                    let step = [direction, Direction::Up][random.next_int_bounded(2) as usize];
                    (step, random.next_int_bounded(3) + 3)
                };
                for _ in 0..length {
                    if !coral.block(world, random, at, state) {
                        break;
                    }
                    at = step.step(at);
                }
                at = Direction::Up.step(step.opposite().step(at));
                for _ in 0..extension {
                    at = forward.step(at);
                    if !coral.block(world, random, at, state) {
                        break;
                    }
                    if random.next_float() < 0.25 {
                        at = Direction::Up.step(at);
                    }
                }
            }
        }
        Shape::Mushroom => {
            let height = random.next_int_bounded(3) + 3;
            let width = random.next_int_bounded(3) + 3;
            let depth = random.next_int_bounded(3) + 3;
            let down = random.next_int_bounded(3) + 1;
            for dx in 0..=width {
                for dy in 0..=height {
                    for dz in 0..=depth {
                        // Faces only: omit the hollow interior and every edge and corner.
                        let faces = i32::from(dx == 0 || dx == width)
                            + i32::from(dy == 0 || dy == height)
                            + i32::from(dz == 0 || dz == depth);
                        if faces == 1 && random.next_float() >= 0.1 {
                            coral.block(
                                world,
                                random,
                                [origin[0] + dx, origin[1] + dy - down, origin[2] + dz],
                                state,
                            );
                        }
                    }
                }
            }
        }
    }
    true
}

impl CompiledCoral {
    /// `CoralFeature.placeCoralBlock`: only water and `#corals` can be replaced, with water above.
    fn block<W: FeatureWorld>(
        &self,
        world: &mut W,
        random: &mut impl RandomSource,
        [x, y, z]: [i32; 3],
        state: u32,
    ) -> bool {
        if (!is_water(world, [x, y, z]) && !self.replaceable.contains(world.block_state(x, y, z)))
            || !is_water(world, [x, y + 1, z])
        {
            return false;
        }
        world.set_block_state(x, y, z, state);
        if random.next_float() < 0.25 {
            if let Some(top) = pick(&self.corals, random) {
                world.set_block_state(x, y + 1, z, top);
            }
        } else if random.next_float() < 0.05 {
            let count = (random.next_int_bounded(4) + 1).to_string();
            let pickle = block_states()
                .default_state("minecraft:sea_pickle")
                .expect("sea pickle");
            world.set_block_state(x, y + 1, z, with_property(pickle, "pickles", &count));
        }
        for direction in HORIZONTAL {
            if random.next_float() < 0.2 {
                let [nx, ny, nz] = direction.step([x, y, z]);
                if is_water(world, [nx, ny, nz])
                    && let Some(fan) = pick(&self.wall_corals, random)
                {
                    world.set_block_state(
                        nx,
                        ny,
                        nz,
                        with_property(fan, "facing", direction.name()),
                    );
                }
            }
        }
        true
    }
}

fn is_water<W: FeatureWorld>(world: &W, [x, y, z]: [i32; 3]) -> bool {
    block_states()
        .properties(world.block_state(x, y, z))
        .is_some_and(|(name, _)| name == "minecraft:water")
}

fn pick(states: &[u32], random: &mut impl RandomSource) -> Option<u32> {
    (!states.is_empty()).then(|| states[random.next_int_bounded(states.len() as i32) as usize])
}

/// `Util.shuffle`, also used by `Direction.Plane.HORIZONTAL.shuffledCopy`.
fn shuffle<T>(values: &mut [T], random: &mut impl RandomSource) {
    for size in (2..=values.len()).rev() {
        let other = random.next_int_bounded(size as i32) as usize;
        values.swap(size - 1, other);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratedChunk, RegionFeatureWorld, WorldgenData};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::json;

    fn data() -> WorldgenData {
        WorldgenData::from_json_entries([
            ("data/minecraft/tags/block/coral_blocks.json", json!({"values": ["minecraft:brain_coral_block"]}).to_string()),
            ("data/minecraft/tags/block/corals.json", json!({"values": ["#minecraft:plants", "minecraft:tube_coral_fan", "minecraft:brain_coral"]}).to_string()),
            ("data/minecraft/tags/block/plants.json", json!({"values": ["minecraft:brain_coral", "minecraft:tube_coral", {"id": "minecraft:missing", "required": false}]}).to_string()),
            ("data/minecraft/tags/block/wall_corals.json", json!({"values": []}).to_string()),
        ]).unwrap()
    }

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn world() -> RegionFeatureWorld {
        RegionFeatureWorld::new([GeneratedChunk::new(0, 0, 0, 32, 1)], 0).unwrap()
    }

    #[test]
    fn tag_selection_keeps_nested_order_and_first_occurrences() {
        let data = data();
        assert_eq!(
            data.block_tag_blocks("minecraft:corals").unwrap(),
            [
                state("brain_coral"),
                state("tube_coral"),
                state("tube_coral_fan")
            ]
        );
    }

    #[test]
    fn empty_block_tag_fails_without_drawing_randomness() {
        let mut data = data();
        data.block_tags
            .get_mut("minecraft:coral_blocks")
            .unwrap()
            .values
            .clear();
        let coral = FeatureCompiler::new(&data)
            .coral("minecraft:coral_tree")
            .unwrap();
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(!place_coral(&mut world(), &coral, &mut random, [8, 8, 8]));
        assert_eq!(
            random.next_int(),
            WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
        );
    }

    #[test]
    fn coral_blocks_replace_plants_but_require_water_above() {
        let data = data();
        let coral = FeatureCompiler::new(&data)
            .coral("minecraft:coral_claw")
            .unwrap();
        for (name, expected) in [
            ("water", true),
            ("tube_coral", true),
            ("tube_coral_fan", true),
            ("tube_coral_block", false),
            ("tube_coral_wall_fan", false),
            ("seagrass", false),
            ("air", false),
        ] {
            let mut world = world();
            world.set_block_state(8, 8, 8, state(name));
            world.set_block_state(8, 9, 8, state("water"));
            let mut random = XoroshiroRandom::new(42);
            assert_eq!(
                coral.block(
                    &mut world,
                    &mut random,
                    [8, 8, 8],
                    state("brain_coral_block")
                ),
                expected,
                "{name}"
            );
            if expected {
                assert_eq!(world.block_state(8, 8, 8), state("brain_coral_block"));
            }
        }
        let mut world = world();
        world.set_block_state(8, 8, 8, state("water"));
        assert!(!coral.block(
            &mut world,
            &mut XoroshiroRandom::new(42),
            [8, 8, 8],
            state("brain_coral_block")
        ));
    }
}
