//! Compares how every block state turns with each rotation and reflects front to back with vanilla's
//! `BlockState.rotate` and `BlockState.mirror`.
mod reference;

use std::collections::BTreeMap;
use std::fs;

use harpas_core::block::block_states;
use harpas_worldgen::{Mirror, Rotation};
use reference::Reference;

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn every_block_state_turns_and_reflects_as_in_vanilla() {
    let reference = Reference::required();
    reference.compile(&[(
        "BlockTransformProbe.java",
        include_str!("BlockTransformProbe.java"),
    )]);
    let output = reference.temp.0.join("block-transform.txt");
    reference.java("BlockTransformProbe", [output.display().to_string()]);
    let text = fs::read_to_string(&output).expect("read the vanilla block transforms");

    let states = block_states();
    let describe = |id: u32| format!("{:?}", states.properties(id).expect("a state"));
    let mut compared = 0;
    // One example per block and transform keeps a systematic difference readable.
    let mut mismatches = BTreeMap::new();
    for line in text.lines() {
        let ids: Vec<u32> = line
            .split(' ')
            .map(|field| field.parse().expect("a state id"))
            .collect();
        let &[id, none, clockwise, half, counterclockwise, front_back] = ids.as_slice() else {
            panic!("malformed line {line}");
        };
        let expected = [
            ("rotate none", none, Rotation::None.rotate_state(id)),
            (
                "rotate clockwise_90",
                clockwise,
                Rotation::Clockwise90.rotate_state(id),
            ),
            ("rotate 180", half, Rotation::Clockwise180.rotate_state(id)),
            (
                "rotate counterclockwise_90",
                counterclockwise,
                Rotation::Counterclockwise90.rotate_state(id),
            ),
            (
                "mirror front_back",
                front_back,
                Mirror::FrontBack.mirror_state(id),
            ),
        ];
        for (transform, vanilla, harpas) in expected {
            if vanilla != harpas {
                let block = states.properties(id).expect("a state").0.to_string();
                mismatches.entry((block, transform)).or_insert_with(|| {
                    format!(
                        "{}: vanilla {}, Harpas {}",
                        describe(id),
                        describe(vanilla),
                        describe(harpas)
                    )
                });
            }
            compared += 1;
        }
    }
    assert_eq!(compared, states.len() as usize * 5, "every state compared");
    assert!(
        mismatches.is_empty(),
        "{} block transforms differ from vanilla:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(60)
            .map(|((block, transform), example)| format!("{block} {transform}: {example}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
