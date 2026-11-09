//! Encode loaded overworld chunks for Java connections, off the logic thread.

use harpas_anvil::{Chunk, ChunkSection};
use harpas_core::block::block_states;
use harpas_core::chunk::{PalettedContainer, Section};
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::player::VisibleChunk;
use harpas_protocol_java::codec;
use harpas_protocol_java::v26_1::play::{self, clientbound};
use harpas_worldgen::{ChunkLight, LightWorld, light_chunk};
use std::sync::OnceLock;

const MIN_SECTION: i8 = -4;
const MAX_SECTION: i8 = 19;
const LIGHT_MIN_SECTION: i8 = MIN_SECTION - 1;
const LIGHT_MAX_SECTION: i8 = MAX_SECTION + 1;
const LIGHT_BYTES: usize = 2048;

/// Builds a Java 26.1 chunk packet. This runs on the player's connection thread.
pub(crate) fn encode(
    visible: VisibleChunk,
    registry: &crate::block_entity_registry::Registry,
) -> clientbound::ChunkDataAndUpdateLight {
    let plains = harpas_java::biome_names()
        .iter()
        .position(|name| *name == "minecraft:plains")
        .expect("the biome registry contains plains") as i32;
    let mut data = Vec::new();
    for y in MIN_SECTION..=MAX_SECTION {
        let section = visible
            .chunk
            .as_deref()
            .and_then(|chunk| section_at(chunk, y))
            .and_then(|section| section.section.as_ref());
        write_section(&mut data, section, plains);
    }
    let (heightmaps, light) = visible.chunk.as_deref().map_or_else(
        || (Vec::new(), empty_light()),
        |chunk| {
            let light = if is_generated(chunk) {
                computed_light(&visible).unwrap_or_else(|| light(chunk))
            } else {
                light(chunk)
            };
            (heightmaps(chunk), light)
        },
    );
    clientbound::ChunkDataAndUpdateLight {
        x: visible.position.x,
        z: visible.position.z,
        heightmaps,
        data,
        block_entities: visible.chunk.as_deref().map_or_else(Vec::new, |chunk| {
            block_entities(chunk, visible.position.x, visible.position.z, registry)
        }),
        light,
    }
}

/// Only explicitly documented client-visible fields are sent, never an entity's full saved NBT.
fn block_entities(
    chunk: &Chunk,
    chunk_x: i32,
    chunk_z: i32,
    registry: &crate::block_entity_registry::Registry,
) -> Vec<play::ChunkBlockEntity> {
    let Some(Nbt::List(List::Compound(entities))) = chunk.rest.get("block_entities") else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for entity in entities {
        let (Some(Nbt::String(name)), Some(Nbt::Int(x)), Some(Nbt::Int(y)), Some(Nbt::Int(z))) = (
            entity.get("id"),
            entity.get("x"),
            entity.get("y"),
            entity.get("z"),
        ) else {
            continue;
        };
        if x.div_euclid(16) != chunk_x || z.div_euclid(16) != chunk_z || !(-64..320).contains(y) {
            continue;
        }
        let Some(kind) = registry.id(name) else {
            continue;
        };
        result.push(play::ChunkBlockEntity {
            position: play::PackedXZ {
                x: x.rem_euclid(16) as u32,
                z: z.rem_euclid(16) as u32,
            },
            y: *y as i16,
            kind,
            data: update_tag(name, entity),
        });
    }
    result
}

