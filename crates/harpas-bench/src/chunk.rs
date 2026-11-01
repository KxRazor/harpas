//! Synthetic chunk columns and their Java and Bedrock encodings
//!
//! The terrain is generated from hashed value noise, not from vanilla's generator: what matters here is that
//! sections have the palette sizes and the mix of runs and noise that real ones have, since that is what decides
//! encoded size and compression cost. Block IDs are plausible magnitudes, not a real registry.

/// Sections in an overworld column, from y = -64 to y = 320.
pub const SECTIONS: usize = 24;
const BLOCKS: usize = 4096;

/// A block type the generator places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// Name, hashed for Bedrock's stable network IDs.
    pub name: &'static str,
    /// Java global block state ID.
    pub java_id: i32,
    /// Bedrock runtime ID: the block's index in the session's palette.
    pub bedrock_runtime_id: i32,
}

macro_rules! blocks {
    ($($ident:ident = $name:literal, $java:literal, $bedrock:literal;)*) => {
        $(
            #[doc = concat!("`minecraft:", $name, "`.")]
            pub const $ident: Block = Block { name: concat!("minecraft:", $name), java_id: $java, bedrock_runtime_id: $bedrock };
        )*
    };
}

blocks! {
    AIR = "air", 0, 12530;
    STONE = "stone", 1, 2532;
    GRANITE = "granite", 2, 6212;
    DIORITE = "diorite", 4, 4180;
    ANDESITE = "andesite", 6, 216;
    GRASS_BLOCK = "grass_block", 9, 6300;
    DIRT = "dirt", 10, 4211;
    BEDROCK = "bedrock", 85, 708;
    WATER = "water", 86, 14102;
    GRAVEL = "gravel", 124, 6290;
    COAL_ORE = "coal_ore", 133, 1250;
    IRON_ORE = "iron_ore", 135, 7860;
    OAK_LOG = "oak_log", 137, 9530;
    OAK_LEAVES = "oak_leaves", 252, 9510;
    SHORT_GRASS = "short_grass", 2048, 11120;
    POPPY = "poppy", 2100, 10700;
    TUFF = "tuff", 22314, 13620;
    COPPER_ORE = "copper_ore", 22950, 3300;
    DEEPSLATE = "deepslate", 26575, 3890;
    DEEPSLATE_IRON_ORE = "deepslate_iron_ore", 26590, 3920;
    DEEPSLATE_DIAMOND_ORE = "deepslate_diamond_ore", 26602, 3901;
}

/// Java's global palette needs 15 bits per block when a section has too many distinct states.
const JAVA_DIRECT_BITS: u32 = 15;
/// Biome sent for every biome cell.
const JAVA_PLAINS: i32 = 40;

/// One 16×16×16 section as the engine keeps it: a palette and one palette index per block, indexed
/// `y * 256 + z * 16 + x` as on the Java wire, plus the count of non-air blocks.
#[derive(Clone)]
pub struct Section {
    palette: Vec<Block>,
    indices: Vec<u16>,
    non_air: u16,
}

impl Section {
    fn fill(block: Block) -> Section {
        let non_air = if block == AIR { 0 } else { BLOCKS as u16 };
        Section {
            palette: vec![block],
            indices: vec![0; BLOCKS],
            non_air,
        }
    }

    fn set(&mut self, x: usize, y: usize, z: usize, block: Block) {
        let i = y * 256 + z * 16 + x;
        let old = self.palette[self.indices[i] as usize];
        let index = match self.palette.iter().position(|p| *p == block) {
            Some(index) => index,
            None => {
                self.palette.push(block);
                self.palette.len() - 1
            }
        };
        self.indices[i] = index as u16;
        self.non_air = self.non_air + u16::from(old == AIR) - u16::from(block == AIR);
    }

    /// Blocks the section's indices refer to.
    pub fn palette(&self) -> &[Block] {
        &self.palette
    }
}

/// A column of [`SECTIONS`] sections at chunk coordinates `x`, `z`.
pub struct Column {
    /// Chunk X.
    pub x: i32,
    /// Chunk Z.
    pub z: i32,
    /// Sections from the bottom of the world.
    pub sections: Vec<Section>,
    /// Index of the section that contains the ground.
    pub surface: usize,
}

impl Column {
    /// Generates a plains-like column: deepslate, then stone with caves and ores, a grass surface with a tree
    /// and a pond, then air.
    pub fn generate(x: i32, z: i32) -> Column {
        let surface = 8;
        let sections = (0..SECTIONS)
            .map(|i| match i {
                _ if i < surface - 4 => underground(x, i, z, true),
                _ if i < surface => underground(x, i, z, false),
                _ if i == surface => surface_section(x, i, z),
                _ => Section::fill(AIR),
            })
            .collect();
        Column {
            x,
            z,
            sections,
            surface,
        }
    }
}

