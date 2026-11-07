//! `harpas.toml`, Harpas' own settings
//!
//! Every setting in it belongs to a feature, and arrives with that feature; this version reads `eula`
//! (`crate::eula`) and `generation.features`. A broken file stops the server instead of being ignored, and settings this version does not
//! know, or that belong in `server.properties`, are reported (`docs/operations.md`).

use std::fs;
use std::io;
use std::path::Path;

use crate::properties;

/// Harpas settings this version reads, as dotted paths.
const KNOWN: &[&str] = &["eula", "generation.features"];

/// Reads `path`. A missing file has no settings.
pub fn load(path: &Path) -> io::Result<toml::Table> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(e) => return Err(e),
    };
    text.parse().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not valid TOML: {e}", path.display()),
        )
    })
}

/// Whether new chunks run the FEATURES stage, which places only ores so far (`docs/world.md`). Off by
/// default: a chunk saved after FEATURES never runs it again, in Harpas or vanilla.
pub fn generation_features(table: &toml::Table) -> io::Result<bool> {
    let Some(generation) = table.get("generation") else {
        return Ok(false);
    };
    let value = generation
        .as_table()
        .ok_or_else(|| invalid("`generation` must be a table"))?
        .get("features");
    match value {
        None => Ok(false),
        Some(toml::Value::Boolean(enabled)) => Ok(*enabled),
        Some(_) => Err(invalid("`generation.features` must be true or false")),
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("harpas.toml: {message}"),
    )
}

/// A warning for every setting in `table` that is ignored.
pub fn check(table: &toml::Table) -> Vec<String> {
    let mut warnings = Vec::new();
    unknown(table, "", &mut warnings);
    warnings
}

fn unknown(table: &toml::Table, prefix: &str, warnings: &mut Vec<String>) {
    for (key, value) in table {
        let path = format!("{prefix}{key}");
        if KNOWN.contains(&path.as_str()) {
            continue;
        }
        match value {
            toml::Value::Table(inner) if KNOWN.iter().any(|k| k.starts_with(&format!("{path}."))) => {
                unknown(inner, &format!("{path}."), warnings);
            }
            _ if prefix.is_empty() && properties::is_vanilla_key(key) => warnings.push(format!(
                "harpas.toml sets `{key}`, which belongs in server.properties; it is ignored"
            )),
            _ => warnings.push(format!(
                "harpas.toml sets `{path}`, which this version of Harpas does not use; it is ignored"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn load_text(text: &str) -> io::Result<toml::Table> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "harpas-toml-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir)?;
        let path = dir.join("harpas.toml");
        fs::write(&path, text)?;
        let result = load(&path);
        let _ = fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn a_missing_file_has_no_settings() {
        assert!(
            load(Path::new("does-not-exist/harpas.toml"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reports_ignored_settings() {
        let table = load_text("eula = true\nmax-players = 5\n[bedrock]\nport = 19132\n").unwrap();
        let warnings = check(&table);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("`bedrock`"));
        assert!(warnings[1].contains("`max-players`, which belongs in server.properties"));
    }

    #[test]
    fn generation_features_are_off_unless_enabled() {
        assert!(!generation_features(&load_text("eula = true\n").unwrap()).unwrap());
        let enabled = load_text("[generation]\nfeatures = true\n").unwrap();
        assert!(generation_features(&enabled).unwrap());
        assert!(check(&enabled).is_empty());
        let wrong = load_text("[generation]\nfeatures = \"yes\"\n").unwrap();
        assert_eq!(
            generation_features(&wrong).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(generation_features(&load_text("generation = 1\n").unwrap()).is_err());
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let error = load_text("eula = \n").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
