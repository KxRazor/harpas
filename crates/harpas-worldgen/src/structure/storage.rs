//! The structure records stored in Anvil proto-chunks. Piece-specific fields are kept until placement.

use std::io;

use harpas_core::nbt::{Compound, List, Nbt};

use super::{
    BoundingBox, Direction, JigsawJunction, JigsawPiece, StructurePiece, StructureStart,
    TemplatePiece,
};
use crate::{
    PoolElementDef, Projection, Rotation, StructureTemplate, StructureTemplates, WorldgenData,
};

pub(super) fn rotation_name(rotation: Rotation) -> &'static str {
    match rotation {
        Rotation::None => "NONE",
        Rotation::Clockwise90 => "CLOCKWISE_90",
        Rotation::Clockwise180 => "CLOCKWISE_180",
        Rotation::Counterclockwise90 => "COUNTERCLOCKWISE_90",
    }
}

fn rotation(name: &str) -> io::Result<Rotation> {
    Rotation::ALL
        .into_iter()
        .find(|&r| rotation_name(r) == name)
        .ok_or_else(|| invalid("invalid structure rotation"))
}

fn projection_name(projection: Projection) -> &'static str {
    match projection {
        Projection::Rigid => "rigid",
        Projection::TerrainMatching => "terrain_matching",
    }
}

fn projection(name: &str) -> io::Result<Projection> {
    match name {
        "rigid" => Ok(Projection::Rigid),
        "terrain_matching" => Ok(Projection::TerrainMatching),
        _ => Err(invalid("invalid pool element projection")),
    }
}

impl StructureStart {
    /// The `starts` entry vanilla can load to continue generating this structure.
    pub fn to_nbt(&self) -> Compound {
        let mut tag = Compound::new();
        tag.insert("id", self.structure.clone());
        tag.insert("ChunkX", self.chunk.0);
        tag.insert("ChunkZ", self.chunk.1);
        tag.insert("references", self.references);
        tag.insert(
            "Children",
            List::Compound(self.pieces.iter().map(StructurePiece::to_nbt).collect()),
        );
        tag
    }

    /// Reads a saved start for terrain adaptation. Unknown piece fields are preserved when written again.
    /// An `INVALID` start has no pieces and returns `None`.
    pub fn from_nbt(
        tag: &Compound,
        data: &WorldgenData,
        templates: &StructureTemplates,
    ) -> io::Result<Option<Self>> {
        let structure = string(tag, "id")?;
        if structure == "INVALID" {
            return Ok(None);
        }
        let definition = data
            .structures
            .get(structure)
            .ok_or_else(|| invalid("unknown saved structure"))?;
        let pieces = compounds(tag, "Children")?
            .iter()
            .map(|tag| StructurePiece::from_nbt(tag, templates))
            .collect::<io::Result<Vec<_>>>()?;
        let Some(first) = pieces.first() else {
            return Err(invalid("valid structure has no pieces"));
        };
        let mut bounding_box = first.bounding_box;
        for piece in &pieces[1..] {
            bounding_box.encapsulate(&piece.bounding_box);
        }
        if definition.terrain_adaptation != "none" {
            for axis in 0..3 {
                bounding_box.min[axis] = bounding_box.min[axis]
                    .checked_sub(12)
                    .ok_or_else(|| invalid("structure bounds overflow"))?;
                bounding_box.max[axis] = bounding_box.max[axis]
                    .checked_add(12)
                    .ok_or_else(|| invalid("structure bounds overflow"))?;
            }
        }
        Ok(Some(Self {
            structure: structure.to_string(),
            chunk: (integer(tag, "ChunkX")?, integer(tag, "ChunkZ")?),
            references: integer(tag, "references")?,
            pieces,
            bounding_box,
        }))
    }
}

