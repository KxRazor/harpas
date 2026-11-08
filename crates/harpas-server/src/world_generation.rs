//! World seed and Overworld generator initialization.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkGenerator, ChunkSource};
use harpas_worldgen::{BiomeSourceDef, MultiNoiseParameterListDef, WorldPresetDef, WorldgenData};
use log::{info, warn};

use crate::properties::Properties;

/// Initializes a normal Overworld before its worker pool can create or save chunks. With `features`, new
/// chunks run the FEATURES stage before players receive them; otherwise they stay at `carvers`.
pub(crate) fn prepare(
    world: &Path,
    properties: &Properties,
    pack: &Path,
    features: bool,
) -> io::Result<Option<Arc<dyn ChunkSource>>> {
    let level = harpas_anvil::read_level(world)?;
    let saved = harpas_anvil::read_world_gen_settings(world)?;
    if level.is_some() && saved.is_none() {
        warn!(
            "{} has no generation settings; missing chunks will not be generated",
            world.display()
        );
        return Ok(None);
    }
    if let Some((level, _)) = &level
        && !has_only_vanilla_pack(level)
    {
        warn!(
            "{} uses unsupported data packs; missing chunks will not be generated",
            world.display()
        );
        return Ok(None);
    }
    if level.is_none() && saved.is_some() && world.join("dimensions").try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "world has chunks and generation settings but no level.dat; refusing to recreate world metadata",
        ));
    }
    let data = Arc::new(WorldgenData::from_zip_path(pack).map_err(io::Error::other)?);
    let templates = Arc::new(
        harpas_worldgen::StructureTemplates::from_zip_path(pack).map_err(io::Error::other)?,
    );
    let (root, seed) = if let Some((root, source)) = saved {
        if source == harpas_anvil::Source::Backup {
            warn!(
                "Using the backup world generation settings in {}",
                world.display()
            );
        }
        let seed = seed_from_root(&root)?;
        if !normal_overworld(&root) {
            warn!(
                "{} uses an unsupported Overworld generator; missing chunks will not be generated",
                world.display()
            );
            return Ok(None);
        }
        (root, seed)
    } else {
        if properties.string("initial-enabled-packs", "vanilla").trim() != "vanilla" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "world generation currently supports only the vanilla data pack",
            ));
        }
        let kind = properties.string("level-type", "minecraft:normal");
        if kind != "minecraft:normal" && kind != "normal" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("world generation currently supports minecraft:normal, not {kind}"),
            ));
        }
        let preset = data.world_presets.get("minecraft:normal").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the vanilla world preset is missing",
            )
        })?;
        let template = settings_template(preset, properties.parsed("generate-structures", true))?;
        if !normal_overworld(&template) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the normal world preset has an unsupported Overworld",
            ));
        }
        let proposed = match harpas_worldgen::parse_level_seed(&properties.string("level-seed", ""))
        {
            Some(seed) => seed,
            None => {
                let mut bytes = [0_u8; 8];
                getrandom::fill(&mut bytes).map_err(io::Error::other)?;
                i64::from_ne_bytes(bytes)
            }
        };
        // Validate the complete generator before creating persistent world files.
        harpas_engine::worldgen::overworld_generator(
            proposed,
            Arc::clone(&data),
            Arc::clone(&templates),
            structures_enabled(&template)?,
            harpas_java::biome_names(),
        )?;
        let (root, seed, _) =
            harpas_anvil::open_or_create_world_gen_settings(world, &template, proposed)?;
        (root, seed)
    };
    if !normal_overworld(&root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the normal world preset has an unsupported Overworld",
        ));
    }
    let generator = harpas_engine::worldgen::overworld_generator(
        seed,
        Arc::clone(&data),
        templates,
        structures_enabled(&root)?,
        harpas_java::biome_names(),
    )?;
    if level.is_none() {
        let spawn_y = spawn_height(Arc::clone(&generator))?;
        harpas_anvil::write_level(world, &new_level(properties, spawn_y))?;
    }
    let source = if features {
        info!(
            "New chunks run the FEATURES stage, which places only ores and simple vegetation so far"
        );
        harpas_engine::worldgen::decorated(generator, seed, &data)?
    } else {
        harpas_engine::world::generator_source(generator)
    };
    info!("Prepared Overworld terrain generation for seed {seed}");
    Ok(Some(source))
}

fn structures_enabled(root: &Compound) -> io::Result<bool> {
    let Some(Nbt::Compound(data)) = root.get("data") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "world generation settings have no data",
        ));
    };
    match data.get("generate_structures") {
        Some(Nbt::Byte(value)) => Ok(*value != 0),
        None => Ok(true),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid generate_structures setting",
        )),
    }
}

