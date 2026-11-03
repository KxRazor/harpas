//! Structure templates: vanilla's `StructureTemplate`, read from `data/<namespace>/structure/**/*.nbt`, as far as
//! laying out jigsaw structures needs them.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use flate2::read::GzDecoder;
use harpas_core::math::position_seed;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{LegacyRandom, RandomSource};

use crate::data::WorldgenDataError;
use crate::structure::BoundingBox;

/// The data version of the templates Harpas reads; vanilla would upgrade older ones with its data fixers.
const DATA_VERSION: i32 = 4786;

/// A rotation around the Y axis (`Rotation`), in vanilla's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// No rotation.
    None,
    /// A quarter turn clockwise, seen from above.
    Clockwise90,
    /// A half turn.
    Clockwise180,
    /// A quarter turn counterclockwise, seen from above.
    Counterclockwise90,
}

impl Rotation {
    /// All rotations, in vanilla's order (`Rotation.values`).
    pub const ALL: [Self; 4] = [
        Self::None,
        Self::Clockwise90,
        Self::Clockwise180,
        Self::Counterclockwise90,
    ];

    /// Rotates a position around the origin (`StructureTemplate.transform` with no mirror and pivot).
    pub const fn transform(self, [x, y, z]: [i32; 3]) -> [i32; 3] {
        match self {
            Self::None => [x, y, z],
            Self::Clockwise90 => [-z, y, x],
            Self::Clockwise180 => [-x, y, -z],
            Self::Counterclockwise90 => [z, y, -x],
        }
    }

    /// Rotates a direction; up and down stay (`Rotation.rotate`).
    pub const fn rotate(self, facing: Facing) -> Facing {
        let horizontal = match facing {
            Facing::Down | Facing::Up => return facing,
            Facing::North => 0,
            Facing::East => 1,
            Facing::South => 2,
            Facing::West => 3,
        };
        let turns = match self {
            Self::None => 0,
            Self::Clockwise90 => 1,
            Self::Clockwise180 => 2,
            Self::Counterclockwise90 => 3,
        };
        match (horizontal + turns) % 4 {
            0 => Facing::North,
            1 => Facing::East,
            2 => Facing::South,
            _ => Facing::West,
        }
    }
}

/// One of the six directions (`Direction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    /// Towards negative Y.
    Down,
    /// Towards positive Y.
    Up,
    /// Towards negative Z.
    North,
    /// Towards positive Z.
    South,
    /// Towards negative X.
    West,
    /// Towards positive X.
    East,
}

impl Facing {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "down" => Self::Down,
            "up" => Self::Up,
            "north" => Self::North,
            "south" => Self::South,
            "west" => Self::West,
            "east" => Self::East,
            _ => return None,
        })
    }

    /// The direction's name, as in block states.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Up => "up",
            Self::North => "north",
            Self::South => "south",
            Self::West => "west",
            Self::East => "east",
        }
    }

    /// Whether the direction is north, south, west or east.
    pub const fn is_horizontal(self) -> bool {
        !matches!(self, Self::Down | Self::Up)
    }
}

/// How a jigsaw block may turn to meet another (`JigsawBlockEntity.JointType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JointType {
    /// The connected piece may take any rotation.
    Rollable,
    /// The connected piece keeps its top aligned with this jigsaw's.
    Aligned,
}

/// A jigsaw block of a template (`StructureTemplate.JigsawBlockInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JigsawBlock {
    /// Its position, relative to the template's origin or, from [`StructureTemplate::jigsaws`], in the world.
    pub pos: [i32; 3],
    /// The direction its front faces.
    pub front: Facing,
    /// The direction its top faces.
    pub top: Facing,
    /// The name other jigsaws target.
    pub name: String,
    /// The pool its connected piece comes from.
    pub pool: String,
    /// The name of the jigsaw it connects to.
    pub target: String,
    /// How the connected piece may turn.
    pub joint: JointType,
    /// The order in which pieces connected here are placed.
    pub placement_priority: i32,
    /// The order in which this jigsaw is tried among the piece's jigsaws.
    pub selection_priority: i32,
}

/// A structure template (`StructureTemplate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureTemplate {
    /// Its size in blocks along X, Y and Z.
    pub size: [i32; 3],
    /// The jigsaw blocks of each palette, in vanilla's block order.
    palettes: Vec<Vec<JigsawBlock>>,
}

impl StructureTemplate {
    /// The template vanilla creates for a location with no template: no blocks and a size of zero
    /// (`StructureTemplateManager.getOrCreate`).
    pub const fn empty() -> Self {
        Self {
            size: [0, 0, 0],
            palettes: Vec::new(),
        }
    }

