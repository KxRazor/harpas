//! Files written whole: synced writes, and gzip NBT files kept with a backup as vanilla keeps them
//!
//! Vanilla saves `level.dat` and player data by writing the new file next to the current one, then moving the
//! current one to `<name>_old` and the new one into place. A crash at any point leaves either the current
//! file or the backup, and reading falls back to the backup when the current file is missing or damaged.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use harpas_core::nbt::{Compound, Nbt, NbtError};

/// The most bytes a compressed chunk or NBT file may inflate to, and the most a `.mcc` file may hold: 256 MiB.
///
/// Vanilla has no such limit. A world may be a download, and a megabyte of zlib can inflate to a gigabyte; a
/// chunk that vanilla itself can still load is far smaller than this.
pub(crate) const MAX_INFLATED: usize = 256 * 1024 * 1024;

/// The most memory the NBT of a chunk or file may take once read: 1 GiB. Values take more memory than their
/// bytes, a list of empty compounds most of all (`harpas_core::nbt`).
const MAX_NBT_MEMORY: usize = 1024 * 1024 * 1024;

/// Reads `compressed` to its end, or fails once it gives more than `limit` bytes.
pub(crate) fn inflate(compressed: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    compressed.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("inflates to more than {limit} bytes"),
        ));
    }
    Ok(bytes)
}

/// Reads the NBT of a chunk or file within [`MAX_NBT_MEMORY`].
pub(crate) fn read_nbt(mut nbt: &[u8]) -> Result<Nbt, NbtError> {
    Nbt::read_named_within(&mut nbt, MAX_NBT_MEMORY).map(|(_, root)| root)
}

pub(crate) fn encode_nbt(root: &Compound) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    root.write_named_within("", &mut bytes, MAX_INFLATED, MAX_NBT_MEMORY)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(bytes)
}

pub(crate) fn encode_gzip(root: &Compound) -> io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&encode_nbt(root)?)?;
    encoder.finish()
}

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
    let mut file = create_staged(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

pub(crate) fn create_staged(path: &Path) -> io::Result<File> {
    // Unlink an interrupted staging file rather than opening it for truncation.
    remove_if_present(path)?;
    OpenOptions::new().write(true).create_new(true).open(path)
}

pub(crate) fn remove_if_present(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    metadata.is_symlink()
}

fn regular_file(path: &Path, metadata: &Metadata) -> io::Result<()> {
    if !metadata.is_file() || is_link(metadata) {
        return Err(invalid(path, "expected a regular file without links"));
    }
    Ok(())
}

pub(crate) fn check_regular_file(path: &Path) -> io::Result<()> {
    regular_file(path, &fs::symlink_metadata(path)?)
}

pub(crate) fn open_regular(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    check_regular_file(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    regular_file(path, &file.metadata()?)?;
    Ok(file)
}

pub(crate) fn check_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => Ok(()),
        Ok(_) => Err(invalid(path, "expected a directory without links")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Checks components owned by the world, below the root explicitly chosen by the operator.
pub(crate) fn checked_dir(world: &Path, relative: &Path) -> io::Result<PathBuf> {
    let mut dir = world.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(invalid(relative, "expected a relative world directory"));
        };
        dir.push(name);
        check_dir(&dir)?;
    }
    Ok(dir)
}

/// Persists directory entries on Unix. Other platforms only have process-interruption recovery.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Creates a directory tree, persisting each new entry before using it.
pub(crate) fn create_synced_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = dir.parent() {
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if !dir.try_exists()? {
            create_synced_dir(parent)?;
            fs::create_dir_all(dir)?;
        }
        // Also sync an existing entry: the previous attempt may have failed after mkdir.
        return sync_dir(parent);
    }
    fs::create_dir_all(dir)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Reads a gzip NBT file; a missing file is `None`.
fn read_gzip(path: &Path) -> io::Result<Option<Compound>> {
    let file = match open_regular(path, OpenOptions::new().read(true)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let nbt = inflate(GzDecoder::new(file), MAX_INFLATED).map_err(|error| match error.kind() {
        io::ErrorKind::InvalidInput | io::ErrorKind::UnexpectedEof => invalid(path, error),
        _ => contextual(path, error),
    })?;
    match read_nbt(&nbt) {
        Ok(Nbt::Compound(root)) => Ok(Some(root)),
        Ok(_) => Err(invalid(path, "the NBT root is not a compound")),
        Err(e) => Err(invalid(path, e)),
    }
}

fn read_document<T>(
    path: &Path,
    decode: &impl Fn(Compound) -> io::Result<T>,
) -> io::Result<Option<T>> {
    read_gzip(path)?
        .map(|root| decode(root).map_err(|error| contextual(path, error)))
        .transpose()
}

pub(crate) fn compound_field(mut root: Compound, key: &str) -> io::Result<Compound> {
    match root.remove(key) {
        Some(Nbt::Compound(data)) => Ok(data),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("no {key} compound"),
        )),
    }
}

/// Reads and validates the gzip NBT file, falling back to `<path>_old`.
pub(crate) fn read_with_backup<T>(
    path: &Path,
    decode: impl Fn(Compound) -> io::Result<T>,
) -> io::Result<Option<(T, Source)>> {
    let current = read_document(path, &decode);
    if let Ok(Some(root)) = current {
        return Ok(Some((root, Source::Current)));
    }
    match read_document(&with_suffix(path, "_old"), &decode) {
        Ok(Some(root)) => Ok(Some((root, Source::Backup))),
        Err(error)
            if matches!(current, Ok(None))
                || (current
                    .as_ref()
                    .is_err_and(|e| e.kind() == io::ErrorKind::InvalidData)
                    && error.kind() != io::ErrorKind::InvalidData) =>
        {
            Err(error)
        }
        _ => current.map(|_| None),
    }
}

/// Installs a synced replacement, rotating the primary only if it is readable.
pub(crate) fn write_with_backup<T>(
    path: &Path,
    new_path: &Path,
    root: &Compound,
    decode: impl Fn(Compound) -> io::Result<T>,
) -> io::Result<()> {
    let gzip = encode_gzip(root)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_synced_dir(parent)?;
    write_synced(new_path, &gzip)?;
    match read_document(path, &decode) {
        Ok(Some(_)) => {
            fs::rename(path, with_suffix(path, "_old"))?;
            sync_dir(parent)?;
        }
        Ok(None) => {}
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {}
        Err(error) => return Err(error),
    }
    fs::rename(new_path, path)?;
    sync_dir(parent)
}

fn invalid(path: &Path, error: impl ToString) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {}", path.display(), error.to_string()),
    )
}

fn contextual(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
