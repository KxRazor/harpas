//! The global 26.1 saved data for maps, shared by all dimensions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use harpas_core::nbt::{Compound, Nbt};

use crate::{DATA_VERSION, file};

/// Reads map records and allocates IDs in `data/minecraft/maps`. One writer owns the index.
pub struct MapFiles {
    dir: PathBuf,
    last_id: Option<i32>,
}

impl MapFiles {
    /// Creates a handle to the world's maps without accessing the disk.
    pub fn new(world: &Path) -> Self {
        Self {
            dir: world.join("data/minecraft/maps"),
            last_id: None,
        }
    }

    /// Reads the `data` compound for an existing map.
    pub fn read(&self, id: i32) -> io::Result<Option<Compound>> {
        read_data(&self.dir.join(format!("{id}.dat")))
    }

    /// Reserves an ID and saves the new map before any item can refer to it.
    pub fn create(&mut self, data: Compound) -> io::Result<i32> {
        let index = self.dir.join("last_id.dat");
        let last = match self.last_id {
            Some(last) => last,
            None => {
                let index = read_data(&index)?;
                let last = match index.as_ref().and_then(|index| index.get("map")) {
                    Some(Nbt::Int(last)) if *last >= -1 => *last,
                    None => -1,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid map index",
                        ));
                    }
                };
                // A restored index or an interrupted save must never let us overwrite another map.
                match fs::read_dir(&self.dir) {
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
        write_data(&index, Compound::from_iter([("map", id)]))?;
        self.last_id = Some(id);
        write_data(&self.dir.join(format!("{id}.dat")), data)?;
        Ok(id)
    }
}

fn read_data(path: &Path) -> io::Result<Option<Compound>> {
    let Some((mut root, _)) = file::read_with_backup(path)? else {
        return Ok(None);
    };
    match root.remove("data") {
        Some(Nbt::Compound(data)) => Ok(Some(data)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "map file has no data compound",
        )),
    }
}

fn write_data(path: &Path, data: Compound) -> io::Result<()> {
    let mut root = Compound::from_iter([("DataVersion", DATA_VERSION)]);
    root.insert("data", data);
    file::write_with_backup(path, &path.with_extension("dat_new"), &root)
}