impl StructurePiece {
    /// The piece record, including its placement state and pool element or template.
    pub fn to_nbt(&self) -> Compound {
        let mut tag = self.save_data.clone();
        tag.insert("id", self.kind.clone());
        tag.insert(
            "BB",
            Nbt::IntArray(
                self.bounding_box
                    .min
                    .into_iter()
                    .chain(self.bounding_box.max)
                    .collect(),
            ),
        );
        tag.insert("O", self.orientation.map_or(-1, Direction::data_value));
        tag.insert("GD", self.gen_depth);
        if let Some(template) = &self.template {
            tag.insert("Template", template.template.clone());
            for (key, value) in ["TPX", "TPY", "TPZ"].into_iter().zip(template.position) {
                tag.insert(key, value);
            }
            let ruined = self.kind == "minecraft:rupo";
            tag.insert(
                if ruined { "Rotation" } else { "Rot" },
                rotation_name(template.rotation),
            );
            if ruined {
                tag.insert(
                    "Mirror",
                    match template.mirror {
                        crate::Mirror::None => "NONE",
                        crate::Mirror::FrontBack => "FRONT_BACK",
                    },
                );
            }
        }
        if let Some(jigsaw) = &self.jigsaw {
            for (key, value) in ["PosX", "PosY", "PosZ"].into_iter().zip(jigsaw.position) {
                tag.insert(key, value);
            }
            tag.insert("ground_level_delta", jigsaw.ground_level_delta);
            tag.insert("rotation", rotation_name(jigsaw.rotation));
            // A saved element can contain inline processors not needed by terrain adaptation.
            if tag.get("pool_element").is_none() {
                tag.insert("pool_element", element_tag(&jigsaw.element));
            }
            tag.insert(
                "junctions",
                List::Compound(
                    self.junctions
                        .iter()
                        .map(|junction| {
                            let mut tag = Compound::new();
                            for (key, value) in ["source_x", "source_ground_y", "source_z"]
                                .into_iter()
                                .zip(junction.source)
                            {
                                tag.insert(key, value);
                            }
                            tag.insert("delta_y", junction.delta_y);
                            tag.insert(
                                "dest_proj",
                                if junction.destination_rigid {
                                    "rigid"
                                } else {
                                    "terrain_matching"
                                },
                            );
                            tag
                        })
                        .collect(),
                ),
            );
        }
        tag
    }

    fn from_nbt(tag: &Compound, templates: &StructureTemplates) -> io::Result<Self> {
        let kind = string(tag, "id")?.to_string();
        let Some(Nbt::IntArray(bounds)) = tag.get("BB") else {
            return Err(invalid("piece has no bounding box"));
        };
        if bounds.len() != 6 || (0..3).any(|axis| bounds[axis] > bounds[axis + 3]) {
            return Err(invalid("invalid piece bounding box"));
        }
        let orientation = match integer(tag, "O")? {
            -1 => None,
            0 => Some(Direction::South),
            1 => Some(Direction::West),
            2 => Some(Direction::North),
            3 => Some(Direction::East),
            _ => return Err(invalid("invalid piece orientation")),
        };
        let mut junctions = Vec::new();
        let jigsaw = if kind == "minecraft:jigsaw" {
            for tag in compounds(tag, "junctions")? {
                junctions.push(JigsawJunction {
                    source: [
                        integer(tag, "source_x")?,
                        integer(tag, "source_ground_y")?,
                        integer(tag, "source_z")?,
                    ],
                    delta_y: integer(tag, "delta_y")?,
                    destination_rigid: projection(string(tag, "dest_proj")?)? == Projection::Rigid,
                });
            }
            Some(JigsawPiece {
                element: read_element(compound(tag, "pool_element")?)?,
                position: [
                    integer(tag, "PosX")?,
                    integer(tag, "PosY")?,
                    integer(tag, "PosZ")?,
                ],
                rotation: rotation(string(tag, "rotation")?)?,
                ground_level_delta: integer(tag, "ground_level_delta")?,
            })
        } else {
            None
        };
        let template = if matches!(
            kind.as_str(),
            "minecraft:iglu" | "minecraft:shipwreck" | "minecraft:orp"
        ) {
            let template = string(tag, "Template")?.to_string();
            let pivot = if kind == "minecraft:orp" {
                [0, 0, 0]
            } else if kind == "minecraft:shipwreck" {
                [4, 0, 15]
            } else {
                super::igloo::settings(&template)
                    .ok_or_else(|| invalid("unknown igloo template"))?
                    .0
            };
            Some(TemplatePiece {
                template,
                position: [
                    integer(tag, "TPX")?,
                    integer(tag, "TPY")?,
                    integer(tag, "TPZ")?,
                ],
                rotation: rotation(string(tag, "Rot")?)?,
                mirror: crate::Mirror::None,
                pivot,
            })
        } else {
            None
        };
        let bounding_box = if let Some(template) = &template {
            templates
                .get(&template.template)
                .unwrap_or(&StructureTemplate::empty())
                .bounding_box_around(
                    template.position,
                    template.rotation,
                    template.mirror,
                    template.pivot,
                )
        } else if let Some(jigsaw) = &jigsaw {
            // Vanilla reconstructs the template bounds, dropping the placement expansion hack's reservation.
            element_bounds(&jigsaw.element, jigsaw.position, jigsaw.rotation, templates)?
        } else {
            BoundingBox {
                min: bounds[..3].try_into().unwrap(),
                max: bounds[3..].try_into().unwrap(),
            }
        };
        Ok(Self {
            kind,
            bounding_box,
            orientation,
            gen_depth: integer(tag, "GD")?,
            junctions,
            jigsaw,
            template,
            save_data: tag.clone(),
        })
    }
}

