//! The light of chunks, computed on worker threads.
//!
//! A chunk generated through carvers or features arrives without light. Its light is computed once, from its
//! blocks and its eight neighbors, as soon as they are loaded, and shared by every player it is sent to. A final
//! chunk that ran FEATURES is then saved with it, at vanilla's `light` status.
//!
//! A lit chunk is lit again the same way when a block within 15 blocks of it changes its light, and the new light
//! is written into its sections.

use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use harpas_anvil::{Chunk, ChunkSection};
use harpas_core::chunk::Section;
use harpas_core::nbt::{Compound, Nbt};
use harpas_worldgen::{LightWorld, light_chunk};

use crate::block::BlockPos;
use crate::player::NEIGHBORS;
use crate::world::ChunkPos;

const MIN_SECTION: i32 = -4;
const SECTIONS: usize = 24;

/// One section's sky or block light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layer {
    /// Every block has this level. Most layers are all fifteen or all zero, and are kept as one value.
    Uniform(u8),
    /// Four bits per block, packed as vanilla packs them.
    Values(Box<[u8]>),
}

impl Layer {
    fn is_valid(&self) -> bool {
        match self {
            Self::Uniform(level) => *level < 16,
            Self::Values(bytes) => bytes.len() == LAYER_BYTES,
        }
    }

    fn new(values: Vec<u8>) -> Self {
        match values.first() {
            Some(&first) if first >> 4 == first & 15 && values.iter().all(|v| *v == first) => {
                Self::Uniform(first & 15)
            }
            _ => Self::Values(values.into()),
        }
    }

    fn at(&self, index: usize) -> u8 {
        match self {
            Self::Uniform(level) => *level,
            Self::Values(bytes) => nibble(bytes[index / 2], index),
        }
    }
}

fn nibble(byte: u8, index: usize) -> u8 {
    (byte >> ((index & 1) * 4)) & 15
}

fn light_index(pos: BlockPos) -> usize {
    (((pos.y & 15) << 8) | ((pos.z & 15) << 4) | (pos.x & 15)) as usize
}

/// One section's light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionLight {
    /// The section's height, in sections.
    pub y: i32,
    /// Sky light.
    pub sky: Layer,
    /// Block light.
    pub block: Layer,
}

/// The computed light of one chunk, from the light section below the world to the one above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Light {
    /// The sections, lowest first.
    pub sections: Vec<SectionLight>,
}

impl Light {
    pub(crate) fn is_valid(&self) -> bool {
        let mut seen = [false; SECTIONS + 2];
        self.sections.iter().all(|section| {
            let Some(index) = section
                .y
                .checked_sub(MIN_SECTION - 1)
                .and_then(|index| seen.get_mut(index as usize))
            else {
                return false;
            };
            !std::mem::replace(index, true) && section.sky.is_valid() && section.block.is_valid()
        })
    }

    /// Whether a full sky layer can be omitted without changing the light inherited from above.
    pub fn inherits_full_sky(&self, y: i32) -> bool {
        self.sections
            .iter()
            .filter(|section| section.y > y)
            .min_by_key(|section| section.y)
            .is_none_or(|above| match &above.sky {
                Layer::Uniform(level) => *level == 15,
                Layer::Values(bytes) => bytes
                    .get(..SKY_ROW_BYTES)
                    .is_some_and(|row| row.iter().all(|&byte| byte == 255)),
            })
    }

    pub(crate) fn brightness(&self, pos: BlockPos) -> Option<u8> {
        let section = self
            .sections
            .iter()
            .find(|section| section.y == pos.y >> 4)?;
        let index = light_index(pos);
        Some(section.block.at(index).max(section.sky.at(index)))
    }
}

/// Raw brightness in a lit Anvil chunk. A missing sky layer inherits light from the bottom of the
/// next stored sky layer above it, or from the open sky if there is none.
pub(crate) fn saved_brightness(chunk: &Chunk, pos: BlockPos) -> Option<u8> {
    if chunk.rest.get("isLightOn") != Some(&Nbt::Byte(1)) {
        return None;
    }
    let section_y = pos.y >> 4;
    let index = light_index(pos);
    let block = chunk
        .sections
        .iter()
        .find(|section| i32::from(section.y) == section_y)
        .and_then(|section| saved_layer(&section.rest, "BlockLight"))
        .map_or(0, |bytes| nibble(bytes[index / 2] as u8, index));
    let sky = saved_sky(chunk, section_y).map_or(15, |(y, bytes)| {
        let index = if y == section_y { index } else { index & 255 };
        nibble(bytes[index / 2] as u8, index)
    });
    Some(block.max(sky))
}

