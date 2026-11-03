//! Region files: 32×32 chunks in 4 KiB sectors
//!
//! The first sector holds each chunk's location (an offset in sectors and a count), the second its last save
//! time in seconds. A chunk's data starts with its length and a compression byte; a chunk that would need more
//! than 255 sectors is stored in its own `c.x.z.mcc` file, and the region keeps only the compression byte with
//! 128 added. Harpas writes zlib, as vanilla does, and reads gzip, zlib and uncompressed chunks.
//!
//! Individual chunks can be read without loading their whole region. A region is saved whole: the new file is
//! written under a temporary name and renamed over the old one (`docs/world.md#saving`).

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::ZlibEncoder;
use harpas_core::nbt::{Compound, Nbt};

use crate::file::write_atomically;

const SECTOR: usize = 4096;
const CHUNKS: usize = 1024;
/// The most sectors a location's one-byte count can give a chunk.
const MAX_SECTORS: usize = 255;

const GZIP: u8 = 1;
const ZLIB: u8 = 2;
const UNCOMPRESSED: u8 = 3;
const LZ4: u8 = 4;
const CUSTOM: u8 = 127;
/// Added to the compression byte of a chunk stored in a `.mcc` file.
const EXTERNAL: u8 = 128;

/// The region that holds chunk `chunk_x`, `chunk_z`.
pub fn region_of(chunk_x: i32, chunk_z: i32) -> (i32, i32) {
    (chunk_x >> 5, chunk_z >> 5)
}

/// Reads one chunk from `dir` without reading the rest of its region. A missing region or entry is `None`.
pub fn read_chunk(dir: &Path, chunk_x: i32, chunk_z: i32) -> io::Result<Option<Compound>> {
    let (region_x, region_z) = region_of(chunk_x, chunk_z);
    let path = dir.join(format!("r.{region_x}.{region_z}.mca"));
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let file_len = file.metadata()?.len();
    if file_len < (2 * SECTOR) as u64 {
        return Ok(None);
    }
    let index = (chunk_x & 31) as u64 + (chunk_z & 31) as u64 * 32;
    file.seek(SeekFrom::Start(index * 4))?;
    let mut location = [0; 4];
    file.read_exact(&mut location)?;
    let location = u32::from_be_bytes(location);
    if location == 0 {
        return Ok(None);
    }
    let offset = u64::from(location >> 8);
    let sectors = u64::from(location & 0xff);
    let start = offset * SECTOR as u64;
    if offset < 2 || sectors == 0 || start + 5 > file_len {
        return Err(invalid(format!(
            "location {offset}+{sectors} is outside the region file"
        )));
    }
    file.seek(SeekFrom::Start(start))?;
    let mut header = [0; 5];
    file.read_exact(&mut header)?;
    let length = u64::from(u32::from_be_bytes(header[..4].try_into().unwrap()));
    let compression = header[4];
    let data = if compression & EXTERNAL != 0 {
        let path = dir.join(format!("c.{chunk_x}.{chunk_z}.mcc"));
        fs::read(&path).map_err(|error| invalid(format!("{}: {error}", path.display())))?
    } else {
        if length == 0 || length > sectors * SECTOR as u64 - 4 || start + 4 + length > file_len {
            return Err(invalid(format!(
                "length {length} does not fit its {sectors} sectors"
            )));
        }
        let mut data = vec![0; (length - 1) as usize];
        file.read_exact(&mut data)?;
        data
    };
    decode_chunk(compression & !EXTERNAL, &data).map(Some)
}

/// What a region holds for one chunk.
#[derive(Debug)]
enum Slot {
    Empty,
    Stored {
        /// Seconds since the Unix epoch at the last save.
        timestamp: u32,
        compression: u8,
        /// The compressed chunk.
        data: Vec<u8>,
    },
    /// The region's entry for the chunk cannot be read; saving the region drops it, as vanilla does when it
    /// writes the chunk again.
    Corrupt(String),
}

/// A region file, `r.x.z.mca`, read whole into memory.
#[derive(Debug)]
pub struct Region {
    dir: PathBuf,
    x: i32,
    z: i32,
    slots: Vec<Slot>,
    /// Which chunks have a `.mcc` file on disk, to remove the ones a save no longer needs.
    external: Vec<bool>,
}