/// Fields observed in vanilla's chunk updates. For other types the client gets the type and position, but no
/// saved NBT; a container's inventory, for example, is only sent when the player opens it.
fn update_tag(name: &str, entity: &Compound) -> Option<Nbt> {
    if matches!(name, "minecraft:sign" | "minecraft:hanging_sign") {
        let mut data = Compound::new();
        for field in ["front_text", "back_text"] {
            if let Some(Nbt::Compound(source)) = entity.get(field) {
                let mut text = Compound::new();
                copy_fields(
                    &mut text,
                    source,
                    &["messages", "filtered_messages", "color", "has_glowing_text"],
                );
                data.insert(field, text);
            }
        }
        if let Some(Nbt::Byte(waxed)) = entity.get("is_waxed") {
            data.insert("is_waxed", *waxed);
        }
        return Some(Nbt::Compound(data));
    }
    let fields: &[&str] = match name {
        "minecraft:banner" => &["components"],
        "minecraft:beacon" => &["Levels", "primary_effect", "secondary_effect"],
        "minecraft:brushable_block" => &["item"],
        "minecraft:campfire" => &["Items"],
        "minecraft:decorated_pot" => &["item", "sherds"],
        "minecraft:end_gateway" => &["Age", "exit_portal", "ExactTeleport"],
        "minecraft:jigsaw" => &[
            "joint",
            "name",
            "pool",
            "final_state",
            "placement_priority",
            "selection_priority",
            "target",
        ],
        "minecraft:mob_spawner" => &[
            "MaxNearbyEntities",
            "RequiredPlayerRange",
            "SpawnCount",
            "SpawnData",
            "MaxSpawnDelay",
            "SpawnRange",
            "Delay",
            "MinSpawnDelay",
        ],
        "minecraft:piston" => &["blockState", "facing", "progress", "source", "extending"],
        "minecraft:shelf" => &["align_items_to_bottom", "Items"],
        "minecraft:skull" => &["profile", "note_block_sound"],
        "minecraft:structure_block" => &[
            "metadata",
            "mirror",
            "ignoreEntities",
            "powered",
            "seed",
            "author",
            "rotation",
            "posX",
            "mode",
            "posY",
            "sizeX",
            "posZ",
            "integrity",
            "showair",
            "name",
            "strict",
            "sizeY",
            "sizeZ",
            "showboundingbox",
        ],
        "minecraft:test_block" => &["mode", "powered", "message"],
        "minecraft:test_instance_block" => &["data"],
        "minecraft:trial_spawner" => &["spawn_data"],
        "minecraft:vault" => &["shared_data"],
        _ => return None,
    };
    let mut data = Compound::new();
    copy_fields(&mut data, entity, fields);
    // These blocks have no update tag when none of their visible fields is present.
    if matches!(
        name,
        "minecraft:brushable_block"
            | "minecraft:decorated_pot"
            | "minecraft:skull"
            | "minecraft:trial_spawner"
    ) && data.is_empty()
    {
        None
    } else {
        Some(Nbt::Compound(data))
    }
}

fn copy_fields(to: &mut Compound, from: &Compound, fields: &[&str]) {
    for &field in fields {
        if let Some(value) = from.get(field) {
            to.insert(field, value.clone());
        }
    }
}

fn section_at(chunk: &Chunk, y: i8) -> Option<&ChunkSection> {
    chunk.sections.iter().find(|section| section.y == y)
}

fn write_section(out: &mut Vec<u8>, section: Option<&Section>, plains: i32) {
    let Some(section) = section else {
        codec::write_i16(out, 0);
        codec::write_i16(out, 0);
        codec::write_u8(out, 0);
        codec::write_varint(out, 0); // air
        codec::write_u8(out, 0);
        codec::write_varint(out, plains);
        return;
    };
    let blocks = section.block_states();
    let kinds = state_kinds();
    let mut non_air = 0_i16;
    let mut fluid = 0_i16;
    if blocks.bits() == 0 {
        let (air, has_fluid) = kinds[blocks.get(0) as usize];
        non_air = if air { 0 } else { 4096 };
        fluid = if has_fluid { 4096 } else { 0 };
    } else {
        for index in 0..4096 {
            let (air, has_fluid) = kinds[blocks.get(index) as usize];
            non_air += i16::from(!air);
            fluid += i16::from(has_fluid);
        }
    }
    codec::write_i16(out, non_air);
    codec::write_i16(out, fluid);
    write_container(out, blocks);
    write_container(out, section.biomes());
}

