//! Structures whose pieces are whole templates: vanilla's `IglooPieces`, `ShipwreckPieces` and `OceanRuinPieces`.

use harpas_core::random::RandomSource;

use super::start::{BoundingBox, Direction, StructurePiece, TemplatePiece};
use crate::data::{OceanRuinDef, WorldgenDataError};
use crate::template::{Rotation, StructureTemplates};

/// The Y these structures are made at, before they are placed on the terrain.
const START_Y: i32 = 90;
/// The widest and tallest template a shipwreck may have here. Vanilla places larger ones by a different rule
/// (`ShipwreckPiece.isTooBigToFitInWorldGenRegion`), which none of its own templates reaches.
const SHIPWRECK_MAX_SIZE: i32 = 32;
/// The point shipwrecks turn around.
const SHIPWRECK_PIVOT: [i32; 3] = [4, 0, 15];
const BEACHED_SHIPWRECKS: [&str; 11] = [
    "with_mast",
    "sideways_full",
    "sideways_fronthalf",
    "sideways_backhalf",
    "rightsideup_full",
    "rightsideup_fronthalf",
    "rightsideup_backhalf",
    "with_mast_degraded",
    "rightsideup_full_degraded",
    "rightsideup_fronthalf_degraded",
    "rightsideup_backhalf_degraded",
];
const OCEAN_SHIPWRECKS: [&str; 20] = [
    "with_mast",
    "upsidedown_full",
    "upsidedown_fronthalf",
    "upsidedown_backhalf",
    "sideways_full",
    "sideways_fronthalf",
    "sideways_backhalf",
    "rightsideup_full",
    "rightsideup_fronthalf",
    "rightsideup_backhalf",
    "with_mast_degraded",
    "upsidedown_full_degraded",
    "upsidedown_fronthalf_degraded",
    "upsidedown_backhalf_degraded",
    "sideways_full_degraded",
    "sideways_fronthalf_degraded",
    "sideways_backhalf_degraded",
    "rightsideup_full_degraded",
    "rightsideup_fronthalf_degraded",
    "rightsideup_backhalf_degraded",
];
/// The numbers of the large cold ruins' templates.
const BIG_COLD_RUINS: [i32; 4] = [1, 2, 3, 8];
/// The numbers of the large warm ruins' templates.
const BIG_WARM_RUINS: [i32; 4] = [4, 5, 6, 7];
/// How many small ruins' templates there are of each kind.
const SMALL_RUINS: i32 = 8;

/// A template piece. These all report facing north, whatever their rotation.
fn piece(
    kind: &'static str,
    templates: &StructureTemplates,
    template: String,
    position: [i32; 3],
    rotation: Rotation,
    pivot: [i32; 3],
) -> Result<StructurePiece, WorldgenDataError> {
    let found = templates
        .get(&template)
        .ok_or_else(|| WorldgenDataError::InvalidDefinition {
            context: template.clone(),
            message: "unknown structure template".to_string(),
        })?;
    Ok(StructurePiece {
        kind,
        bounding_box: found.bounding_box_around(position, rotation, pivot),
        orientation: Some(Direction::North),
        gen_depth: 0,
        junctions: Vec::new(),
        jigsaw: None,
        template: Some(TemplatePiece {
            template,
            position,
            rotation,
            pivot,
        }),
    })
}

/// `IglooPieces.addPieces`: half of the igloos have a laboratory, reached by a ladder 4 to 11 sections long.
pub(super) fn igloo(
    templates: &StructureTemplates,
    random: &mut impl RandomSource,
    x: i32,
    z: i32,
) -> Result<Vec<StructurePiece>, WorldgenDataError> {
    let rotation = Rotation::random(random);
    let add = |name: &str, pivot, [dx, dy, dz]: [i32; 3], depth| {
        piece(
            "minecraft:iglu",
            templates,
            format!("minecraft:igloo/{name}"),
            [x + dx, START_Y + dy - depth, z + dz],
            rotation,
            pivot,
        )
    };
    let mut pieces = Vec::new();
    if random.next_double() < 0.5 {
        let depth = random.next_int_bounded(8) + 4;
        pieces.push(add("bottom", [3, 6, 7], [0, -3, -2], depth * 3)?);
        for section in 0..depth - 1 {
            pieces.push(add("middle", [1, 3, 1], [2, -3, 4], section * 3)?);
        }
    }
    pieces.push(add("top", [3, 5, 5], [0, 0, 0], 0)?);
    Ok(pieces)
}

