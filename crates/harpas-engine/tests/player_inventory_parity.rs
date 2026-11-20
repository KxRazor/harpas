//! Vanilla-generated inventory/equipment NBT and component network values, decoded and saved by Harpas,
//! then read again with vanilla's native inventory and item codecs.
#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::fs;

use harpas_core::nbt::{Compound, Nbt};
use harpas_engine::item_data::ItemCodec;
use harpas_engine::player_data::PlayerInventory;
use reference::Reference;
use serde_json::{Value, json};

fn read(path: &std::path::Path) -> Compound {
    let bytes = fs::read(path).unwrap();
    let mut bytes = bytes.as_slice();
    let Nbt::Compound(root) = Nbt::read_network(&mut bytes).unwrap() else {
        panic!("expected compound");
    };
    assert!(bytes.is_empty());
    root
}

fn describe(stack: &harpas_engine::inventory::ItemStack) -> Value {
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    json!({"item":if stack.is_empty() { 0 } else { stack.item }, "count":if stack.is_empty() { 0 } else { stack.count },
        "added":stack.components.added.iter().map(|(id, bytes)| json!([id,hex(bytes)])).collect::<Vec<_>>(), "removed":stack.components.removed})
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn player_inventory_nbt_and_component_values_survive_a_vanilla_round_trip() {
    let reference = Reference::required();
    reference.compile(&[(
        "PlayerInventoryProbe.java",
        include_str!("PlayerInventoryProbe.java"),
    )]);
    let original_path = reference.temp.0.join("original.nbt");
    let values_path = reference.temp.0.join("values.json");
    reference.java(
        "PlayerInventoryProbe",
        [
            "write".to_owned(),
            original_path.display().to_string(),
            values_path.display().to_string(),
        ],
    );
    let values: Value = serde_json::from_slice(&fs::read(values_path).unwrap()).unwrap();
    let names: Vec<String> = serde_json::from_value(values["components"].clone()).unwrap();
    let enchantments: Vec<String> = serde_json::from_value(values["enchantments"].clone()).unwrap();
    let enchantment_refs: Vec<_> = enchantments.iter().map(String::as_str).collect();
    let potions: Vec<String> = serde_json::from_value(values["potions"].clone()).unwrap();
    let codec = ItemCodec::new(&names)
        .with_enchantments(&enchantment_refs)
        .with_potions(&potions);
    let original = read(&original_path);
    let inventory = PlayerInventory::read(original.clone(), &codec).unwrap();
    for index in 0..41 {
        let slot = match index {
            0..=8 => 36 + index,
            9..=35 => index,
            36..=39 => 44 - index,
            40 => 45,
            _ => unreachable!(),
        };
        assert_eq!(
            describe(&inventory.slots[slot]),
            values["slots"][index],
            "inventory slot {index}"
        );
    }
    let saved = inventory.to_nbt(&codec).unwrap();
    assert_eq!(
        saved, original,
        "Harpas preserves the native inventory and unrelated fields"
    );
    let harpas_path = reference.temp.0.join("harpas.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(saved).write_network(&mut bytes);
    fs::write(&harpas_path, bytes).unwrap();
    let round_trip_path = reference.temp.0.join("round-trip.nbt");
    reference.java(
        "PlayerInventoryProbe",
        [
            "read".to_owned(),
            harpas_path.display().to_string(),
            round_trip_path.display().to_string(),
        ],
    );
    let round_trip = read(&round_trip_path);
    for field in ["Inventory", "equipment", "SelectedItemSlot"] {
        assert_eq!(
            round_trip.get(field),
            original.get(field),
            "vanilla reopened {field}"
        );
    }
}
