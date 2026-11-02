//! A chunk's NBT turned into sections and back
//!
//! A chunk's `sections` list holds, for each section, its height `Y`, its block states and biomes as palettes of
//! names with packed indices, and its light. [`Chunk::read`] turns the palettes into global IDs, the block states
//! through the fixed block state registry and the biomes through the biome registry the server loaded, since
//! data packs can add biomes. Every other field, of the chunk and of each section, is kept as it was, so a chunk
//! saved again differs from the one read only where it changed.
//!
//! Like vanilla, a damaged section is not fatal: an unknown block becomes air, an unknown biome becomes plains,
//! and a container that cannot be read is replaced by its default, each with a warning.

use std::io;

use harpas_core::block::{BlockStates, block_states};
use harpas_core::chunk::{Packed, PalettedContainer, Section, Strategy};
use harpas_core::nbt::{Compound, List, Nbt};

/// The biome a section takes where its biomes are missing or unknown, as in vanilla.
const DEFAULT_BIOME: &str = "minecraft:plains";

/// A chunk read from Anvil.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// The sections in the order the file lists them.
    pub sections: Vec<ChunkSection>,
    /// The chunk's other fields, kept as they were.
    pub rest: Compound,
}

/// One entry of a chunk's `sections` list.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkSection {
    /// The section's height, in sections.
    pub y: i8,
    /// Blocks and biomes, or `None` for the sections vanilla keeps only for their light, just above and below
    /// the world.
    pub section: Option<Section>,
    /// The section's other fields, such as its light, kept as they were.
    pub rest: Compound,
}

impl Chunk {
    /// Reads a chunk's NBT. `biomes` is the biome registry, an entry's ID being its position. Returns the chunk
    /// and a warning for each part replaced by its default.
    pub fn read(mut nbt: Compound, biomes: &[&str]) -> io::Result<(Self, Vec<String>)> {
        let entries = match nbt.remove("sections") {
            None | Some(Nbt::List(List::Empty)) => Vec::new(),
            Some(Nbt::List(List::Compound(entries))) => entries,
            Some(_) => return Err(invalid("`sections` is not a list of compounds")),
        };
        let mut warnings = Vec::new();
        let sections = entries
            .into_iter()
            .map(|entry| read_section(entry, biomes, &mut warnings))
            .collect::<io::Result<_>>()?;
        Ok((
            Self {
                sections,
                rest: nbt,
            },
            warnings,
        ))
    }

    /// The chunk's NBT, with the palettes compacted as vanilla writes them.
    pub fn to_nbt(&self, biomes: &[&str]) -> Compound {
        let mut nbt = self.rest.clone();
        let sections = self
            .sections
            .iter()
            .map(|section| write_section(section, biomes))
            .collect();
        nbt.insert("sections", Nbt::List(List::Compound(sections)));
        nbt
    }
}

fn block_strategy(states: &BlockStates) -> Strategy {
    Strategy::block_states(states.global_bits())
}

fn biome_strategy(biomes: &[&str]) -> Strategy {
    Strategy::biomes((usize::BITS - biomes.len().saturating_sub(1).leading_zeros()) as u8)
}

fn default_biome(biomes: &[&str]) -> u32 {
    biomes
        .iter()
        .position(|name| *name == DEFAULT_BIOME)
        .unwrap_or(0) as u32
}

fn read_section(
    mut nbt: Compound,
    biomes: &[&str],
    warnings: &mut Vec<String>,
) -> io::Result<ChunkSection> {
    let y = match nbt.remove("Y") {
        Some(Nbt::Byte(y)) => y,
        _ => return Err(invalid("a section has no `Y`")),
    };
    let block_nbt = nbt.remove("block_states");
    let biome_nbt = nbt.remove("biomes");
    let section = if block_nbt.is_none() && biome_nbt.is_none() {
        None
    } else {
        let states = block_states();
        let mut warn =
            |what: &str, problem: String| warnings.push(format!("section {y}: {what}: {problem}"));
        let blocks = block_nbt
            .map(|nbt| read_block_states(nbt, states, &mut warn))
            .unwrap_or_else(|| PalettedContainer::filled(block_strategy(states), 0));
        let biomes = biome_nbt
            .map(|nbt| read_biomes(nbt, biomes, &mut warn))
            .unwrap_or_else(|| {
                PalettedContainer::filled(biome_strategy(biomes), default_biome(biomes))
            });
        Some(Section::new(blocks, biomes))
    };
    Ok(ChunkSection {
        y,
        section,
        rest: nbt,
    })
}

