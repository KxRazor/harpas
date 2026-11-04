//! Ruined portals: vanilla's `RuinedPortalStructure`.

use harpas_core::random::RandomSource;

use super::start::{StructurePiece, TemplatePiece};
use super::templated::placed_piece;
use crate::data::{RuinedPortalPlacement, RuinedPortalSetupDef, WorldgenDataError};
use crate::generator::{HeightmapType, NoiseBasedChunkGenerator};
use crate::template::{Mirror, Rotation, StructureTemplates};

/// The chance of one of the giant portals.
const GIANT_PORTAL_CHANCE: f32 = 0.05;
const GIANT_PORTALS: i32 = 3;
const STANDARD_PORTALS: i32 = 10;
/// How far above the minimum Y a portal stays.
const MIN_Y_MARGIN: i32 = 15;
/// How many corners of a portal must stand on terrain.
const CORNERS_ON_TERRAIN: usize = 3;

/// `RuinedPortalStructure.findGenerationPoint`: the generation point and the portal's piece.
pub(super) fn start(
    generator: &NoiseBasedChunkGenerator,
    templates: &StructureTemplates,
    setups: &[RuinedPortalSetupDef],
    random: &mut impl RandomSource,
    x: i32,
    z: i32,
) -> Result<([i32; 3], StructurePiece), WorldgenDataError> {
    let setup = choose_setup(setups, random);
    let air_pocket = sample(random, setup.air_pocket_probability);
    let template = if random.next_float() < GIANT_PORTAL_CHANCE {
        let number = 1 + random.next_int_bounded(GIANT_PORTALS);
        format!("minecraft:ruined_portal/giant_portal_{number}")
    } else {
        let number = 1 + random.next_int_bounded(STANDARD_PORTALS);
        format!("minecraft:ruined_portal/portal_{number}")
    };
    let rotation = Rotation::random(random);
    let mirror = if random.next_float() < 0.5 {
        Mirror::None
    } else {
        Mirror::FrontBack
    };
    let found = templates
        .get(&template)
        .ok_or_else(|| WorldgenDataError::InvalidDefinition {
            context: template.clone(),
            message: "unknown structure template".to_string(),
        })?;
    let pivot = [found.size[0] / 2, 0, found.size[2] / 2];
    let bounds = found.bounding_box_around([x, 0, z], rotation, mirror, pivot);
    let kind = heightmap(setup.placement);
    let surface = generator.base_height(
        bounds.min[0] + (bounds.max[0] - bounds.min[0] + 1) / 2,
        bounds.min[2] + (bounds.max[2] - bounds.min[2] + 1) / 2,
        kind,
    ) - 1;
    let min_y = generator.min_y() + MIN_Y_MARGIN;
    let span = bounds.max[1] - bounds.min[1] + 1;
    let y = first_y(random, setup.placement, air_pocket, surface, span, min_y);
    let corners = [
        (bounds.min[0], bounds.min[2]),
        (bounds.max[0], bounds.min[2]),
        (bounds.min[0], bounds.max[2]),
        (bounds.max[0], bounds.max[2]),
    ];
    let columns = corners.map(|(x, z)| generator.base_column(x, z));
    let y = settle_y(&columns, generator.min_y(), y, min_y, |state| {
        kind.matches(state)
    });
    let position = [x, y, z];
    let piece = placed_piece(
        "minecraft:rupo",
        templates,
        TemplatePiece {
            template,
            position,
            rotation,
            mirror,
            pivot,
        },
    )?;
    Ok((position, piece))
}

/// `RuinedPortalPiece.getHeightMapType`.
const fn heightmap(placement: RuinedPortalPlacement) -> HeightmapType {
    match placement {
        RuinedPortalPlacement::OnOceanFloor => HeightmapType::OceanFloorWg,
        _ => HeightmapType::WorldSurfaceWg,
    }
}

/// Chooses a setup by weight. The last one takes what rounding leaves over, where vanilla would fail.
fn choose_setup<'s>(
    setups: &'s [RuinedPortalSetupDef],
    random: &mut impl RandomSource,
) -> &'s RuinedPortalSetupDef {
    if let [only] = setups {
        return only;
    }
    let total: f32 = setups.iter().map(|setup| setup.weight).sum();
    let mut choice = random.next_float();
    for setup in setups {
        choice -= setup.weight / total;
        if choice < 0.0 {
            return setup;
        }
    }
    &setups[setups.len() - 1]
}

/// `RuinedPortalStructure.sample`: a certain result draws nothing.
fn sample(random: &mut impl RandomSource, probability: f32) -> bool {
    if probability == 0.0 {
        false
    } else if probability == 1.0 {
        true
    } else {
        random.next_float() < probability
    }
}

