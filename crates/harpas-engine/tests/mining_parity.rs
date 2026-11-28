//! Plain wooden, stone, iron and diamond pickaxes against Vanilla 26.1's block progress and tool wear.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::fs;
use std::path::Path;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, Nbt};
use harpas_engine::block::destroy_progress;
use harpas_engine::item_data::ItemCodec;
use harpas_engine::mining::MiningRules;
use harpas_worldgen::WorldgenData;
use serde_json::Value;

fn compare(pack: &Path, expected: &Value) {
    let rules = MiningRules::new(&WorldgenData::from_zip_path(pack).unwrap()).unwrap();
    let codec = ItemCodec::default();
    for (name, value) in expected["components"].as_object().unwrap() {
        let saved = Compound::from_iter([
            ("id", Nbt::from("minecraft:wooden_pickaxe")),
            (
                "components",
                Nbt::from(Compound::from_iter([(
                    format!("!{name}"),
                    Compound::default(),
                )])),
            ),
        ]);
        assert_eq!(
            codec.read(&saved).unwrap().components.removed,
            [value.as_i64().unwrap() as i32]
        );
    }
    for case in expected["cases"].as_array().unwrap() {
        let block = case["block"].as_str().unwrap();
        let tool = case["tool"].as_str().unwrap();
        let state = block_states().default_state(block).unwrap();
        let with_damage = |damage: Option<i32>| {
            let mut saved = Compound::from_iter([("id", tool)]);
            if let Some(damage) = damage {
                saved.insert(
                    "components",
                    Compound::from_iter([("minecraft:damage", damage)]),
                );
            }
            codec.read(&saved).unwrap()
        };
        let mut fresh = with_damage(None);
        let mining = rules.for_block(state, &fresh);
        assert_eq!(
            f64::from(mining.speed),
            case["speed"].as_f64().unwrap(),
            "{tool} on {block}"
        );
        assert_eq!(
            mining.can_harvest,
            case["harvest"].as_bool().unwrap(),
            "{tool} on {block}"
        );
        assert_eq!(
            u64::from(destroy_progress(state, mining.speed, mining.can_harvest).to_bits()),
            case["progress_bits"].as_u64().unwrap(),
            "{tool} on {block}"
        );
        assert!(!mining.wear(&codec, &mut fresh).unwrap());
        let damage = case["damage"].as_i64().unwrap() as i32;
        assert_eq!(
            fresh,
            with_damage((damage > 0).then_some(damage)),
            "{tool} on {block}"
        );
        assert_eq!(codec.read(&codec.write(&fresh).unwrap()).unwrap(), fresh);

        let max = case["maximum"].as_i64().unwrap() as i32;
        let mut almost_broken = with_damage(Some(max - 2));
        assert!(!mining.wear(&codec, &mut almost_broken).unwrap());
        assert_eq!(
            almost_broken,
            with_damage(Some(case["near_limit_damage"].as_i64().unwrap() as i32))
        );
        let broken = mining.wear(&codec, &mut almost_broken).unwrap();
        assert_eq!(
            broken,
            case["broken"].as_bool().unwrap(),
            "{tool} on {block}"
        );
        assert_eq!(almost_broken.is_empty(), broken);
        if !broken {
            assert_eq!(
                almost_broken,
                with_damage(Some(case["limit_damage"].as_i64().unwrap() as i32))
            );
        }
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn wooden_stone_iron_and_diamond_pickaxes_match_vanilla() {
    let reference = reference::Reference::required();
    reference.compile(&[("MiningProbe.java", include_str!("MiningProbe.java"))]);
    let report = reference.temp.0.join("mining.json");
    reference.java("MiningProbe", [report.display().to_string()]);
    let expected: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    compare(&reference.server_jar, &expected);
}
