//! The light of generated chunks, computed on worker threads.
//!
//! A chunk generated through carvers or features is saved without light. Its light is computed once, from
//! its blocks and its eight neighbors, as soon as they are loaded, and shared by every player it is sent to.

use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use harpas_anvil::Chunk;
use harpas_core::chunk::Section;
use harpas_worldgen::{LightWorld, light_chunk};

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
    fn new(values: Vec<u8>) -> Self {
        match values.first() {
            Some(&first) if first >> 4 == first & 15 && values.iter().all(|v| *v == first) => {
                Self::Uniform(first & 15)
            }
            _ => Self::Values(values.into()),
        }
    }
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
