//! The world folder as Java 26.1 lays it out: `level.dat`, player data and dimension folders
//!
//! Since 26.1 every dimension, the overworld included, lives in `dimensions/<namespace>/<path>/` with its
//! `region/`, `entities/` and `poi/` folders, and player data lives in `players/data/<uuid>.dat`. `level.dat` and
//! player data are gzip NBT files saved with a backup (`crate::file`). Design: `docs/world.md`.

use std::io;
use std::path::{Path, PathBuf};

use harpas_core::nbt::{Compound, Nbt};

use crate::file::{self, Source};

/// The folder of the dimension `id`, such as `minecraft:overworld`, in the world folder `world`. An ID without
/// a namespace is in `minecraft`.
pub fn dimension_dir(world: &Path, id: &str) -> PathBuf {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    world.join("dimensions").join(namespace).join(path)
}

/// Reads the `Data` compound of `level.dat`, falling back to `level.dat_old`. `None` when the world has neither.
pub fn read_level(world: &Path) -> io::Result<Option<(Compound, Source)>> {
    let path = world.join("level.dat");
    let Some((mut root, source)) = file::read_with_backup(&path)? else {
        return Ok(None);
    };
    match root.remove("Data") {
        Some(Nbt::Compound(data)) => Ok(Some((data, source))),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} has no Data compound", path.display()),
        )),
    }
}

/// Saves `data` as the `Data` compound of `level.dat`, through `level.dat_new` and keeping the previous file as
/// `level.dat_old`, as vanilla does.
pub fn write_level(world: &Path, data: &Compound) -> io::Result<()> {
    let mut root = Compound::new();
    root.insert("Data", data.clone());
    file::write_with_backup(
        &world.join("level.dat"),
        &world.join("level.dat_new"),
        &root,
    )
}

/// `players/data/<uuid>.dat`, with the UUID in its hyphenated form.
fn player_path(world: &Path, uuid: u128) -> PathBuf {
    let hex = format!("{uuid:032x}");
    let name = format!(
        "{}-{}-{}-{}-{}.dat",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    world.join("players").join("data").join(name)
}

/// Reads a player's data, falling back to its `_old` backup. `None` for a player who has never played.
pub fn read_player(world: &Path, uuid: u128) -> io::Result<Option<(Compound, Source)>> {
    file::read_with_backup(&player_path(world, uuid))
}

/// Saves a player's data, keeping the previous file as `<uuid>.dat_old`, as vanilla does.
pub fn write_player(world: &Path, uuid: u128, data: &Compound) -> io::Result<()> {
    let path = player_path(world, uuid);
    file::write_with_backup(&path, &path.with_extension("dat.tmp"), data)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use flate2::Compression;
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;

    use super::*;

    /// A world folder that is removed when dropped.
    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "harpas-world-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn level(name: &str) -> Compound {
        let mut data = Compound::new();
        data.insert("DataVersion", 4786);
        data.insert("LevelName", name);
        data
    }

    fn files(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn dimensions_live_under_dimensions() {
        let world = Path::new("world");
        assert_eq!(
            dimension_dir(world, "minecraft:overworld"),
            Path::new("world/dimensions/minecraft/overworld")
        );
        assert_eq!(
            dimension_dir(world, "the_nether"),
            Path::new("world/dimensions/minecraft/the_nether")
        );
        assert_eq!(
            dimension_dir(world, "example:moon"),
            Path::new("world/dimensions/example/moon")
        );
    }

    #[test]
    fn level_dat_is_gzip_nbt_with_a_data_compound() {
        let folder = Folder::new();
        assert_eq!(read_level(&folder.0).unwrap(), None);
        write_level(&folder.0, &level("first")).unwrap();
        assert_eq!(files(&folder.0), ["level.dat"]);

        let bytes = fs::read(folder.0.join("level.dat")).unwrap();
        let mut nbt = Vec::new();
        GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut nbt)
            .unwrap();
        let (name, root) = Nbt::read_named(&mut nbt.as_slice()).unwrap();
        assert_eq!(name, "");
        let mut expected = Compound::new();
        expected.insert("Data", level("first"));
        assert_eq!(root, Nbt::Compound(expected));

        assert_eq!(
            read_level(&folder.0).unwrap(),
            Some((level("first"), Source::Current))
        );
    }

    #[test]
    fn saving_keeps_the_previous_file_as_a_backup() {
        let folder = Folder::new();
        write_level(&folder.0, &level("first")).unwrap();
        write_level(&folder.0, &level("second")).unwrap();
        assert_eq!(files(&folder.0), ["level.dat", "level.dat_old"]);
        assert_eq!(
            read_level(&folder.0).unwrap(),
            Some((level("second"), Source::Current))
        );

        // A damaged level.dat, or none at all after a crash between the renames, falls back to the backup.
        fs::write(folder.0.join("level.dat"), b"not gzip").unwrap();
        assert_eq!(
            read_level(&folder.0).unwrap(),
            Some((level("first"), Source::Backup))
        );
        fs::remove_file(folder.0.join("level.dat")).unwrap();
        assert_eq!(
            read_level(&folder.0).unwrap(),
            Some((level("first"), Source::Backup))
        );
    }

    #[test]
    fn damaged_files_without_a_backup_are_errors() {
        let folder = Folder::new();
        fs::write(folder.0.join("level.dat"), b"not gzip").unwrap();
        assert!(read_level(&folder.0).is_err());

        // Valid NBT without a Data compound.
        let mut nbt = Vec::new();
        Nbt::Compound(level("bare")).write_named("", &mut nbt);
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&nbt).unwrap();
        fs::write(folder.0.join("level.dat"), gzip.finish().unwrap()).unwrap();
        let error = read_level(&folder.0).unwrap_err();
        assert!(error.to_string().contains("no Data compound"));
    }

    #[test]
    fn player_data_is_named_by_hyphenated_uuid() {
        let folder = Folder::new();
        let uuid = 0x069a79f4_44e9_4726_a5be_fca90e38aaf5;
        assert_eq!(read_player(&folder.0, uuid).unwrap(), None);
        let mut data = Compound::new();
        data.insert("Health", 20.0f32);
        write_player(&folder.0, uuid, &data).unwrap();
        write_player(&folder.0, uuid, &data).unwrap();
        assert_eq!(
            files(&folder.0.join("players/data")),
            [
                "069a79f4-44e9-4726-a5be-fca90e38aaf5.dat",
                "069a79f4-44e9-4726-a5be-fca90e38aaf5.dat_old"
            ]
        );
        assert_eq!(
            read_player(&folder.0, uuid).unwrap(),
            Some((data, Source::Current))
        );
    }
}