fn underground(cx: i32, cy: usize, cz: i32, deep: bool) -> Section {
    let mut section = Section::fill(if deep { DEEPSLATE } else { STONE });
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                let (wx, wy, wz) = (
                    cx * 16 + x as i32,
                    cy as i32 * 16 + y as i32,
                    cz * 16 + z as i32,
                );
                let block = if cy == 0 && y == 0 {
                    BEDROCK
                } else if noise(wx, wy, wz, 8, 1) < 0.2 {
                    AIR
                } else if noise(wx, wy, wz, 4, 2) > 0.82 {
                    [GRANITE, DIORITE, ANDESITE, TUFF, GRAVEL]
                        [(hash(wx >> 3, wy >> 3, wz >> 3, 3) % 5) as usize]
                } else {
                    match hash(wx, wy, wz, 4) % 1000 {
                        0..=7 if deep => DEEPSLATE_IRON_ORE,
                        8..=9 if deep => DEEPSLATE_DIAMOND_ORE,
                        0..=11 if !deep => COAL_ORE,
                        12..=17 if !deep => IRON_ORE,
                        18..=21 if !deep => COPPER_ORE,
                        _ => continue,
                    }
                };
                section.set(x, y, z, block);
            }
        }
    }
    section
}

fn surface_section(cx: i32, cy: usize, cz: i32) -> Section {
    const SEA: usize = 7;
    let mut section = Section::fill(AIR);
    for z in 0..16 {
        for x in 0..16 {
            let (wx, wz) = (cx * 16 + x as i32, cz * 16 + z as i32);
            let height = 4 + (noise(wx, 0, wz, 16, 5) * 8.0) as usize;
            for y in 0..16 {
                let block = match y {
                    _ if y + 3 < height => STONE,
                    _ if y < height => DIRT,
                    _ if y == height && height < SEA => DIRT,
                    _ if y == height => GRASS_BLOCK,
                    _ if y <= SEA && height < SEA => WATER,
                    _ if y == height + 1 => match hash(wx, cy as i32, wz, 6) % 100 {
                        0..=14 => SHORT_GRASS,
                        15..=16 => POPPY,
                        _ => continue,
                    },
                    _ => continue,
                };
                section.set(x, y, z, block);
            }
        }
    }
    // One tree near the middle of the chunk.
    let base = 13;
    for (x, y, z) in cube(5, 10..14, 5) {
        let (dx, dy, dz) = (x as i32 - 7, y as i32 - 12, z as i32 - 7);
        if dx * dx + dy * dy + dz * dz <= 6 {
            section.set(x, y, z, OAK_LEAVES);
        }
    }
    for y in base - 4..base {
        section.set(7, y, 7, OAK_LOG);
    }
    section
}

fn cube(
    x0: usize,
    ys: std::ops::Range<usize>,
    z0: usize,
) -> impl Iterator<Item = (usize, usize, usize)> {
    ys.flat_map(move |y| (z0..z0 + 5).flat_map(move |z| (x0..x0 + 5).map(move |x| (x, y, z))))
}