    /// Reads a template from its NBT (`StructureTemplate.load`).
    pub fn from_nbt(context: &str, root: &Compound) -> Result<Self, WorldgenDataError> {
        let invalid = |message: &str| WorldgenDataError::InvalidDefinition {
            context: context.to_string(),
            message: message.to_string(),
        };
        match root.get("DataVersion") {
            Some(Nbt::Int(DATA_VERSION)) => {}
            _ => return Err(invalid("template is not from Minecraft 26.1")),
        }
        let size = match root.get("size") {
            Some(Nbt::List(List::Int(size))) if size.len() == 3 => [size[0], size[1], size[2]],
            _ => return Err(invalid("template has no size")),
        };
        let palettes: Vec<&[Compound]> = match (root.get("palettes"), root.get("palette")) {
            (Some(Nbt::List(List::List(palettes))), _) => palettes
                .iter()
                .map(|palette| match palette {
                    List::Compound(states) => Ok(states.as_slice()),
                    List::Empty => Ok(&[][..]),
                    _ => Err(invalid("palette entries must be compounds")),
                })
                .collect::<Result<_, _>>()?,
            (_, Some(Nbt::List(List::Compound(states)))) => vec![states.as_slice()],
            (_, Some(Nbt::List(List::Empty)) | None) => vec![&[][..]],
            _ => return Err(invalid("template has no palette")),
        };
        let blocks: &[Compound] = match root.get("blocks") {
            Some(Nbt::List(List::Compound(blocks))) => blocks,
            Some(Nbt::List(List::Empty)) | None => &[],
            _ => return Err(invalid("template blocks must be compounds")),
        };
        let palettes = palettes
            .into_iter()
            .map(|states| jigsaws_of(states, blocks, &invalid))
            .collect::<Result<_, _>>()?;
        Ok(Self { size, palettes })
    }

    /// The blocks the template covers when placed at `position` with `rotation`
    /// (`StructureTemplate.getBoundingBox`).
    pub fn bounding_box(&self, [x, y, z]: [i32; 3], rotation: Rotation) -> BoundingBox {
        let a = rotation.transform([0, 0, 0]);
        let b = rotation.transform(self.size.map(|value| value - 1));
        BoundingBox {
            min: [a[0].min(b[0]) + x, a[1].min(b[1]) + y, a[2].min(b[2]) + z],
            max: [a[0].max(b[0]) + x, a[1].max(b[1]) + y, a[2].max(b[2]) + z],
        }
    }

    /// The palette vanilla picks for a template placed at `position` (`StructurePlaceSettings.getRandomPalette`).
    pub fn palette_index(&self, position: [i32; 3]) -> usize {
        let [x, y, z] = position;
        let mut random = LegacyRandom::new(position_seed(x, y, z));
        random.next_int_bounded(self.palettes.len() as i32) as usize
    }

    /// How many palettes the template has; shipwrecks have several.
    pub fn palette_count(&self) -> usize {
        self.palettes.len()
    }

    /// The jigsaw blocks when placed at `position` with `rotation`, from the palette vanilla picks for that position
    /// (`StructureTemplate.getJigsaws`).
    pub fn jigsaws(&self, position: [i32; 3], rotation: Rotation) -> Vec<JigsawBlock> {
        if self.palettes.is_empty() {
            return Vec::new();
        }
        self.palettes[self.palette_index(position)]
            .iter()
            .map(|jigsaw| {
                let pos = rotation.transform(jigsaw.pos);
                JigsawBlock {
                    pos: [
                        pos[0] + position[0],
                        pos[1] + position[1],
                        pos[2] + position[2],
                    ],
                    front: rotation.rotate(jigsaw.front),
                    top: rotation.rotate(jigsaw.top),
                    ..jigsaw.clone()
                }
            })
            .collect()
    }
}

/// `StructureTemplate.getJointType`: the saved joint, or by default aligned for a jigsaw facing sideways.
fn joint_type(saved: Option<&str>, front: Facing) -> JointType {
    match saved {
        Some("rollable") => JointType::Rollable,
        Some("aligned") => JointType::Aligned,
        _ if front.is_horizontal() => JointType::Aligned,
        _ => JointType::Rollable,
    }
}

/// The jigsaw blocks of one palette. Vanilla keeps blocks with NBT, jigsaws among them, last and sorted by Y, X
/// and Z (`StructureTemplate.buildInfoList`).
fn jigsaws_of(
    states: &[Compound],
    blocks: &[Compound],
    invalid: &impl Fn(&str) -> WorldgenDataError,
) -> Result<Vec<JigsawBlock>, WorldgenDataError> {
    let string = |compound: &Compound, key: &str| match compound.get(key) {
        Some(Nbt::String(value)) => Some(value.clone()),
        _ => None,
    };
    let int = |compound: &Compound, key: &str| match compound.get(key) {
        Some(Nbt::Int(value)) => *value,
        _ => 0,
    };
    let mut jigsaws = Vec::new();
    for block in blocks {
        let state = match block.get("state") {
            Some(Nbt::Int(state)) => *state,
            _ => 0,
        };
        let Some(palette_state) = states.get(state as usize) else {
            continue;
        };
        if string(palette_state, "Name").as_deref() != Some("minecraft:jigsaw") {
            continue;
        }
        let Some(Nbt::Compound(nbt)) = block.get("nbt") else {
            return Err(invalid("jigsaw without NBT"));
        };
        let pos = match block.get("pos") {
            Some(Nbt::List(List::Int(pos))) if pos.len() == 3 => [pos[0], pos[1], pos[2]],
            _ => return Err(invalid("block has no position")),
        };
        let orientation = match palette_state.get("Properties") {
            Some(Nbt::Compound(properties)) => string(properties, "orientation"),
            _ => None,
        }
        .unwrap_or_else(|| "north_up".to_string());
        let (front, top) = orientation
            .split_once('_')
            .and_then(|(front, top)| Some((Facing::from_name(front)?, Facing::from_name(top)?)))
            .ok_or_else(|| invalid("invalid jigsaw orientation"))?;
        let joint = joint_type(string(nbt, "joint").as_deref(), front);
        let name_or_empty = |key| string(nbt, key).unwrap_or_else(|| "minecraft:empty".to_string());
        jigsaws.push(JigsawBlock {
            pos,
            front,
            top,
            name: name_or_empty("name"),
            pool: name_or_empty("pool"),
            target: name_or_empty("target"),
            joint,
            placement_priority: int(nbt, "placement_priority"),
            selection_priority: int(nbt, "selection_priority"),
        });
    }
    jigsaws.sort_by_key(|jigsaw| (jigsaw.pos[1], jigsaw.pos[0], jigsaw.pos[2]));
    Ok(jigsaws)
}

