//! Natural creature spawning during the Overworld chunk tick.

use std::collections::HashSet;
use std::sync::Arc;

use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::{LegacyRandom, RandomSource};
use harpas_worldgen::data::CreatureSpawn;
use harpas_worldgen::placement::{Fluid, fluid};
use harpas_worldgen::{BiomeManager, WorldgenData, WorldgenDataError};

use crate::block::{self, BlockPos, MAX_Y, MIN_Y};
use crate::entity::{Animal, Kind, animal_bounds};
use crate::entity_data::persistent;
use crate::player::{AnimalSpecies, AnimalVariant, GameMode, PlayerState};
use crate::world::{ChunkPos, ChunkState, World};

const CREATURE_INTERVAL: i64 = 400;
const CREATURE_CAP: usize = 10;
const CAP_RADIUS: u8 = 8;
const CAP_CHUNKS: usize = (2 * CAP_RADIUS as usize + 1).pow(2);
const MIN_DISTANCE: f64 = 24.0;
const MAX_DISTANCE: f64 = 128.0;
const MAX_COW_PACK: usize = 4;

/// Natural spawning data for temperate cows in the plains.
#[derive(Debug)]
pub struct SpawningRules {
    plains_creatures: Vec<CreatureSpawn>,
    total_weight: i32,
    creature_types: HashSet<String>,
    substrate: Vec<bool>,
    simulation_distance: u8,
}

impl SpawningRules {
    /// Reads the creature pool and eligible blocks from the installed vanilla data.
    pub fn new(
        data: &WorldgenData,
        simulation_distance: u32,
    ) -> Result<Option<Self>, WorldgenDataError> {
        let Some(plains_creatures) = data.creature_spawns.get("minecraft:plains") else {
            return Ok(None);
        };
        let total_weight = plains_creatures
            .iter()
            .try_fold(0_i32, |sum, entry| sum.checked_add(entry.weight))
            .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                context: "minecraft:plains".into(),
                message: "creature spawn weights overflow".into(),
            })?;
        if !plains_creatures
            .iter()
            .any(|entry| entry.entity == "minecraft:cow" && entry.weight > 0)
        {
            return Ok(None);
        }
        let creature_types = data
            .creature_spawns
            .values()
            .flatten()
            .map(|entry| entry.entity.clone())
            .collect();
        Ok(Some(Self {
            plains_creatures: plains_creatures.clone(),
            total_weight,
            creature_types,
            substrate: data.block_tag_states("minecraft:animals_spawnable_on")?,
            simulation_distance: simulation_distance.clamp(2, u32::from(CAP_RADIUS)) as u8,
        }))
    }

    fn cow_group(&self, random: &mut LegacyRandom) -> usize {
        let mut roll = random.next_int_bounded(self.total_weight);
        let selected = self
            .plains_creatures
            .iter()
            .find(|entry| {
                roll -= entry.weight;
                roll < 0
            })
            .expect("one entry covers the selected weight");
        if selected.entity == "minecraft:cow" {
            (random.next_int_between_inclusive(selected.min_count, selected.max_count) as usize)
                .min(MAX_COW_PACK)
        } else {
            0
        }
    }
}

pub(crate) struct NaturalSpawning {
    pub(crate) rules: Option<Arc<SpawningRules>>,
    enabled: bool,
    plains: u32,
    biomes: BiomeManager,
    random: LegacyRandom,
}

impl NaturalSpawning {
    pub(crate) fn new(
        game_rules: Option<&Compound>,
        plains: u32,
        world_seed: i64,
        random_seed: i64,
    ) -> Self {
        let enabled = |name| game_rules.and_then(|rules| rules.get(name)) != Some(&Nbt::Byte(0));
        Self {
            rules: None,
            enabled: enabled("minecraft:spawn_mobs"),
            plains,
            biomes: BiomeManager::from_world_seed(world_seed),
            random: LegacyRandom::new(random_seed),
        }
    }
}

impl World {
    pub(crate) fn tick_natural_spawning(&mut self, players: &[PlayerState]) {
        if !self.spawning.enabled || self.game_time.rem_euclid(CREATURE_INTERVAL) != 0 {
            return;
        }
        let Some(rules) = self.spawning.rules.clone() else {
            return;
        };
        let players: Vec<_> = players
            .iter()
            .filter(|player| {
                player.data_ready
                    && !player.disconnecting
                    && !player.is_dead()
                    && player.player.profile.game_mode != GameMode::Spectator
            })
            .map(|player| {
                let p = player.player.position;
                ([p.x, p.y, p.z], ChunkPos::at(p.x, p.z))
            })
            .collect();
        let cap_chunks: HashSet<_> = players
            .iter()
            .flat_map(|(_, center)| center.view(CAP_RADIUS))
            .collect();
        let cap = CREATURE_CAP * cap_chunks.len() / CAP_CHUNKS;
        if cap == 0 {
            return;
        }
        let mut creatures = self.opaque_creature_count(&rules.creature_types)
            + self
                .entities
                .order
                .iter()
                .filter(|&&handle| {
                    let slot = self.entities.slot(handle);
                    matches!(&self.entities.kind[slot], Some(Kind::Animal(animal))
                    if animal.health > 0.0 && !persistent(&animal.nbt))
                })
                .count();
        if creatures >= cap {
            return;
        }

        let mut chunks: Vec<_> = cap_chunks
            .into_iter()
            .filter(|&chunk| {
                self.wanted_set.contains(&chunk)
                    && matches!(self.chunk(chunk), Some(ChunkState::Loaded(_)))
                    && players.iter().any(|&([x, _, z], center)| {
                        let dx = f64::from(chunk.x * 16 + 8) - x;
                        let dz = f64::from(chunk.z * 16 + 8) - z;
                        chunk.within(center, rules.simulation_distance)
                            && dx * dx + dz * dz < MAX_DISTANCE * MAX_DISTANCE
                    })
            })
            .collect();
        chunks.sort_unstable_by_key(|chunk| (chunk.x, chunk.z));
        for index in (1..chunks.len()).rev() {
            let other = self.spawning.random.next_int_bounded(index as i32 + 1) as usize;
            chunks.swap(index, other);
        }
        let spawn = self.spawn_position();
        let world_spawn = [spawn.x, spawn.y + 0.5, spawn.z];
        let eligible: HashSet<_> = chunks.iter().copied().collect();
        for chunk in chunks {
            creatures += self.try_cow_packs(chunk, &rules, |pos, at| {
                if !eligible.contains(&pos) {
                    return false;
                }
                let closest = players
                    .iter()
                    .map(|(player, _)| distance_squared(at, *player))
                    .fold(f64::INFINITY, f64::min);
                closest > MIN_DISTANCE * MIN_DISTANCE
                    && closest <= MAX_DISTANCE * MAX_DISTANCE
                    && distance_squared(at, world_spawn) > MIN_DISTANCE * MIN_DISTANCE
            });
            // The category cap is checked between chunks, so a pack may take the count over it.
            if creatures >= cap {
                break;
            }
        }
    }