fn element_bounds(
    element: &PoolElementDef,
    position: [i32; 3],
    rotation: Rotation,
    templates: &StructureTemplates,
) -> io::Result<BoundingBox> {
    match element {
        PoolElementDef::Single { location, .. } => Ok(templates
            .get(location)
            .unwrap_or(&StructureTemplate::empty())
            .bounding_box(position, rotation)),
        PoolElementDef::Feature { .. } => Ok(BoundingBox {
            min: position,
            max: position,
        }),
        PoolElementDef::List { elements, .. } => {
            let mut bounds = elements
                .iter()
                .filter(|e| !matches!(e, PoolElementDef::Empty))
                .map(|e| element_bounds(e, position, rotation, templates));
            let mut result = bounds
                .next()
                .ok_or_else(|| invalid("empty list pool element"))??;
            for next in bounds {
                result.encapsulate(&next?);
            }
            Ok(result)
        }
        PoolElementDef::Empty => Err(invalid("empty pool element has no bounding box")),
    }
}

fn element_tag(element: &PoolElementDef) -> Compound {
    let mut tag = Compound::new();
    let (kind, p) = match element {
        PoolElementDef::Single {
            location,
            processors,
            projection,
            legacy,
        } => {
            tag.insert("location", location.clone());
            tag.insert(
                "processors",
                processors.as_ref().map_or_else(
                    || {
                        let mut empty = Compound::new();
                        empty.insert("processors", List::Empty);
                        Nbt::Compound(empty)
                    },
                    |name| Nbt::String(name.clone()),
                ),
            );
            (
                if *legacy {
                    "legacy_single_pool_element"
                } else {
                    "single_pool_element"
                },
                Some(*projection),
            )
        }
        PoolElementDef::Feature {
            feature,
            projection,
        } => {
            tag.insert("feature", feature.clone());
            ("feature_pool_element", Some(*projection))
        }
        PoolElementDef::List {
            elements,
            projection,
        } => {
            tag.insert(
                "elements",
                List::Compound(elements.iter().map(element_tag).collect()),
            );
            ("list_pool_element", Some(*projection))
        }
        PoolElementDef::Empty => ("empty_pool_element", None),
    };
    tag.insert("element_type", format!("minecraft:{kind}"));
    if let Some(p) = p {
        tag.insert("projection", projection_name(p));
    }
    tag
}

