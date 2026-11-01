//! Generates Harpas' packet codecs from its protocol schema
//!
//! Each protocol crate keeps its schema in `schema/<version>/`, and its build script calls [`generate_dir`].
//! The `harpas-protocol-codegen check` command compares a schema with minecraft-data, the source it is built
//! from. Design: `docs/protocol-codecs.md`.

pub mod mcdata;
pub mod rust;
pub mod schema;

use std::fs;
use std::path::Path;

use schema::Version;

/// Loads every version under `dir`, one subdirectory each, in name order.
pub fn load_dir(dir: &Path) -> Result<Vec<Version>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut dirs = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    dirs.iter().map(|path| Version::load(path)).collect()
}

/// Generates the Rust codecs for every version under `dir`.
pub fn generate_dir(dir: &Path) -> Result<String, String> {
    Ok(rust::generate(&load_dir(dir)?))
}
