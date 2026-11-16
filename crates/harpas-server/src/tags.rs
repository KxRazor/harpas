//! Vanilla's tags, resolved at startup from the data pack and the locally generated built-in registries
//! (`docs/java-connection.md#tags`).

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use harpas_java::Tags;
use log::{info, warn};

use crate::block_entity_registry;

/// The tags sent to Java clients. Without the built-in registries only the synchronized registries' tags are
/// vanilla's; the others the client requires are sent empty, and the server says how to fix it.
pub(crate) fn load(server: &Path, pack: &Path) -> io::Result<Tags> {
    let builtin = match block_entity_registry::load_builtin(server) {
        Ok(Some(builtin)) => builtin,
        result => {
            if let Err(error) = result {
                warn!("Could not load the built-in registries: {error}");
            }
            warn!(
                "Block, item, fluid and entity tags are unavailable, so clients do not see water as water. Install \
                 Java 25 and run `harpas vanilla-data generate-registry`"
            );
            Default::default()
        }
    };
    let files = tag_files(pack)?;
    let (tags, errors) = Tags::resolve(
        files
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_slice())),
        &builtin,
    );
    for error in errors {
        warn!("Couldn't load {error}");
    }
    info!(
        "Loaded {} tags of {} registries",
        tags.registries()
            .iter()
            .map(|registry| registry.tags.len())
            .sum::<usize>(),
        tags.registries().len()
    );
    Ok(tags)
}

/// The files under `data/<namespace>/tags/` of the zipped pack, by path.
fn tag_files(pack: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
    let invalid = |error: zip::result::ZipError| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {error}", pack.display()),
        )
    };
    let mut zip = zip::ZipArchive::new(File::open(pack)?).map_err(invalid)?;
    let mut files = Vec::new();
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(invalid)?;
        let name = file.name().map_err(invalid)?.into_owned();
        let is_tag = name
            .strip_prefix("data/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(_, rest)| rest.starts_with("tags/") && rest.ends_with(".json"));
        if !is_tag || file.is_dir() {
            continue;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        files.push((name, contents));
    }
    Ok(files)
}
