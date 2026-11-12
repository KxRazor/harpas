//! Compares the global order of the placed features in each generation step with vanilla's `FeatureSorter`.
//! The blocks the stage places are compared by `feature_blocks_parity`.
mod reference;

use std::fs;

use harpas_worldgen::{FeatureIndex, WorldgenData};
use reference::Reference;

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn feature_stage_matches_vanilla() {
    let reference = Reference::required();
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    reference.compile(&[(
        "FeatureStageProbe.java",
        include_str!("FeatureStageProbe.java"),
    )]);
    let index = FeatureIndex::overworld(&data).expect("compile the feature index");
    let names: Vec<_> = data.placed_features.keys().collect();
    let output = reference.temp.0.join("features.txt");
    reference.java("FeatureStageProbe", [output.display().to_string()]);
    let text = fs::read_to_string(&output).expect("read the vanilla feature order");
    let mut step = 0;
    for line in text.lines() {
        let expected: Vec<_> = line
            .strip_prefix("step")
            .expect("a step line")
            .split_whitespace()
            .collect();
        let actual: Vec<_> = index
            .step_order(step)
            .unwrap_or_default()
            .iter()
            .map(|&placed| names[placed].as_str())
            .collect();
        assert_eq!(actual, expected, "order of generation step {step}");
        step += 1;
    }
    assert_eq!(step, 11, "generation steps");
}
