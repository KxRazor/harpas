//! Compares structure templates and template pools with an owner-provided server.

mod reference;

use std::fs::File;
use std::io::{BufRead, BufReader};

use harpas_core::random::LegacyRandom;
use harpas_worldgen::{JointType, Rotation, StructureTemplates, WorldgenData};
use reference::Reference;

/// Positions the templates are placed at, as in `TemplateProbe.POSITIONS`.
const POSITIONS: [[i32; 3]; 2] = [[10, 64, -7], [1234, -20, 5678]];

#[test]
fn templates_and_pools_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping vanilla templates: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    let templates =
        StructureTemplates::from_zip_path(&reference.server_jar).expect("load vanilla templates");
    let mut ids: Vec<_> = templates.ids().collect();
    ids.sort();

    let mut expected = Vec::new();
    let mut jigsaw_count = 0;
    for id in &ids {
        let template = templates.get(id).unwrap();
        let [x, y, z] = template.size;
        expected.push(format!("template {id} {x} {y} {z}"));
        for position in POSITIONS {
            if template.palette_count() > 1 {
                expected.push(format!(
                    "palette {} of {}",
                    template.palette_index(position),
                    template.palette_count()
                ));
            }
            for (index, rotation) in Rotation::ALL.into_iter().enumerate() {
                let b = template.bounding_box(position, rotation);
                expected.push(format!(
                    "box {index} {} {} {} {} {} {}",
                    b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]
                ));
                for jigsaw in template.jigsaws(position, rotation) {
                    jigsaw_count += 1;
                    let [x, y, z] = jigsaw.pos;
                    let joint = match jigsaw.joint {
                        JointType::Rollable => "rollable",
                        JointType::Aligned => "aligned",
                    };
                    expected.push(format!(
                        "jigsaw {x} {y} {z} {}_{} {} {} {} {joint} {} {}",
                        jigsaw.front.name(),
                        jigsaw.top.name(),
                        jigsaw.name,
                        jigsaw.pool,
                        jigsaw.target,
                        jigsaw.placement_priority,
                        jigsaw.selection_priority
                    ));
                }
            }
        }
    }
    let mut pool_ids: Vec<_> = data.template_pools.keys().collect();
    pool_ids.sort();
    for id in &pool_ids {
        let pool = &data.template_pools[*id];
        expected.push(format!(
            "pool {id} {} {}",
            pool.fallback,
            pool.templates().len()
        ));
        for (element, weight) in &pool.elements {
            expected.push(format!(
                "element {weight} {}",
                reference::describe_element(element)
            ));
        }
        let shuffled: Vec<_> = pool
            .shuffled_templates(&mut LegacyRandom::new(7))
            .into_iter()
            .map(reference::describe_element)
            .collect();
        expected.push(format!("shuffled {}", shuffled.join(" | ")));
    }

    let id_file = reference.temp.0.join("template-ids.txt");
    std::fs::write(&id_file, ids.join("\n") + "\n").unwrap();
    reference.compile(&[
        (
            "StructurePlacementProbe.java",
            include_str!("StructurePlacementProbe.java"),
        ),
        ("TemplateProbe.java", include_str!("TemplateProbe.java")),
    ]);
    let output = reference.temp.0.join("templates.txt");
    reference.java(
        "TemplateProbe",
        [output.display().to_string(), id_file.display().to_string()],
    );
    let vanilla: Vec<_> = BufReader::new(File::open(&output).unwrap())
        .lines()
        .map(Result::unwrap)
        .collect();
    for (index, (vanilla, harpas)) in vanilla.iter().zip(&expected).enumerate() {
        assert_eq!(vanilla, harpas, "line {index}");
    }
    assert_eq!(vanilla.len(), expected.len(), "number of lines");
    eprintln!(
        "{} templates with {jigsaw_count} placed jigsaws and {} pools match",
        ids.len(),
        pool_ids.len()
    );
}
