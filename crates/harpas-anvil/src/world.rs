//! The world folder as Java 26.1 lays it out: level and generation settings, player data and dimensions
//!
//! Since 26.1 every dimension, the overworld included, lives in `dimensions/<namespace>/<path>/` with its
//! `region/`, `entities/` and `poi/` folders, and player data lives in `players/data/<uuid>.dat`. `level.dat` and
//! player data are gzip NBT files saved with a backup (`crate::file`). Generation settings, including the seed,
//! live separately in `data/minecraft/world_gen_settings.dat`. Design: `docs/world.md`.

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

/// Returns the saved Overworld spawn block position, when present in a 26.1 `level.dat` Data compound.
pub fn overworld_spawn(data: &Compound) -> Option<[i32; 3]> {
    let Nbt::Compound(spawn) = data.get("spawn")? else {
        return None;
    };
    if spawn.get("dimension") != Some(&Nbt::from("minecraft:overworld")) {
        return None;
    }
    let Nbt::IntArray(pos) = spawn.get("pos")? else {
        return None;
    };
    pos.as_slice().try_into().ok()
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

/// Reads the root compound of `data/minecraft/world_gen_settings.dat`, including its `DataVersion` and
/// `data` compound. Returns `None` only when neither the file nor its backup exists.
pub fn read_world_gen_settings(world: &Path) -> io::Result<Option<(Compound, Source)>> {
    let path = world
        .join("data")
        .join("minecraft")
        .join("world_gen_settings.dat");
    let Some((root, source)) = file::read_with_backup(&path)? else {
        return Ok(None);
    };
    if !matches!(root.get("data"), Some(Nbt::Compound(_))) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} has no data compound", path.display()),
        ));
    }
    Ok(Some((root, source)))
}

/// Reads the saved world seed, if the world generation settings exist.
/// A present settings file without a long-valued seed is an error, not a request for a new seed.
pub fn read_world_seed(world: &Path) -> io::Result<Option<(i64, Source)>> {
    let Some((root, source)) = read_world_gen_settings(world)? else {
        return Ok(None);
    };
    Ok(Some((settings_seed(&root)?, source)))
}

fn settings_seed(root: &Compound) -> io::Result<i64> {
    let Some(Nbt::Compound(data)) = root.get("data") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "world generation settings have no data compound",
        ));
    };
    match data.get("seed") {
        Some(Nbt::Long(seed)) => Ok(*seed),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "world generation settings have no long-valued seed",
        )),
    }
}

/// Opens existing generation settings, or creates them for a new world using `new_seed` and `template`.
///
/// Existing settings always win over the proposed seed. The template must contain the complete `data`
/// compound, including the dimensions; its seed is replaced before the file is written. A world with a
/// `level.dat` or a region directory but no generation settings is rejected rather than assigned a new seed.
/// `None` as the source means settings were created; otherwise it identifies the file that was read.
pub fn open_or_create_world_gen_settings(
    world: &Path,
    template: &Compound,
    new_seed: i64,
) -> io::Result<(Compound, i64, Option<Source>)> {
    if let Some((root, source)) = read_world_gen_settings(world)? {
        let seed = settings_seed(&root)?;
        return Ok((root, seed, Some(source)));
    }
    if read_level(world)?.is_some()
        || world.join("dimensions").try_exists()?
        || world.join("region").try_exists()?
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "existing world has no world generation settings; refusing to replace its seed",
        ));
    }
    let mut root = template.clone();
    let Some(Nbt::Compound(mut data)) = root.remove("data") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "world generation template has no data compound",
        ));
    };
    if !matches!(data.get("dimensions"), Some(Nbt::Compound(_))) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "world generation template has no dimensions compound",
        ));
    }
    data.insert("seed", new_seed);
    root.insert("data", data);
    write_world_gen_settings(world, &root)?;
    Ok((root, new_seed, None))
}

