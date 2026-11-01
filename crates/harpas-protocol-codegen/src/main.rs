//! `harpas-protocol-codegen`: tools for the protocol schema
//!
//! - `check <schema-version-dir> <protocol.json>` compares a schema with minecraft-data.
//! - `print <schema-dir>` prints the generated Rust code.

use std::path::Path;
use std::process::ExitCode;
use std::{env, fs};

use harpas_protocol_codegen::{generate_dir, mcdata, schema::Version};

const USAGE: &str = "usage:\n  harpas-protocol-codegen check <schema-version-dir> <protocol.json>\n  \
                     harpas-protocol-codegen print <schema-dir>";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check", schema, protocol] => check(Path::new(schema), Path::new(protocol)),
        ["print", schema] => generate_dir(Path::new(schema)).map(|code| print!("{code}")),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn check(schema: &Path, protocol: &Path) -> Result<(), String> {
    let version = Version::load(schema)?;
    let text = fs::read_to_string(protocol).map_err(|e| format!("{}: {e}", protocol.display()))?;
    let json = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", protocol.display()))?;
    let report = mcdata::check(&version, &json);
    for state in &report.missing_states {
        println!("note: the schema has no {state} state yet");
    }
    for error in &report.errors {
        println!("error: {error}");
    }
    if report.errors.is_empty() {
        println!("Java {} matches minecraft-data", version.minecraft);
        Ok(())
    } else {
        Err(format!(
            "{} differences with minecraft-data",
            report.errors.len()
        ))
    }
}
