//! Vanilla writes an entity region, Harpas saves the same items, and vanilla loads and saves them again.
#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::fs;

use harpas_core::nbt::{List, Nbt};
use harpas_engine::entity_data::EntityChunk;
use harpas_engine::item_data::ItemCodec;
use harpas_engine::world::ChunkPos;
use reference::Reference;

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn item_entities_survive_a_vanilla_region_round_trip() {
    let reference = Reference::required();
    reference.compile(&[("ItemEntityProbe.java", include_str!("ItemEntityProbe.java"))]);
    let original_dir = reference.temp.0.join("vanilla/entities");
    let components = reference.temp.0.join("components.json");
    reference.java(
        "ItemEntityProbe",
        [
            "write".to_owned(),
            original_dir.display().to_string(),
            components.display().to_string(),
        ],
    );
    let names: Vec<String> = serde_json::from_slice(&fs::read(components).unwrap()).unwrap();
    let codec = ItemCodec::new(&names);
    let pos = ChunkPos { x: -1, z: 2 };
    let original = harpas_anvil::read_chunk(&original_dir, pos.x, pos.z)
        .unwrap()
        .unwrap();
    let mut entities = EntityChunk::read(original.clone(), pos, &codec).unwrap();
    assert_eq!(entities.items.len(), 6);
    for (i, item) in entities.items.iter().enumerate() {
        assert_eq!(
            item.uuid,
            0x123456789abcdef0_fedcba9876543210 + ((i as u128) << 64) - i as u128
        );
        assert_eq!(
            item.position,
            [-0.5 - i as f64, 64.25 + i as f64, 32.5 + i as f64]
        );
        assert_eq!(item.velocity, [0.125 * i as f64, -0.25, -0.0625 * i as f64]);
        assert_eq!(item.on_ground, i % 2 == 0);
        assert_eq!(item.stack.name(), Some("minecraft:stone"));
        assert_eq!(item.stack.count, i as i32 + 1);
        assert_eq!(item.age, [0, 100, 200, 300, -32768, 5999][i]);
        assert_eq!(item.pickup_delay, [0, 10, 20, 32767, 40, 50][i]);
        assert_eq!(item.health, if i % 2 == 0 { 5.0 } else { 2.0 });
    }
    assert_eq!(entities.to_nbt(pos, &codec).unwrap(), original);

    // Write actual changes so both codecs are exercised beyond preservation of the input NBT.
    entities.items[0].age = 81;
    entities.items[0].position[0] -= 0.125;
    entities.items[1].stack.count += 1;
    let edited = entities.to_nbt(pos, &codec).unwrap();
    let harpas_dir = reference.temp.0.join("harpas/entities");
    let (rx, rz) = harpas_anvil::region_of(pos.x, pos.z);
    harpas_anvil::write_chunks(&harpas_dir, rx, rz, &[((pos.x, pos.z), &edited)]).unwrap();
    let round_trip_path = reference.temp.0.join("vanilla-reopened.nbt");
    reference.java(
        "ItemEntityProbe",
        [
            "read".to_owned(),
            harpas_dir.display().to_string(),
            round_trip_path.display().to_string(),
        ],
    );
    let bytes = fs::read(round_trip_path).unwrap();
    let mut bytes = bytes.as_slice();
    let Nbt::Compound(reopened) = Nbt::read_network(&mut bytes).unwrap() else {
        panic!("expected a chunk compound");
    };
    assert!(bytes.is_empty());
    assert!(
        matches!(reopened.get("Entities"), Some(Nbt::List(List::Compound(items))) if items.len() == 6)
    );
    assert_eq!(reopened, edited);
}
