//! The global 26.1 saved data for maps, shared by all dimensions.

use std::fs;
use std::io::{self, Write};
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
        let id = self.next_id()?;
        self.create_batch(id, &[data])?;
        Ok(id)
    }

    /// Looks up the next ID without reserving it or changing the world.
    pub fn next_id(&self) -> io::Result<i32> {
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
        last.checked_add(1)
            .ok_or_else(|| io::Error::other("map IDs exhausted"))
    }

    /// Saves consecutive maps for one interaction, starting at the expected next ID.
    pub fn create_batch(&mut self, first: i32, data: &[Compound]) -> io::Result<()> {
        let Some(last_offset) = data.len().checked_sub(1) else {
            return Ok(());
        };
        if self.next_id()? != first {
            return Err(io::Error::other("map index changed before publication"));
        }
        let last = i32::try_from(last_offset)
            .ok()
            .and_then(|offset| first.checked_add(offset))
            .ok_or_else(|| io::Error::other("map IDs exhausted"))?;
        let dir = self.directory()?;
        let maps = data
            .iter()
            .map(|data| file::encode_gzip(&document(data.clone())))
            .collect::<io::Result<Vec<_>>>()?;
        file::create_synced_dir(&dir)?;
        let mut staged = Vec::new();
        let mut published = Vec::new();
        let result = (|| {
            for (id, gzip) in (first..=last).zip(maps) {
                let path = dir.join(format!("{id}.dat"));
                let temp = path.with_extension("dat_new");
                let mut file = file::create_staged(&temp)?;
                staged.push((temp, path));
                file.write_all(&gzip)?;
                file.sync_all()?;
            }
            let index = dir.join("last_id.dat");
            let root = document(Compound::from_iter([("map", last)]));
            file::write_with_backup(&index, &index.with_extension("dat_new"), &root, map_index)?;
            self.last_id = Some(last);
            for (temp, path) in &staged {
                match fs::symlink_metadata(path) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("{} already exists", path.display()),
                        ));
                    }
                }
                fs::rename(temp, path)?;
                published.push(path.clone());
            }
            file::sync_dir(&dir)
        })();
        if let Err(error) = result {
            // A saved index may keep reserved IDs, but a failed interaction has no new map files.
            let cleanup = published
                .iter()
                .chain(staged.iter().map(|(temp, _)| temp))
                .try_for_each(|path| file::remove_if_present(path).map(|_| ()))
                .and_then(|()| file::sync_dir(&dir));
            if let Err(cleanup) = cleanup {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{error}; cleaning up map files: {cleanup}"),
                ));
            }
            return Err(error);
        }
        Ok(())
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

fn document(data: Compound) -> Compound {
    let mut root = Compound::from_iter([("DataVersion", DATA_VERSION)]);
    root.insert("data", data);
    root
}