fn hash(x: i32, y: i32, z: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ (z as u32).wrapping_mul(0xcb1a_b31f);
    h ^= seed.wrapping_mul(0x9e37_79b9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h
}

/// Trilinear value noise in `[0, 1)` with lattice spacing `scale`.
fn noise(x: i32, y: i32, z: i32, scale: i32, seed: u32) -> f32 {
    let cell = |v: i32| {
        (
            v.div_euclid(scale),
            v.rem_euclid(scale) as f32 / scale as f32,
        )
    };
    let ((x0, fx), (y0, fy), (z0, fz)) = (cell(x), cell(y), cell(z));
    let corner =
        |dx, dy, dz| (hash(x0 + dx, y0 + dy, z0 + dz, seed) >> 8) as f32 / (1 << 24) as f32;
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let plane = |dz| {
        let bottom = lerp(corner(0, 0, dz), corner(1, 0, dz), fx);
        let top = lerp(corner(0, 1, dz), corner(1, 1, dz), fx);
        lerp(bottom, top, fy)
    };
    lerp(plane(0), plane(1), fz)
}

// ---- Java -------------------------------------------------------------------------------------------------

fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    while value >= 0x80 {
        buf.push(value as u8 | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

fn bits_for(len: usize) -> u32 {
    usize::BITS - (len.max(2) - 1).leading_zeros()
}

/// Packs `values` into big-endian longs, `64 / bits` values per long with no value spanning two longs.
fn pack_longs(buf: &mut Vec<u8>, values: impl Iterator<Item = u64>, bits: u32) {
    let per_long = (64 / bits) as usize;
    let mut long = 0u64;
    let mut count = 0;
    for value in values {
        long |= value << (bits as usize * count);
        count += 1;
        if count == per_long {
            buf.extend_from_slice(&long.to_be_bytes());
            (long, count) = (0, 0);
        }
    }
    if count > 0 {
        buf.extend_from_slice(&long.to_be_bytes());
    }
}

/// Appends a section in the Java format: non-air count, block states and biomes as paletted containers.
pub fn encode_java_section(buf: &mut Vec<u8>, section: &Section) {
    buf.extend_from_slice(&(section.non_air as i16).to_be_bytes());
    let palette = section.palette();
    if palette.len() == 1 {
        buf.push(0);
        write_varint(buf, palette[0].java_id);
    } else {
        let bits = bits_for(palette.len()).max(4);
        if bits <= 8 {
            buf.push(bits as u8);
            write_varint(buf, palette.len() as i32);
            for block in palette {
                write_varint(buf, block.java_id);
            }
            pack_longs(buf, section.indices.iter().map(|i| u64::from(*i)), bits);
        } else {
            buf.push(JAVA_DIRECT_BITS as u8);
            let global = section
                .indices
                .iter()
                .map(|i| palette[*i as usize].java_id as u64);
            pack_longs(buf, global, JAVA_DIRECT_BITS);
        }
    }
    // Biomes: one value for the whole section.
    buf.push(0);
    write_varint(buf, JAVA_PLAINS);
}

fn write_bitset(buf: &mut Vec<u8>, bits: &[bool]) {
    let longs: Vec<u64> = bits
        .chunks(64)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0, |long, (i, set)| long | (u64::from(*set) << i))
        })
        .collect();
    write_varint(buf, longs.len() as i32);
    for long in longs {
        buf.extend_from_slice(&long.to_be_bytes());
    }
}

/// Appends a Chunk Data and Update Light packet (ID and body, no length prefix) for `column`.
pub fn encode_java_column(buf: &mut Vec<u8>, column: &Column) {
    write_varint(buf, 0x27);
    buf.extend_from_slice(&column.x.to_be_bytes());
    buf.extend_from_slice(&column.z.to_be_bytes());
    // Heightmaps: three maps of 256 9-bit heights.
    let surface_y = (column.surface * 16 + 10) as u64;
    write_varint(buf, 3);
    for kind in [1, 4, 5] {
        write_varint(buf, kind);
        write_varint(buf, 37);
        let mut heights = Vec::new();
        pack_longs(&mut heights, (0..256).map(|i| surface_y + (i % 3)), 9);
        buf.extend_from_slice(&heights);
    }
    let mut data = Vec::new();
    for section in &column.sections {
        encode_java_section(&mut data, section);
    }
    write_varint(buf, data.len() as i32);
    buf.extend_from_slice(&data);
    write_varint(buf, 0); // Block entities.

    // Light sections cover one extra section below and above the world. Sky light is full above the ground,
    // partial in the surface section and dark below; there is no block light.
    let light_sections = SECTIONS + 2;
    let lit = |i: usize| i > column.surface;
    let sky: Vec<bool> = (0..light_sections).map(lit).collect();
    let empty_sky: Vec<bool> = (0..light_sections).map(|i| !lit(i)).collect();
    write_bitset(buf, &sky);
    write_bitset(buf, &[false; 26]);
    write_bitset(buf, &empty_sky);
    write_bitset(buf, &[true; 26]);
    let arrays: Vec<usize> = (0..light_sections).filter(|i| lit(*i)).collect();
    write_varint(buf, arrays.len() as i32);
    for i in arrays {
        write_varint(buf, 2048);
        if i == column.surface + 1 {
            // The surface section: dark below the ground, bright above it.
            buf.extend((0..2048).map(|j| if j < 1024 { 0x00 } else { 0xff }));
        } else {
            buf.extend(std::iter::repeat_n(0xff, 2048));
        }
    }
    write_varint(buf, 0); // Block light arrays.
}

// ---- Bedrock ----------------------------------------------------------------------------------------------

/// How Bedrock block network IDs are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockIds {
    /// Indices into the palette agreed for the session.
    Runtime,
    /// 32-bit hashes of the block state, stable across sessions.
    Hashed,
}