/// Reads a container's palette with `entry` and its data; anything wrong gives `default` and a warning.
fn read_container(
    nbt: Nbt,
    strategy: Strategy,
    default: u32,
    what: &str,
    warn: &mut impl FnMut(&str, String),
    mut entry: impl FnMut(&Nbt, &mut dyn FnMut(String)) -> u32,
) -> PalettedContainer {
    let Nbt::Compound(nbt) = nbt else {
        warn(what, "not a compound".to_owned());
        return PalettedContainer::filled(strategy, default);
    };
    let palette: Vec<Nbt> = match nbt.get("palette") {
        Some(Nbt::List(List::Compound(entries))) => {
            entries.iter().cloned().map(Nbt::Compound).collect()
        }
        Some(Nbt::List(List::String(entries))) => {
            entries.iter().cloned().map(Nbt::String).collect()
        }
        _ => Vec::new(),
    };
    let data = match nbt.get("data") {
        Some(Nbt::LongArray(data)) => data.iter().map(|&long| long as u64).collect(),
        _ => Vec::new(),
    };
    let palette = palette
        .iter()
        .map(|value| entry(value, &mut |problem| warn(what, problem)))
        .collect();
    PalettedContainer::unpack(strategy, Packed { palette, data }).unwrap_or_else(|e| {
        warn(what, e.to_string());
        PalettedContainer::filled(strategy, default)
    })
}