fn read_element(tag: &Compound) -> io::Result<PoolElementDef> {
    let kind = string(tag, "element_type")?;
    if kind == "minecraft:empty_pool_element" {
        return Ok(PoolElementDef::Empty);
    }
    let projection = projection(string(tag, "projection")?)?;
    Ok(match kind {
        "minecraft:single_pool_element" | "minecraft:legacy_single_pool_element" => {
            PoolElementDef::Single {
                location: string(tag, "location")?.to_string(),
                processors: match tag.get("processors") {
                    Some(Nbt::String(name)) => Some(name.clone()),
                    Some(Nbt::Compound(_)) => None,
                    _ => return Err(invalid("invalid pool processors")),
                },
                projection,
                legacy: kind == "minecraft:legacy_single_pool_element",
            }
        }
        "minecraft:feature_pool_element" => PoolElementDef::Feature {
            feature: string(tag, "feature")?.to_string(),
            projection,
        },
        "minecraft:list_pool_element" => PoolElementDef::List {
            elements: compounds(tag, "elements")?
                .iter()
                .map(read_element)
                .collect::<io::Result<_>>()?,
            projection,
        },
        _ => return Err(invalid("unsupported saved pool element")),
    })
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn integer(tag: &Compound, key: &str) -> io::Result<i32> {
    match tag.get(key) {
        Some(Nbt::Int(value)) => Ok(*value),
        _ => Err(invalid(&format!("structure has no integer `{key}`"))),
    }
}
fn string<'a>(tag: &'a Compound, key: &str) -> io::Result<&'a str> {
    match tag.get(key) {
        Some(Nbt::String(value)) => Ok(value),
        _ => Err(invalid(&format!("structure has no string `{key}`"))),
    }
}
fn compound<'a>(tag: &'a Compound, key: &str) -> io::Result<&'a Compound> {
    match tag.get(key) {
        Some(Nbt::Compound(value)) => Ok(value),
        _ => Err(invalid(&format!("structure has no compound `{key}`"))),
    }
}
fn compounds<'a>(tag: &'a Compound, key: &str) -> io::Result<&'a [Compound]> {
    match tag.get(key) {
        Some(Nbt::List(List::Compound(value))) => Ok(value),
        Some(Nbt::List(List::Empty)) => Ok(&[]),
        _ => Err(invalid(&format!("structure has no compound list `{key}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> WorldgenData {
        WorldgenData::from_json_entries([(
            "data/test/worldgen/structure/test.json".to_string(),
            r#"{"type":"minecraft:desert_pyramid","biomes":[],"step":"surface_structures","terrain_adaptation":"beard_box"}"#.to_string(),
        )]).unwrap()
    }

    fn templates() -> StructureTemplates {
        StructureTemplates::with_sizes([("test:template", [9, 11, 9])])
    }

    fn fixture() -> StructureStart {
        StructureStart {
            structure: "test:test".to_string(),
            chunk: (-3, 5),
            references: 7,
            bounding_box: BoundingBox {
                min: [-60, 28, 68],
                max: [-28, 62, 100],
            },
            pieces: vec![StructurePiece {
                kind: "minecraft:jigsaw".to_string(),
                bounding_box: BoundingBox {
                    min: [-48, 40, 80],
                    max: [-40, 50, 88],
                },
                orientation: None,
                gen_depth: 2,
                jigsaw: Some(JigsawPiece {
                    element: PoolElementDef::List {
                        elements: vec![
                            PoolElementDef::Single {
                                location: "test:template".to_string(),
                                processors: Some("test:processor".to_string()),
                                projection: Projection::TerrainMatching,
                                legacy: true,
                            },
                            PoolElementDef::Feature {
                                feature: "test:feature".to_string(),
                                projection: Projection::TerrainMatching,
                            },
                            PoolElementDef::Empty,
                        ],
                        projection: Projection::TerrainMatching,
                    },
                    position: [-48, 40, 88],
                    rotation: Rotation::Counterclockwise90,
                    ground_level_delta: -3,
                }),
                junctions: vec![JigsawJunction {
                    source: [-44, 37, 85],
                    delta_y: 3,
                    destination_rigid: false,
                }],
                template: None,
                save_data: [("unknown", Nbt::Int(123))].into_iter().collect(),
            }],
        }
    }

    #[test]
    fn saved_jigsaw_keeps_projections_junctions_references_and_unknown_fields() {
        let start = fixture();
        let tag = start.to_nbt();
        let loaded = StructureStart::from_nbt(&tag, &data(), &templates())
            .unwrap()
            .unwrap();
        assert_eq!(loaded.to_nbt(), tag);
        assert_eq!(loaded.bounding_box, start.bounding_box);
        assert_eq!(loaded.pieces[0].jigsaw, start.pieces[0].jigsaw);
        assert_eq!(loaded.pieces[0].junctions, start.pieces[0].junctions);
        assert_eq!(loaded.references, 7);
        let original = crate::Beardifier::for_starts(-3, 5, &data(), [&start]).unwrap();
        let restored = crate::Beardifier::for_starts(-3, 5, &data(), [&loaded]).unwrap();
        assert_eq!(
            original.compute(-44, 37, 85).to_bits(),
            restored.compute(-44, 37, 85).to_bits()
        );
    }

    #[test]
    fn ocean_ruin_reload_restores_rotation_origin_and_processors() {
        let templates = StructureTemplates::with_sizes([("test:ruin", [6, 7, 7])]);
        let mut tag = Compound::new();
        tag.insert("id", "minecraft:orp");
        tag.insert("BB", Nbt::IntArray(vec![0, 90, 0, 5, 96, 6]));
        tag.insert("O", 2);
        tag.insert("GD", 0);
        tag.insert("Template", "test:ruin");
        tag.insert("TPX", -18);
        tag.insert("TPY", -60);
        tag.insert("TPZ", 30);
        tag.insert("Rot", "CLOCKWISE_180");
        tag.insert("Integrity", 0.5_f32);
        tag.insert("BiomeType", "COLD");
        tag.insert("IsLarge", 1_i8);
        let loaded = StructurePiece::from_nbt(&tag, &templates).unwrap();
        assert_eq!(loaded.bounding_box.min, [-23, -60, 24]);
        assert_eq!(loaded.bounding_box.max, [-18, -54, 30]);
        let template = loaded.template.as_ref().unwrap();
        assert_eq!(template.position, [-18, -60, 30]);
        assert_eq!(template.pivot, [0, 0, 0]);
        let saved = loaded.to_nbt();
        for key in [
            "Template",
            "Rot",
            "TPX",
            "TPY",
            "TPZ",
            "Integrity",
            "BiomeType",
            "IsLarge",
        ] {
            assert_eq!(saved.get(key), tag.get(key), "{key}");
        }
    }

    #[test]
    fn shipwreck_reload_retains_adjusted_origin_and_reconstructs_rotated_bounds() {
        let templates = StructureTemplates::with_sizes([("test:ship", [9, 9, 28])]);
        let mut tag = Compound::new();
        tag.insert("id", "minecraft:shipwreck");
        tag.insert("BB", Nbt::IntArray(vec![0, 90, 0, 8, 98, 27]));
        tag.insert("O", 2);
        tag.insert("GD", 0);
        tag.insert("Template", "test:ship");
        tag.insert("TPX", 0);
        tag.insert("TPY", 46);
        tag.insert("TPZ", 0);
        tag.insert("Rot", "CLOCKWISE_90");
        tag.insert("isBeached", 1_i8);
        tag.insert("height_adjusted", 1_i8);
        let loaded = StructurePiece::from_nbt(&tag, &templates).unwrap();
        assert_eq!(loaded.bounding_box.min, [-8, 46, 11]);
        assert_eq!(loaded.bounding_box.max, [19, 54, 19]);
        let template = loaded.template.as_ref().unwrap();
        assert_eq!(template.position, [0, 46, 0]);
        assert_eq!(template.pivot, [4, 0, 15]);
        let saved = loaded.to_nbt();
        assert_eq!(saved.get("height_adjusted"), Some(&Nbt::Byte(1)));
        assert_eq!(saved.get("isBeached"), Some(&Nbt::Byte(1)));
        assert_eq!(saved.get("TPY"), Some(&Nbt::Int(46)));
    }

    #[test]
    fn igloo_reload_rebuilds_template_bounds_from_its_original_position() {
        let templates = StructureTemplates::with_sizes([("minecraft:igloo/top", [7, 5, 8])]);
        let mut tag = Compound::new();
        tag.insert("id", "minecraft:iglu");
        tag.insert("BB", Nbt::IntArray(vec![0, 69, 0, 6, 73, 7]));
        tag.insert("O", 2);
        tag.insert("GD", 0);
        tag.insert("Template", "minecraft:igloo/top");
        tag.insert("TPX", 0);
        tag.insert("TPY", 90);
        tag.insert("TPZ", 0);
        tag.insert("Rot", "NONE");
        let loaded = StructurePiece::from_nbt(&tag, &templates).unwrap();
        assert_eq!(loaded.bounding_box.min, [0, 90, 0]);
        assert_eq!(loaded.bounding_box.max, [6, 94, 7]);
        let template = loaded.template.as_ref().unwrap();
        assert_eq!(template.pivot, [3, 5, 5]);
        assert_eq!(template.position, [0, 90, 0]);
        assert_eq!(loaded.to_nbt().get("TPY"), Some(&Nbt::Int(90)));
        tag.insert("Template", "minecraft:igloo/unknown");
        assert!(StructurePiece::from_nbt(&tag, &templates).is_err());
    }

    #[test]
    fn invalid_or_malformed_saved_starts_do_not_become_new_layouts() {
        let invalid = [("id", Nbt::from("INVALID"))].into_iter().collect();
        assert!(
            StructureStart::from_nbt(&invalid, &data(), &templates())
                .unwrap()
                .is_none()
        );
        let mut broken = fixture().to_nbt();
        broken.insert("Children", List::Empty);
        assert!(StructureStart::from_nbt(&broken, &data(), &templates()).is_err());
        let mut broken = fixture().to_nbt();
        let Some(Nbt::List(List::Compound(mut pieces))) = broken.remove("Children") else {
            unreachable!()
        };
        pieces[0].insert("BB", Nbt::IntArray(vec![0, 0, 0, -1, 1, 1]));
        broken.insert("Children", List::Compound(pieces));
        assert!(StructureStart::from_nbt(&broken, &data(), &templates()).is_err());
    }
}