/// The structure templates of a data pack, by resource location.
#[derive(Debug, Clone, Default)]
pub struct StructureTemplates {
    templates: HashMap<String, StructureTemplate>,
}

impl StructureTemplates {
    /// Reads every template under `data/<namespace>/structure/` of a `.zip` archive on disk.
    pub fn from_zip_path(path: impl AsRef<Path>) -> Result<Self, WorldgenDataError> {
        Self::from_zip_reader(File::open(path)?)
    }

    /// Reads every template under `data/<namespace>/structure/` of a seekable `.zip` reader.
    pub fn from_zip_reader<R: Read + Seek>(reader: R) -> Result<Self, WorldgenDataError> {
        let mut archive = zip::ZipArchive::new(reader)?;
        let mut templates = HashMap::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let name = entry.name()?.replace('\\', "/");
            let Some(id) = template_id(&name) else {
                continue;
            };
            let mut compressed = Vec::new();
            entry.read_to_end(&mut compressed)?;
            let mut bytes = Vec::new();
            GzDecoder::new(compressed.as_slice()).read_to_end(&mut bytes)?;
            let root = match Nbt::read_named(&mut bytes.as_slice()) {
                Ok((_, Nbt::Compound(root))) => root,
                _ => {
                    return Err(WorldgenDataError::InvalidDefinition {
                        context: name,
                        message: "template is not an NBT compound".to_string(),
                    });
                }
            };
            templates.insert(id.clone(), StructureTemplate::from_nbt(&id, &root)?);
        }
        Ok(Self { templates })
    }

    /// The template with a resource location such as `minecraft:village/plains/houses/plains_small_house_1`.
    pub fn get(&self, id: &str) -> Option<&StructureTemplate> {
        self.templates.get(id)
    }

    /// The resource locations of all templates, in no particular order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.templates.keys().map(String::as_str)
    }
}

/// `data/<namespace>/structure/<path>.nbt` as `<namespace>:<path>`.
fn template_id(path: &str) -> Option<String> {
    let rest = path.strip_prefix("data/")?;
    let (namespace, rest) = rest.split_once('/')?;
    let id = rest.strip_prefix("structure/")?.strip_suffix(".nbt")?;
    Some(format!("{namespace}:{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_turn_positions_and_horizontal_directions() {
        assert_eq!(Rotation::Clockwise90.transform([1, 2, 3]), [-3, 2, 1]);
        assert_eq!(
            Rotation::Counterclockwise90.transform([1, 2, 3]),
            [3, 2, -1]
        );
        assert_eq!(Rotation::Clockwise180.transform([1, 2, 3]), [-1, 2, -3]);
        assert_eq!(Rotation::Clockwise90.rotate(Facing::North), Facing::East);
        assert_eq!(
            Rotation::Counterclockwise90.rotate(Facing::North),
            Facing::West
        );
        assert_eq!(Rotation::Clockwise180.rotate(Facing::Up), Facing::Up);
    }

    #[test]
    fn jigsaws_facing_sideways_are_aligned_by_default() {
        assert_eq!(joint_type(None, Facing::East), JointType::Aligned);
        assert_eq!(joint_type(None, Facing::Up), JointType::Rollable);
        assert_eq!(
            joint_type(Some("rollable"), Facing::East),
            JointType::Rollable
        );
        assert_eq!(
            joint_type(Some("aligned"), Facing::Down),
            JointType::Aligned
        );
    }

    #[test]
    fn template_ids_come_from_their_paths() {
        assert_eq!(
            template_id(
                "data/minecraft/structure/village/plains/town_centers/plains_fountain_01.nbt"
            )
            .as_deref(),
            Some("minecraft:village/plains/town_centers/plains_fountain_01")
        );
        assert_eq!(
            template_id("data/minecraft/worldgen/biome/plains.json"),
            None
        );
    }
}