fn write_container(out: &mut Vec<u8>, container: &PalettedContainer) {
    let bits = container.bits();
    codec::write_u8(out, bits);
    if let Some(palette) = container.palette() {
        if bits != 0 {
            codec::write_varint(out, palette.len() as i32);
        }
        for &id in palette {
            codec::write_varint(out, id as i32);
        }
    }
    for &word in container.data() {
        codec::write_i64(out, word as i64);
    }
}

/// Air and nonempty fluid states, indexed by the fixed block-state registry.
fn state_kinds() -> &'static [(bool, bool)] {
    static KINDS: OnceLock<Vec<(bool, bool)>> = OnceLock::new();
    KINDS.get_or_init(|| {
        let states = block_states();
        (0..states.len())
            .map(|id| {
                let (name, properties) = states.properties(id).expect("a known block state");
                let air = matches!(
                    name,
                    "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
                );
                let fluid = matches!(
                    name,
                    "minecraft:water"
                        | "minecraft:lava"
                        | "minecraft:bubble_column"
                        | "minecraft:kelp"
                        | "minecraft:kelp_plant"
                        | "minecraft:seagrass"
                        | "minecraft:tall_seagrass"
                ) || properties.contains(&("waterlogged", "true"));
                (air, fluid)
            })
            .collect()
    })
}

fn heightmaps(chunk: &Chunk) -> Vec<play::Heightmap> {
    let Some(Nbt::Compound(maps)) = chunk.rest.get("Heightmaps") else {
        return Vec::new();
    };
    [
        ("WORLD_SURFACE_WG", play::HeightmapType::WorldSurfaceWg),
        ("WORLD_SURFACE", play::HeightmapType::WorldSurface),
        ("OCEAN_FLOOR_WG", play::HeightmapType::OceanFloorWg),
        ("OCEAN_FLOOR", play::HeightmapType::OceanFloor),
        ("MOTION_BLOCKING", play::HeightmapType::MotionBlocking),
        (
            "MOTION_BLOCKING_NO_LEAVES",
            play::HeightmapType::MotionBlockingNoLeaves,
        ),
    ]
    .into_iter()
    .filter_map(|(name, kind)| match maps.get(name) {
        Some(Nbt::LongArray(data)) if data.len() == 37 => Some(play::Heightmap {
            kind,
            data: data.clone(),
        }),
        _ => None,
    })
    .collect()
}

fn empty_light() -> play::LightData {
    play::LightData {
        sky_light_mask: Vec::new(),
        block_light_mask: Vec::new(),
        empty_sky_light_mask: Vec::new(),
        empty_block_light_mask: Vec::new(),
        sky_light: Vec::new(),
        block_light: Vec::new(),
    }
}

/// Whether the chunk was generated through carvers or features, which save no light, and still needs light
/// computed from its blocks. Keep in step with `World::needs_light`, which gathers the neighbors.
fn is_generated(chunk: &Chunk) -> bool {
    matches!(
        chunk.rest.get("Status"),
        Some(Nbt::String(status)) if status == "minecraft:carvers" || status == "minecraft:features"
    )
}

struct VisibleWorld<'a> {
    visible: &'a VisibleChunk,
}

impl LightWorld for VisibleWorld<'_> {
    fn min_y(&self) -> i32 {
        i32::from(MIN_SECTION) * 16
    }

    fn height(&self) -> i32 {
        (i32::from(MAX_SECTION) - i32::from(MIN_SECTION) + 1) * 16
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> u32 {
        let chunk_x = x.div_euclid(16) - self.visible.position.x;
        let chunk_z = z.div_euclid(16) - self.visible.position.z;
        let section_y = y.div_euclid(16) as i8;
        self.visible
            .chunk_at(chunk_x, chunk_z)
            .and_then(|chunk| chunk.sections.iter().find(|section| section.y == section_y))
            .and_then(|section| section.section.as_ref())
            .map(|section| {
                section.block_state((x & 15) as usize, (y & 15) as usize, (z & 15) as usize)
            })
            .unwrap_or(0)
    }
}

