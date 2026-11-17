//! Block collision shapes and the state flags that block predicates and survival rules read.
//!
//! Shapes come from `data/block-shapes-26.1.txt`. The flags follow vanilla's `BlockStateBase`: `isSolid` is the
//! legacy solid flag, forced on or off for some blocks and otherwise computed from the collision shape;
//! `canBeReplaced` and `liquid` are set by a block's properties; `isFaceSturdy` asks whether the support shape
//! covers a whole face, or its center. The support shape is the collision shape except for the blocks listed in
//! `support_shape`. `isSolidRender` asks whether a block that occludes has a full occlusion shape, which is a
//! full collision shape except for the blocks listed in `is_solid_render`. Directional occlusion uses the
//! same shape, except for mud, soul sand and snow, and excludes blocks whose properties call `noOcclusion`. Light
//! occlusion uses the faces of that shape for the blocks that `useShapeForLightOcclusion`, with every layer of snow.

use std::sync::OnceLock;

use crate::block::block_states;

/// One of the six faces of a block (`Direction`), in vanilla's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    /// Negative Y.
    Down,
    /// Positive Y.
    Up,
    /// Negative Z.
    North,
    /// Positive Z.
    South,
    /// Negative X.
    West,
    /// Positive X.
    East,
}

impl Face {
    /// The six faces in vanilla's order.
    pub const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];

    /// The four horizontal faces in vanilla's order (`Direction.Plane.HORIZONTAL`).
    pub const HORIZONTAL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    /// The face on the other side of the block.
    pub const fn opposite(self) -> Self {
        match self {
            Self::Down => Self::Up,
            Self::Up => Self::Down,
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }

    /// The unit offset towards this face.
    pub const fn offset(self) -> [i32; 3] {
        match self {
            Self::Down => [0, -1, 0],
            Self::Up => [0, 1, 0],
            Self::North => [0, 0, -1],
            Self::South => [0, 0, 1],
            Self::West => [-1, 0, 0],
            Self::East => [1, 0, 0],
        }
    }

    /// The axis this face is perpendicular to (0 for X, 1 for Y, 2 for Z), and whether it faces the negative
    /// side.
    const fn axis(self) -> (usize, bool) {
        match self {
            Self::Down => (1, true),
            Self::Up => (1, false),
            Self::North => (2, true),
            Self::South => (2, false),
            Self::West => (0, true),
            Self::East => (0, false),
        }
    }
}

/// One box of a collision shape, in 1/32 of a block from the block's lower corner. Some shapes reach past the
/// block, such as fences, which are 1.5 blocks high.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeBox {
    /// Lower corner.
    pub min: [i8; 3],
    /// Upper corner.
    pub max: [i8; 3],
}

const SOLID: u8 = 1;
const REPLACEABLE: u8 = 1 << 1;
const LIQUID: u8 = 1 << 2;
const BLOCKS_MOTION: u8 = 1 << 3;
const SOLID_RENDER: u8 = 1 << 4;
const BLOCK_ENTITY: u8 = 1 << 5;

/// The collision shapes and behavior flags of every block state.
#[derive(Debug)]
pub struct BlockBehavior {
    shapes: Vec<Vec<ShapeBox>>,
    /// For each state ID, the index of its collision shape.
    state_shapes: Vec<u16>,
    /// For each state ID, [`SOLID`], [`REPLACEABLE`], [`LIQUID`], [`BLOCKS_MOTION`], [`SOLID_RENDER`] and
    /// [`BLOCK_ENTITY`].
    flags: Vec<u8>,
    /// For each state ID, one bit per [`Face`] whose support shape is full, and from bit 8 one per face whose
    /// support shape covers the face's center.
    sturdy: Vec<u16>,
    /// For each shape, its full faces as [`full_faces`] returns them.
    shape_faces: Vec<u16>,
    /// For each state, the full faces of its occlusion shape when it can occlude.
    occlusion_faces: Vec<u8>,
    /// For each state, the index in `light_faces` of the faces it occludes light with, or [`NO_LIGHT_SHAPE`].
    light_shapes: Vec<u16>,
    /// The distinct faces that states occlude light with, one mask per [`Face`].
    light_faces: Vec<[FaceMask; 6]>,
}

/// A face of a shape: 32 rows of 32 bits, one bit per 1/32 of a block it covers.
type FaceMask = [u32; 32];

