//! The Java schema against the minecraft-data subset in `tests/data`.

use std::path::{Path, PathBuf};

use harpas_protocol_codegen::mcdata;
use harpas_protocol_codegen::schema::{Type, Version};
use serde_json::Value;

fn java_schema() -> Version {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../harpas-protocol-java/schema/26.1");
    Version::load(&dir).unwrap()
}

fn minecraft_data() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/minecraft-data-26.1.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn java_26_1_matches_minecraft_data() {
    let report = mcdata::check(&java_schema(), &minecraft_data());
    assert_eq!(report.errors, Vec::<String>::new());
    assert_eq!(report.missing_states, Vec::<String>::new());
    assert_eq!(report.pending.len(), 21);
    assert_eq!(report.unchecked, ["play clientbound PlayerInfoUpdate"]);
}

#[test]
fn play_differences_are_reported() {
    let mut version = java_schema();
    let play = version
        .states
        .iter_mut()
        .find(|state| state.name == "play")
        .unwrap();
    play.bitfields.get_mut("BlockPos").unwrap().fields[2].signed = false;
    play.flags
        .get_mut("TeleportFlags")
        .unwrap()
        .bits
        .remove("rotate_velocity");
    play.unions
        .get_mut("FilterMask")
        .unwrap()
        .variants
        .retain(|variant| variant.name != "PartiallyFiltered");
    play.partial = false;

    let report = mcdata::check(&version, &minecraft_data());
    // A state that is no longer partial reports its missing packets as errors.
    let (missing, wire): (Vec<_>, Vec<_>) = report
        .errors
        .iter()
        .partition(|error| error.ends_with("is not in the schema"));
    assert_eq!(missing.len(), 21);
    assert!(report.pending.is_empty());
    let packets: Vec<_> = wire
        .iter()
        .map(|error| error.split(':').next().unwrap())
        .collect();
    assert_eq!(
        packets,
        [
            "play clientbound SetBlockDestroyStage",
            "play clientbound BlockEntityData",
            "play clientbound BlockAction",
            "play clientbound BlockUpdate",
            "play clientbound GameTestHighlightPosition",
            "play clientbound WorldEvent",
            "play clientbound Login",
            "play clientbound OpenSignEditor",
            "play clientbound PlayerChatMessage",
            "play clientbound SynchronizePlayerPosition",
            "play clientbound Respawn",
            "play clientbound SetDefaultSpawnPosition",
            "play clientbound TeleportEntity",
            "play serverbound QueryBlockEntityTag",
            "play serverbound JigsawGenerate",
            "play serverbound PickItemFromBlock",
            "play serverbound PlayerAction",
            "play serverbound ProgramCommandBlock",
            "play serverbound ProgramJigsawBlock",
            "play serverbound ProgramStructureBlock",
            "play serverbound SetTestBlock",
            "play serverbound UpdateSign",
            "play serverbound TestInstanceBlockAction",
            "play serverbound UseItemOn",
        ]
    );
}

#[test]
fn differences_are_reported() {
    let mut version = java_schema();
    let login = version
        .states
        .iter_mut()
        .find(|state| state.name == "login")
        .unwrap();
    login.serverbound[0].id = 0x09;
    login.serverbound[1].fields.pop();
    login.clientbound[0].fields[0].ty = Type::parse("option(string)").unwrap();
    login
        .serverbound
        .retain(|packet| packet.mcdata != "cookie_response");
    let configuration = version
        .states
        .iter_mut()
        .find(|state| state.name == "configuration")
        .unwrap();
    configuration
        .enums
        .get_mut("ServerLinkType")
        .unwrap()
        .values
        .remove("Announcements");
    let label = configuration.unions.get_mut("ServerLinkLabel").unwrap();
    label.variants.swap(0, 1);
    for (variant, tag) in label.variants.iter_mut().zip([1, 0]) {
        variant.tag = Some(tag);
    }

    let report = mcdata::check(&version, &minecraft_data());
    assert_eq!(
        report.errors,
        [
            "login clientbound LoginDisconnect: wire types are (option(string)), minecraft-data has (string)",
            "login serverbound LoginStart: ID 0x09, minecraft-data has 0x00",
            "login serverbound EncryptionResponse: wire types are (bytes), minecraft-data has (bytes, bytes)",
            "login serverbound: minecraft-data packet `cookie_response` (0x04) is not in the schema",
            "configuration clientbound ServerLinks: wire types are (array((switch bool { 0: (varint[0, 1, 2, 3, 4, 5, 6, 7, 8]), 1: (nbt) }, string))), \
             minecraft-data has (array((switch bool { 0: (nbt), 1: (varint[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]) }, string)))",
        ]
    );
}