fn computed_light(visible: &VisibleChunk) -> Option<play::LightData> {
    let world = VisibleWorld { visible };
    let light = light_chunk(&world, visible.position.x, visible.position.z);
    Some(packet_light(&light))
}

/// Packs computed light like vanilla: full sky layers are omitted, all-zero layers are marked empty, and the
/// remaining arrays follow the mask bits in order.
fn packet_light(light: &ChunkLight) -> play::LightData {
    let mut result = empty_light();
    let mut sky = 0_u64;
    let mut block = 0_u64;
    let mut empty_sky = 0_u64;
    let mut empty_block = 0_u64;
    for section in &light.sections {
        let offset = section.y - i32::from(LIGHT_MIN_SECTION);
        let Ok(offset) = u32::try_from(offset) else {
            continue;
        };
        if offset >= 64 {
            continue;
        }
        let bit = 1_u64 << offset;
        if !section.sky.iter().all(|value| *value == 15) {
            if section.sky.iter().all(|value| *value == 0) {
                empty_sky |= bit;
            } else {
                sky |= bit;
                result.sky_light.push(section.sky.clone());
            }
        }
        if section.block.iter().all(|value| *value == 0) {
            empty_block |= bit;
        } else {
            block |= bit;
            result.block_light.push(section.block.clone());
        }
    }
    if sky != 0 {
        result.sky_light_mask.push(sky as i64);
    }
    if block != 0 {
        result.block_light_mask.push(block as i64);
    }
    if empty_sky != 0 {
        result.empty_sky_light_mask.push(empty_sky as i64);
    }
    if empty_block != 0 {
        result.empty_block_light_mask.push(empty_block as i64);
    }
    result
}

