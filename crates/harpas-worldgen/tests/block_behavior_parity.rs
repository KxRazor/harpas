//! Compares every block state's solid, replaceable, liquid and motion-blocking flags, the full and center
//! support and occlusion of each face, the solid render flag and whether the block has a block entity with vanilla's,
//! then the faces each state occludes light with and whether light passes between every pair of distinct faces.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};
use reference::Reference;

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn block_behavior_matches_vanilla() {
    let reference = Reference::required();
    reference.compile(&[(
        "BlockBehaviorProbe.java",
        include_str!("BlockBehaviorProbe.java"),
    )]);
    let output = reference.temp.0.join("block-behavior.txt");
    reference.java("BlockBehaviorProbe", [output.display().to_string()]);
    let text = fs::read_to_string(&output).expect("read the vanilla block behavior");

    let behavior = block_behavior();
    let states = block_states();
    let mut compared = 0;
    let mut mismatches = Vec::new();
    let light_lines = text.lines().filter(|line| line.starts_with("light "));
    let pair_lines = text.lines().filter(|line| line.starts_with("pair "));
    for line in text.lines().filter(|line| {
        !["block ", "light ", "pair "]
            .iter()
            .any(|p| line.starts_with(p))
    }) {
        let fields: Vec<u32> = line
            .split(' ')
            .map(|field| field.parse().expect("a number"))
            .collect();
        let &[
            id,
            solid,
            replaceable,
            liquid,
            blocks_motion,
            sturdy,
            solid_render,
            block_entity,
            occlusion,
        ] = fields.as_slice()
        else {
            panic!("malformed line {line}");
        };
        let actual_sturdy = Face::ALL.iter().fold(0, |bits, &face| {
            let full = u32::from(behavior.is_face_sturdy(id, face)) << face as u32;
            let center = u32::from(behavior.is_face_center_sturdy(id, face)) << (8 + face as u32);
            bits | full | center
        });
        let actual = (
            u32::from(behavior.is_solid(id)),
            u32::from(behavior.can_be_replaced(id)),
            u32::from(behavior.is_liquid(id)),
            u32::from(behavior.blocks_motion(id)),
            actual_sturdy,
            u32::from(behavior.is_solid_render(id)),
            u32::from(behavior.has_block_entity(id)),
            Face::ALL.iter().fold(0, |bits, &face| {
                bits | u32::from(behavior.is_occlusion_face_full(id, face)) << face as u32
            }),
        );
        let expected = (
            solid,
            replaceable,
            liquid,
            blocks_motion,
            sturdy,
            solid_render,
            block_entity,
            occlusion,
        );
        if actual != expected {
            mismatches.push(format!(
                "{:?}: vanilla {:?}, Harpas {actual:?}",
                states.properties(id),
                expected
            ));
        }
        compared += 1;
    }
    assert_eq!(compared, states.len(), "every block state");
    assert!(
        mismatches.is_empty(),
        "{} of {compared} states differ (solid, replaceable, liquid, blocks motion, sturdy faces, solid render, block entity, occlusion faces):\n{}",
        mismatches.len(),
        mismatches[..mismatches.len().min(30)].join("\n")
    );
    eprintln!("Compared the behavior flags of {compared} block states");

    let mut faces = 0;
    let mut mismatches = Vec::new();
    for line in light_lines {
        let mut fields = line.split(' ').skip(1);
        let id: u32 = fields.next().unwrap().parse().unwrap();
        for (&face, field) in Face::ALL.iter().zip(fields) {
            let expected: Vec<u32> = if field == "-" {
                vec![0; 32]
            } else {
                field
                    .split(',')
                    .map(|row| u32::from_str_radix(row, 16).unwrap())
                    .collect()
            };
            if behavior.light_face(id, face).as_slice() != expected {
                mismatches.push(format!("{:?} {face:?}", states.properties(id)));
            }
            faces += 1;
        }
    }
    assert_eq!(
        faces,
        states.len() as usize * 6,
        "every face of every state"
    );
    let mut pairs = 0;
    for line in pair_lines {
        let fields: Vec<u32> = line
            .split(' ')
            .skip(1)
            .map(|field| field.parse().unwrap())
            .collect();
        let &[from, to, face, occludes] = fields.as_slice() else {
            panic!("malformed line {line}");
        };
        let face = Face::ALL[face as usize];
        if behavior.light_occludes(from, to, face) != (occludes == 1) {
            mismatches.push(format!(
                "{:?} towards {face:?} {:?}: vanilla {}",
                states.properties(from),
                states.properties(to),
                occludes == 1
            ));
        }
        pairs += 1;
    }
    assert!(
        mismatches.is_empty(),
        "{} light faces or pairs differ:\n{}",
        mismatches.len(),
        mismatches[..mismatches.len().min(30)].join("\n")
    );
    eprintln!("Compared {faces} light occlusion faces and {pairs} pairs of faces");
}
