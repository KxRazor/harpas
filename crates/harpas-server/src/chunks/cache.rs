//! Share complete compressed columns, including NBT and light, by immutable snapshot identity.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use harpas_anvil::Chunk;
use harpas_engine::light::Light;
use harpas_engine::player::VisibleChunk;
use harpas_engine::world::ChunkPos;
use harpas_java::{Connection, PreparedPlayPacket};

const MAX_ENTRIES: usize = 4096;
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// One Overworld and Java 26.1 registry, shared by every connection in a server environment.
pub(crate) struct Cache {
    registry: crate::block_entity_registry::Registry,
    state: Mutex<State>,
    max_entries: usize,
    max_bytes: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    position: ChunkPos,
    chunk: usize,
    light: usize,
    threshold: Option<usize>,
}

struct Entry {
    // Weak references prevent allocation addresses from being reused without retaining the chunk's data.
    // They also make Arc::make_mut detach even when the world is the snapshot's only remaining owner.
    _chunk: Option<Weak<Chunk>>,
    _light: Option<Weak<Light>>,
    packet: OnceLock<Arc<PreparedPlayPacket>>,
}

struct Cached {
    entry: Arc<Entry>,
    bytes: usize,
}

#[derive(Default)]
struct State {
    entries: HashMap<Key, Cached>,
    order: VecDeque<Key>,
    bytes: usize,
}

impl State {
    fn trim(&mut self, max_entries: usize, max_bytes: usize) {
        while self.entries.len() > max_entries || self.bytes > max_bytes {
            let key = self.order.pop_front().expect("each entry has a FIFO slot");
            let cached = self.entries.remove(&key).expect("a queued entry exists");
            self.bytes -= cached.bytes;
        }
    }
}

impl Cache {
    pub(crate) fn new(registry: crate::block_entity_registry::Registry) -> Self {
        Self {
            registry,
            state: Mutex::default(),
            max_entries: MAX_ENTRIES,
            max_bytes: MAX_BYTES,
        }
    }

    pub(crate) fn prepare(
        &self,
        visible: VisibleChunk,
        connection: &mut Connection,
    ) -> Arc<PreparedPlayPacket> {
        let key = Key {
            position: visible.position,
            chunk: visible
                .chunk
                .as_ref()
                .map_or(0, |arc| Arc::as_ptr(arc) as usize),
            light: visible
                .light
                .as_ref()
                .map_or(0, |arc| Arc::as_ptr(arc) as usize),
            threshold: connection.compression_threshold(),
        };
        let entry = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let entry = if let Some(cached) = state.entries.get(&key) {
                Arc::clone(&cached.entry)
            } else {
                let entry = Arc::new(Entry {
                    _chunk: visible.chunk.as_ref().map(Arc::downgrade),
                    _light: visible.light.as_ref().map(Arc::downgrade),
                    packet: OnceLock::new(),
                });
                state.order.push_back(key);
                state.entries.insert(
                    key,
                    Cached {
                        entry: Arc::clone(&entry),
                        bytes: 0,
                    },
                );
                entry
            };
            state.trim(self.max_entries, self.max_bytes);
            entry
        };
        // Only viewers of this column wait for its first encoding. Other columns encode in parallel;
        // neither encoding nor compression holds the cache lock or runs on the logic thread.
        let packet = Arc::clone(entry.packet.get_or_init(|| {
            Arc::new(connection.prepare_play_packet(super::encode(visible, &self.registry).into()))
        }));
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = state.entries.get_mut(&key)
            && Arc::ptr_eq(&cached.entry, &entry)
            && cached.bytes == 0
        {
            cached.bytes = packet.retained_bytes();
            state.bytes += packet.retained_bytes();
            state.trim(self.max_entries, self.max_bytes);
        }
        packet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harpas_anvil::ChunkSection;
    use harpas_core::chunk::{PalettedContainer, Section, Strategy};
    use harpas_core::nbt::{Compound, List, Nbt};
    use harpas_java::{FrameReader, ServerInfo, ServerKey, write_frame};
    use harpas_protocol_java::v26_1::{configuration, handshaking, login, play};