    fn try_cow_packs(
        &mut self,
        chunk: ChunkPos,
        rules: &SpawningRules,
        in_range: impl Fn(ChunkPos, [f64; 3]) -> bool,
    ) -> usize {
        let x = chunk.x * 16 + self.spawning.random.next_int_bounded(16);
        let z = chunk.z * 16 + self.spawning.random.next_int_bounded(16);
        let Some(ChunkState::Loaded(terrain)) = self.chunk(chunk) else {
            return 0;
        };
        let top = block::surface_height(terrain, x, z).clamp(MIN_Y, MAX_Y + 1);
        let y = self.spawning.random.next_int_between_inclusive(MIN_Y, top);
        if !(MIN_Y + 1..MAX_Y).contains(&y) {
            return 0;
        }
        let origin = BlockPos { x, y, z };
        if self
            .block_state(origin)
            .is_some_and(|state| block_behavior().is_solid_render(state))
        {
            return 0;
        }
        let mut spawned = 0;
        for _ in 0..3 {
            let group = rules.cow_group(&mut self.spawning.random);
            let mut pos = origin;
            for _ in 0..group {
                let random = &mut self.spawning.random;
                pos.x += random.next_int_bounded(6) - random.next_int_bounded(6);
                pos.z += random.next_int_bounded(6) - random.next_int_bounded(6);
                let at = [f64::from(pos.x) + 0.5, f64::from(y), f64::from(pos.z) + 0.5];
                if !in_range(pos.chunk(), at) || !self.can_spawn_cow(pos, at, rules) {
                    continue;
                }
                let Some(id) = crate::next_entity_id() else {
                    return spawned;
                };
                let cow = Animal::new(
                    AnimalSpecies::Cow,
                    [self.spawning.random.next_float() * 360.0, 0.0],
                    10.0,
                    0.0,
                    AnimalVariant::Temperate,
                    Arc::new(Compound::from_iter([
                        ("id", Nbt::from("minecraft:cow")),
                        ("variant", Nbt::from("minecraft:temperate")),
                    ])),
                );
                self.add_entity(id, at, [0.0; 3], Kind::Animal(cow));
                spawned += 1;
                if spawned == MAX_COW_PACK {
                    return spawned;
                }
            }
        }
        spawned
    }

    fn can_spawn_cow(&self, pos: BlockPos, at: [f64; 3], rules: &SpawningRules) -> bool {
        let below = BlockPos {
            y: pos.y - 1,
            ..pos
        };
        if !self
            .block_state(below)
            .is_some_and(|state| rules.substrate.get(state as usize) == Some(&true))
            || !self.spawn_brightness(pos).is_some_and(|light| light >= 9)
            || self
                .spawning
                .biomes
                .get_biome(pos.x, pos.y, pos.z, |x, y, z| self.quart_biome(x, y, z))
                != self.spawning.plains
            || [pos.y, pos.y + 1].into_iter().any(|y| {
                !self
                    .block_state(BlockPos { y, ..pos })
                    .is_some_and(|state| fluid(state) == Fluid::Empty)
            })
        {
            return false;
        }
        let area = animal_bounds(AnimalSpecies::Cow, at);
        !self.collides(area)
            && !self.entities.order.iter().any(|&handle| {
                let i = self.entities.slot(handle);
                matches!(self.entities.kind[i], Some(Kind::Animal(_))) && {
                    let other = self.entities.animal_box(i);
                    (0..3).all(|axis| {
                        area[0][axis] < other[1][axis] && other[0][axis] < area[1][axis]
                    })
                }
            })
    }

    fn quart_biome(&self, x: i32, y: i32, z: i32) -> u32 {
        let pos = BlockPos {
            x: x * 4,
            y: (y * 4).clamp(MIN_Y, MAX_Y),
            z: z * 4,
        };
        let Some(ChunkState::Loaded(chunk)) = self.chunk(pos.chunk()) else {
            return u32::MAX;
        };
        chunk
            .sections
            .iter()
            .find(|section| i32::from(section.y) == pos.y >> 4)
            .and_then(|section| section.section.as_ref())
            .map_or(u32::MAX, |section| {
                section.biome(
                    (x & 3) as usize,
                    ((pos.y >> 2) & 3) as usize,
                    (z & 3) as usize,
                )
            })
    }
}

fn distance_squared(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum()
}