/// A state that occludes light by its block light dampening alone.
const NO_LIGHT_SHAPE: u16 = u16::MAX;

/// The block behavior of Java 26.1.
pub fn block_behavior() -> &'static BlockBehavior {
    static BEHAVIOR: OnceLock<BlockBehavior> = OnceLock::new();
    BEHAVIOR.get_or_init(|| BlockBehavior::parse(include_str!("../data/block-shapes-26.1.txt")))
}

impl BlockBehavior {
    fn parse(text: &str) -> Self {
        let states = block_states();
        let mut shapes: Vec<Vec<ShapeBox>> = Vec::new();
        let mut state_shapes = Vec::with_capacity(states.len() as usize);
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let mut fields = line.split(' ');
            let name = fields.next().expect("a line name");
            if name == "shape" {
                let values: Vec<i8> = fields
                    .map(|value| value.parse().expect("a shape coordinate"))
                    .collect();
                let (boxes, rest) = values.as_chunks::<6>();
                assert!(rest.is_empty(), "shape boxes have six coordinates");
                shapes.push(
                    boxes
                        .iter()
                        .map(|&[x1, y1, z1, x2, y2, z2]| ShapeBox {
                            min: [x1, y1, z1],
                            max: [x2, y2, z2],
                        })
                        .collect::<Vec<_>>(),
                );
                continue;
            }
            for field in fields {
                let (shape, count) = field.split_once('*').unwrap_or((field, "1"));
                let shape: u16 = shape.parse().expect("a shape index");
                let count: usize = count.parse().expect("a state count");
                state_shapes.extend(std::iter::repeat_n(shape, count));
            }
            let first = states
                .default_state(&format!("minecraft:{name}"))
                .expect("a known block");
            assert!(
                state_shapes.len() > first as usize,
                "shapes of {name} follow the registry"
            );
        }
        assert_eq!(
            state_shapes.len(),
            states.len() as usize,
            "one shape per block state"
        );
        let shape_faces: Vec<u16> = shapes.iter().map(|boxes| full_faces(boxes)).collect();
        let mut flags = Vec::with_capacity(state_shapes.len());
        let mut sturdy = Vec::with_capacity(state_shapes.len());
        let mut occlusion_faces = Vec::with_capacity(state_shapes.len());
        let mut light_shapes = Vec::with_capacity(state_shapes.len());
        let mut light_faces = Vec::new();
        // Light faces by collision shape index, or by snow layers past the shapes.
        let mut light_face_index: std::collections::HashMap<usize, u16> =
            std::collections::HashMap::new();
        for (id, &shape) in state_shapes.iter().enumerate() {
            let (name, properties) = states.properties(id as u32).expect("known block state");
            let name = name.strip_prefix("minecraft:").unwrap_or(name);
            let boxes = &shapes[shape as usize];
            let mut flag = 0;
            if is_solid(name, boxes) {
                flag |= SOLID;
                if !matches!(name, "cobweb" | "bamboo_sapling") {
                    flag |= BLOCKS_MOTION;
                }
            }
            if REPLACEABLE_BLOCKS.contains(&name) {
                flag |= REPLACEABLE;
            }
            if matches!(name, "water" | "lava" | "bubble_column") {
                flag |= LIQUID;
            }
            if is_solid_render(name, &properties, boxes) {
                flag |= SOLID_RENDER;
            }
            if has_block_entity(name) {
                flag |= BLOCK_ENTITY;
            }
            flags.push(flag);
            occlusion_faces.push(if no_occlusion(name) {
                0
            } else {
                match name {
                    "mud" | "soul_sand" => 63,
                    "snow" if properties.contains(&("layers", "8")) => 63,
                    "snow" => 1 << Face::Down as u8,
                    _ => (shape_faces[shape as usize] & 63) as u8,
                }
            });
            light_shapes.push(if uses_shape_for_light_occlusion(name, &properties) {
                // A snow layer occludes with all its layers, one more than it collides with.
                let layers = properties
                    .iter()
                    .find(|(key, _)| *key == "layers")
                    .map(|(_, value)| value.parse::<i8>().expect("snow layers"));
                let (key, boxes) = match (name, layers) {
                    ("snow", Some(layers)) => (
                        shapes.len() + layers as usize,
                        vec![ShapeBox {
                            min: [0; 3],
                            max: [32, layers * 4, 32],
                        }],
                    ),
                    _ => (shape as usize, shapes[shape as usize].clone()),
                };
                *light_face_index.entry(key).or_insert_with(|| {
                    light_faces.push(Face::ALL.map(|face| face_mask(&boxes, face)));
                    u16::try_from(light_faces.len() - 1).expect("fewer light shapes than u16")
                })
            } else {
                NO_LIGHT_SHAPE
            });
            sturdy.push(match support_shape(name, &properties) {
                Some(support) => full_faces(&support),
                None => shape_faces[shape as usize],
            });
        }
        Self {
            shapes,
            state_shapes,
            flags,
            sturdy,
            shape_faces,
            occlusion_faces,
            light_shapes,
            light_faces,
        }
    }

    /// The collision shape of a block state with no surrounding blocks; empty for an unknown state.
    pub fn collision_shape(&self, id: u32) -> &[ShapeBox] {
        self.state_shapes
            .get(id as usize)
            .map_or(&[], |&shape| self.shapes[shape as usize].as_slice())
    }

    /// The legacy solid flag (`BlockState.isSolid`).
    pub fn is_solid(&self, id: u32) -> bool {
        self.flag(id, SOLID)
    }

    /// Whether placing a block may replace this state (`BlockState.canBeReplaced()`).
    pub fn can_be_replaced(&self, id: u32) -> bool {
        self.flag(id, REPLACEABLE)
    }

    /// Whether the state stops movement: solid, but not cobweb or a bamboo sapling (`BlockState.blocksMotion`).
    /// The ocean floor and motion-blocking heightmaps use it.
    pub fn blocks_motion(&self, id: u32) -> bool {
        self.flag(id, BLOCKS_MOTION)
    }

    /// Whether the block is a liquid block: water, lava or a bubble column (`BlockState.liquid`).
    pub fn is_liquid(&self, id: u32) -> bool {
        self.flag(id, LIQUID)
    }

    /// Whether the state's support shape covers a whole face (`isFaceSturdy` with `SupportType.FULL`).
    pub fn is_face_sturdy(&self, id: u32, face: Face) -> bool {
        self.sturdy
            .get(id as usize)
            .is_some_and(|faces| faces & (1 << face as u8) != 0)
    }

    /// Whether the state's collision shape covers a whole face (`Block.isFaceFull` of `getCollisionShape`).
    pub fn is_collision_face_full(&self, id: u32, face: Face) -> bool {
        self.state_shapes
            .get(id as usize)
            .is_some_and(|&shape| self.shape_faces[shape as usize] & (1 << face as u8) != 0)
    }

    /// Whether the state's support shape covers the center of a face (`isFaceSturdy` with `SupportType.CENTER`,
    /// as `Block.canSupportCenter` asks): the middle 2/16 square of the top or bottom, or the middle 2/16 strip
    /// of a side from the bottom to 10/16 up.
    pub fn is_face_center_sturdy(&self, id: u32, face: Face) -> bool {
        self.sturdy
            .get(id as usize)
            .is_some_and(|faces| faces & (1 << (8 + face as u8)) != 0)
    }

    /// Whether the state occludes the blocks beside it entirely (`BlockState.isSolidRender`): the block can
    /// occlude and its occlusion shape is a full block.
    pub fn is_solid_render(&self, id: u32) -> bool {
        self.flag(id, SOLID_RENDER)
    }

    /// Whether the state can occlude and its occlusion shape covers a whole face (`Block.isFaceFull` of
    /// `getOcclusionShape`). Unlike support, glass and other blocks with `noOcclusion` never close a face.
    pub fn is_occlusion_face_full(&self, id: u32, face: Face) -> bool {
        self.occlusion_faces
            .get(id as usize)
            .is_some_and(|faces| faces & (1 << face as u8) != 0)
    }

    /// The face of the shape a state occludes light with (`LightEngine.getOcclusionShape`), as 32 rows of 32 bits:
    /// empty unless the state can occlude and uses its shape for light occlusion. Rows run along Y for the X faces
    /// and along X otherwise; bits run along Z for the X and Y faces and along Y for the Z faces.
    pub fn light_face(&self, id: u32, face: Face) -> FaceMask {
        match self.light_shapes.get(id as usize) {
            Some(&index) if index != NO_LIGHT_SHAPE => {
                self.light_faces[index as usize][face as usize]
            }
            _ => [0; 32],
        }
    }

    /// Whether the state occludes light with its shape (`useShapeForLightOcclusion`, for a state that can
    /// occlude), as slabs and stairs do.
    pub fn uses_light_shape(&self, id: u32) -> bool {
        self.light_shapes
            .get(id as usize)
            .is_some_and(|&index| index != NO_LIGHT_SHAPE)
    }

    /// Whether light cannot pass from a block of state `from` to the block beside it towards `face`, of state `to`
    /// (`LightEngine.shapeOccludes`): the face of `from` and the opposite face of `to` cover the whole face between
    /// them together. Only blocks that use their shape for light occlusion, such as slabs and stairs, have faces.
    pub fn light_occludes(&self, from: u32, to: u32, face: Face) -> bool {
        let from = self.light_shapes.get(from as usize).copied();
        let to = self.light_shapes.get(to as usize).copied();
        let faces = |index: Option<u16>, face: Face| match index {
            Some(index) if index != NO_LIGHT_SHAPE => {
                Some(&self.light_faces[index as usize][face as usize])
            }
            _ => None,
        };
        match (faces(from, face), faces(to, face.opposite())) {
            (None, None) => false,
            (Some(a), None) | (None, Some(a)) => a.iter().all(|row| *row == u32::MAX),
            (Some(a), Some(b)) => a.iter().zip(b).all(|(a, b)| a | b == u32::MAX),
        }
    }

    /// Whether the state's block has a block entity (`BlockState.hasBlockEntity`).
    pub fn has_block_entity(&self, id: u32) -> bool {
        self.flag(id, BLOCK_ENTITY)
    }

    fn flag(&self, id: u32, flag: u8) -> bool {
        self.flags
            .get(id as usize)
            .is_some_and(|flags| flags & flag != 0)
    }
}