fn read_block_states(
    nbt: Nbt,
    states: &BlockStates,
    warn: &mut impl FnMut(&str, String),
) -> PalettedContainer {
    read_container(
        nbt,
        block_strategy(states),
        0,
        "block_states",
        warn,
        |entry, warn| {
            // A palette of strings names default states.
            let (name, properties) = match entry {
                Nbt::Compound(entry) => (entry.get("Name"), entry.get("Properties")),
                name => (Some(name), None),
            };
            let Some(Nbt::String(name)) = name else {
                warn("a palette entry has no name".to_owned());
                return 0;
            };
            let properties = match properties {
                Some(Nbt::Compound(properties)) => properties
                    .iter()
                    .filter_map(|(key, value)| match value {
                        Nbt::String(value) => Some((key, value.as_str())),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            match states.state(name, properties) {
                Some((id, ignored)) => {
                    if !ignored.is_empty() {
                        warn(format!("{name} has no {}", ignored.join(", ")));
                    }
                    id
                }
                None => {
                    warn(format!("unknown block {name}, replaced by air"));
                    0
                }
            }
        },
    )
}

fn read_biomes(
    nbt: Nbt,
    biomes: &[&str],
    warn: &mut impl FnMut(&str, String),
) -> PalettedContainer {
    let default = default_biome(biomes);
    read_container(
        nbt,
        biome_strategy(biomes),
        default,
        "biomes",
        warn,
        |entry, warn| {
            let Nbt::String(name) = entry else {
                warn("a palette entry is not a name".to_owned());
                return default;
            };
            match biomes.iter().position(|biome| biome == name) {
                Some(id) => id as u32,
                None => {
                    warn(format!("unknown biome {name}, replaced by {DEFAULT_BIOME}"));
                    default
                }
            }
        },
    )
}

fn write_section(section: &ChunkSection, biomes: &[&str]) -> Compound {
    let mut nbt = Compound::new();
    nbt.insert("Y", section.y);
    if let Some(blocks_and_biomes) = &section.section {
        let states = block_states();
        let block_palette = |id: u32| {
            let (name, properties) = states.properties(id).expect("a known block state");
            let mut entry = Compound::new();
            entry.insert("Name", name);
            if !properties.is_empty() {
                let properties: Compound = properties.into_iter().collect();
                entry.insert("Properties", properties);
            }
            entry
        };
        let packed = blocks_and_biomes.block_states().pack();
        let palette = packed.palette.iter().map(|&id| block_palette(id)).collect();
        nbt.insert(
            "block_states",
            container(Nbt::List(List::Compound(palette)), packed.data),
        );
        let packed = blocks_and_biomes.biomes().pack();
        let palette = packed
            .palette
            .iter()
            .map(|&id| biomes[id as usize].to_owned())
            .collect();
        nbt.insert(
            "biomes",
            container(Nbt::List(List::String(palette)), packed.data),
        );
    }
    for (key, value) in section.rest.iter() {
        nbt.insert(key, value.clone());
    }
    nbt
}

/// A container compound; vanilla leaves `data` out for a single value.
fn container(palette: Nbt, data: Vec<u64>) -> Compound {
    let mut nbt = Compound::new();
    nbt.insert("palette", palette);
    if !data.is_empty() {
        nbt.insert(
            "data",
            Nbt::LongArray(data.into_iter().map(|long| long as i64).collect()),
        );
    }
    nbt
}

fn invalid(error: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIOMES: &[&str] = &["minecraft:badlands", "minecraft:forest", "minecraft:plains"];

    fn block(name: &str, properties: &[(&str, &str)]) -> Compound {
        let mut entry = Compound::new();
        entry.insert("Name", name);
        if !properties.is_empty() {
            let properties: Compound = properties.iter().copied().collect();
            entry.insert("Properties", properties);
        }
        entry
    }

    fn container(palette: Nbt, data: Option<Vec<i64>>) -> Nbt {
        let mut nbt = Compound::new();
        nbt.insert("palette", palette);
        if let Some(data) = data {
            nbt.insert("data", Nbt::LongArray(data));
        }
        Nbt::Compound(nbt)
    }

    fn section(y: i8, block_states: Option<Nbt>, biomes: Option<Nbt>) -> Compound {
        let mut nbt = Compound::new();
        nbt.insert("Y", y);
        if let Some(block_states) = block_states {
            nbt.insert("block_states", block_states);
        }
        if let Some(biomes) = biomes {
            nbt.insert("biomes", biomes);
        }
        nbt.insert("SkyLight", Nbt::ByteArray(vec![-1; 2048]));
        nbt
    }

    /// A chunk as vanilla writes one: a light-only section below the world, a section of stone with oak stairs
    /// in its first block over two biomes, and a section of air in one biome. Palettes are in order of first use.
    fn vanilla_chunk() -> Compound {
        // Stairs (index 0) in the first block, stone (index 1) in the others, 4 bits each.
        let mut stairs_data = vec![0x1111_1111_1111_1111_i64; 256];
        stairs_data[0] = 0x1111_1111_1111_1110;
        // Forest (index 0) in the lower half, badlands (index 1) in the upper half, 1 bit each.
        let biome_data = vec![0xffff_ffff_0000_0000_u64 as i64];
        let mut chunk = Compound::new();
        chunk.insert("DataVersion", 4786);
        chunk.insert("xPos", 3);
        chunk.insert("Status", "minecraft:full");
        let sections = vec![
            section(-5, None, None),
            section(
                -4,
                Some(container(
                    Nbt::List(List::Compound(vec![
                        block(
                            "minecraft:oak_stairs",
                            &[
                                ("facing", "east"),
                                ("half", "top"),
                                ("shape", "straight"),
                                ("waterlogged", "false"),
                            ],
                        ),
                        block("minecraft:stone", &[]),
                    ])),
                    Some(stairs_data),
                )),
                Some(container(
                    Nbt::List(List::String(vec![
                        "minecraft:forest".into(),
                        "minecraft:badlands".into(),
                    ])),
                    Some(biome_data),
                )),
            ),
            section(
                -3,
                Some(container(
                    Nbt::List(List::Compound(vec![block("minecraft:air", &[])])),
                    None,
                )),
                Some(container(
                    Nbt::List(List::String(vec!["minecraft:plains".into()])),
                    None,
                )),
            ),
        ];
        chunk.insert("sections", Nbt::List(List::Compound(sections)));
        chunk
    }

    #[test]
    fn reads_sections_into_global_ids() {
        let (chunk, warnings) = Chunk::read(vanilla_chunk(), BIOMES).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(chunk.sections.len(), 3);
        assert_eq!(chunk.sections[0].y, -5);
        assert!(chunk.sections[0].section.is_none());
        assert!(chunk.sections[0].rest.get("SkyLight").is_some());

        let section = chunk.sections[1].section.as_ref().unwrap();
        let states = block_states();
        let stairs = states
            .state(
                "minecraft:oak_stairs",
                [("facing", "east"), ("half", "top")],
            )
            .unwrap()
            .0;
        assert_eq!(section.block_state(0, 0, 0), stairs);
        assert_eq!(section.block_state(1, 0, 0), 1);
        assert_eq!(section.block_state(15, 15, 15), 1);
        // Biomes 0 to 31 are forest, 32 to 63 badlands.
        assert_eq!(section.biome(0, 0, 0), 1);
        assert_eq!(section.biome(3, 3, 3), 0);
        assert_eq!(
            chunk.sections[2].section.as_ref().unwrap().biome(0, 0, 0),
            2
        );
        assert_eq!(chunk.rest.get("Status"), Some(&Nbt::from("minecraft:full")));
        assert!(chunk.rest.get("sections").is_none());
    }

    #[test]
    fn an_unchanged_chunk_is_written_as_it_was_read() {
        let (chunk, _) = Chunk::read(vanilla_chunk(), BIOMES).unwrap();
        assert_eq!(chunk.to_nbt(BIOMES), vanilla_chunk());
    }

    #[test]
    fn changed_sections_are_written_compacted() {
        let (mut chunk, _) = Chunk::read(vanilla_chunk(), BIOMES).unwrap();
        let section = chunk.sections[1].section.as_mut().unwrap();
        section.set_block_state(0, 0, 0, 1);
        let nbt = chunk.to_nbt(BIOMES);
        let Some(Nbt::List(List::Compound(sections))) = nbt.get("sections") else {
            panic!("no sections");
        };
        let Some(Nbt::Compound(blocks)) = sections[1].get("block_states") else {
            panic!("no block states");
        };
        // Only stone is left: one palette entry and no data.
        assert_eq!(
            blocks.get("palette"),
            Some(&Nbt::List(List::Compound(vec![block(
                "minecraft:stone",
                &[]
            )])))
        );
        assert!(blocks.get("data").is_none());
        let (again, warnings) = Chunk::read(nbt, BIOMES).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            again.sections[1]
                .section
                .as_ref()
                .unwrap()
                .block_state(0, 0, 0),
            1
        );
    }

    #[test]
    fn damaged_sections_fall_back_to_defaults_with_warnings() {
        let mut chunk = Compound::new();
        let sections = vec![
            section(
                0,
                Some(container(
                    Nbt::List(List::Compound(vec![
                        block("minecraft:not_a_block", &[]),
                        block("minecraft:stone", &[("color", "red")]),
                    ])),
                    // The wrong length for two entries.
                    Some(vec![0; 10]),
                )),
                Some(container(
                    Nbt::List(List::String(vec!["example:moon".into()])),
                    None,
                )),
            ),
            // No containers at all for a section that has blocks: air and plains.
            section(1, Some(Nbt::Int(3)), None),
        ];
        chunk.insert("sections", Nbt::List(List::Compound(sections)));
        let (chunk, warnings) = Chunk::read(chunk, BIOMES).unwrap();
        assert_eq!(warnings.len(), 5, "{warnings:?}");
        assert!(warnings[0].contains("unknown block minecraft:not_a_block"));
        assert!(warnings[1].contains("minecraft:stone has no color"));
        assert!(warnings[2].contains("Invalid length given for storage"));
        assert!(warnings[3].contains("unknown biome example:moon"));
        assert!(warnings[4].contains("not a compound"));
        for entry in &chunk.sections {
            let section = entry.section.as_ref().unwrap();
            assert_eq!(section.block_state(4, 4, 4), 0);
            assert_eq!(section.biome(1, 1, 1), 2);
        }
    }

    #[test]
    fn a_malformed_section_list_is_an_error() {
        let mut chunk = Compound::new();
        chunk.insert("sections", Nbt::Int(1));
        assert!(Chunk::read(chunk, BIOMES).is_err());
        let mut chunk = Compound::new();
        chunk.insert("sections", Nbt::List(List::Compound(vec![Compound::new()])));
        assert!(Chunk::read(chunk, BIOMES).is_err());
    }
}
