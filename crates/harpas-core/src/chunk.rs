//! Chunk sections: 16×16×16 blocks and 4×4×4 biomes in paletted containers, as vanilla stores them
//!
//! A [`PalettedContainer`] keeps its values the way vanilla does in memory: one value for the whole container,
//! a palette and packed indices into it, or global IDs once the palette would need too many bits. Vanilla's
//! Java chunk packets carry this form as it is, so encoding a section is a copy. [`PalettedContainer::pack`]
//! and [`PalettedContainer::unpack`] give and take the compacted form of vanilla's Anvil files.
//!
//! Every change to a [`Section`] gives it a new version, unique in the process, which the shared encoding
//! caches key on (`docs/engine-core.md#6-chunks`).

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// How a kind of container stores its values: vanilla's `PalettedContainer.Strategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strategy {
    /// Values in a container.
    len: usize,
    /// Fewest bits per index a palette uses.
    min_bits: u8,
    /// Most bits per index a palette uses; past it, the container stores global IDs.
    max_bits: u8,
    /// Bits per global ID: the base-2 logarithm of the registry's size, rounded up.
    global_bits: u8,
}

impl Strategy {
    /// Block states of a section: 4,096 values, palettes of 4 to 8 bits.
    pub const fn block_states(global_bits: u8) -> Self {
        Self {
            len: 4096,
            min_bits: 4,
            max_bits: 8,
            global_bits,
        }
    }

    /// Biomes of a section: 64 values, palettes of 1 to 3 bits.
    pub const fn biomes(global_bits: u8) -> Self {
        Self {
            len: 64,
            min_bits: 1,
            max_bits: 3,
            global_bits,
        }
    }

    /// Bits per index in memory for a palette of `size` values.
    fn bits(&self, size: usize) -> u8 {
        match ceil_log2(size) {
            0 => 0,
            bits if bits <= self.max_bits => bits.max(self.min_bits),
            _ => self.global_bits,
        }
    }

    /// Bits per index in Anvil files for a palette of `size` values, which always keep a palette.
    fn packed_bits(&self, size: usize) -> u8 {
        match ceil_log2(size) {
            0 => 0,
            bits if bits <= self.max_bits => bits.max(self.min_bits),
            bits => bits,
        }
    }
}

/// The values of a container in vanilla's Anvil form: a palette in order of first use, and indices into it
/// packed into longs. A container with one value has no data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packed {
    /// The values used, each once.
    pub palette: Vec<u32>,
    /// Indices into the palette.
    pub data: Vec<u64>,
}

/// Why [`PalettedContainer::unpack`] refused a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnpackError {
    /// The palette is empty.
    EmptyPalette,
    /// The data does not have the length the palette size calls for.
    Length {
        /// Longs in the data.
        got: usize,
        /// Longs the palette size calls for.
        expected: usize,
    },
    /// An index points past the end of the palette.
    Index {
        /// The index.
        index: u64,
        /// Values in the palette.
        palette: usize,
    },
}

impl fmt::Display for UnpackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPalette => write!(f, "empty palette"),
            // Vanilla's message for the same check.
            Self::Length { got, expected } => write!(
                f,
                "Invalid length given for storage, got: {got} but expected: {expected}"
            ),
            Self::Index { index, palette } => {
                write!(f, "palette index {index} out of a palette of {palette}")
            }
        }
    }
}

impl Error for UnpackError {}

/// Values packed into longs, `64 / bits` per long and none across two longs, lowest bits first: vanilla's
/// `SimpleBitStorage`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BitStorage {
    bits: u8,
    data: Vec<u64>,
}

impl BitStorage {
    fn new(bits: u8, len: usize) -> Self {
        let data = if bits == 0 {
            Vec::new()
        } else {
            vec![0; longs(bits, len)]
        };
        Self { bits, data }
    }

    fn per_long(&self) -> usize {
        64 / usize::from(self.bits)
    }

    fn mask(&self) -> u64 {
        (1 << self.bits) - 1
    }

