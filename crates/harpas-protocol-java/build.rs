//! Generates the codecs from `schema/` into `$OUT_DIR/codecs.rs`.

use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema");
    println!("cargo::rerun-if-changed={}", schema.display());
    let code = harpas_protocol_codegen::generate_dir(&schema)
        .unwrap_or_else(|e| panic!("invalid schema: {e}"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    fs::write(out.join("codecs.rs"), code).expect("failed to write the generated codecs");
}