fn light(chunk: &Chunk) -> play::LightData {
    let mut result = empty_light();
    let mut sky = 0_u64;
    let mut block = 0_u64;
    let mut empty_sky = 0_u64;
    let mut empty_block = 0_u64;
    for y in LIGHT_MIN_SECTION..=LIGHT_MAX_SECTION {
        let Some(section) = section_at(chunk, y) else {
            continue;
        };
        let bit = 1_u64 << (y - LIGHT_MIN_SECTION);
        for (name, mask, empty, arrays) in [
            ("SkyLight", &mut sky, &mut empty_sky, &mut result.sky_light),
            (
                "BlockLight",
                &mut block,
                &mut empty_block,
                &mut result.block_light,
            ),
        ] {
            if let Some(Nbt::ByteArray(data)) = section.rest.get(name)
                && data.len() == LIGHT_BYTES
            {
                if data.iter().all(|&byte| byte == 0) {
                    *empty |= bit;
                } else {
                    *mask |= bit;
                    arrays.push(data.iter().map(|&byte| byte as u8).collect());
                }
            }
        }
    }
    if sky != 0 {
        result.sky_light_mask.push(sky as i64);
    }
    if block != 0 {
        result.block_light_mask.push(block as i64);
    }
    if empty_sky != 0 {
        result.empty_sky_light_mask.push(empty_sky as i64);
    }
    if empty_block != 0 {
        result.empty_block_light_mask.push(empty_block as i64);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harpas_anvil::ChunkSection;
    use harpas_core::chunk::{PalettedContainer, Strategy};
    use harpas_core::nbt::Compound;
    use harpas_engine::world::ChunkPos;

    use super::*;

    fn section() -> Section {
        let states = block_states();
        let mut blocks = PalettedContainer::filled(Strategy::block_states(states.global_bits()), 0);
        blocks.set(0, states.default_state("minecraft:stone").unwrap());
        blocks.set(1, states.default_state("minecraft:water").unwrap());
        blocks.set(
            2,
            states
                .state("minecraft:oak_stairs", [("waterlogged", "true")])
                .unwrap()
                .0,
        );
        blocks.set(3, states.default_state("minecraft:cave_air").unwrap());
        let biomes = PalettedContainer::filled(Strategy::biomes(7), 1);
        Section::new(blocks, biomes)
    }

    #[test]
    fn sections_send_their_counts_palettes_and_packed_indices() {
        let section = section();
        let mut data = Vec::new();
        write_section(&mut data, Some(&section), 0);
        let mut input = data.as_slice();
        assert_eq!(codec::read_i16(&mut input).unwrap(), 3);
        assert_eq!(codec::read_i16(&mut input).unwrap(), 2);
        assert_eq!(codec::read_u8(&mut input).unwrap(), 4);
        let palette_len = codec::read_varint(&mut input).unwrap() as usize;
        let palette: Vec<_> = (0..palette_len)
            .map(|_| codec::read_varint(&mut input).unwrap() as u32)
            .collect();
        let first = codec::read_i64(&mut input).unwrap() as u64;
        for index in 0..4 {
            let palette_index = ((first >> (index * 4)) & 15) as usize;
            assert_eq!(palette[palette_index], section.block_states().get(index));
        }
        for _ in 1..256 {
            codec::read_i64(&mut input).unwrap();
        }
        assert_eq!(codec::read_u8(&mut input).unwrap(), 0);
        assert_eq!(codec::read_varint(&mut input).unwrap(), 1);
        assert!(input.is_empty());
    }

    #[test]
    fn global_palettes_have_no_indirect_palette_or_length_prefix() {
        let states = block_states();
        let mut blocks = PalettedContainer::filled(Strategy::block_states(states.global_bits()), 0);
        for index in 0..300 {
            blocks.set(index, index as u32);
        }
        assert!(blocks.palette().is_none());
        let mut data = Vec::new();
        write_container(&mut data, &blocks);
        assert_eq!(data[0], states.global_bits());
        assert_eq!(data.len(), 1 + blocks.data().len() * 8);
        assert_eq!(&data[1..9], &blocks.data()[0].to_be_bytes());
    }

    #[test]
    fn sends_saved_heightmaps_and_light_in_section_order() {
        let mut low = Compound::new();
        low.insert("SkyLight", Nbt::ByteArray(vec![-1; LIGHT_BYTES]));
        low.insert("BlockLight", Nbt::ByteArray(vec![0; LIGHT_BYTES]));
        let mut high = Compound::new();
        high.insert("SkyLight", Nbt::ByteArray(vec![1; LIGHT_BYTES]));
        high.insert("BlockLight", Nbt::ByteArray(vec![5; 10])); // damaged arrays are not sent
        let mut maps = Compound::new();
        maps.insert("MOTION_BLOCKING", Nbt::LongArray(vec![42; 37]));
        maps.insert("WORLD_SURFACE", Nbt::LongArray(vec![9; 3]));
        let mut rest = Compound::new();
        rest.insert("Heightmaps", maps);
        let chunk = Chunk {
            sections: vec![
                ChunkSection {
                    y: 19,
                    section: None,
                    rest: high,
                },
                ChunkSection {
                    y: -5,
                    section: None,
                    rest: low,
                },
                ChunkSection {
                    y: -4,
                    section: Some(section()),
                    rest: Compound::new(),
                },
            ],
            rest,
        };
        let packet = encode(
            VisibleChunk {
                position: ChunkPos { x: -1, z: 2 },
                chunk: Some(Arc::new(chunk)),
                neighbors: std::array::from_fn(|_| None),
            },
            &crate::block_entity_registry::Registry::default(),
        );
        assert_eq!((packet.x, packet.z), (-1, 2));
        assert_eq!(packet.heightmaps.len(), 1);
        assert_eq!(
            packet.heightmaps[0].kind,
            play::HeightmapType::MotionBlocking
        );
        assert_eq!(packet.heightmaps[0].data, vec![42; 37]);
        assert_eq!(packet.light.sky_light_mask, vec![(1 | (1 << 24))]);
        assert_eq!(packet.light.empty_block_light_mask, vec![1]);
        assert!(packet.light.block_light_mask.is_empty());
        assert_eq!(
            packet.light.sky_light,
            vec![vec![255; LIGHT_BYTES], vec![1; LIGHT_BYTES]]
        );
        assert_eq!(&packet.data[..4], &[0, 3, 0, 2]);
    }

    #[test]
    fn generated_chunks_get_computed_light_at_carvers_and_features() {
        let light_for = |status: &str| {
            let mut rest = Compound::new();
            rest.insert("Status", status);
            let chunk = Chunk {
                sections: vec![ChunkSection {
                    y: -4,
                    section: Some(section()),
                    rest: Compound::new(),
                }],
                rest,
            };
            encode(
                VisibleChunk {
                    position: ChunkPos { x: 0, z: 0 },
                    chunk: Some(Arc::new(chunk)),
                    neighbors: std::array::from_fn(|_| None),
                },
                &crate::block_entity_registry::Registry::default(),
            )
            .light
        };
        let carvers = light_for("minecraft:carvers");
        assert!(!carvers.sky_light.is_empty());
        assert_eq!(light_for("minecraft:features"), carvers);
        // A finished chunk without saved light sends none, as before.
        assert!(light_for("minecraft:full").sky_light.is_empty());
    }

    #[test]
    fn computed_light_omits_full_sky_and_marks_empty_layers() {
        let mut light = ChunkLight {
            x: 0,
            z: 0,
            sections: Vec::new(),
        };
        for y in -5..=20 {
            let mut sky = vec![15_u8; LIGHT_BYTES];
            let mut block = vec![0_u8; LIGHT_BYTES];
            if y < 0 {
                sky.fill(0);
            } else if y == 0 {
                sky[0] = 7;
            } else if y == 1 {
                block[0] = 3;
            }
            light
                .sections
                .push(harpas_worldgen::LightSection { y, sky, block });
        }
        let packet = packet_light(&light);
        assert_eq!(packet.sky_light_mask, vec![1 << 5]);
        let mut expected_sky = vec![15_u8; LIGHT_BYTES];
        expected_sky[0] = 7;
        assert_eq!(packet.sky_light, vec![expected_sky]);
        assert_eq!(packet.block_light_mask, vec![1 << 6]);
        assert_eq!(packet.block_light.len(), 1);
        assert_eq!(packet.empty_sky_light_mask, vec![0b1_1111]);
        assert_eq!(
            packet.empty_block_light_mask,
            vec![(((1_i64 << 26) - 1) & !(1 << 6))]
        );
    }

    #[test]
    fn unsupported_block_entity_storage_is_not_leaked_to_the_client() {
        let mut entity = Compound::new();
        entity.insert("id", "minecraft:chest");
        entity.insert("x", 1);
        entity.insert("y", 65);
        entity.insert("z", 1);
        entity.insert("PrivateContents", "not a client update");
        let mut rest = Compound::new();
        rest.insert("block_entities", Nbt::List(List::Compound(vec![entity])));
        let chunk = Arc::new(Chunk {
            sections: Vec::new(),
            rest,
        });
        let packet = encode(
            VisibleChunk {
                position: ChunkPos { x: 0, z: 0 },
                chunk: Some(Arc::clone(&chunk)),
                neighbors: std::array::from_fn(|_| None),
            },
            &crate::block_entity_registry::Registry::synthetic(&[("minecraft:chest", 99)]),
        );
        assert_eq!(packet.block_entities.len(), 1);
        assert_eq!(packet.block_entities[0].kind, 99);
        assert!(packet.block_entities[0].data.is_none());
        let packet: clientbound::Packet = packet.into();
        let mut wire = Vec::new();
        packet.encode(&mut wire);
        assert!(
            !wire
                .windows(b"not a client update".len())
                .any(|bytes| bytes == b"not a client update")
        );
        assert!(
            matches!(chunk.rest.get("block_entities"), Some(Nbt::List(List::Compound(entities))) if entities.len() == 1)
        );
    }

    #[test]
    fn sign_updates_use_local_ids_and_only_visible_fields() {
        let registry = crate::block_entity_registry::Registry::synthetic(&[("minecraft:sign", 47)]);
        let mut text = Compound::new();
        text.insert("messages", Nbt::List(List::String(vec!["hello".into()])));
        text.insert("color", "blue");
        text.insert("private_nested", "secret");
        let mut sign = Compound::new();
        sign.insert("id", "minecraft:sign");
        sign.insert("x", -1);
        sign.insert("y", -64);
        sign.insert("z", 33);
        sign.insert("front_text", text);
        sign.insert("is_waxed", 1_i8);
        sign.insert("private_top_level", "secret");
        let mut other_chunk = sign.clone();
        other_chunk.insert("x", 0);
        let mut rest = Compound::new();
        rest.insert(
            "block_entities",
            Nbt::List(List::Compound(vec![sign, other_chunk])),
        );
        let chunk = Chunk {
            sections: Vec::new(),
            rest,
        };
        let entities = block_entities(&chunk, -1, 2, &registry);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].kind, 47);
        assert_eq!(
            (
                entities[0].position.x,
                entities[0].position.z,
                entities[0].y
            ),
            (15, 1, -64)
        );
        let Some(Nbt::Compound(data)) = &entities[0].data else {
            panic!("expected sign data")
        };
        assert_eq!(data.get("is_waxed"), Some(&Nbt::Byte(1)));
        assert!(data.get("private_top_level").is_none());
        let Some(Nbt::Compound(text)) = data.get("front_text") else {
            panic!("expected text")
        };
        assert!(text.get("messages").is_some());
        assert!(text.get("private_nested").is_none());
    }

    #[test]
    fn moving_piston_updates_include_its_moving_block() {
        let registry =
            crate::block_entity_registry::Registry::synthetic(&[("minecraft:piston", 91)]);
        let mut moved_block = Compound::new();
        moved_block.insert("Name", "minecraft:stone");
        let mut piston = Compound::new();
        piston.insert("id", "minecraft:piston");
        piston.insert("x", -1);
        piston.insert("y", 65);
        piston.insert("z", 2);
        piston.insert("blockState", moved_block.clone());
        piston.insert("facing", 5_i8);
        piston.insert("progress", 0.5_f32);
        piston.insert("source", 0_i8);
        piston.insert("extending", 1_i8);
        piston.insert("private_storage", "not a client update");
        let mut rest = Compound::new();
        rest.insert("block_entities", Nbt::List(List::Compound(vec![piston])));
        let chunk = Chunk {
            sections: Vec::new(),
            rest,
        };
        let entities = block_entities(&chunk, -1, 0, &registry);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].kind, 91);
        assert_eq!(
            (
                entities[0].position.x,
                entities[0].position.z,
                entities[0].y
            ),
            (15, 2, 65)
        );
        let Some(Nbt::Compound(data)) = &entities[0].data else {
            panic!("expected moving piston data");
        };
        assert_eq!(data.get("blockState"), Some(&Nbt::Compound(moved_block)));
        assert_eq!(data.get("facing"), Some(&Nbt::Byte(5)));
        assert_eq!(data.get("progress"), Some(&Nbt::Float(0.5)));
        assert_eq!(data.get("source"), Some(&Nbt::Byte(0)));
        assert_eq!(data.get("extending"), Some(&Nbt::Byte(1)));
        assert!(data.get("private_storage").is_none());
    }

    #[test]
    fn spawner_and_vault_updates_exclude_server_only_data() {
        let mut spawner = Compound::new();
        spawner.insert("SpawnData", "visible");
        spawner.insert("SpawnPotentials", "private");
        let Some(Nbt::Compound(sent)) = update_tag("minecraft:mob_spawner", &spawner) else {
            panic!("expected a spawner update");
        };
        assert_eq!(sent.get("SpawnData"), Some(&Nbt::from("visible")));
        assert!(sent.get("SpawnPotentials").is_none());

        let mut vault = Compound::new();
        vault.insert("shared_data", Compound::new());
        vault.insert("server_data", "private");
        let Some(Nbt::Compound(sent)) = update_tag("minecraft:vault", &vault) else {
            panic!("expected a vault update");
        };
        assert!(sent.get("shared_data").is_some());
        assert!(sent.get("server_data").is_none());
    }
}