    fn get(&self, index: usize) -> u64 {
        if self.bits == 0 {
            return 0;
        }
        let per_long = self.per_long();
        let shift = (index % per_long) * usize::from(self.bits);
        (self.data[index / per_long] >> shift) & self.mask()
    }

    fn set(&mut self, index: usize, value: u64) {
        let per_long = self.per_long();
        let shift = (index % per_long) * usize::from(self.bits);
        let mask = self.mask() << shift;
        let long = &mut self.data[index / per_long];
        *long = (*long & !mask) | (value << shift);
    }
}

/// Longs needed for `len` values of `bits` bits.
fn longs(bits: u8, len: usize) -> usize {
    len.div_ceil(64 / usize::from(bits))
}

fn ceil_log2(size: usize) -> u8 {
    if size <= 1 {
        0
    } else {
        (usize::BITS - (size - 1).leading_zeros()) as u8
    }
}

/// A container of global IDs, block states or biomes, stored as vanilla stores it in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalettedContainer {
    strategy: Strategy,
    /// The values indices refer to; empty once the storage holds global IDs.
    palette: Vec<u32>,
    storage: BitStorage,
}

impl PalettedContainer {
    /// A container with `value` everywhere.
    pub fn filled(strategy: Strategy, value: u32) -> Self {
        Self {
            strategy,
            palette: vec![value],
            storage: BitStorage::new(0, strategy.len),
        }
    }

    /// Bits per value in memory, as Java chunk packets carry them: 0 for a single value, palette bits, or the
    /// global bits.
    pub fn bits(&self) -> u8 {
        self.storage.bits
    }

    /// The palette, or `None` when the container stores global IDs.
    pub fn palette(&self) -> Option<&[u32]> {
        (!self.is_global()).then_some(self.palette.as_slice())
    }

    /// The packed indices, or global IDs, as Java chunk packets carry them.
    pub fn data(&self) -> &[u64] {
        &self.storage.data
    }

    fn is_global(&self) -> bool {
        self.palette.is_empty()
    }

    /// The value at `index`.
    pub fn get(&self, index: usize) -> u32 {
        let raw = self.storage.get(index);
        if self.is_global() {
            raw as u32
        } else {
            self.palette[raw as usize]
        }
    }

    /// Sets the value at `index` and returns the old one. The palette grows as vanilla's does, and never shrinks.
    pub fn set(&mut self, index: usize, value: u32) -> u32 {
        assert!(index < self.strategy.len, "index {index} out of range");
        let old = self.get(index);
        if old == value {
            return old;
        }
        let raw = if self.is_global() {
            value
        } else if let Some(position) = self.palette.iter().position(|&v| v == value) {
            position as u32
        } else {
            self.palette.push(value);
            self.resize();
            if self.is_global() {
                value
            } else {
                self.palette.len() as u32 - 1
            }
        };
        self.storage.set(index, u64::from(raw));
        old
    }

    /// Moves the storage to the bits the palette now needs.
    fn resize(&mut self) {
        let bits = self.strategy.bits(self.palette.len());
        if bits == self.storage.bits {
            return;
        }
        let global = bits == self.strategy.global_bits && bits > self.strategy.max_bits;
        let mut storage = BitStorage::new(bits, self.strategy.len);
        for index in 0..self.strategy.len {
            let raw = self.storage.get(index);
            let value = if global {
                u64::from(self.palette[raw as usize])
            } else {
                raw
            };
            storage.set(index, value);
        }
        self.storage = storage;
        if global {
            self.palette.clear();
        }
    }

    /// The container in vanilla's Anvil form.
    pub fn pack(&self) -> Packed {
        let mut palette = Vec::new();
        let mut positions = HashMap::new();
        let indices: Vec<u64> = (0..self.strategy.len)
            .map(|index| {
                let value = self.get(index);
                *positions.entry(value).or_insert_with(|| {
                    palette.push(value);
                    palette.len() as u64 - 1
                })
            })
            .collect();
        let bits = self.strategy.packed_bits(palette.len());
        let data = if bits == 0 {
            Vec::new()
        } else {
            let mut storage = BitStorage::new(bits, self.strategy.len);
            for (index, raw) in indices.into_iter().enumerate() {
                storage.set(index, raw);
            }
            storage.data
        };
        Packed { palette, data }
    }