fn saved_sky(chunk: &Chunk, section_y: i32) -> Option<(i32, &[i8])> {
    chunk
        .sections
        .iter()
        .filter(|section| i32::from(section.y) >= section_y)
        .filter_map(|section| {
            Some((
                i32::from(section.y),
                saved_layer(&section.rest, "SkyLight")?,
            ))
        })
        .min_by_key(|(y, _)| *y)
}

fn saved_layer<'a>(rest: &'a Compound, name: &str) -> Option<&'a [i8]> {
    match rest.get(name) {
        Some(Nbt::ByteArray(bytes)) if bytes.len() == LAYER_BYTES => Some(bytes),
        _ => None,
    }
}

pub(crate) fn has_valid_saved_light(chunk: &Chunk) -> bool {
    chunk.rest.get("isLightOn") == Some(&Nbt::Byte(1)) && valid_saved_layers(chunk)
}

fn valid_saved_layers(chunk: &Chunk) -> bool {
    chunk.sections.iter().all(|section| {
        ["SkyLight", "BlockLight"].into_iter().all(|name| {
            section.rest.get(name).is_none() || saved_layer(&section.rest, name).is_some()
        })
    })
}

/// The chunk being lit and the chunks around it, with their sections by height.
struct Volume<'a> {
    position: ChunkPos,
    /// Indexed by `(dz + 1) * 3 + (dx + 1)`.
    sections: [[Option<&'a Section>; SECTIONS]; 9],
}

impl LightWorld for Volume<'_> {
    fn min_y(&self) -> i32 {
        MIN_SECTION * 16
    }

    fn height(&self) -> i32 {
        SECTIONS as i32 * 16
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> u32 {
        let dx = x.div_euclid(16) - self.position.x;
        let dz = z.div_euclid(16) - self.position.z;
        let section = y.div_euclid(16) - MIN_SECTION;
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dz) {
            return 0;
        }
        usize::try_from(section)
            .ok()
            .and_then(|section| self.sections[((dz + 1) * 3 + dx + 1) as usize].get(section))
            .copied()
            .flatten()
            .map_or(0, |section| {
                section.block_state((x & 15) as usize, (y & 15) as usize, (z & 15) as usize)
            })
    }
}

/// Lights `chunk` at `position` from its blocks and those of its neighbors, clockwise from the northwest.
/// A missing neighbor counts as air.
pub fn light(position: ChunkPos, chunk: &Chunk, neighbors: &[Option<Arc<Chunk>>; 8]) -> Light {
    let mut sections = [[None; SECTIONS]; 9];
    for (slot, source) in sections.iter_mut().zip(volume_chunks(chunk, neighbors)) {
        let Some(source) = source else { continue };
        for section in &source.sections {
            let Ok(at) = usize::try_from(i32::from(section.y) - MIN_SECTION) else {
                continue;
            };
            if let Some(entry) = slot.get_mut(at) {
                *entry = section.section.as_ref();
            }
        }
    }
    let volume = Volume { position, sections };
    Light {
        sections: light_chunk(&volume, position.x, position.z)
            .sections
            .into_iter()
            .map(|section| SectionLight {
                y: section.y,
                sky: Layer::new(section.sky),
                block: Layer::new(section.block),
            })
            .collect(),
    }
}

/// The bytes of a saved layer: four bits per block, as vanilla's `DataLayer` stores them.
fn layer_bytes(layer: &Layer) -> Vec<i8> {
    match layer {
        Layer::Uniform(level) => vec![(level << 4 | level) as i8; LAYER_BYTES],
        Layer::Values(values) => values.iter().map(|&byte| byte as i8).collect(),
    }
}

const LAYER_BYTES: usize = 2048;
const SKY_ROW_BYTES: usize = LAYER_BYTES / 16;

/// `chunk` at vanilla's `light` status, with `light` in its sections, as vanilla saves a chunk it has lit.
pub fn with_light(chunk: &Chunk, light: &Light) -> Chunk {
    let mut lit = chunk.clone();
    write_light(&mut lit, light);
    if lit.rest.get("isLightOn") == Some(&Nbt::Byte(1)) {
        lit.rest.insert("Status", "minecraft:light");
    }
    lit
}

