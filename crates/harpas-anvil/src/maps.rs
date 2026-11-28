//! The global 26.1 saved data for maps, shared by all dimensions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use harpas_core::nbt::{Compound, Nbt};

use crate::{DATA_VERSION, file};

/// Reads map records and allocates IDs in `data/minecraft/maps`. One writer owns the index.
pub struct MapFiles {
    world: PathBuf,
    last_id: Option<i32>,
}

impl MapFiles {
    /// Creates a handle to the world's maps without accessing the disk.
    pub fn new(world: &Path) -> Self {
        Self {
            world: world.to_owned(),
            last_id: None,
        }
    }

    /// Reads the `data` compound for an existing map.
    pub fn read(&self, id: i32) -> io::Result<Option<Compound>> {
        let path = self.directory()?.join(format!("{id}.dat"));
        Ok(file::read_with_backup(&path, map_data)?.map(|(data, _)| data))
    }

    /// Reserves an ID and saves the new map before any item can refer to it.
    pub fn create(&mut self, data: Compound) -> io::Result<i32> {
        let dir = self.directory()?;
        let index = dir.join("last_id.dat");
        let last = match self.last_id {
            Some(last) => last,
            None => {
                let last = file::read_with_backup(&index, map_index)?.map_or(-1, |(id, _)| id);
                // A restored index or an interrupted save must never let us overwrite another map.
                match fs::read_dir(&dir) {
                    Ok(entries) => {
                        let mut highest = last;
                        for entry in entries {
                            let entry = entry?;
                            let name = entry.file_name();
                            if let Some(id) = name.to_str().and_then(|name| {
                                name.strip_suffix(".dat")
                                    .or_else(|| name.strip_suffix(".dat_old"))?
                                    .parse::<i32>()
                                    .ok()
                            }) {
                                highest = highest.max(id);
                            }
                        }
                        highest
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => last,
                    Err(error) => return Err(error),
                }
            }
        };
        let id = last
            .checked_add(1)
            .ok_or_else(|| io::Error::other("map IDs exhausted"))?;
        write_data(&index, Compound::from_iter([("map", id)]), map_index)?;
        self.last_id = Some(id);
        write_data(&dir.join(format!("{id}.dat")), data, map_data)?;
        Ok(id)
    }

    fn directory(&self) -> io::Result<PathBuf> {
        file::checked_dir(&self.world, Path::new("data/minecraft/maps"))
    }
}

fn map_data(root: Compound) -> io::Result<Compound> {
    file::compound_field(root, "data")
}

fn map_index(root: Compound) -> io::Result<i32> {
    match map_data(root)?.get("map") {
        Some(Nbt::Int(last)) if *last >= -1 => Ok(*last),
        None => Ok(-1),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid map index",
        )),
    }
}

fn write_data<T>(
    path: &Path,
    data: Compound,
    decode: impl Fn(Compound) -> io::Result<T>,
) -> io::Result<()> {
    let mut root = Compound::from_iter([("DataVersion", DATA_VERSION)]);
    root.insert("data", data);
    file::write_with_backup(path, &path.with_extension("dat_new"), &root, decode)
}