fn seed_from_root(root: &Compound) -> io::Result<i64> {
    let version = match root.get("DataVersion") {
        Some(Nbt::Int(version)) => *version,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "world generation settings have no data version",
            ));
        }
    };
    if !harpas_anvil::SUPPORTED
        .iter()
        .any(|(supported, _)| *supported == version)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported world generation settings data version {version}"),
        ));
    }
    match root.get("data") {
        Some(Nbt::Compound(data)) => match data.get("seed") {
            Some(Nbt::Long(seed)) => Ok(*seed),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "world generation settings have no long seed",
            )),
        },
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "world generation settings have no data compound",
        )),
    }
}

fn has_only_vanilla_pack(level: &Compound) -> bool {
    let Some(Nbt::Compound(packs)) = level.get("DataPacks") else {
        return false;
    };
    match packs.get("Enabled") {
        Some(Nbt::List(List::String(enabled))) => enabled.as_slice() == ["vanilla"],
        _ => false,
    }
}

fn normal_overworld(root: &Compound) -> bool {
    let Some(Nbt::Compound(data)) = root.get("data") else {
        return false;
    };
    let Some(Nbt::Compound(dimensions)) = data.get("dimensions") else {
        return false;
    };
    let Some(Nbt::Compound(overworld)) = dimensions.get("minecraft:overworld") else {
        return false;
    };
    let Some(Nbt::Compound(generator)) = overworld.get("generator") else {
        return false;
    };
    let Some(Nbt::Compound(biome)) = generator.get("biome_source") else {
        return false;
    };
    overworld.get("type") == Some(&Nbt::from("minecraft:overworld"))
        && generator.get("type") == Some(&Nbt::from("minecraft:noise"))
        && generator.get("settings") == Some(&Nbt::from("minecraft:overworld"))
        && biome.get("type") == Some(&Nbt::from("minecraft:multi_noise"))
        && biome.get("preset") == Some(&Nbt::from("minecraft:overworld"))
}

fn settings_template(preset: &WorldPresetDef, structures: bool) -> io::Result<Compound> {
    let mut dimensions = Compound::new();
    for (name, def) in &preset.dimensions {
        let mut dimension = Compound::new();
        dimension.insert("type", def.dimension_type.as_str());
        let mut generator = Compound::new();
        generator.insert("type", def.generator_type.as_str());
        let settings = def.settings.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "preset requires unsupported generator settings",
            )
        })?;
        generator.insert("settings", settings);
        let mut source = Compound::new();
        match def.biome_source.as_ref() {
            Some(BiomeSourceDef::MultiNoise(MultiNoiseParameterListDef::Preset(preset))) => {
                source.insert("type", "minecraft:multi_noise");
                source.insert("preset", preset.as_str());
            }
            Some(BiomeSourceDef::TheEnd) => {
                source.insert("type", "minecraft:the_end");
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "preset requires an unsupported biome source",
                ));
            }
        }
        generator.insert("biome_source", source);
        dimension.insert("generator", generator);
        dimensions.insert(name.as_str(), dimension);
    }
    let mut data = Compound::new();
    data.insert("seed", 0_i64);
    data.insert("bonus_chest", 0_i8);
    data.insert("generate_structures", i8::from(structures));
    data.insert("dimensions", dimensions);
    let mut root = Compound::new();
    root.insert("DataVersion", harpas_anvil::DATA_VERSION);
    root.insert("data", data);
    Ok(root)
}

fn spawn_height(generator: Arc<ChunkGenerator>) -> io::Result<i32> {
    let chunk = std::thread::Builder::new()
        .name("harpas-spawn-generation".into())
        .spawn(move || generator(harpas_engine::world::ChunkPos { x: 0, z: 0 }, &|_| Ok(None)))?
        .join()
        .map_err(|_| io::Error::other("spawn generation worker panicked"))??;
    let Some(Nbt::Int(section_y)) = chunk.rest.get("yPos") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generated spawn chunk has no minimum Y",
        ));
    };
    let Some(Nbt::Compound(maps)) = chunk.rest.get("Heightmaps") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generated spawn chunk has no heightmaps",
        ));
    };
    let Some(Nbt::LongArray(values)) = maps.get("MOTION_BLOCKING") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generated spawn chunk has no motion heightmap",
        ));
    };
    let height = chunk.sections.len() * 16;
    let bits = (usize::BITS - height.leading_zeros()) as usize;
    let first = values
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty spawn heightmap"))?;
    let offset = (*first as u64 & ((1_u64 << bits) - 1)) as usize;
    if offset == 0 || offset > height {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "spawn height is outside the generated chunk",
        ));
    }
    Ok(section_y * 16 + offset as i32)
}