/// Blocks whose properties make them replaceable (`BlockBehaviour.Properties.replaceable`).
const REPLACEABLE_BLOCKS: [&str; 29] = [
    "air",
    "water",
    "lava",
    "short_grass",
    "fern",
    "dead_bush",
    "bush",
    "short_dry_grass",
    "tall_dry_grass",
    "seagrass",
    "tall_seagrass",
    "fire",
    "soul_fire",
    "snow",
    "vine",
    "glow_lichen",
    "resin_clump",
    "light",
    "tall_grass",
    "large_fern",
    "structure_void",
    "void_air",
    "cave_air",
    "bubble_column",
    "warped_roots",
    "nether_sprouts",
    "crimson_roots",
    "leaf_litter",
    "hanging_roots",
];

/// `BlockStateBase.calculateSolid`: forced on, forced off, never for blocks with a dynamic shape, and otherwise
/// a collision shape whose bounds are at least 0.729 blocks on average, or a full block high.
fn is_solid(name: &str, boxes: &[ShapeBox]) -> bool {
    if forced_solid(name) {
        return true;
    }
    if matches!(
        name,
        "azalea"
            | "big_dripleaf"
            | "chorus_flower"
            | "chorus_plant"
            | "end_rod"
            | "flowering_azalea"
            | "ladder"
            | "snow"
    ) || has_dynamic_shape(name)
        || boxes.is_empty()
    {
        return false;
    }
    let size = |axis: usize| {
        let min = boxes.iter().map(|b| b.min[axis]).min().unwrap_or(0);
        let max = boxes.iter().map(|b| b.max[axis]).max().unwrap_or(0);
        f64::from(max - min) / 32.0
    };
    let (x, y, z) = (size(0), size(1), size(2));
    (x + y + z) / 3.0 >= 0.729_166_666_666_666_6 || y >= 1.0
}