impl Region {
    /// Reads region `x`, `z` from the folder `dir`; a missing file is an empty region. Chunks whose entry cannot
    /// be read are reported when they are read, so one damaged chunk does not hide the others.
    pub fn open(dir: &Path, x: i32, z: i32) -> io::Result<Self> {
        let mut region = Self {
            dir: dir.to_owned(),
            x,
            z,
            slots: (0..CHUNKS).map(|_| Slot::Empty).collect(),
            external: vec![false; CHUNKS],
        };
        let file = match fs::read(region.path()) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(region),
            Err(e) => return Err(e),
        };
        // A file too short for its header has no chunks.
        if file.len() < 2 * SECTOR {
            return Ok(region);
        }
        for index in 0..CHUNKS {
            let location = be_u32(&file, index * 4);
            if location == 0 {
                continue;
            }
            let timestamp = be_u32(&file, SECTOR + index * 4);
            region.slots[index] = match region.read_slot(&file, index, location) {
                Ok((compression, data)) => {
                    region.external[index] = compression & EXTERNAL != 0;
                    Slot::Stored {
                        timestamp,
                        compression: compression & !EXTERNAL,
                        data,
                    }
                }
                Err(reason) => Slot::Corrupt(reason),
            };
        }
        Ok(region)
    }

    /// The compression byte and compressed data of the chunk at `index`, whose location is `location`.
    fn read_slot(&self, file: &[u8], index: usize, location: u32) -> Result<(u8, Vec<u8>), String> {
        let offset = (location >> 8) as usize;
        let sectors = (location & 0xff) as usize;
        let start = offset * SECTOR;
        if offset < 2 || sectors == 0 || start + 5 > file.len() {
            return Err(format!(
                "location {offset}+{sectors} is outside the region file"
            ));
        }
        let length = be_u32(file, start) as usize;
        let compression = file[start + 4];
        if compression & EXTERNAL != 0 {
            let path = self.external_path(index);
            let data = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            return Ok((compression, data));
        }
        if length == 0 || length > sectors * SECTOR - 4 || start + 4 + length > file.len() {
            return Err(format!(
                "length {length} does not fit its {sectors} sectors"
            ));
        }
        Ok((compression, file[start + 5..start + 4 + length].to_vec()))
    }

    /// `r.x.z.mca` in the region's folder.
    pub fn path(&self) -> PathBuf {
        self.dir.join(format!("r.{}.{}.mca", self.x, self.z))
    }

    /// `c.x.z.mcc` for the chunk at `index`, named by its chunk coordinates.
    fn external_path(&self, index: usize) -> PathBuf {
        let x = self.x * 32 + (index % 32) as i32;
        let z = self.z * 32 + (index / 32) as i32;
        self.dir.join(format!("c.{x}.{z}.mcc"))
    }

    fn index(&self, chunk_x: i32, chunk_z: i32) -> usize {
        assert_eq!(
            region_of(chunk_x, chunk_z),
            (self.x, self.z),
            "chunk {chunk_x} {chunk_z} is not in region {} {}",
            self.x,
            self.z
        );
        (chunk_x & 31) as usize + (chunk_z & 31) as usize * 32
    }

    /// The chunk's NBT, or `None` if the region has no such chunk.
    pub fn chunk(&self, chunk_x: i32, chunk_z: i32) -> io::Result<Option<Compound>> {
        let (compression, data) = match &self.slots[self.index(chunk_x, chunk_z)] {
            Slot::Empty => return Ok(None),
            Slot::Corrupt(reason) => return Err(invalid(reason)),
            Slot::Stored {
                compression, data, ..
            } => (*compression, data.as_slice()),
        };
        decode_chunk(compression, data).map(Some)
    }

    /// The time of the chunk's last save, in seconds since the Unix epoch.
    pub fn timestamp(&self, chunk_x: i32, chunk_z: i32) -> Option<u32> {
        match &self.slots[self.index(chunk_x, chunk_z)] {
            Slot::Stored { timestamp, .. } => Some(*timestamp),
            _ => None,
        }
    }

    /// Sets the chunk's NBT, compressed with zlib as vanilla does, and stamps it with the current time.
    pub fn set_chunk(&mut self, chunk_x: i32, chunk_z: i32, chunk: &Compound) {
        let index = self.index(chunk_x, chunk_z);
        let mut nbt = Vec::new();
        Nbt::Compound(chunk.clone()).write_named("", &mut nbt);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&nbt)
            .expect("writing to a Vec cannot fail");
        let data = encoder.finish().expect("writing to a Vec cannot fail");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_secs() as u32);
        self.slots[index] = Slot::Stored {
            timestamp,
            compression: ZLIB,
            data,
        };
    }

    /// Removes the chunk.
    pub fn remove_chunk(&mut self, chunk_x: i32, chunk_z: i32) {
        let index = self.index(chunk_x, chunk_z);
        self.slots[index] = Slot::Empty;
    }

    /// Writes the region file and the `.mcc` files of its large chunks, and removes `.mcc` files it no longer
    /// needs. Chunks whose entry could not be read are dropped.
    pub fn save(&mut self) -> io::Result<()> {
        let mut file = vec![0; 2 * SECTOR];
        let mut external = vec![false; CHUNKS];
        for (index, slot) in self.slots.iter().enumerate() {
            let Slot::Stored {
                timestamp,
                compression,
                data,
            } = slot
            else {
                continue;
            };
            let offset = file.len() / SECTOR;
            if (data.len() + 5).div_ceil(SECTOR) > MAX_SECTORS {
                write_atomically(&self.external_path(index), data)?;
                external[index] = true;
                file.extend_from_slice(&1u32.to_be_bytes());
                file.push(compression | EXTERNAL);
            } else {
                file.extend_from_slice(&(data.len() as u32 + 1).to_be_bytes());
                file.push(*compression);
                file.extend_from_slice(data);
            }
            file.resize(file.len().next_multiple_of(SECTOR), 0);
            let sectors = file.len() / SECTOR - offset;
            if offset >= 1 << 24 {
                return Err(invalid("region file larger than its locations can address"));
            }
            let location = (offset as u32) << 8 | sectors as u32;
            file[index * 4..index * 4 + 4].copy_from_slice(&location.to_be_bytes());
            file[SECTOR + index * 4..SECTOR + index * 4 + 4]
                .copy_from_slice(&timestamp.to_be_bytes());
        }
        fs::create_dir_all(&self.dir)?;
        write_atomically(&self.path(), &file)?;
        let stale = (0..CHUNKS).filter(|&index| self.external[index] && !external[index]);
        for index in stale {
            match fs::remove_file(self.external_path(index)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        self.external = external;
        Ok(())
    }
}

/// The algorithm a custom-compressed chunk names: an unsigned 16-bit length and a namespaced string.
fn decode_chunk(compression: u8, data: &[u8]) -> io::Result<Compound> {
    let mut nbt = Vec::new();
    match compression {
        GZIP => GzDecoder::new(data).read_to_end(&mut nbt)?,
        ZLIB => ZlibDecoder::new(data).read_to_end(&mut nbt)?,
        UNCOMPRESSED => {
            nbt.extend_from_slice(data);
            nbt.len()
        }
        LZ4 => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "LZ4 chunks are not supported yet; set region-file-compression=deflate and open the world once \
                 with vanilla",
            ));
        }
        CUSTOM => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "chunk compressed with a custom algorithm ({})",
                    custom_name(data)
                ),
            ));
        }
        other => return Err(invalid(format!("unknown chunk compression {other}"))),
    };
    match Nbt::read_named(&mut nbt.as_slice()).map_err(invalid)? {
        (_, Nbt::Compound(chunk)) => Ok(chunk),
        _ => Err(invalid("the chunk's NBT root is not a compound")),
    }
}

