//! Region saves as one transaction with their external `.mcc` files
//!
//! A region and the `.mcc` files of its large chunks are separate files, and no file system renames several
//! files at once. A save therefore goes through a redo journal, `r.x.z.mca.journal`:
//!
//! 1. Every new file is written whole and synced under a staged name, `<name>.tmp`.
//! 2. The journal, listing the files to install and the `.mcc` files to remove, is written and synced as
//!    `r.x.z.mca.journal.tmp`, then renamed into place. Once it exists, the save is committed.
//! 3. The staged files are renamed over the current ones, the stale `.mcc` files removed and the journal
//!    removed, the folder being synced between steps so they reach the disk in this order.
//!
//! An interruption before the commit leaves the previous files untouched, with staged files that are ignored
//! and overwritten by the next save or removed by [`recover`]. An interruption after it is completed the next
//! time the region is opened or [`recover`] runs: installing a file whose staged copy is gone is skipped, so
//! completing a journal twice is harmless. Design: `docs/world.md#saving`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::file::write_synced;
use crate::region::region_of;

const HEADER: &str = "harpas region journal 1";
const END: &str = "end";
const STAGED: &str = ".tmp";
const JOURNAL: &str = ".journal";

/// What an interrupted save left in a region folder, and what [`recover`] did with it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    /// Regions whose committed save was completed.
    pub completed: Vec<PathBuf>,
    /// Staged files of saves interrupted before their commit, removed.
    pub discarded: Vec<PathBuf>,
}

/// Completes the committed saves in region folder `dir` and removes what uncommitted saves staged. Run it before
/// reading a folder whose last save may have been interrupted, with no save running on it; a missing folder has
/// nothing to recover.
pub fn recover(dir: &Path) -> io::Result<Recovery> {
    let mut recovery = Recovery::default();
    let mut names = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<Vec<_>>>()?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(recovery),
        Err(e) => return Err(e),
    };
    names.sort();
    for name in &names {
        let Some(region) = name.strip_suffix(JOURNAL) else {
            continue;
        };
        let Some((x, z)) = parse_region(region) else {
            continue;
        };
        if complete(dir, x, z)? {
            recovery.completed.push(dir.join(region));
        }
    }
    for name in &names {
        let Some(staged) = name.strip_suffix(STAGED) else {
            continue;
        };
        let ours = parse_region(staged).is_some()
            || parse_external(staged).is_some()
            || staged
                .strip_suffix(JOURNAL)
                .and_then(parse_region)
                .is_some();
        let path = dir.join(name);
        if ours && remove_if_present(&path)? {
            recovery.discarded.push(path);
        }
    }
    if !recovery.discarded.is_empty() {
        sync_dir(dir)?;
    }
    Ok(recovery)
}

/// The files one region save installs and removes, by name in the region's folder.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Journal {
    install: Vec<String>,
    remove: Vec<String>,
}

impl Journal {
    /// Writes `bytes` synced as the staged copy of `name`, to be installed when the journal is applied.
    pub(crate) fn stage(&mut self, dir: &Path, name: String, bytes: &[u8]) -> io::Result<()> {
        write_synced(&dir.join(staged(&name)), bytes)?;
        self.install.push(name);
        Ok(())
    }

    /// Removes `name` when the journal is applied.
    pub(crate) fn remove(&mut self, name: String) {
        self.remove.push(name);
    }

    fn encode(&self) -> String {
        let mut text = format!("{HEADER}\n");
        for name in &self.install {
            text += &format!("install {name}\n");
        }
        for name in &self.remove {
            text += &format!("remove {name}\n");
        }
        text + END + "\n"
    }

    /// Reads the journal of region `x`, `z`, refusing anything but its own files.
    fn decode(text: &str, x: i32, z: i32) -> Result<Self, String> {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err("unknown journal format".to_owned());
        }
        let mut journal = Self::default();
        for line in lines.by_ref() {
            if line == END {
                break;
            }
            let (action, name) = line
                .split_once(' ')
                .ok_or_else(|| format!("unreadable journal line {line:?}"))?;
            let ours = parse_region(name) == Some((x, z))
                || parse_external(name).map(|(cx, cz)| region_of(cx, cz)) == Some((x, z));
            if !ours {
                return Err(format!("the journal names another region's file, {name}"));
            }
            match action {
                "install" => journal.install.push(name.to_owned()),
                "remove" => journal.remove.push(name.to_owned()),
                _ => return Err(format!("unreadable journal line {line:?}")),
            }
        }
        if lines.next().is_some() || !text.ends_with(&format!("\n{END}\n")) {
            return Err("incomplete journal".to_owned());
        }
        Ok(journal)
    }

    /// Commits the save of region `x`, `z`, then installs its files. `step` runs before each change to the
    /// disk; an error from it or from the disk stops the save where it is, as a crash would.
    pub(crate) fn commit(
        &self,
        dir: &Path,
        x: i32,
        z: i32,
        step: &mut dyn FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        let journal = journal_path(dir, x, z);
        let staged_journal = staged_path(&journal);
        step()?;
        write_synced(&staged_journal, self.encode().as_bytes())?;
        // The staged files and the journal reach the disk before the journal can name them.
        step()?;
        sync_dir(dir)?;
        step()?;
        fs::rename(&staged_journal, &journal)?;
        step()?;
        sync_dir(dir)?;
        self.apply(dir, &journal, step)
    }

    fn apply(
        &self,
        dir: &Path,
        journal: &Path,
        step: &mut dyn FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        for name in &self.install {
            step()?;
            let path = dir.join(name);
            match fs::rename(staged_path(&path), &path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound && path.is_file() => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "{}: the journal installs {name}, which is missing",
                            journal.display()
                        ),
                    ));
                }
                result => result?,
            }
        }
        for name in &self.remove {
            step()?;
            remove_if_present(&dir.join(name))?;
        }
        // The installed files reach the disk before the journal that would install them again goes, and the
        // journal is gone for good before the next save stages files under the same names.
        step()?;
        sync_dir(dir)?;
        step()?;
        fs::remove_file(journal)?;
        step()?;
        sync_dir(dir)
    }
}

