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
    assert_eq!(report.missing_states, ["configuration", "play"]);
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

    let report = mcdata::check(&version, &minecraft_data());
    assert_eq!(
        report.errors,
        [
            "login clientbound LoginDisconnect: wire types are (option(string)), minecraft-data has (string)",
            "login serverbound LoginStart: ID 0x09, minecraft-data has 0x00",
            "login serverbound EncryptionResponse: wire types are (bytes), minecraft-data has (bytes, bytes)",
            "login serverbound: minecraft-data packet `cookie_response` (0x04) is not in the schema",
        ]
    );
}