/// Whether a block is an `EntityBlock` (`BlockState.hasBlockEntity`), for every one of its states.
fn has_block_entity(name: &str) -> bool {
    const SUFFIXES: [&str; 10] = [
        "_bed",
        "_shelf",
        "_sign",
        "_skull",
        "_banner",
        "shulker_box",
        "copper_chest",
        "copper_golem_statue",
        "command_block",
        "_head",
    ];
    (name != "piston_head" && SUFFIXES.iter().any(|suffix| name.ends_with(suffix)))
        || matches!(
            name,
            "barrel"
                | "beacon"
                | "bee_nest"
                | "beehive"
                | "bell"
                | "blast_furnace"
                | "brewing_stand"
                | "calibrated_sculk_sensor"
                | "campfire"
                | "chest"
                | "chiseled_bookshelf"
                | "comparator"
                | "conduit"
                | "crafter"
                | "creaking_heart"
                | "daylight_detector"
                | "decorated_pot"
                | "dispenser"
                | "dropper"
                | "enchanting_table"
                | "end_gateway"
                | "end_portal"
                | "ender_chest"
                | "furnace"
                | "hopper"
                | "jigsaw"
                | "jukebox"
                | "lectern"
                | "moving_piston"
                | "sculk_catalyst"
                | "sculk_sensor"
                | "sculk_shrieker"
                | "smoker"
                | "soul_campfire"
                | "spawner"
                | "structure_block"
                | "suspicious_gravel"
                | "suspicious_sand"
                | "test_block"
                | "test_instance_block"
                | "trapped_chest"
                | "trial_spawner"
                | "vault"
        )
}