/// `ShipwreckStructure.generatePieces`.
pub(super) fn shipwreck(
    templates: &StructureTemplates,
    random: &mut impl RandomSource,
    beached: bool,
    x: i32,
    z: i32,
) -> Result<Vec<StructurePiece>, WorldgenDataError> {
    let rotation = Rotation::random(random);
    let names: &[&str] = if beached {
        &BEACHED_SHIPWRECKS
    } else {
        &OCEAN_SHIPWRECKS
    };
    let name = names[random.next_int_bounded(names.len() as i32) as usize];
    let template = format!("minecraft:shipwreck/{name}");
    if let Some(found) = templates.get(&template)
        && (found.size[0] > SHIPWRECK_MAX_SIZE || found.size[1] > SHIPWRECK_MAX_SIZE)
    {
        return Err(WorldgenDataError::InvalidDefinition {
            context: template,
            message: "shipwreck template too large to place".to_string(),
        });
    }
    let start = piece(
        "minecraft:shipwreck",
        templates,
        template,
        [x, START_Y, z],
        rotation,
        SHIPWRECK_PIVOT,
    )?;
    Ok(vec![start])
}

/// `OceanRuinPieces.addPieces`: a ruin and, if it is large, sometimes small ones around it.
pub(super) fn ocean_ruin(
    templates: &StructureTemplates,
    random: &mut impl RandomSource,
    settings: OceanRuinDef,
    x: i32,
    z: i32,
) -> Result<Vec<StructurePiece>, WorldgenDataError> {
    let position = [x, START_Y, z];
    let rotation = Rotation::random(random);
    let large = random.next_float() <= settings.large_probability;
    let mut pieces = Vec::new();
    add_ruin(
        &mut pieces,
        templates,
        random,
        settings.cold,
        large,
        position,
        rotation,
    )?;
    if large && random.next_float() <= settings.cluster_probability {
        let corner = offset(rotation.transform([15, 0, 15]), position);
        let large_box = BoundingBox::from_corners(position, corner);
        let mut places = cluster_places(random, large_box.min);
        for _ in 0..random.next_int_between_inclusive(4, 8) {
            if places.is_empty() {
                break;
            }
            let place = places.remove(random.next_int_bounded(places.len() as i32) as usize);
            let rotation = Rotation::random(random);
            let corner = offset(rotation.transform([5, 0, 6]), place);
            let small_box = BoundingBox::from_corners(place, corner);
            if !large_box.intersects_columns(
                small_box.min[0],
                small_box.min[2],
                small_box.max[0],
                small_box.max[2],
            ) {
                add_ruin(
                    &mut pieces,
                    templates,
                    random,
                    settings.cold,
                    false,
                    place,
                    rotation,
                )?;
            }
        }
    }
    Ok(pieces)
}

const fn offset([x, y, z]: [i32; 3], [dx, dy, dz]: [i32; 3]) -> [i32; 3] {
    [x + dx, y + dy, z + dz]
}

/// `OceanRuinPieces.allPositions`: one candidate in each of the eight areas around a large ruin.
fn cluster_places(random: &mut impl RandomSource, [x, y, z]: [i32; 3]) -> Vec<[i32; 3]> {
    let areas = [
        (-16, 1, 8, 16, 1, 7),
        (-16, 1, 8, 0, 1, 7),
        (-16, 1, 8, -16, 4, 8),
        (0, 1, 7, 16, 1, 7),
        (0, 1, 7, -16, 4, 6),
        (16, 1, 7, 16, 3, 8),
        (16, 1, 7, 0, 1, 7),
        (16, 1, 7, -16, 4, 8),
    ];
    areas
        .into_iter()
        .map(|(dx, min_x, max_x, dz, min_z, max_z)| {
            let px = x + dx + random.next_int_between_inclusive(min_x, max_x);
            let pz = z + dz + random.next_int_between_inclusive(min_z, max_z);
            [px, y, pz]
        })
        .collect()
}