/// Writes `light` into the sections of `chunk`, keeping its status.
///
/// A full sky layer is left out when it can be inherited from above, and every block layer is written, so
/// the chunk is sent with the same light whether it was computed or read back. `isLightOn` tells vanilla that
/// the light is correct, so it keeps it instead of lighting the chunk again.
pub fn write_light(chunk: &mut Chunk, light: &Light) {
    if !light.is_valid() {
        chunk.rest.insert("isLightOn", Nbt::Byte(0));
        return;
    }
    for section in &light.sections {
        let y = section.y as i8;
        let index = match chunk.sections.iter().position(|saved| saved.y == y) {
            Some(index) => index,
            None => {
                chunk.sections.push(ChunkSection {
                    y,
                    section: None,
                    rest: Compound::new(),
                });
                chunk.sections.len() - 1
            }
        };
        let rest = &mut chunk.sections[index].rest;
        if section.sky == Layer::Uniform(15) && light.inherits_full_sky(section.y) {
            rest.remove("SkyLight");
        } else {
            rest.insert("SkyLight", Nbt::ByteArray(layer_bytes(&section.sky)));
        }
        rest.insert("BlockLight", Nbt::ByteArray(layer_bytes(&section.block)));
    }
    chunk.sections.sort_by_key(|section| section.y);
    let valid = valid_saved_layers(chunk);
    chunk.rest.insert("isLightOn", Nbt::Byte(i8::from(valid)));
}

/// The light represented by saved layers: missing sky inherits the bottom row of the next layer above.
pub fn saved_light(chunk: &Chunk, like: &Light) -> Light {
    let layer = |rest: Option<&Compound>, name: &str, missing: u8| match rest
        .and_then(|r| saved_layer(r, name))
    {
        Some(bytes) => Layer::new(bytes.iter().map(|&byte| byte as u8).collect()),
        _ => Layer::Uniform(missing),
    };
    Light {
        sections: like
            .sections
            .iter()
            .map(|section| {
                let rest = chunk
                    .sections
                    .iter()
                    .find(|saved| i32::from(saved.y) == section.y)
                    .map(|saved| &saved.rest);
                SectionLight {
                    y: section.y,
                    sky: saved_sky(chunk, section.y).map_or(Layer::Uniform(15), |(y, bytes)| {
                        let bytes = if y == section.y {
                            bytes.iter().map(|&byte| byte as u8).collect()
                        } else {
                            bytes[..SKY_ROW_BYTES]
                                .iter()
                                .cycle()
                                .take(LAYER_BYTES)
                                .map(|&byte| byte as u8)
                                .collect()
                        };
                        Layer::new(bytes)
                    }),
                    block: layer(rest, "BlockLight", 0),
                }
            })
            .collect(),
    }
}

/// The sections of `new` whose sky or block light differ from those of `old`, with both layers.
pub fn changed_sections(old: &Light, new: &Light) -> Light {
    Light {
        sections: new
            .sections
            .iter()
            .filter(|section| !old.sections.contains(section))
            .cloned()
            .collect(),
    }
}

/// The nine chunks of a light volume in the order of [`Volume::sections`].
fn volume_chunks<'a>(
    chunk: &'a Chunk,
    neighbors: &'a [Option<Arc<Chunk>>; 8],
) -> [Option<&'a Chunk>; 9] {
    let mut chunks = [None; 9];
    chunks[4] = Some(chunk);
    for (&(dx, dz), neighbor) in NEIGHBORS.iter().zip(neighbors) {
        chunks[((dz + 1) * 3 + dx + 1) as usize] = neighbor.as_deref();
    }
    chunks
}

/// A chunk to light, with the chunks it is lit from.
pub(crate) struct Job {
    pub(crate) pos: ChunkPos,
    /// Tells this request from an earlier one for the same chunk, made before the chunk changed.
    pub(crate) id: u64,
    pub(crate) chunk: Arc<Chunk>,
    pub(crate) neighbors: [Option<Arc<Chunk>>; 8],
}

pub(crate) struct Lit {
    pub(crate) pos: ChunkPos,
    pub(crate) id: u64,
    /// `None` when the computation failed.
    pub(crate) light: Option<Arc<Light>>,
}

/// The threads that light chunks, fed from a bounded queue.
pub(crate) struct Lighter {
    pub(crate) requests: Option<SyncSender<Job>>,
    pub(crate) completed: Receiver<Lit>,
    threads: Vec<JoinHandle<()>>,
}

impl Lighter {
    pub(crate) fn start(workers: usize, capacity: usize) -> io::Result<Self> {
        let (requests, receiver) = mpsc::sync_channel::<Job>(capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        let (sender, completed) = mpsc::channel();
        let mut lighter = Self {
            requests: Some(requests),
            completed,
            threads: Vec::with_capacity(workers),
        };
        for index in 0..workers {
            let receiver = Arc::clone(&receiver);
            let sender = sender.clone();
            // A failure here drops `lighter`, which stops the threads already started.
            lighter.threads.push(
                thread::Builder::new()
                    .name(format!("harpas-light-{index}"))
                    .spawn(move || {
                        loop {
                            let request = receiver.lock().unwrap_or_else(|e| e.into_inner()).recv();
                            let Ok(job) = request else { break };
                            let light =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    Arc::new(light(job.pos, &job.chunk, &job.neighbors))
                                }))
                                .ok();
                            let lit = Lit {
                                pos: job.pos,
                                id: job.id,
                                light,
                            };
                            if sender.send(lit).is_err() {
                                break;
                            }
                        }
                    })?,
            );
        }
        Ok(lighter)
    }
}

