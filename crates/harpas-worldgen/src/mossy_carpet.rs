//! Pale moss carpets (`MossyCarpetBlock`): a carpet on the ground, or a topper above one, with a side on each
//! horizontal face something can hold.
//!
//! A carpet's side is low where the block beside it has a full face towards it, tall where the carpet above it
//! has a side there too, and gone where the carpet below has none. `simple_block` places a carpet through
//! `placeAt`, which also grows a topper above it whose sides each keep by a coin flip from the region's own random
//! source (`WorldGenRegion.getRandom`), not the feature's.

use harpas_core::block::block_states;
use harpas_core::random::RandomSource;

use crate::feature::FeatureWorld;
use crate::shape_update::{Direction, can_attach_to, property, with_property};

const BLOCK: &str = "minecraft:pale_moss_carpet";
const SIDES: [(Direction, &str); 4] = [
    (Direction::North, "north"),
    (Direction::East, "east"),
    (Direction::South, "south"),
    (Direction::West, "west"),
];

/// Whether a state is a pale moss carpet.
pub(crate) fn is_carpet(state: u32) -> bool {
    block_states()
        .properties(state)
        .is_some_and(|(name, _)| name == BLOCK)
}

fn base(state: u32) -> bool {
    property(state, "bottom") == Some("true")
}

/// `MossyCarpetBlock.canSurvive`: a carpet on any block that is not air; a topper on a carpet.
pub(crate) fn can_survive(world: &impl FeatureWorld, [x, y, z]: [i32; 3], state: u32) -> bool {
    let below = world.block_state(x, y - 1, z);
    if base(state) {
        !block_states().is_air(below)
    } else {
        is_carpet(below) && base(below)
    }
}

/// `MossyCarpetBlock.hasFaces`.
fn has_faces(state: u32) -> bool {
    base(state)
        || SIDES
            .iter()
            .any(|(_, side)| property(state, side) != Some("none"))
}

/// `MossyCarpetBlock.getUpdatedState`.
fn updated_state(
    world: &impl FeatureWorld,
    mut state: u32,
    position: [i32; 3],
    create_sides: bool,
) -> u32 {
    let create_sides = create_sides || base(state);
    let [x, y, z] = position;
    let above = world.block_state(x, y + 1, z);
    let below = world.block_state(x, y - 1, z);
    for (direction, side) in SIDES {
        let mut value = if can_attach_to(world, direction.step(position), direction) {
            if create_sides {
                "low"
            } else {
                property(state, side).unwrap_or("none")
            }
        } else {
            "none"
        };
        if value == "low" {
            if is_carpet(above) && property(above, side) != Some("none") && !base(above) {
                value = "tall";
            }
            if !base(state) && is_carpet(below) && property(below, side) == Some("none") {
                value = "none";
            }
        }
        state = with_property(state, side, value);
    }
    state
}

/// `MossyCarpetBlock.updateShape`: air once it cannot survive or has no faces left.
pub(crate) fn update_shape(
    world: &impl FeatureWorld,
    state: u32,
    position: [i32; 3],
    air: u32,
) -> u32 {
    if !can_survive(world, position, state) {
        return air;
    }
    let updated = updated_state(world, state, position, false);
    if has_faces(updated) { updated } else { air }
}

/// `MossyCarpetBlock.placeAt`: a carpet with its sides, then a topper above it where there is room, with each of
/// its sides kept by a coin flip of `random`.
pub(crate) fn place_at(
    world: &mut impl FeatureWorld,
    position: [i32; 3],
    random: &mut impl RandomSource,
) {
    let Some(carpet) = block_states().default_state(BLOCK) else {
        return;
    };
    let [x, y, z] = position;
    let state = updated_state(world, carpet, position, true);
    world.set_block_state(x, y, z, state);
    let topper = topper_with_side_chance(world, carpet, position, random);
    if !block_states().is_air(topper) {
        world.set_block_state(x, y + 1, z, topper);
        let state = updated_state(world, state, position, true);
        world.set_block_state(x, y, z, state);
    }
}

/// `MossyCarpetBlock.createTopperWithSideChance`.
fn topper_with_side_chance(
    world: &impl FeatureWorld,
    carpet: u32,
    position: [i32; 3],
    random: &mut impl RandomSource,
) -> u32 {
    let air = block_states().default_state("minecraft:air").unwrap_or(0);
    let above = Direction::Up.step(position);
    let current = world.block_state(above[0], above[1], above[2]);
    let carpet_above = is_carpet(current);
    if (carpet_above && base(current))
        || (!carpet_above
            && !harpas_core::block_behavior::block_behavior().can_be_replaced(current))
    {
        return air;
    }
    let mut topper = updated_state(world, with_property(carpet, "bottom", "false"), above, true);
    for (_, side) in SIDES {
        if property(topper, side) != Some("none") && !random.next_bool() {
            topper = with_property(topper, side, "none");
        }
    }
    if has_faces(topper) && topper != current {
        topper
    } else {
        air
    }
}
