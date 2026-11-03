//! Files written whole: synced writes, and gzip NBT files kept with a backup as vanilla keeps them
//!
//! Vanilla saves `level.dat` and player data by writing the new file next to the current one, then moving the
//! current one to `<name>_old` and the new one into place. A crash at any point leaves either the current
//! file or the backup, and reading falls back to the backup when the current file is missing or damaged.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use harpas_core::nbt::{Compound, Nbt};

/// Which file a value was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The file itself.
    Current,
    /// Its backup, `<name>_old`, because the file was missing or damaged.
    Backup,
}

/// Writes `path` whole and syncs it to the disk.
pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Reads a gzip NBT file; a missing file is `None`.
fn read_gzip(path: &Path) -> io::Result<Option<Compound>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut nbt = Vec::new();
    GzDecoder::new(file).read_to_end(&mut nbt)?;
    match Nbt::read_named(&mut nbt.as_slice()) {
        Ok((_, Nbt::Compound(root))) => Ok(Some(root)),
        Ok(_) => Err(invalid(path, "the NBT root is not a compound")),
        Err(e) => Err(invalid(path, e)),
    }
}

/// Reads the gzip NBT file `path`, or its backup `<path>_old` when it is missing or cannot be read. An error is
/// returned only when neither can be read and the file exists.
pub(crate) fn read_with_backup(path: &Path) -> io::Result<Option<(Compound, Source)>> {
    let current = read_gzip(path);
    if let Ok(Some(root)) = current {
        return Ok(Some((root, Source::Current)));
    }
    match read_gzip(&with_suffix(path, "_old")) {
        Ok(Some(root)) => Ok(Some((root, Source::Backup))),
        _ => current.map(|_| None),
    }
}

/// Writes `root` as the gzip NBT file `path`: first as `new_path`, then the current file becomes `<path>_old`
/// and the new one takes its place.
pub(crate) fn write_with_backup(path: &Path, new_path: &Path, root: &Compound) -> io::Result<()> {
    let mut nbt = Vec::new();
    Nbt::Compound(root.clone()).write_named("", &mut nbt);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&nbt)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_synced(new_path, &encoder.finish()?)?;
    let backup = with_suffix(path, "_old");
    match fs::remove_file(&backup) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    match fs::rename(path, &backup) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    fs::rename(new_path, path)
}

fn invalid(path: &Path, error: impl ToString) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {}", path.display(), error.to_string()),
    )
}