impl Drop for Lighter {
    fn drop(&mut self) {
        self.requests.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relight_detects_dark_sky_inherited_from_another_section() {
        let mut saved = Chunk {
            sections: vec![ChunkSection {
                y: 1,
                rest: Compound::from_iter([("SkyLight", Nbt::ByteArray(vec![0; LAYER_BYTES]))]),
                section: None,
            }],
            rest: Compound::from_iter([("isLightOn", Nbt::Byte(1))]),
        };
        let sky = Light {
            sections: vec![SectionLight {
                y: 0,
                sky: Layer::Uniform(15),
                block: Layer::Uniform(0),
            }],
        };
        assert!(has_valid_saved_light(&saved));
        let previous = saved_light(&saved, &sky);
        assert_eq!(previous.sections[0].sky, Layer::Uniform(0));
        assert_eq!(changed_sections(&previous, &sky), sky);
        saved.sections[0]
            .rest
            .insert("BlockLight", Nbt::ByteArray(vec![0]));
        assert!(!has_valid_saved_light(&saved));
        let mut invalid = sky;
        invalid.sections[0].sky = Layer::Uniform(16);
        write_light(&mut saved, &invalid);
        assert_eq!(saved.rest.get("isLightOn"), Some(&Nbt::Byte(0)));
    }

    #[test]
    fn a_lit_chunk_is_saved_at_light_with_every_layer_but_full_sky() {
        let states = harpas_core::block::block_states();
        let blocks = harpas_core::chunk::PalettedContainer::filled(
            harpas_core::chunk::Strategy::block_states(states.global_bits()),
            states.default_state("minecraft:stone").unwrap(),
        );
        let biomes = harpas_core::chunk::PalettedContainer::filled(
            harpas_core::chunk::Strategy::biomes(1),
            0,
        );
        let mut rest = Compound::new();
        rest.insert("Status", "minecraft:features");
        let mut section_rest = Compound::new();
        section_rest.insert("SkyLight", Nbt::ByteArray(vec![1; LAYER_BYTES]));
        let chunk = Chunk {
            sections: vec![ChunkSection {
                y: 1,
                section: Some(Section::new(blocks, biomes)),
                rest: section_rest,
            }],
            rest,
        };
        let mut values = vec![0x21_u8; LAYER_BYTES];
        values[7] = 0xf0;
        let sections = (-5..=20)
            .map(|y| SectionLight {
                y,
                sky: match y {
                    ..0 => Layer::Uniform(0),
                    0 => Layer::Values(values.clone().into()),
                    _ => Layer::Uniform(15),
                },
                block: if y == 20 {
                    Layer::Uniform(3)
                } else {
                    Layer::Uniform(0)
                },
            })
            .collect();
        let lit = with_light(&chunk, &Light { sections });
        assert_eq!(lit.rest.get("Status"), Some(&Nbt::from("minecraft:light")));
        assert_eq!(lit.rest.get("isLightOn"), Some(&Nbt::Byte(1)));
        let ys: Vec<_> = lit.sections.iter().map(|section| section.y).collect();
        assert_eq!(ys, (-5..=20).collect::<Vec<i8>>());
        let at = |y: i8| &lit.sections[(y + 5) as usize];
        assert!(at(-5).section.is_none() && at(20).section.is_none());
        assert_eq!(at(1).section, chunk.sections[0].section);
        assert_eq!(
            at(-5).rest.get("SkyLight"),
            Some(&Nbt::ByteArray(vec![0; LAYER_BYTES]))
        );
        let packed: Vec<_> = values.iter().map(|&byte| byte as i8).collect();
        assert_eq!(at(0).rest.get("SkyLight"), Some(&Nbt::ByteArray(packed)));
        // Full sky is left out, also where an earlier save had a layer.
        assert_eq!(at(1).rest.get("SkyLight"), None);
        assert_eq!(
            at(20).rest.get("BlockLight"),
            Some(&Nbt::ByteArray(vec![0x33; LAYER_BYTES]))
        );
        assert_eq!(
            at(1).rest.get("BlockLight"),
            Some(&Nbt::ByteArray(vec![0; LAYER_BYTES]))
        );
    }
}