/// `BlockStateBase.isSolidRender`: blocks that can occlude (whose properties do not call `noOcclusion`) with a
/// full occlusion shape. The occlusion shape is the full collision shape for every block whose collision shape is
/// full, and also a full block for mud, soul sand and eight layers of snow, which are lower to walk on.
fn is_solid_render(name: &str, properties: &[(&str, &str)], boxes: &[ShapeBox]) -> bool {
    const FULL: ShapeBox = ShapeBox {
        min: [0; 3],
        max: [32; 3],
    };
    let full_occlusion = match name {
        "mud" | "soul_sand" => true,
        "snow" => properties.contains(&("layers", "8")),
        _ => boxes == [FULL],
    };
    !no_occlusion(name) && full_occlusion
}

/// Blocks with a full collision face whose properties call `noOcclusion`.
fn no_occlusion(name: &str) -> bool {
    [
        "_leaves",
        "glass",
        "shulker_box",
        "copper_grate",
        "_door",
        "_trapdoor",
        "campfire",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
        || matches!(
            name,
            "azalea"
                | "flowering_azalea"
                | "barrier"
                | "beacon"
                | "chorus_flower"
                | "frosted_ice"
                | "ice"
                | "ladder"
                | "mangrove_roots"
                | "pale_moss_carpet"
                | "scaffolding"
                | "slime_block"
                | "spawner"
                | "test_instance_block"
                | "trial_spawner"
                | "vault"
        )
}

/// Blocks whose properties call `forceSolidOn`, by name pattern. Resin brick walls are the one wall that
/// does not.
fn forced_solid(name: &str) -> bool {
    if name == "resin_brick_wall" {
        return false;
    }
    const SUFFIXES: [&str; 11] = [
        "_fence_gate",
        "_sign",
        "_pressure_plate",
        "_banner",
        "_wall",
        "chain",
        "lantern",
        "lightning_rod",
        "_amethyst_bud",
        "candle_cake",
        "shulker_box",
    ];
    let dead_coral = name.starts_with("dead_")
        && (name.ends_with("_coral")
            || name.ends_with("_coral_fan")
            || name.ends_with("_wall_fan"));
    dead_coral
        || SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        || matches!(
            name,
            "amethyst_cluster"
                | "bamboo"
                | "bamboo_sapling"
                | "bell"
                | "cake"
                | "cobweb"
                | "conduit"
                | "dried_ghast"
                | "moving_piston"
                | "oak_fence"
                | "pointed_dripstone"
                | "sculk_vein"
                | "turtle_egg"
        )
}

/// Blocks whose properties call `dynamicShape`; their solid flag is never computed from a shape.
fn has_dynamic_shape(name: &str) -> bool {
    name.ends_with("shulker_box")
        || matches!(
            name,
            "bamboo" | "moving_piston" | "pointed_dripstone" | "powder_snow" | "scaffolding"
        )
}

/// The support shape of blocks that override `getBlockSupportShape`; None for the collision shape.
fn support_shape(name: &str, properties: &[(&str, &str)]) -> Option<Vec<ShapeBox>> {
    let value = |key: &str| {
        properties
            .iter()
            .find(|(property, _)| *property == key)
            .map(|(_, value)| *value)
    };
    // A shape spanning X stays as it is for a block facing north or south, and turns to span Z otherwise.
    let along_z = matches!(value("facing"), Some("north" | "south"));
    let turned = |b: ShapeBox, turn: bool| {
        if turn {
            ShapeBox {
                min: [b.min[2], b.min[1], b.min[0]],
                max: [b.max[2], b.max[1], b.max[0]],
            }
        } else {
            b
        }
    };
    if name.ends_with("_leaves") {
        return Some(Vec::new());
    }
    if name.ends_with("_fence_gate") {
        return Some(if value("open") == Some("true") {
            Vec::new()
        } else {
            vec![turned(column(16, 4, 5, 24), !along_z)]
        });
    }
    if name.ends_with("_wall_hanging_sign") {
        return Some(
            [column(16, 4, 14, 16), column(14, 2, 0, 10)]
                .map(|b| turned(b, !along_z))
                .to_vec(),
        );
    }
    if name.ends_with("_hanging_sign") {
        // The outline shape, which only four of the sixteen rotations narrow.
        return Some(match value("rotation") {
            Some("0" | "8") => vec![column(14, 2, 0, 10)],
            Some("4" | "12") => vec![turned(column(14, 2, 0, 10), true)],
            _ => vec![column(10, 10, 0, 16)],
        });
    }
    match name {
        "chorus_flower" => Some(vec![column(14, 14, 0, 15)]),
        "soul_sand" | "mud" => Some(vec![column(16, 16, 0, 16)]),
        "snow" => {
            let layers: i8 = value("layers").and_then(|v| v.parse().ok()).unwrap_or(1);
            Some(vec![column(16, 16, 0, layers * 2)])
        }
        _ => None,
    }
}

/// `Block.column(sizeX, sizeZ, minY, maxY)` in 1/16 of a block: a box centered in X and Z.
const fn column(size_x: i8, size_z: i8, min_y: i8, max_y: i8) -> ShapeBox {
    ShapeBox {
        min: [16 - size_x, min_y * 2, 16 - size_z],
        max: [16 + size_x, max_y * 2, 16 + size_z],
    }
}

/// One bit per [`Face`] that the boxes cover entirely (`Block.isFaceFull`), and from bit 8 one per face whose
/// center they cover (`SupportType.CENTER`).
/// Whether a block uses its shape to occlude light (`BlockState.useShapeForLightOcclusion`): slabs other than
/// double ones, stairs, extended pistons and piston heads, shelves, sculk sensors and shriekers, daylight
/// detectors, dirt paths, enchanting tables, end portal frames, farmland, lecterns, snow layers and stonecutters.
/// They can all occlude.
fn uses_shape_for_light_occlusion(name: &str, properties: &[(&str, &str)]) -> bool {
    if name.ends_with("_slab") {
        return !properties.contains(&("type", "double"));
    }
    if matches!(name, "piston" | "sticky_piston") {
        return properties.contains(&("extended", "true"));
    }
    name.ends_with("_stairs")
        || name.ends_with("_shelf")
        || matches!(
            name,
            "calibrated_sculk_sensor"
                | "daylight_detector"
                | "dirt_path"
                | "enchanting_table"
                | "end_portal_frame"
                | "farmland"
                | "lectern"
                | "piston_head"
                | "sculk_sensor"
                | "sculk_shrieker"
                | "snow"
                | "stonecutter"
        )
}

/// The face of a shape (`VoxelShape.getFaceShape`): the boxes that reach the face's side of the block, projected
/// onto it. Rows run along the first of the other two axes, bits along the second.
fn face_mask(boxes: &[ShapeBox], face: Face) -> FaceMask {
    let (axis, negative) = face.axis();
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let mut mask = [0_u32; 32];
    for b in boxes {
        let touches = if negative {
            b.min[axis] <= 0 && b.max[axis] > 0
        } else {
            b.min[axis] < 32 && b.max[axis] >= 32
        };
        if !touches {
            continue;
        }
        let clip = |value: i8| value.clamp(0, 32) as u32;
        let columns =
            (clip(b.min[v])..clip(b.max[v])).fold(0_u32, |bits, column| bits | 1 << column);
        for row in clip(b.min[u])..clip(b.max[u]) {
            mask[row as usize] |= columns;
        }
    }
    mask
}

fn full_faces(boxes: &[ShapeBox]) -> u16 {
    let mut faces = 0;
    for face in Face::ALL {
        let (axis, negative) = face.axis();
        let (u, v) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        let mut covered = [0_u32; 32];
        for b in boxes {
            let touches = if negative {
                b.min[axis] <= 0
            } else {
                b.max[axis] >= 32
            };
            if !touches {
                continue;
            }
            let clip = |value: i8| value.clamp(0, 32) as u32;
            let columns =
                (clip(b.min[v])..clip(b.max[v])).fold(0_u32, |bits, column| bits | 1 << column);
            for row in clip(b.min[u])..clip(b.max[u]) {
                covered[row as usize] |= columns;
            }
        }
        if covered.iter().all(|row| *row == u32::MAX) {
            faces |= 1 << face as u8;
        }
        // The center column (`Block.column(2, 0, 10)`) is 2/16 wide in X and Z, from 7/16 to 9/16, and rises
        // 10/16 from the bottom: on the top and bottom faces it is the middle square, on the side faces a
        // middle strip from the bottom up.
        const MIDDLE: u32 = (1 << 18) - (1 << 14);
        const LOW: u32 = (1 << 20) - 1;
        let (rows, columns) = match axis {
            0 => (0..20, MIDDLE),
            1 => (14..18, MIDDLE),
            _ => (14..18, LOW),
        };
        if covered[rows].iter().all(|row| row & columns == columns) {
            faces |= 1 << (8 + face as u8);
        }
    }
    faces
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
    fn every_state_has_a_shape() {
        let behavior = block_behavior();
        assert_eq!(behavior.state_shapes.len(), block_states().len() as usize);
        assert!(
            behavior
                .collision_shape(state("minecraft:air", &[]))
                .is_empty()
        );
        assert_eq!(
            behavior.collision_shape(state("minecraft:stone", &[])),
            [ShapeBox {
                min: [0; 3],
                max: [32; 3]
            }]
        );
    }

    #[test]
    fn occlusion_distinguishes_glass_partial_shapes_and_snow() {
        let behavior = block_behavior();
        let glass = state("minecraft:glass", &[]);
        assert!(behavior.is_face_sturdy(glass, Face::Up));
        assert!(!behavior.is_occlusion_face_full(glass, Face::Up));
        let slab = state("minecraft:oak_slab", &[("type", "top")]);
        assert!(behavior.is_occlusion_face_full(slab, Face::Up));
        assert!(!behavior.is_occlusion_face_full(slab, Face::Down));
        let snow = state("minecraft:snow", &[("layers", "1")]);
        assert!(behavior.collision_shape(snow).is_empty());
        assert!(behavior.is_occlusion_face_full(snow, Face::Down));
        assert!(!behavior.is_occlusion_face_full(snow, Face::Up));
        assert!(!behavior.is_occlusion_face_full(u32::MAX, Face::Up));
    }

    #[test]
    fn flags_follow_vanilla_rules() {
        let behavior = block_behavior();
        let stone = state("minecraft:stone", &[]);
        assert!(behavior.is_solid(stone));
        assert!(!behavior.can_be_replaced(stone));
        assert!(
            Face::ALL
                .iter()
                .all(|&face| behavior.is_face_sturdy(stone, face))
        );

        let grass = state("minecraft:short_grass", &[]);
        assert!(!behavior.is_solid(grass) && behavior.can_be_replaced(grass));

        let water = state("minecraft:water", &[]);
        assert!(behavior.is_liquid(water) && behavior.can_be_replaced(water));

        let leaves = state("minecraft:oak_leaves", &[]);
        assert!(behavior.is_solid(leaves));
        assert!(!behavior.is_face_sturdy(leaves, Face::Up));

        let slab = state("minecraft:oak_slab", &[("type", "bottom")]);
        assert!(behavior.is_face_sturdy(slab, Face::Down));
        assert!(!behavior.is_face_sturdy(slab, Face::Up));

        let one_layer = state("minecraft:snow", &[("layers", "1")]);
        assert!(!behavior.is_solid(one_layer));
        assert!(behavior.is_face_sturdy(one_layer, Face::Down));
        assert!(!behavior.is_face_sturdy(one_layer, Face::Up));

        assert!(behavior.is_solid(state("minecraft:oak_sign", &[])));
        let cobweb = state("minecraft:cobweb", &[]);
        assert!(behavior.is_solid(cobweb) && !behavior.blocks_motion(cobweb));
        assert!(behavior.blocks_motion(stone));
    }
}