/// FNV-1a 32-bit hash of the block name, standing in for the hash of the block state's canonical NBT.
pub fn fnv1a(name: &str) -> i32 {
    let mut hash = 0x811c_9dc5u32;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as i32
}

fn write_zigzag32(buf: &mut Vec<u8>, value: i32) {
    write_varint(buf, (value << 1) ^ (value >> 31));
}

/// Appends a section in Bedrock's sub-chunk format, version 9, with one block storage.
pub fn encode_bedrock_section(buf: &mut Vec<u8>, section: &Section, y_index: i8, ids: BedrockIds) {
    let id = |block: &Block| match ids {
        BedrockIds::Runtime => block.bedrock_runtime_id,
        BedrockIds::Hashed => fnv1a(block.name),
    };
    buf.extend_from_slice(&[9, 1, y_index as u8]);
    let palette = section.palette();
    if palette.len() == 1 {
        // A single-value storage: no words and no palette size *(verify)*.
        buf.push(1);
        write_zigzag32(buf, id(&palette[0]));
        return;
    }
    let needed = bits_for(palette.len());
    let bits = [1, 2, 3, 4, 5, 6, 8, 16]
        .into_iter()
        .find(|b| *b >= needed)
        .unwrap();
    buf.push(((bits as u8) << 1) | 1);
    let per_word = 32 / bits;
    // Bedrock orders blocks x, z, y.
    let mut word = 0u32;
    let mut count = 0;
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..16 {
                word |= u32::from(section.indices[y * 256 + z * 16 + x]) << (bits * count);
                count += 1;
                if count == per_word {
                    buf.extend_from_slice(&word.to_le_bytes());
                    (word, count) = (0, 0);
                }
            }
        }
    }
    if count > 0 {
        buf.extend_from_slice(&word.to_le_bytes());
    }
    write_zigzag32(buf, palette.len() as i32);
    for block in palette {
        write_zigzag32(buf, id(block));
    }
}

/// Appends an uncompressed batch with one sub-chunk packet per section of `column`.
pub fn encode_bedrock_batch(buf: &mut Vec<u8>, column: &Column, ids: BedrockIds) {
    let mut packet = Vec::new();
    for (i, section) in column.sections.iter().enumerate() {
        packet.clear();
        write_varint(&mut packet, 0xae); // SubChunkPacket.
        // Header: cache flag, dimension, position, entry offset and result, as a fixed 16 bytes.
        packet.extend_from_slice(&[0; 16]);
        encode_bedrock_section(&mut packet, section, i as i8 - 4, ids);
        write_varint(buf, packet.len() as i32);
        buf.extend_from_slice(&packet);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_sizes_look_like_real_terrain() {
        let column = Column::generate(3, -7);
        let sizes: Vec<usize> = column.sections.iter().map(|s| s.palette().len()).collect();
        assert!(sizes[..4].iter().all(|n| (4..=9).contains(n)), "{sizes:?}");
        assert!(
            sizes[4..8].iter().all(|n| (5..=12).contains(n)),
            "{sizes:?}"
        );
        assert!((7..=12).contains(&sizes[8]), "{sizes:?}");
        assert!(sizes[9..].iter().all(|n| *n == 1), "{sizes:?}");
    }

    #[test]
    fn java_section_has_the_documented_size() {
        // 4 bits per block: 16 blocks per long, 256 longs.
        let mut section = Section::fill(STONE);
        section.set(0, 0, 0, AIR);
        let mut buf = Vec::new();
        encode_java_section(&mut buf, &section);
        let palette = 1 + 1 + 1; // Count, then stone and air, one byte each.
        assert_eq!(buf.len(), 2 + 1 + palette + 256 * 8 + 2);
        assert_eq!(&buf[..2], &4095_i16.to_be_bytes());
    }

    #[test]
    fn bedrock_words_hold_whole_values() {
        // 3 distinct blocks need 2 bits: 16 per word, 256 words.
        let mut section = Section::fill(STONE);
        section.set(0, 0, 0, AIR);
        section.set(0, 1, 0, DIRT);
        let mut buf = Vec::new();
        encode_bedrock_section(&mut buf, &section, 0, BedrockIds::Runtime);
        assert_eq!(buf[3], (2 << 1) | 1);
        // The palette is stone, air, dirt. The first word is the column x = 0, z = 0 from the bottom: air,
        // dirt, then stone.
        let word = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        assert_eq!([word & 0b11, word >> 2 & 0b11, word >> 4 & 0b11], [1, 2, 0]);
        // Then the palette size, 3, as a zigzag VarInt.
        assert_eq!(buf[4 + 256 * 4], 6);
    }
}