    /// Reads a container in vanilla's Anvil form, checking it as vanilla does.
    pub fn unpack(strategy: Strategy, packed: Packed) -> Result<Self, UnpackError> {
        let Packed { palette, data } = packed;
        if palette.is_empty() {
            return Err(UnpackError::EmptyPalette);
        }
        let bits = strategy.packed_bits(palette.len());
        if bits == 0 {
            return Ok(Self::filled(strategy, palette[0]));
        }
        let expected = longs(bits, strategy.len);
        if data.len() != expected {
            return Err(UnpackError::Length {
                got: data.len(),
                expected,
            });
        }
        let stored = BitStorage { bits, data };
        for index in 0..strategy.len {
            let raw = stored.get(index);
            if raw as usize >= palette.len() {
                return Err(UnpackError::Index {
                    index: raw,
                    palette: palette.len(),
                });
            }
        }
        let mut container = Self {
            strategy,
            palette,
            storage: stored,
        };
        // Anvil keeps a palette past the bits memory allows, and fewer bits than memory uses for small ones.
        container.resize();
        Ok(container)
    }
}

/// The next section version; versions are never reused, so a section loaded again never matches a cached one.
static NEXT_VERSION: AtomicU64 = AtomicU64::new(1);

fn next_version() -> u64 {
    NEXT_VERSION.fetch_add(1, Ordering::Relaxed)
}

/// A 16×16×16 section of a chunk: block states and biomes, and a version that changes with every change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    block_states: PalettedContainer,
    biomes: PalettedContainer,
    version: u64,
}

impl Section {
    /// A section from its containers.
    pub fn new(block_states: PalettedContainer, biomes: PalettedContainer) -> Self {
        assert_eq!(block_states.strategy.len, 4096, "not block states");
        assert_eq!(biomes.strategy.len, 64, "not biomes");
        Self {
            block_states,
            biomes,
            version: next_version(),
        }
    }

    /// The block states, indexed Y, then Z, then X.
    pub fn block_states(&self) -> &PalettedContainer {
        &self.block_states
    }

    /// The biomes, indexed Y, then Z, then X.
    pub fn biomes(&self) -> &PalettedContainer {
        &self.biomes
    }

    /// The block state at section coordinates 0 to 15.
    pub fn block_state(&self, x: usize, y: usize, z: usize) -> u32 {
        self.block_states.get(block_index(x, y, z))
    }

    /// Sets a block state and returns the old one.
    pub fn set_block_state(&mut self, x: usize, y: usize, z: usize, state: u32) -> u32 {
        let old = self.block_states.set(block_index(x, y, z), state);
        if old != state {
            self.version = next_version();
        }
        old
    }

    /// The biome at biome coordinates 0 to 3, one per 4×4×4 blocks.
    pub fn biome(&self, x: usize, y: usize, z: usize) -> u32 {
        self.biomes.get(biome_index(x, y, z))
    }

    /// Sets a biome and returns the old one.
    pub fn set_biome(&mut self, x: usize, y: usize, z: usize, biome: u32) -> u32 {
        let old = self.biomes.set(biome_index(x, y, z), biome);
        if old != biome {
            self.version = next_version();
        }
        old
    }

    /// Changes whenever the section changes, and is never shared with another section.
    pub fn version(&self) -> u64 {
        self.version
    }
}

/// Vanilla's order: Y, then Z, then X.
fn block_index(x: usize, y: usize, z: usize) -> usize {
    assert!(
        x < 16 && y < 16 && z < 16,
        "block {x} {y} {z} out of the section"
    );
    (y << 8) | (z << 4) | x
}