fn new_level(properties: &Properties, spawn_y: i32) -> Compound {
    let mut version = Compound::new();
    version.insert("Name", harpas_anvil::MINECRAFT_VERSION);
    version.insert("Id", harpas_anvil::DATA_VERSION);
    version.insert("Series", "main");
    version.insert("Snapshot", 0_i8);
    let mut difficulty = Compound::new();
    difficulty.insert("difficulty", properties.string("difficulty", "easy"));
    difficulty.insert("hardcore", i8::from(properties.parsed("hardcore", false)));
    difficulty.insert("locked", 0_i8);
    let mut spawn = Compound::new();
    spawn.insert("dimension", "minecraft:overworld");
    spawn.insert("pos", Nbt::IntArray(vec![0, spawn_y, 0]));
    spawn.insert("yaw", 0_f32);
    spawn.insert("pitch", 0_f32);
    let mut packs = Compound::new();
    packs.insert("Enabled", Nbt::List(List::String(vec!["vanilla".into()])));
    packs.insert("Disabled", Nbt::List(List::Empty));
    let game_mode = match properties.string("gamemode", "survival").as_str() {
        "creative" => 1,
        "adventure" => 2,
        "spectator" => 3,
        _ => 0,
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0);
    let mut level = Compound::new();
    level.insert("DataVersion", harpas_anvil::DATA_VERSION);
    level.insert("Version", version);
    level.insert("version", 19133_i32);
    level.insert("LevelName", properties.string("level-name", "world"));
    level.insert("difficulty_settings", difficulty);
    level.insert("spawn", spawn);
    level.insert("DataPacks", packs);
    level.insert("Time", 0_i64);
    level.insert("GameType", game_mode);
    level.insert("LastPlayed", timestamp);
    level.insert("initialized", 1_i8);
    level.insert("allowCommands", 0_i8);
    level.insert("WasModded", 1_i8);
    level.insert(
        "ServerBrands",
        Nbt::List(List::String(vec!["harpas".into()])),
    );
    level
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Folder(PathBuf);
    impl Folder {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "harpas-worldgen-setup-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saved_structure_setting_controls_generation() {
        let mut data = Compound::new();
        data.insert("generate_structures", 0_i8);
        let mut root = Compound::new();
        root.insert("data", data.clone());
        assert!(!structures_enabled(&root).unwrap());
        data.insert("generate_structures", 1_i8);
        root.insert("data", data.clone());
        assert!(structures_enabled(&root).unwrap());
        data.insert("generate_structures", "false");
        root.insert("data", data);
        assert!(structures_enabled(&root).is_err());
    }

    #[test]
    fn worlds_with_no_saved_generation_settings_are_opened_without_reseeding() {
        let folder = Folder::new();
        let mut level = Compound::new();
        level.insert("DataVersion", harpas_anvil::DATA_VERSION);
        harpas_anvil::write_level(&folder.0, &level).unwrap();
        let props = Properties::parse("level-seed=123\n");
        assert!(
            prepare(&folder.0, &props, Path::new("missing.zip"), false)
                .unwrap()
                .is_none()
        );
        assert!(
            harpas_anvil::read_world_gen_settings(&folder.0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn custom_data_packs_do_not_generate_with_vanilla_only_settings() {
        let folder = Folder::new();
        let mut packs = Compound::new();
        packs.insert(
            "Enabled",
            Nbt::List(List::String(vec!["vanilla".into(), "file/custom".into()])),
        );
        let mut level = Compound::new();
        level.insert("DataVersion", harpas_anvil::DATA_VERSION);
        level.insert("DataPacks", packs);
        harpas_anvil::write_level(&folder.0, &level).unwrap();
        let mut data = Compound::new();
        data.insert("seed", 17_i64);
        let mut root = Compound::new();
        root.insert("DataVersion", harpas_anvil::DATA_VERSION);
        root.insert("data", data);
        harpas_anvil::write_world_gen_settings(&folder.0, &root).unwrap();
        let props = Properties::parse("level-seed=99\n");
        assert!(
            prepare(&folder.0, &props, Path::new("missing.zip"), false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            harpas_anvil::read_world_seed(&folder.0).unwrap().unwrap().0,
            17
        );
    }
}
