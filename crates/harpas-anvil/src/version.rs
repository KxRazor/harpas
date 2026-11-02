//! Which Minecraft version saved a world or a chunk
//!
//! Every `level.dat` and every chunk records the data version of the game that saved it. Harpas opens only
//! worlds of its server version and that version's hotfixes, listed in [`SUPPORTED`]: an older world needs
//! vanilla's upgrade first, and any other holds data Harpas does not understand and could damage
//! (`docs/world.md#supported-world-versions`).
//!
//! Vanilla upgrades chunks only when it loads them, so a world opened once in a newer vanilla can still hold old
//! chunks; vanilla's `--forceUpgrade` upgrades every chunk at once. Chunks are checked one by one for that
//! reason.

use std::error::Error;
use std::fmt;

use harpas_core::nbt::{Compound, Nbt};

/// The Minecraft version whose worlds Harpas opens.
pub const MINECRAFT_VERSION: &str = "26.1";

/// The data version 26.1 saves, `world_version` in its server jar's `version.json`.
pub const DATA_VERSION: i32 = 4786;

/// The releases whose worlds Harpas opens, by data version: 26.1 and its hotfixes, which changed nothing in the
/// save format and share its protocol (775) and data pack format (101.1). Each data version is `world_version`
/// in the release's server jar.
pub const SUPPORTED: &[(i32, &str)] = &[(4786, "26.1"), (4788, "26.1.1"), (4790, "26.1.2")];

/// The supported releases and their data versions, for messages.
fn supported() -> String {
    let list = |items: Vec<String>| match items.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    };
    format!(
        "{} (data versions {})",
        list(SUPPORTED.iter().map(|(_, name)| name.to_string()).collect()),
        list(
            SUPPORTED
                .iter()
                .map(|(version, _)| version.to_string())
                .collect()
        )
    )
}

/// The newest supported release, whose vanilla server upgrades older worlds.
fn newest() -> &'static str {
    SUPPORTED.last().expect("a supported release").1
}

/// A world or chunk saved by another Minecraft version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionError {
    /// The data version that saved it; `None` before 1.9, which recorded none.
    pub data_version: Option<i32>,
    /// The name of the version that saved it, when `level.dat` records it.
    pub name: Option<String>,
    /// What was checked: a world or a chunk.
    what: &'static str,
}

impl VersionError {
    /// Whether it was saved after 26.1, by a version Harpas does not know, rather than by an older one.
    pub fn is_newer(&self) -> bool {
        self.data_version
            .is_some_and(|version| version > DATA_VERSION)
    }
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let saved_by = match (&self.name, self.data_version) {
            (Some(name), Some(version)) => format!("Minecraft {name} (data version {version})"),
            (None, Some(version)) => format!("data version {version}"),
            (_, None) => "a version before 1.9".to_owned(),
        };
        if self.is_newer() {
            write!(
                f,
                "this {} was saved by {saved_by}, which is not one of the versions Harpas opens: {}. Harpas does \
                 not open it, to avoid damaging data it does not understand; use a Harpas version for that \
                 Minecraft version, or a backup from one of those",
                self.what,
                supported()
            )
        } else {
            write!(
                f,
                "this {} was saved by {saved_by}, older than {MINECRAFT_VERSION} (data version {DATA_VERSION}). \
                 Upgrade it first with the vanilla {} server, which upgrades every chunk when started once with \
                 `java -jar server.jar --forceUpgrade nogui`",
                self.what,
                newest()
            )
        }
    }
}

impl Error for VersionError {}

/// Checks the data version of a world's `level.dat`, given its `Data` compound.
pub fn check_level(data: &Compound) -> Result<(), VersionError> {
    let name = match data.get("Version") {
        Some(Nbt::Compound(version)) => match version.get("Name") {
            Some(Nbt::String(name)) => Some(name.clone()),
            _ => None,
        },
        _ => None,
    };
    check(data, name, "world")
}

/// Checks the data version of a chunk's NBT.
pub fn check_chunk(chunk: &Compound) -> Result<(), VersionError> {
    check(chunk, None, "chunk")
}

fn check(nbt: &Compound, name: Option<String>, what: &'static str) -> Result<(), VersionError> {
    let data_version = match nbt.get("DataVersion") {
        Some(Nbt::Int(version)) => Some(*version),
        _ => None,
    };
    if SUPPORTED
        .iter()
        .any(|&(version, _)| data_version == Some(version))
    {
        return Ok(());
    }
    Err(VersionError {
        data_version,
        name,
        what,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(data_version: Option<i32>, name: Option<&str>) -> Compound {
        let mut data = Compound::new();
        if let Some(data_version) = data_version {
            data.insert("DataVersion", data_version);
        }
        if let Some(name) = name {
            let mut version = Compound::new();
            version.insert("Id", data_version.unwrap_or(0));
            version.insert("Name", name);
            data.insert("Version", version);
        }
        data
    }

    #[test]
    fn worlds_of_26_1_and_its_hotfixes_open() {
        assert_eq!(check_level(&level(Some(4786), Some("26.1"))), Ok(()));
        assert_eq!(check_level(&level(Some(4786), None)), Ok(()));
        assert_eq!(check_level(&level(Some(4788), Some("26.1.1"))), Ok(()));
        assert_eq!(check_level(&level(Some(4790), Some("26.1.2"))), Ok(()));
    }

    #[test]
    fn older_worlds_explain_how_to_upgrade() {
        let error = check_level(&level(Some(4440), Some("1.21.8"))).unwrap_err();
        assert!(!error.is_newer());
        let message = error.to_string();
        assert!(message.starts_with(
            "this world was saved by Minecraft 1.21.8 (data version 4440), older than 26.1"
        ));
        assert!(message.contains("vanilla 26.1.2 server"));
        assert!(message.contains("--forceUpgrade"));

        // Before 1.9 there was no data version at all.
        let error = check_level(&level(None, None)).unwrap_err();
        assert_eq!(error.data_version, None);
        assert!(!error.is_newer());
        assert!(error.to_string().contains("a version before 1.9"));
    }

    #[test]
    fn newer_worlds_are_left_alone() {
        let error = check_level(&level(Some(4900), Some("26.3"))).unwrap_err();
        assert!(error.is_newer());
        assert!(error.to_string().contains(
            "not one of the versions Harpas opens: 26.1, 26.1.1 and 26.1.2 (data versions 4786, 4788 and 4790)"
        ));
        assert!(error.to_string().contains("avoid damaging data"));
        // A data version between the hotfixes belongs to no release Harpas knows.
        assert!(
            check_level(&level(Some(4787), None))
                .unwrap_err()
                .is_newer()
        );
    }

    #[test]
    fn chunks_are_checked_on_their_own() {
        let mut chunk = Compound::new();
        chunk.insert("DataVersion", DATA_VERSION);
        assert_eq!(check_chunk(&chunk), Ok(()));
        chunk.insert("DataVersion", 3955);
        let error = check_chunk(&chunk).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("this chunk was saved by data version 3955")
        );
        // A data version of the wrong type counts as missing.
        chunk.insert("DataVersion", "4786");
        assert_eq!(check_chunk(&chunk).unwrap_err().data_version, None);
    }
}