    fn connection(threshold: i32) -> Connection {
        static KEY: OnceLock<Arc<ServerKey>> = OnceLock::new();
        let mut connection = Connection::new(Arc::clone(
            KEY.get_or_init(|| Arc::new(ServerKey::generate())),
        ));
        let info = ServerInfo {
            online_mode: false,
            compression_threshold: threshold,
            ..ServerInfo::default()
        };
        let handshake: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
            protocol_version: 775,
            server_address: "localhost".into(),
            server_port: 25565,
            intent: handshaking::Intent::Login,
        }
        .into();
        let login: login::serverbound::Packet = login::serverbound::LoginStart {
            name: "Steve".into(),
            player_uuid: 0,
        }
        .into();
        let mut wire = Vec::new();
        write_frame(&mut wire, |buf| handshake.encode(buf));
        write_frame(&mut wire, |buf| login.encode(buf));
        connection.receive(&wire, &info).unwrap();
        assert_eq!(
            connection.compression_threshold(),
            usize::try_from(threshold).ok()
        );
        connection
    }

    fn visible() -> VisibleChunk {
        let section = Section::new(
            PalettedContainer::filled(Strategy::block_states(16), 0),
            PalettedContainer::filled(Strategy::biomes(7), 1),
        );
        VisibleChunk {
            position: ChunkPos { x: -2, z: 3 },
            chunk: Some(Arc::new(Chunk {
                sections: vec![ChunkSection {
                    y: 0,
                    section: Some(section),
                    rest: Compound::new(),
                }],
                rest: Compound::new(),
            })),
            light: None,
        }
    }

    fn play_connection() -> Connection {
        let mut connection = connection(-1);
        connection.use_external_chunks();
        let info = ServerInfo {
            online_mode: false,
            compression_threshold: -1,
            ..ServerInfo::default()
        };
        let ack: login::serverbound::Packet = login::serverbound::LoginAcknowledged.into();
        let packs: configuration::serverbound::Packet =
            configuration::serverbound::SelectKnownPacks {
                packs: vec![configuration::KnownPack {
                    namespace: "minecraft".into(),
                    id: "core".into(),
                    version: "26.1".into(),
                }],
            }
            .into();
        let done: configuration::serverbound::Packet =
            configuration::serverbound::AcknowledgeFinishConfiguration.into();
        let mut wire = Vec::new();
        write_frame(&mut wire, |buf| ack.encode(buf));
        write_frame(&mut wire, |buf| packs.encode(buf));
        write_frame(&mut wire, |buf| done.encode(buf));
        connection.receive(&wire, &info).unwrap();
        assert_eq!(connection.state(), harpas_java::State::Play);
        connection.take_output();
        connection
    }

    fn decode(
        connection: &mut Connection,
        packet: &PreparedPlayPacket,
    ) -> play::clientbound::Packet {
        assert!(connection.send_prepared_play_packet(packet));
        let mut reader = FrameReader::default();
        reader.push(&connection.take_output());
        let packet =
            play::clientbound::Packet::decode(&reader.next_frame().unwrap().unwrap()).unwrap();
        assert_eq!(reader.next_frame().unwrap(), None);
        packet
    }

    #[test]
    fn simultaneous_viewers_share_one_compressed_packet() {
        let cache = Cache::new(crate::block_entity_registry::Registry::default());
        let visible = visible();
        let barrier = std::sync::Barrier::new(8);
        let packets = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut connection = connection(0);
                        barrier.wait();
                        cache.prepare(visible.clone(), &mut connection)
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        for packet in &packets {
            assert!(Arc::ptr_eq(packet, &packets[0]));
        }
        let state = cache.state.lock().unwrap();
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.bytes, packets[0].retained_bytes());
    }

    #[test]
    fn all_chunk_contents_and_computed_light_invalidate_the_packet() {
        let cache = Cache::new(crate::block_entity_registry::Registry::synthetic(&[(
            "minecraft:sign",
            47,
        )]));
        let original = visible();
        let mut connection = play_connection();
        let first = cache.prepare(original.clone(), &mut connection);
        let original_packet = decode(&mut connection, &first);
        assert_eq!(
            original_packet,
            super::super::encode(original.clone(), &cache.registry).into()
        );
        for field in [
            "blocks",
            "biomes",
            "heightmaps",
            "block_entities",
            "saved_light",
        ] {
            let mut changed = original.clone();
            let chunk = Arc::make_mut(changed.chunk.as_mut().unwrap());
            match field {
                "blocks" => {
                    chunk.sections[0]
                        .section
                        .as_mut()
                        .unwrap()
                        .set_block_state(0, 0, 0, 1);
                }
                "biomes" => {
                    chunk.sections[0]
                        .section
                        .as_mut()
                        .unwrap()
                        .set_biome(0, 0, 0, 2);
                }
                "heightmaps" => {
                    let mut maps = Compound::new();
                    maps.insert("MOTION_BLOCKING", Nbt::LongArray(vec![42; 37]));
                    chunk.rest.insert("Heightmaps", maps);
                }
                "block_entities" => {
                    let mut sign = Compound::new();
                    sign.insert("id", "minecraft:sign");
                    sign.insert("x", -32);
                    sign.insert("y", 1);
                    sign.insert("z", 48);
                    sign.insert("is_waxed", 1_i8);
                    chunk
                        .rest
                        .insert("block_entities", Nbt::List(List::Compound(vec![sign])));
                }
                "saved_light" => {
                    chunk.sections[0]
                        .rest
                        .insert("SkyLight", Nbt::ByteArray(vec![7; 2048]));
                }
                _ => unreachable!(),
            }
            let expected = super::super::encode(changed.clone(), &cache.registry).into();
            let prepared = cache.prepare(changed, &mut connection);
            assert!(!Arc::ptr_eq(&first, &prepared), "{field}");
            let actual = decode(&mut connection, &prepared);
            assert_ne!(original_packet, actual, "{field}");
            assert_eq!(actual, expected, "{field}");
        }
        let mut changed = original.clone();
        changed.light = Some(Arc::new(Light {
            sections: Vec::new(),
        }));
        let light = cache.prepare(changed.clone(), &mut connection);
        assert!(!Arc::ptr_eq(&first, &light));
        Arc::make_mut(changed.light.as_mut().unwrap())
            .sections
            .push(harpas_engine::light::SectionLight {
                y: 0,
                sky: harpas_engine::light::Layer::Uniform(7),
                block: harpas_engine::light::Layer::Uniform(0),
            });
        let expected = super::super::encode(changed.clone(), &cache.registry).into();
        let changed_light = cache.prepare(changed, &mut connection);
        assert!(!Arc::ptr_eq(&light, &changed_light));
        assert_eq!(decode(&mut connection, &changed_light), expected);
        assert!(Arc::ptr_eq(
            &first,
            &cache.prepare(original, &mut connection)
        ));
    }

    #[test]
    fn compression_position_registry_and_reloaded_snapshots_are_separate() {
        let cache = Cache::new(crate::block_entity_registry::Registry::default());
        let mut uncompressed = connection(-1);
        let mut compressed = connection(256);
        let mut different_threshold = connection(0);
        let original = visible();
        let first = cache.prepare(original.clone(), &mut uncompressed);
        assert!(!Arc::ptr_eq(
            &first,
            &cache.prepare(original.clone(), &mut compressed)
        ));
        let compressed_packet = cache.prepare(original.clone(), &mut compressed);
        assert!(!Arc::ptr_eq(
            &compressed_packet,
            &cache.prepare(original.clone(), &mut different_threshold)
        ));
        let mut other_position = original.clone();
        other_position.position.x += 1;
        assert!(!Arc::ptr_eq(
            &first,
            &cache.prepare(other_position, &mut uncompressed)
        ));
        let reloaded = VisibleChunk {
            chunk: original.chunk.as_deref().cloned().map(Arc::new),
            ..original.clone()
        };
        assert!(!Arc::ptr_eq(
            &first,
            &cache.prepare(reloaded, &mut uncompressed)
        ));
        // A cache belongs to one registry and world, even when snapshots happen to be shared.
        let other_world = Cache::new(crate::block_entity_registry::Registry::synthetic(&[(
            "minecraft:sign",
            99,
        )]));
        assert!(!Arc::ptr_eq(
            &first,
            &other_world.prepare(original, &mut uncompressed)
        ));
    }

    #[test]
    fn unloading_and_copy_on_write_do_not_reuse_stale_packets() {
        let cache = Cache::new(crate::block_entity_registry::Registry::default());
        let mut connection = connection(-1);
        let mut visible = visible();
        let snapshot = Arc::downgrade(visible.chunk.as_ref().unwrap());
        let first = cache.prepare(visible.clone(), &mut connection);
        assert_eq!(Arc::strong_count(visible.chunk.as_ref().unwrap()), 1);
        Arc::make_mut(visible.chunk.as_mut().unwrap())
            .rest
            .insert("Heightmaps", Compound::new());
        assert!(snapshot.upgrade().is_none());
        assert!(!Arc::ptr_eq(
            &first,
            &cache.prepare(visible.clone(), &mut connection)
        ));
        let snapshot = Arc::downgrade(visible.chunk.as_ref().unwrap());
        drop(visible);
        assert!(
            snapshot.upgrade().is_none(),
            "the cache must not keep unloaded chunk data alive"
        );
    }

    #[test]
    fn entry_and_buffer_limits_evict_packets_without_invalidating_users() {
        let mut cache = Cache::new(crate::block_entity_registry::Registry::default());
        cache.max_entries = 2;
        let mut connection = connection(-1);
        let original = visible();
        let first = cache.prepare(original.clone(), &mut connection);
        for x in 0..10 {
            cache.prepare(
                VisibleChunk {
                    position: ChunkPos { x, z: 0 },
                    chunk: None,
                    light: None,
                },
                &mut connection,
            );
        }
        assert_eq!(cache.state.lock().unwrap().entries.len(), 2);
        assert!(!Arc::ptr_eq(
            &first,
            &cache.prepare(original.clone(), &mut connection)
        ));
        assert!(first.retained_bytes() > 0);
        cache.max_bytes = first.retained_bytes();
        for x in 0..10 {
            cache.prepare(
                VisibleChunk {
                    position: ChunkPos { x, z: 0 },
                    chunk: None,
                    light: None,
                },
                &mut connection,
            );
            assert!(cache.state.lock().unwrap().bytes <= cache.max_bytes);
        }
        cache.max_bytes = 0;
        cache.prepare(original, &mut connection);
        let state = cache.state.lock().unwrap();
        assert!(state.entries.is_empty());
        assert!(state.order.is_empty());
        assert_eq!(state.bytes, 0);
    }
}