fn custom_name(data: &[u8]) -> String {
    let length = data
        .get(..2)
        .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as usize);
    data.get(2..2 + length)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_else(|| "unnamed".to_owned())
}

fn be_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn invalid(error: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use flate2::write::GzEncoder;

    use super::*;

    /// A world folder that is removed when dropped.
    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "harpas-region-{}-{}",
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

    fn chunk(x: i32, z: i32) -> Compound {
        let mut chunk = Compound::new();
        chunk.insert("DataVersion", 4786);
        chunk.insert("xPos", x);
        chunk.insert("zPos", z);
        chunk.insert("Status", "minecraft:full");
        chunk
    }

    /// A chunk's NBT as a region file stores it, before compression.
    fn nbt(chunk: &Compound) -> Vec<u8> {
        let mut buf = Vec::new();
        Nbt::Compound(chunk.clone()).write_named("", &mut buf);
        buf
    }

    /// A region file with one chunk at `index`, stored at sector 2 as vanilla lays it out.
    fn region_file(index: usize, compression: u8, data: &[u8]) -> Vec<u8> {
        let mut file = vec![0; 2 * SECTOR];
        file.extend_from_slice(&(data.len() as u32 + 1).to_be_bytes());
        file.push(compression);
        file.extend_from_slice(data);
        file.resize(file.len().next_multiple_of(SECTOR), 0);
        let sectors = (file.len() / SECTOR - 2) as u32;
        file[index * 4..index * 4 + 4].copy_from_slice(&(2 << 8 | sectors).to_be_bytes());
        file[SECTOR + index * 4..SECTOR + index * 4 + 4]
            .copy_from_slice(&1_700_000_000u32.to_be_bytes());
        file
    }

    /// Bytes that do not compress, so a chunk holding them needs more sectors than a region can give.
    fn noise(len: usize) -> Vec<i8> {
        let mut state = 0x9e37_79b9_u32;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as i8
            })
            .collect()
    }

    #[test]
    fn regions_are_found_by_arithmetic_shift() {
        assert_eq!(region_of(30, -3), (0, -1));
        assert_eq!(region_of(1500, -600), (46, -19));
        assert_eq!(region_of(-32, -33), (-1, -2));
    }

    #[test]
    fn a_missing_file_is_an_empty_region() {
        let folder = Folder::new();
        let region = Region::open(&folder.0, 0, 0).unwrap();
        assert_eq!(region.chunk(3, 4).unwrap(), None);
        assert_eq!(region.timestamp(3, 4), None);
        assert_eq!(read_chunk(&folder.0, 3, 4).unwrap(), None);
    }

    #[test]
    fn saved_chunks_read_back_from_a_sector_aligned_file() {
        let folder = Folder::new();
        let mut region = Region::open(&folder.0, -1, 2).unwrap();
        region.set_chunk(-1, 64, &chunk(-1, 64));
        region.set_chunk(-32, 95, &chunk(-32, 95));
        region.save().unwrap();

        let file = fs::read(folder.0.join("r.-1.2.mca")).unwrap();
        assert_eq!(file.len() % SECTOR, 0);
        // Chunk -1, 64 is index 31: the first chunk saved, at sector 2.
        assert_eq!(be_u32(&file, 31 * 4), 2 << 8 | 1);
        assert_eq!(file[2 * SECTOR + 4], ZLIB);

        let region = Region::open(&folder.0, -1, 2).unwrap();
        assert_eq!(region.chunk(-1, 64).unwrap(), Some(chunk(-1, 64)));
        assert_eq!(region.chunk(-32, 95).unwrap(), Some(chunk(-32, 95)));
        assert_eq!(region.chunk(-2, 64).unwrap(), None);
        assert_eq!(read_chunk(&folder.0, -1, 64).unwrap(), Some(chunk(-1, 64)));
        assert_eq!(
            read_chunk(&folder.0, -32, 95).unwrap(),
            Some(chunk(-32, 95))
        );
        assert_eq!(read_chunk(&folder.0, -2, 64).unwrap(), None);
        assert!(region.timestamp(-1, 64).unwrap() > 1_700_000_000);
    }

    #[test]
    fn reads_every_compression_vanilla_writes() {
        let nbt = nbt(&chunk(5, 0));
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&nbt).unwrap();
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
        zlib.write_all(&nbt).unwrap();
        for (compression, data) in [
            (GZIP, gzip.finish().unwrap()),
            (ZLIB, zlib.finish().unwrap()),
            (UNCOMPRESSED, nbt.clone()),
        ] {
            let folder = Folder::new();
            fs::write(
                folder.0.join("r.0.0.mca"),
                region_file(5, compression, &data),
            )
            .unwrap();
            let region = Region::open(&folder.0, 0, 0).unwrap();
            assert_eq!(
                region.chunk(5, 0).unwrap(),
                Some(chunk(5, 0)),
                "compression {compression}"
            );
            assert_eq!(region.timestamp(5, 0), Some(1_700_000_000));
            assert_eq!(read_chunk(&folder.0, 5, 0).unwrap(), Some(chunk(5, 0)));
        }
    }

    #[test]
    fn lz4_and_custom_compression_are_reported() {
        let folder = Folder::new();
        fs::write(folder.0.join("r.0.0.mca"), region_file(0, LZ4, b"x")).unwrap();
        let error = Region::open(&folder.0, 0, 0)
            .unwrap()
            .chunk(0, 0)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            read_chunk(&folder.0, 0, 0).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );

        let mut custom = vec![0, 11];
        custom.extend_from_slice(b"example:zst");
        fs::write(folder.0.join("r.0.0.mca"), region_file(0, CUSTOM, &custom)).unwrap();
        let error = Region::open(&folder.0, 0, 0)
            .unwrap()
            .chunk(0, 0)
            .unwrap_err();
        assert!(error.to_string().contains("example:zst"));
    }

    #[test]
    fn a_damaged_entry_does_not_hide_other_chunks() {
        let folder = Folder::new();
        let mut file = region_file(0, UNCOMPRESSED, &nbt(&chunk(0, 0)));
        // Chunk 1, 0 points past the end of the file; chunk 2, 0 claims more bytes than its sector holds.
        file[4..8].copy_from_slice(&(90 << 8 | 1u32).to_be_bytes());
        file[8..12].copy_from_slice(&(2 << 8 | 1u32).to_be_bytes());
        let mut wrong_length = file.clone();
        wrong_length[2 * SECTOR..2 * SECTOR + 4].copy_from_slice(&5000u32.to_be_bytes());
        fs::write(folder.0.join("r.0.0.mca"), &file).unwrap();
        let region = Region::open(&folder.0, 0, 0).unwrap();
        assert_eq!(region.chunk(0, 0).unwrap(), Some(chunk(0, 0)));
        assert_eq!(read_chunk(&folder.0, 0, 0).unwrap(), Some(chunk(0, 0)));
        assert_eq!(
            read_chunk(&folder.0, 1, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            region.chunk(1, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        fs::write(folder.0.join("r.0.0.mca"), &wrong_length).unwrap();
        let mut region = Region::open(&folder.0, 0, 0).unwrap();
        assert!(region.chunk(0, 0).is_err());
        assert!(region.chunk(2, 0).is_err());
        assert!(read_chunk(&folder.0, 2, 0).is_err());
        // Saving drops the entries it cannot read.
        region.save().unwrap();
        assert_eq!(
            fs::read(folder.0.join("r.0.0.mca")).unwrap().len(),
            2 * SECTOR
        );
    }

    #[test]
    fn large_chunks_go_to_mcc_files_and_back() {
        let folder = Folder::new();
        let mut large = chunk(33, -1);
        // Over 1020 KiB once compressed.
        large.insert("Noise", Nbt::ByteArray(noise(1_100_000)));
        let mut region = Region::open(&folder.0, 1, -1).unwrap();
        region.set_chunk(33, -1, &large);
        region.set_chunk(34, -1, &chunk(34, -1));
        region.save().unwrap();

        let mcc = folder.0.join("c.33.-1.mcc");
        assert!(mcc.is_file());
        let file = fs::read(folder.0.join("r.1.-1.mca")).unwrap();
        // Index 1 + 31 * 32: a one-sector stub with length 1 and the compression marked external.
        let location = be_u32(&file, (1 + 31 * 32) * 4);
        assert_eq!(location & 0xff, 1);
        let start = (location >> 8) as usize * SECTOR;
        assert_eq!(be_u32(&file, start), 1);
        assert_eq!(file[start + 4], ZLIB | EXTERNAL);

        let mut region = Region::open(&folder.0, 1, -1).unwrap();
        assert_eq!(region.chunk(33, -1).unwrap(), Some(large.clone()));
        assert_eq!(read_chunk(&folder.0, 33, -1).unwrap(), Some(large));
        assert_eq!(region.chunk(34, -1).unwrap(), Some(chunk(34, -1)));

        // Once the chunk is small again, its .mcc file goes.
        region.set_chunk(33, -1, &chunk(33, -1));
        region.save().unwrap();
        assert!(!mcc.exists());
        let region = Region::open(&folder.0, 1, -1).unwrap();
        assert_eq!(region.chunk(33, -1).unwrap(), Some(chunk(33, -1)));
        assert!(fs::read_dir(&folder.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn removed_chunks_are_not_saved() {
        let folder = Folder::new();
        let mut region = Region::open(&folder.0, 0, 0).unwrap();
        region.set_chunk(7, 7, &chunk(7, 7));
        region.save().unwrap();
        region.remove_chunk(7, 7);
        region.save().unwrap();
        assert_eq!(
            Region::open(&folder.0, 0, 0).unwrap().chunk(7, 7).unwrap(),
            None
        );
    }

    #[test]
    #[should_panic(expected = "not in region")]
    fn chunks_outside_the_region_are_refused() {
        let folder = Folder::new();
        let _ = Region::open(&folder.0, 0, 0).unwrap().chunk(32, 0);
    }
}