/// Saves the complete root compound of `data/minecraft/world_gen_settings.dat` with a crash-safe backup.
pub fn write_world_gen_settings(world: &Path, root: &Compound) -> io::Result<()> {
    if !matches!(root.get("data"), Some(Nbt::Compound(_))) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "world generation settings have no data compound",
        ));
    }
    let path = world
        .join("data")
        .join("minecraft")
        .join("world_gen_settings.dat");
    file::write_with_backup(&path, &path.with_extension("dat_new"), root)
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
    fn reads_only_a_well_formed_overworld_spawn() {
        let mut spawn = Compound::new();
        spawn.insert("dimension", "minecraft:overworld");
        spawn.insert("pos", Nbt::IntArray(vec![-32, 71, 0]));
        let mut level = Compound::new();
        level.insert("spawn", spawn.clone());
        assert_eq!(overworld_spawn(&level), Some([-32, 71, 0]));
        spawn.insert("dimension", "minecraft:the_nether");
        level.insert("spawn", spawn);
        assert_eq!(overworld_spawn(&level), None);
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

    #[test]
    fn world_generation_settings_are_saved_separately_and_preserve_unknown_fields() {
        let folder = Folder::new();
        assert_eq!(read_world_gen_settings(&folder.0).unwrap(), None);
        assert_eq!(read_world_seed(&folder.0).unwrap(), None);
        let mut settings = Compound::new();
        settings.insert("seed", -42_i64);
        settings.insert("generate_structures", 1_i8);
        settings.insert("custom_setting", "kept");
        let mut root = Compound::new();
        root.insert("DataVersion", 4786);
        root.insert("data", settings);
        root.insert("future_root_field", 7_i32);

        write_world_gen_settings(&folder.0, &root).unwrap();
        assert_eq!(
            read_world_seed(&folder.0).unwrap(),
            Some((-42, Source::Current))
        );
        assert_eq!(
            read_world_gen_settings(&folder.0).unwrap(),
            Some((root.clone(), Source::Current))
        );
        write_world_gen_settings(&folder.0, &root).unwrap();
        let path = folder.0.join("data/minecraft/world_gen_settings.dat");
        fs::write(&path, b"invalid gzip").unwrap();
        assert_eq!(
            read_world_gen_settings(&folder.0).unwrap(),
            Some((root, Source::Backup))
        );
        assert_eq!(
            read_world_seed(&folder.0).unwrap(),
            Some((-42, Source::Backup))
        );
        assert!(write_world_gen_settings(&folder.0, &Compound::new()).is_err());
    }

    #[test]
    fn a_present_world_generation_file_must_contain_a_long_seed() {
        let folder = Folder::new();
        let mut settings = Compound::new();
        let mut data = Compound::new();
        data.insert("seed", 42_i32);
        settings.insert("data", data);
        write_world_gen_settings(&folder.0, &settings).unwrap();
        let error = read_world_seed(&folder.0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_missing_settings_file_with_a_damaged_backup_is_not_a_new_world() {
        let folder = Folder::new();
        let dir = folder.0.join("data/minecraft");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("world_gen_settings.dat_old"), b"not gzip").unwrap();
        assert!(read_world_gen_settings(&folder.0).is_err());
        assert!(read_world_seed(&folder.0).is_err());
    }

    #[test]
    fn initializing_generation_persists_the_first_seed_and_never_replaces_an_existing_world() {
        let folder = Folder::new();
        let mut data = Compound::new();
        data.insert("dimensions", Compound::new());
        data.insert("custom_field", "preserved");
        let mut template = Compound::new();
        template.insert("DataVersion", crate::DATA_VERSION);
        template.insert("data", data);

        let (created, seed, source) =
            open_or_create_world_gen_settings(&folder.0, &template, -123).unwrap();
        assert_eq!((seed, source), (-123, None));
        let (reopened, seed, source) =
            open_or_create_world_gen_settings(&folder.0, &Compound::new(), 999).unwrap();
        assert_eq!((seed, source), (-123, Some(Source::Current)));
        assert_eq!(created, reopened);
        let Some(Nbt::Compound(settings)) = reopened.get("data") else {
            panic!("settings should be retained");
        };
        assert_eq!(settings.get("custom_field"), Some(&Nbt::from("preserved")));

        let existing = Folder::new();
        write_level(&existing.0, &level("existing")).unwrap();
        assert!(open_or_create_world_gen_settings(&existing.0, &template, 999).is_err());
        assert_eq!(read_world_gen_settings(&existing.0).unwrap(), None);

        let orphaned = Folder::new();
        fs::create_dir_all(orphaned.0.join("dimensions")).unwrap();
        assert!(open_or_create_world_gen_settings(&orphaned.0, &template, 999).is_err());
    }
}