/// `OceanRuinPieces.addPiece`: cold ruins are three templates that fill each other in, warm ones a single template.
fn add_ruin(
    pieces: &mut Vec<StructurePiece>,
    templates: &StructureTemplates,
    random: &mut impl RandomSource,
    cold: bool,
    large: bool,
    position: [i32; 3],
    rotation: Rotation,
) -> Result<(), WorldgenDataError> {
    let mut add = |name: String| {
        pieces.push(piece(
            "minecraft:orp",
            templates,
            format!("minecraft:underwater_ruin/{name}"),
            position,
            rotation,
            [0, 0, 0],
        )?);
        Ok(())
    };
    let prefix = if large { "big_" } else { "" };
    if !cold {
        let number = if large {
            BIG_WARM_RUINS[random.next_int_bounded(BIG_WARM_RUINS.len() as i32) as usize]
        } else {
            random.next_int_bounded(SMALL_RUINS) + 1
        };
        return add(format!("{prefix}warm_{number}"));
    }
    let number = if large {
        BIG_COLD_RUINS[random.next_int_bounded(BIG_COLD_RUINS.len() as i32) as usize]
    } else {
        random.next_int_bounded(SMALL_RUINS) + 1
    };
    for kind in ["brick", "cracked", "mossy"] {
        add(format!("{prefix}{kind}_{number}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use harpas_core::random::LegacyRandom;

    use super::*;

    fn ruin_templates() -> StructureTemplates {
        let mut names = Vec::new();
        for kind in ["brick", "cracked", "mossy", "warm"] {
            for number in 1..=SMALL_RUINS {
                names.push(format!("minecraft:underwater_ruin/{kind}_{number}"));
                names.push(format!("minecraft:underwater_ruin/big_{kind}_{number}"));
            }
        }
        StructureTemplates::with_sizes(names.iter().map(|name| (name.as_str(), [8, 8, 8])))
    }

    #[test]
    fn igloos_put_their_ladder_between_the_laboratory_and_the_top() {
        let templates = StructureTemplates::with_sizes([
            ("minecraft:igloo/top", [7, 5, 8]),
            ("minecraft:igloo/middle", [3, 3, 3]),
            ("minecraft:igloo/bottom", [7, 6, 9]),
        ]);
        let mut seen_laboratory = false;
        for seed in 0..32 {
            let pieces = igloo(&templates, &mut LegacyRandom::new(seed), 16, -32).unwrap();
            let names: Vec<_> = pieces
                .iter()
                .map(|piece| piece.template.as_ref().unwrap().template.as_str())
                .collect();
            assert_eq!(names.last(), Some(&"minecraft:igloo/top"));
            assert_eq!(pieces.last().unwrap().bounding_box.min[1], START_Y);
            if names.len() > 1 {
                seen_laboratory = true;
                let sections = names.len() - 2;
                assert!((3..=10).contains(&sections), "{sections} sections");
                assert_eq!(names[0], "minecraft:igloo/bottom");
                let depth = sections as i32 + 1;
                assert_eq!(pieces[0].bounding_box.min[1], START_Y - 3 - depth * 3);
            }
        }
        assert!(seen_laboratory);
    }

    #[test]
    fn beached_shipwrecks_are_never_upside_down() {
        assert!(
            BEACHED_SHIPWRECKS
                .iter()
                .all(|name| !name.contains("upside"))
        );
        assert_eq!(
            OCEAN_SHIPWRECKS
                .iter()
                .filter(|name| name.contains("upside"))
                .count(),
            6
        );
    }

    #[test]
    fn a_shipwreck_too_tall_for_a_region_is_refused() {
        let names = OCEAN_SHIPWRECKS.map(|name| format!("minecraft:shipwreck/{name}"));
        let templates =
            StructureTemplates::with_sizes(names.iter().map(|name| (name.as_str(), [9, 33, 28])));
        assert!(shipwreck(&templates, &mut LegacyRandom::new(1), false, 0, 0).is_err());
        let templates =
            StructureTemplates::with_sizes(names.iter().map(|name| (name.as_str(), [9, 21, 28])));
        let pieces = shipwreck(&templates, &mut LegacyRandom::new(1), false, 0, 0).unwrap();
        assert_eq!(pieces[0].template.as_ref().unwrap().pivot, SHIPWRECK_PIVOT);
    }

    #[test]
    fn cold_ruins_are_three_templates_with_the_same_number() {
        let settings = OceanRuinDef {
            cold: true,
            large_probability: 0.0,
            cluster_probability: 0.0,
        };
        let pieces =
            ocean_ruin(&ruin_templates(), &mut LegacyRandom::new(3), settings, 0, 0).unwrap();
        let names: Vec<_> = pieces
            .iter()
            .map(|piece| piece.template.as_ref().unwrap().template.clone())
            .collect();
        let number = names[0].rsplit('_').next().unwrap();
        assert_eq!(names.len(), 3);
        for (name, kind) in names.iter().zip(["brick", "cracked", "mossy"]) {
            assert_eq!(*name, format!("minecraft:underwater_ruin/{kind}_{number}"));
        }
    }

    #[test]
    fn clusters_keep_small_ruins_away_from_the_large_one() {
        let settings = OceanRuinDef {
            cold: false,
            large_probability: 1.0,
            cluster_probability: 1.0,
        };
        for seed in 0..32 {
            let pieces = ocean_ruin(
                &ruin_templates(),
                &mut LegacyRandom::new(seed),
                settings,
                0,
                0,
            )
            .unwrap();
            assert!(
                pieces[0]
                    .template
                    .as_ref()
                    .unwrap()
                    .template
                    .contains("big_warm_")
            );
            assert!(pieces.len() > 1, "seed {seed} has no cluster");
            for small in &pieces[1..] {
                assert!(small.template.as_ref().unwrap().template.contains("/warm_"));
            }
        }
    }
}