/// The Y a placement starts from (`RuinedPortalStructure.findSuitableY`).
fn first_y(
    random: &mut impl RandomSource,
    placement: RuinedPortalPlacement,
    air_pocket: bool,
    surface: i32,
    span: i32,
    min_y: i32,
) -> i32 {
    let within = |random: &mut _, min: i32, max: i32| {
        if min < max {
            RandomSource::next_int_between_inclusive(random, min, max)
        } else {
            max
        }
    };
    match placement {
        RuinedPortalPlacement::InNether if air_pocket => random.next_int_between_inclusive(32, 100),
        RuinedPortalPlacement::InNether if random.next_float() < 0.5 => {
            random.next_int_between_inclusive(27, 29)
        }
        RuinedPortalPlacement::InNether => random.next_int_between_inclusive(29, 100),
        RuinedPortalPlacement::InMountain => within(random, 70, surface - span),
        RuinedPortalPlacement::Underground => within(random, min_y, surface - span),
        RuinedPortalPlacement::PartlyBuried => {
            surface - span + random.next_int_between_inclusive(2, 8)
        }
        RuinedPortalPlacement::OnLandSurface | RuinedPortalPlacement::OnOceanFloor => surface,
    }
}

/// Moves down from `y` until enough corners stand on terrain, stopping at `min_y`. Above or below a column there is
/// only air.
fn settle_y(
    columns: &[Vec<u32>],
    column_min_y: i32,
    mut y: i32,
    min_y: i32,
    is_terrain: impl Fn(u32) -> bool,
) -> i32 {
    while y > min_y {
        let on_terrain = columns
            .iter()
            .filter(|column| {
                usize::try_from(y - column_min_y)
                    .ok()
                    .and_then(|index| column.get(index))
                    .is_some_and(|state| is_terrain(*state))
            })
            .count();
        if on_terrain >= CORNERS_ON_TERRAIN {
            return y;
        }
        y -= 1;
    }
    y
}

#[cfg(test)]
mod tests {
    use harpas_core::random::LegacyRandom;

    use super::*;

    fn setup(placement: RuinedPortalPlacement, weight: f32) -> RuinedPortalSetupDef {
        RuinedPortalSetupDef {
            placement,
            air_pocket_probability: 0.5,
            weight,
        }
    }

    #[test]
    fn a_certain_sample_draws_nothing() {
        let mut random = LegacyRandom::new(1);
        let mut untouched = LegacyRandom::new(1);
        assert!(!sample(&mut random, 0.0));
        assert!(sample(&mut random, 1.0));
        assert_eq!(random.next_int(), untouched.next_int());
    }

    #[test]
    fn a_single_setup_draws_nothing_and_weights_split_the_rest() {
        let only = [setup(RuinedPortalPlacement::Underground, 0.5)];
        let mut random = LegacyRandom::new(1);
        let mut untouched = LegacyRandom::new(1);
        assert_eq!(choose_setup(&only, &mut random), &only[0]);
        assert_eq!(random.next_int(), untouched.next_int());

        let both = [
            setup(RuinedPortalPlacement::Underground, 3.0),
            setup(RuinedPortalPlacement::OnLandSurface, 1.0),
        ];
        let underground = (0..1000)
            .filter(|seed| {
                choose_setup(&both, &mut LegacyRandom::new(*seed)).placement
                    == RuinedPortalPlacement::Underground
            })
            .count();
        assert!((650..850).contains(&underground), "{underground}");
    }

    #[test]
    fn placements_start_from_their_own_heights() {
        let mut random = LegacyRandom::new(5);
        let first =
            |random: &mut LegacyRandom, placement| first_y(random, placement, false, 100, 10, -49);
        assert_eq!(
            first(&mut random, RuinedPortalPlacement::OnLandSurface),
            100
        );
        assert_eq!(first(&mut random, RuinedPortalPlacement::OnOceanFloor), 100);
        for _ in 0..64 {
            let buried = first(&mut random, RuinedPortalPlacement::PartlyBuried);
            assert!((92..=98).contains(&buried), "{buried}");
            let mountain = first(&mut random, RuinedPortalPlacement::InMountain);
            assert!((70..=90).contains(&mountain), "{mountain}");
            let underground = first(&mut random, RuinedPortalPlacement::Underground);
            assert!((-49..=90).contains(&underground), "{underground}");
        }
        let low = first_y(
            &mut random,
            RuinedPortalPlacement::InMountain,
            false,
            60,
            10,
            -49,
        );
        assert_eq!(low, 50);
    }

    #[test]
    fn a_portal_settles_where_three_corners_stand_on_terrain() {
        let column = |top: usize| {
            let mut column = vec![0; 32];
            column[..top].fill(1);
            column
        };
        let columns = [column(20), column(16), column(12), column(4)];
        let settle = |y, min| settle_y(&columns, 0, y, min, |state| state == 1);
        assert_eq!(settle(31, 2), 11);
        assert_eq!(settle(8, 2), 8);
        assert_eq!(settle(31, 15), 15);
        assert_eq!(settle(100, 2), 11);
    }
}