fn biome_index(x: usize, y: usize, z: usize) -> usize {
    assert!(
        x < 4 && y < 4 && z < 4,
        "biome {x} {y} {z} out of the section"
    );
    (y << 4) | (z << 2) | x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 26.1 has fewer than 32,768 block states and fewer than 128 biomes.
    const BLOCKS: Strategy = Strategy::block_states(15);
    const BIOMES: Strategy = Strategy::biomes(7);

    /// A reproducible sequence of indices, so tests cover scattered writes.
    fn scattered(count: usize, len: usize) -> impl Iterator<Item = usize> {
        let mut state = 0x2545_f491_u64;
        (0..count).map(move |_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 33) as usize % len
        })
    }

    #[test]
    fn storage_packs_values_like_vanillas_simple_bit_storage() {
        // 5 bits: 12 values per long, the last 4 bits unused, none across two longs.
        let mut storage = BitStorage::new(5, 4096);
        assert_eq!(storage.data.len(), 342);
        storage.set(0, 1);
        storage.set(11, 31);
        storage.set(12, 3);
        assert_eq!(storage.data[0], 1 | (31 << 55));
        assert_eq!(storage.data[1], 3);
        assert_eq!(
            (storage.get(11), storage.get(12), storage.get(13)),
            (31, 3, 0)
        );
        assert_eq!(BitStorage::new(4, 4096).data.len(), 256);
        assert_eq!(BitStorage::new(15, 4096).data.len(), 1024);
        assert_eq!(BitStorage::new(1, 64).data.len(), 1);
    }

    #[test]
    fn block_palettes_grow_from_one_value_to_global_ids() {
        let mut container = PalettedContainer::filled(BLOCKS, 0);
        assert_eq!((container.bits(), container.data().len()), (0, 0));
        // Vanilla uses 4 bits from the second value, then one bit more for each doubling, up to 8.
        let mut expected = vec![0];
        for (value, bits) in [(1, 4), (15, 4), (16, 5), (100, 7), (255, 8), (256, 15)] {
            while (expected.len() as u32) <= value {
                let next = expected.len() as u32;
                container.set(next as usize, next + 1000);
                expected.push(next + 1000);
            }
            assert_eq!(container.bits(), bits, "after {} values", expected.len());
        }
        assert_eq!(container.palette(), None);
        for (index, &value) in expected.iter().enumerate() {
            assert_eq!(container.get(index), value);
        }
        assert_eq!(container.get(4095), 0);
    }

    #[test]
    fn biome_palettes_use_one_to_three_bits() {
        let mut container = PalettedContainer::filled(BIOMES, 40);
        for (value, bits) in [
            (1, 1),
            (2, 2),
            (3, 2),
            (4, 3),
            (5, 3),
            (6, 3),
            (7, 3),
            (8, 7),
        ] {
            container.set(value, value as u32);
            assert_eq!(container.bits(), bits, "after setting {value}");
        }
        assert_eq!(container.get(8), 8);
        assert_eq!(container.get(63), 40);
    }

    #[test]
    fn scattered_writes_read_back() {
        let mut container = PalettedContainer::filled(BLOCKS, 0);
        let mut reference = vec![0u32; 4096];
        for (step, index) in scattered(20_000, 4096).enumerate() {
            let value = (step % 300) as u32 * 7;
            assert_eq!(container.set(index, value), reference[index]);
            reference[index] = value;
        }
        assert!((0..4096).all(|index| container.get(index) == reference[index]));
    }

    #[test]
    fn packing_compacts_the_palette_in_order_of_first_use() {
        let mut container = PalettedContainer::filled(BLOCKS, 9);
        container.set(5, 7);
        container.set(3, 8);
        // 7 is no longer used; memory keeps it, Anvil does not.
        container.set(5, 9);
        assert_eq!(container.palette(), Some(&[9, 7, 8][..]));
        let packed = container.pack();
        assert_eq!(packed.palette, [9, 8]);
        assert_eq!(packed.data.len(), 256);
        assert_eq!(packed.data[0], 1 << 12);

        assert_eq!(
            PalettedContainer::filled(BIOMES, 3).pack(),
            Packed {
                palette: vec![3],
                data: Vec::new()
            }
        );
    }

    #[test]
    fn anvil_bits_follow_vanilla() {
        for (size, bits) in [
            (1, 0),
            (2, 4),
            (16, 4),
            (17, 5),
            (256, 8),
            (257, 9),
            (600, 10),
        ] {
            assert_eq!(BLOCKS.packed_bits(size), bits, "{size} block states");
        }
        for (size, bits) in [(1, 0), (2, 1), (3, 2), (4, 2), (5, 3), (8, 3), (9, 4)] {
            assert_eq!(BIOMES.packed_bits(size), bits, "{size} biomes");
        }
    }

    #[test]
    fn packing_and_unpacking_round_trip() {
        for distinct in [1, 2, 17, 300, 1000] {
            let mut container = PalettedContainer::filled(BLOCKS, 0);
            for (step, index) in scattered(8000, 4096).enumerate() {
                container.set(index, (step % distinct) as u32);
            }
            let packed = container.pack();
            let unpacked = PalettedContainer::unpack(BLOCKS, packed.clone()).unwrap();
            assert!(
                (0..4096).all(|i| unpacked.get(i) == container.get(i)),
                "{distinct} values"
            );
            assert_eq!(unpacked.pack(), packed);
        }
    }

    #[test]
    fn unpacking_moves_to_the_bits_memory_uses() {
        // 300 values: 9 bits in Anvil, global IDs in memory.
        let palette: Vec<u32> = (0..300).map(|v| v * 3).collect();
        let mut storage = BitStorage::new(9, 4096);
        for index in 0..4096 {
            storage.set(index, (index % 300) as u64);
        }
        let packed = Packed {
            palette,
            data: storage.data,
        };
        let container = PalettedContainer::unpack(BLOCKS, packed).unwrap();
        assert_eq!((container.bits(), container.palette()), (15, None));
        assert_eq!(container.get(301), 3);

        // 3 biomes: 2 bits in both.
        let packed = Packed {
            palette: vec![1, 2, 3],
            data: vec![0b10_01_00, 0],
        };
        let container = PalettedContainer::unpack(BIOMES, packed).unwrap();
        assert_eq!(container.bits(), 2);
        assert_eq!(
            (container.get(0), container.get(1), container.get(2)),
            (1, 2, 3)
        );
    }

    #[test]
    fn unpacking_refuses_what_vanilla_refuses() {
        let unpack = |palette: Vec<u32>, data: Vec<u64>| {
            PalettedContainer::unpack(BLOCKS, Packed { palette, data })
        };
        assert_eq!(
            unpack(Vec::new(), Vec::new()),
            Err(UnpackError::EmptyPalette)
        );
        assert_eq!(
            unpack(vec![1, 2], vec![0; 255]),
            Err(UnpackError::Length {
                got: 255,
                expected: 256
            })
        );
        assert_eq!(
            unpack(vec![1, 2], vec![2; 256]),
            Err(UnpackError::Index {
                index: 2,
                palette: 2
            })
        );
        // One value needs no data.
        assert_eq!(unpack(vec![5], Vec::new()).unwrap().get(17), 5);
    }

    #[test]
    fn sections_change_version_only_when_they_change() {
        let mut section = Section::new(
            PalettedContainer::filled(BLOCKS, 0),
            PalettedContainer::filled(BIOMES, 40),
        );
        let other = section.clone();
        let start = section.version();
        assert_eq!(section.set_block_state(1, 2, 3, 0), 0);
        assert_eq!(section.set_biome(0, 0, 0, 40), 40);
        assert_eq!(section.version(), start);

        assert_eq!(section.set_block_state(1, 2, 3, 9), 0);
        let after_block = section.version();
        assert!(after_block > start);
        assert_eq!(section.block_state(1, 2, 3), 9);
        assert_eq!(section.block_states().get((2 << 8) | (3 << 4) | 1), 9);

        assert_eq!(section.set_biome(3, 2, 1, 7), 40);
        assert!(section.version() > after_block);
        assert_eq!(section.biomes().get((2 << 4) | (1 << 2) | 3), 7);

        // A new section never takes a version another one had.
        let fresh = Section::new(
            PalettedContainer::filled(BLOCKS, 0),
            PalettedContainer::filled(BIOMES, 40),
        );
        assert!(fresh.version() > section.version());
        assert_eq!(other.version(), start);
    }
}