/// Whether a save of region `x`, `z` in `dir` was committed and not completed.
pub(crate) fn pending(dir: &Path, x: i32, z: i32) -> io::Result<bool> {
    journal_path(dir, x, z).try_exists()
}

/// Completes the committed save of region `x`, `z` in `dir`, if there is one, and says whether there was.
pub(crate) fn complete(dir: &Path, x: i32, z: i32) -> io::Result<bool> {
    let path = journal_path(dir, x, z);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let journal = Journal::decode(&text, x, z).map_err(|reason| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {reason}", path.display()),
        )
    })?;
    journal.apply(dir, &path, &mut || Ok(()))?;
    Ok(true)
}

/// `r.x.z.mca.journal` in `dir`.
fn journal_path(dir: &Path, x: i32, z: i32) -> PathBuf {
    dir.join(format!("r.{x}.{z}.mca{JOURNAL}"))
}

fn staged(name: &str) -> String {
    format!("{name}{STAGED}")
}

fn staged_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(STAGED);
    PathBuf::from(name)
}

/// The coordinates in a region file name, `r.x.z.mca`.
fn parse_region(name: &str) -> Option<(i32, i32)> {
    parse_coordinates(name.strip_prefix("r.")?.strip_suffix(".mca")?)
}

/// The chunk coordinates in an external chunk file name, `c.x.z.mcc`.
fn parse_external(name: &str) -> Option<(i32, i32)> {
    parse_coordinates(name.strip_prefix("c.")?.strip_suffix(".mcc")?)
}

fn parse_coordinates(text: &str) -> Option<(i32, i32)> {
    let (x, z) = text.split_once('.')?;
    let number = |text: &str| {
        let digits = text.strip_prefix('-').unwrap_or(text);
        // Only the canonical form, so no two names stand for the same file.
        let canonical = !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && (digits == "0" || !digits.starts_with('0'))
            && text != "-0";
        canonical.then(|| text.parse().ok()).flatten()
    };
    Some((number(x)?, number(z)?))
}

fn remove_if_present(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Makes the folder's entries, as created, renamed and removed so far, durable. Windows cannot open a folder
/// as a file to sync it; NTFS journals its own entries.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_canonical_names_are_ours() {
        assert_eq!(parse_region("r.-1.20.mca"), Some((-1, 20)));
        assert_eq!(parse_external("c.0.-33.mcc"), Some((0, -33)));
        for name in [
            "r.01.0.mca",
            "r.-0.0.mca",
            "r.+1.0.mca",
            "r.1.mca",
            "r.1.2.3.mca",
            "r.a.0.mca",
            "c.0.0.mca",
            "r.0.0.mca.tmp",
        ] {
            assert_eq!(parse_region(name), None, "{name}");
        }
        assert_eq!(parse_external("c.0.0.mca"), None);
    }

    #[test]
    fn journals_read_back_and_refuse_other_files() {
        let mut journal = Journal::default();
        journal.install.push("c.33.-1.mcc".to_owned());
        journal.install.push("r.1.-1.mca".to_owned());
        journal.remove("c.40.-2.mcc".to_owned());
        let text = journal.encode();
        assert_eq!(Journal::decode(&text, 1, -1), Ok(journal));

        assert!(Journal::decode(&text, 0, 0).is_err());
        assert!(Journal::decode(&text[..text.len() - 4], 1, -1).is_err());
        let escape = format!("{HEADER}\ninstall ../level.dat\n{END}\n");
        assert!(Journal::decode(&escape, 1, -1).is_err());
        let other = format!("{HEADER}\nremove c.0.0.mcc\n{END}\n");
        assert!(Journal::decode(&other, 1, -1).is_err());
        assert!(Journal::decode("", 1, -1).is_err());
    }
}
