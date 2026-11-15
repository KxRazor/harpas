//! Block states turned and reflected with a template (`BlockState.rotate` and `BlockState.mirror`).
//!
//! Directions in `facing`, `orientation` and rail shapes turn, connection properties such as a fence's `north`
//! move to the face they now name, `axis` swaps X and Z on a quarter turn and the 16 `rotation` steps of signs,
//! banners and skulls turn by four per quarter. Compared with vanilla for every state (`block_transform_parity`).

use harpas_core::block::block_states;

use super::{Facing, Mirror, Rotation};

impl Rotation {
    /// The state turned with a template (`BlockState.rotate`).
    pub fn rotate_state(self, state: u32) -> u32 {
        let turns = self as u8;
        transform(
            state,
            |facing| self.rotate(facing),
            turns % 2 == 1,
            |step| (step + 4 * turns) % 16,
            false,
        )
    }
}

impl Mirror {
    /// The state reflected with a template (`BlockState.mirror`).
    pub fn mirror_state(self, state: u32) -> u32 {
        let reflect = |facing| match facing {
            Facing::West => Facing::East,
            Facing::East => Facing::West,
            other => other,
        };
        match self {
            Self::None => state,
            Self::FrontBack => transform(state, reflect, false, |step| (16 - step) % 16, true),
        }
    }
}

/// Applies a horizontal direction map to every property that names or depends on a direction.
fn transform(
    state: u32,
    turn: impl Fn(Facing) -> Facing,
    swaps_axes: bool,
    turn_rotation: impl Fn(u8) -> u8,
    reflects: bool,
) -> u32 {
    let states = block_states();
    let (name, properties) = states.properties(state).expect("a block state");
    let turn_name = |name| turned(name, &turn);
    // Fire and chorus plants keep their faces.
    let fixed_faces = matches!(name, "minecraft:fire" | "minecraft:chorus_plant");
    let faces_x =
        properties.contains(&("facing", "east")) || properties.contains(&("facing", "west"));
    let values: Vec<(&str, String)> = properties
        .iter()
        .map(|&(key, value)| {
            let value = match key {
                // Anvils keep their facing in a mirror.
                "facing" if reflects && name.ends_with("anvil") => value.to_string(),
                "facing" => turn_name(value).to_string(),
                "axis" if swaps_axes => match value {
                    "x" => "z",
                    "z" => "x",
                    other => other,
                }
                .to_string(),
                "rotation" => turn_rotation(value.parse().expect("a rotation step")).to_string(),
                "orientation" => {
                    let (front, top) = value.split_once('_').expect("an orientation");
                    format!("{}_{}", turn_name(front), turn_name(top))
                }
                "shape" if name.ends_with("rail") => rail_shape(value, &turn_name),
                // A reflected door swaps its hinge. A reflected stair keeps its inner corners, and its
                // outer corners too unless it faces along X; chests keep their side.
                "hinge" if reflects => swap_sides(value),
                "shape" if reflects && name.ends_with("_stairs") && faces_x => swap_sides(value),
                _ => value.to_string(),
            };
            (key, value)
        })
        .collect();
    let turned_values = values.iter().map(|(key, value)| {
        let key = if fixed_faces { key } else { turn_name(key) };
        (key, value.as_str())
    });
    states.state(name, turned_values).expect("the same block").0
}

/// `left` and `outer_left` become `right` and `outer_right`, and the reverse; inner corners stay.
fn swap_sides(value: &str) -> String {
    match value {
        "left" => "right",
        "right" => "left",
        "outer_left" => "outer_right",
        "outer_right" => "outer_left",
        other => other,
    }
    .to_string()
}

/// A horizontal direction's name turned; other values stay.
fn turned<'a>(name: &'a str, turn: &impl Fn(Facing) -> Facing) -> &'a str {
    let facing = match name {
        "north" => Facing::North,
        "south" => Facing::South,
        "west" => Facing::West,
        "east" => Facing::East,
        _ => return name,
    };
    turn(facing).name()
}

/// A rail shape with both of its ends turned, written in the order the block names it: straight rails
/// start with north or east, curves with north or south.
fn rail_shape<'a>(value: &'a str, turn_name: &impl Fn(&'a str) -> &'a str) -> String {
    if let Some(end) = value.strip_prefix("ascending_") {
        return format!("ascending_{}", turn_name(end));
    }
    let (a, b) = value.split_once('_').expect("a rail shape");
    match (turn_name(a), turn_name(b)) {
        (a @ "south", b @ "north")
        | (a @ "west", b @ "east")
        | (a @ ("east" | "west"), b @ ("north" | "south")) => format!("{b}_{a}"),
        (a, b) => format!("{a}_{b}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str, properties: &[(&str, &str)]) -> u32 {
        block_states()
            .state(name, properties.iter().copied())
            .unwrap()
            .0
    }

    #[test]
    fn turns_directions_connections_and_rail_ends() {
        let fence = |props: &[(&str, &str)]| state("minecraft:oak_fence", props);
        assert_eq!(
            Rotation::Clockwise90.rotate_state(fence(&[("north", "true")])),
            fence(&[("east", "true")])
        );
        let rail = |shape| state("minecraft:rail", &[("shape", shape)]);
        assert_eq!(
            Rotation::Clockwise90.rotate_state(rail("north_east")),
            rail("south_east")
        );
        assert_eq!(
            Rotation::Clockwise180.rotate_state(rail("north_south")),
            rail("north_south")
        );
        let sign = |step| state("minecraft:oak_sign", &[("rotation", step)]);
        assert_eq!(
            Rotation::Counterclockwise90.rotate_state(sign("2")),
            sign("14")
        );
        assert_eq!(Mirror::FrontBack.mirror_state(sign("4")), sign("12"));
    }

    #[test]
    fn mirrors_keep_vanilla_exceptions() {
        let stairs = |facing, shape| {
            state(
                "minecraft:oak_stairs",
                &[("facing", facing), ("shape", shape)],
            )
        };
        assert_eq!(
            Mirror::FrontBack.mirror_state(stairs("north", "outer_left")),
            stairs("north", "outer_left")
        );
        assert_eq!(
            Mirror::FrontBack.mirror_state(stairs("east", "outer_left")),
            stairs("west", "outer_right")
        );
        assert_eq!(
            Mirror::FrontBack.mirror_state(stairs("east", "inner_left")),
            stairs("west", "inner_left")
        );
        let anvil = state("minecraft:anvil", &[("facing", "west")]);
        assert_eq!(Mirror::FrontBack.mirror_state(anvil), anvil);
        let fire = state("minecraft:fire", &[("north", "true")]);
        assert_eq!(Rotation::Clockwise90.rotate_state(fire), fire);
    }
}
