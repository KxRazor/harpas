//! `harpas.toml`, Harpas' own settings
//!
//! Every setting in it belongs to a feature, and arrives with that feature; this version has none yet. The file
//! is still read, so that a broken file stops the server instead of being ignored, and so that settings this
//! version does not know, or that belong in `server.properties`, are reported (`docs/operations.md`).

use std::fs;
use std::io;
use std::path::Path;

use crate::properties;

/// Harpas settings this version reads, as dotted paths.
const KNOWN: &[&str] = &[];

/// Reads `path` and returns a warning for every setting that is ignored. A missing file has no settings.
pub fn check(path: &Path) -> io::Result<Vec<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let table: toml::Table = text.parse().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not valid TOML: {e}", path.display()),
        )
    })?;
    let mut warnings = Vec::new();
    unknown(&table, "", &mut warnings);
    Ok(warnings)
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

    fn check_text(text: &str) -> io::Result<Vec<String>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "harpas-toml-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir)?;
        let path = dir.join("harpas.toml");
        fs::write(&path, text)?;
        let result = check(&path);
        let _ = fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn a_missing_file_has_no_settings() {
        assert!(
            check(Path::new("does-not-exist/harpas.toml"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reports_ignored_settings() {
        let warnings = check_text("max-players = 5\n[bedrock]\nport = 19132\n").unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("`bedrock`"));
        assert!(warnings[1].contains("`max-players`, which belongs in server.properties"));
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let error = check_text("eula = \n").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
