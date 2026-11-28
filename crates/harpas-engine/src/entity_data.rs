//! Supported entities in vanilla's per-chunk entity regions. Other entities and unrelated NBT survive saves.

use std::io;
use std::sync::Arc;

use harpas_core::nbt::{Compound, List, Nbt};

use crate::entity::Animal;
use crate::inventory::ItemStack;
use crate::item_data::{ItemCodec, invalid};
use crate::player::{AnimalSpecies, AnimalVariant};
use crate::world::ChunkPos;

/// The persistent state of an item entity, independent of its process-local network ID and tracking state.
#[derive(Debug, Clone)]
pub struct SavedItem {
    /// Persistent entity UUID.
    pub uuid: u128,
    /// Feet position in world coordinates.
    pub position: [f64; 3],
    /// Movement per tick.
    pub velocity: [f64; 3],
    /// Whether the entity rests on a block.
    pub on_ground: bool,
    /// The carried stack, including its component patch.
    pub stack: ItemStack,
    /// Ticks alive, including vanilla's infinite lifetime sentinel.
    pub age: i32,
    /// Ticks before pickup, including vanilla's infinite delay sentinel.
    pub pickup_delay: i32,
    /// Remaining item health.
    pub health: f32,
    pub(crate) rest: Arc<Compound>,
}

fn read_uuid(root: &Compound) -> io::Result<u128> {
    match root.get("UUID") {
        Some(Nbt::IntArray(v)) if v.len() == 4 => Ok(v
            .iter()
            .fold(0_u128, |uuid, &word| (uuid << 32) | u128::from(word as u32))),
        _ => Err(invalid("entity has no four-int UUID")),
    }
}

fn uuid_tag(uuid: u128) -> Nbt {
    Nbt::IntArray((0..4).rev().map(|i| (uuid >> (i * 32)) as i32).collect())
}

fn vector(root: &Compound, name: &str) -> io::Result<[f64; 3]> {
    match root.get(name) {
        Some(Nbt::List(List::Double(v))) if v.len() == 3 && v.iter().all(|v| v.is_finite()) => {
            Ok([v[0], v[1], v[2]])
        }
        _ => Err(invalid(format!(
            "entity {name} must contain three finite doubles"
        ))),
    }
}

fn position(root: &Compound, chunk: ChunkPos) -> io::Result<[f64; 3]> {
    let pos = vector(root, "Pos")?;
    check_position(pos, chunk)?;
    Ok(pos)
}

fn check_position([x, y, z]: [f64; 3], chunk: ChunkPos) -> io::Result<()> {
    if ![x, y, z].into_iter().all(f64::is_finite)
        || x.abs() > 30_000_512.0
        || y.abs() > 20_000_000.0
        || z.abs() > 30_000_512.0
        || ChunkPos::at(x, z) != chunk
    {
        return Err(invalid(
            "entity position is outside its saved chunk or world limits",
        ));
    }
    Ok(())
}

fn check_motion(motion: [f64; 3]) -> io::Result<()> {
    if !motion.into_iter().all(|v| (-10.0..=10.0).contains(&v)) {
        return Err(invalid("entity Motion cannot round trip"));
    }
    Ok(())
}

fn velocity(root: &Compound) -> io::Result<[f64; 3]> {
    // Vanilla discards individual velocity components above ten blocks per tick on load.
    Ok(vector(root, "Motion")?.map(|v| if v.abs() > 10.0 { 0.0 } else { v }))
}

impl SavedItem {
    fn read(root: &Compound, chunk: ChunkPos, codec: &ItemCodec) -> io::Result<Self> {
        let uuid = read_uuid(root)?;
        let short = |name, default| match root.get(name) {
            None => Ok(default),
            Some(Nbt::Short(n)) => Ok(i32::from(*n)),
            _ => Err(invalid(format!("item entity {name} must be a short"))),
        };
        let Some(Nbt::Compound(stack)) = root.get("Item") else {
            return Err(invalid("item entity has no Item compound"));
        };
        let stack = codec.read(stack)?;
        if stack.is_empty() {
            return Err(invalid("item entity holds an empty stack"));
        }
        let health = short("Health", 5)?;
        if health <= 0 {
            return Err(invalid("expected a living item entity"));
        }
        let mut rest = root.clone();
        for key in [
            "id",
            "UUID",
            "Pos",
            "Motion",
            "OnGround",
            "Item",
            "Age",
            "PickupDelay",
            "Health",
        ] {
            rest.remove(key);
        }
        Ok(Self {
            uuid,
            position: position(root, chunk)?,
            velocity: velocity(root)?,
            on_ground: nbt_bool(root, "OnGround"),
            stack,
            age: short("Age", 0)?,
            pickup_delay: short("PickupDelay", 0)?,
            health: health as f32,
            rest: Arc::new(rest),
        })
    }

    fn to_nbt(&self, chunk: ChunkPos, codec: &ItemCodec) -> io::Result<Compound> {
        check_position(self.position, chunk)?;
        check_motion(self.velocity)?;
        let age = i16::try_from(self.age).map_err(|_| invalid("item Age does not fit a short"))?;
        let delay = i16::try_from(self.pickup_delay)
            .map_err(|_| invalid("item PickupDelay does not fit a short"))?;
        let health = self.health as i16;
        if self.stack.is_empty() || health <= 0 || f32::from(health) != self.health {
            return Err(invalid(
                "item entity must have a live stack and positive short Health",
            ));
        }
        let mut root = (*self.rest).clone();
        root.insert("id", "minecraft:item");
        root.insert("UUID", uuid_tag(self.uuid));
        root.insert("Pos", Nbt::List(List::Double(self.position.to_vec())));
        root.insert("Motion", Nbt::List(List::Double(self.velocity.to_vec())));
        root.insert("OnGround", Nbt::Byte(i8::from(self.on_ground)));
        root.insert("Item", Nbt::Compound(codec.write(&self.stack)?));
        root.insert("Age", Nbt::Short(age));
        root.insert("PickupDelay", Nbt::Short(delay));
        root.insert("Health", Nbt::Short(health));
        // Required by the base entity codec for newly dropped items. Loaded entities retain their values.
        for (name, value) in [
            ("Rotation", Nbt::List(List::Float(vec![0.0, 0.0]))),
            ("fall_distance", Nbt::Double(0.0)),
            ("Fire", Nbt::Short(0)),
            ("Air", Nbt::Short(300)),
            ("Invulnerable", Nbt::Byte(0)),
            ("PortalCooldown", Nbt::Int(0)),
        ] {
            if root.get(name).is_none() {
                root.insert(name, value);
            }
        }
        Ok(root)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SavedAnimal {
    pub(crate) uuid: u128,
    pub(crate) position: [f64; 3],
    pub(crate) velocity: [f64; 3],
    pub(crate) on_ground: bool,
    pub(crate) animal: Animal,
}

impl SavedAnimal {
    fn read(root: &Compound, chunk: ChunkPos, species: AnimalSpecies) -> io::Result<Self> {
        match root.get("Age") {
            Some(Nbt::Int(age)) if *age >= 0 => {}
            None => {}
            _ => return Err(invalid("expected an adult animal")),
        }
        let rotation = match root.get("Rotation") {
            Some(Nbt::List(List::Float(v))) if v.len() == 2 && v.iter().all(|v| v.is_finite()) => {
                [v[0], v[1]]
            }
            _ => return Err(invalid("animal Rotation must contain two finite floats")),
        };
        let health = match root.get("Health") {
            Some(Nbt::Float(value)) if value.is_finite() && *value > 0.0 => *value,
            _ => return Err(invalid("expected a living animal")),
        };
        let fall_distance = match root.get("fall_distance") {
            Some(Nbt::Double(value)) if value.is_finite() => *value,
            None => 0.0,
            _ => return Err(invalid("animal fall_distance must be a finite double")),
        };
        let variant = match root.get("variant") {
            Some(Nbt::String(name)) => match name.as_str() {
                "minecraft:cold" | "cold" => AnimalVariant::Cold,
                "minecraft:temperate" | "temperate" => AnimalVariant::Temperate,
                "minecraft:warm" | "warm" => AnimalVariant::Warm,
                _ => return Err(invalid("unsupported animal variant")),
            },
            None => AnimalVariant::Temperate,
            _ => return Err(invalid("animal variant must be a string")),
        };
        Ok(Self {
            uuid: read_uuid(root)?,
            position: position(root, chunk)?,
            velocity: velocity(root)?,
            on_ground: nbt_bool(root, "OnGround"),
            animal: Animal::new(
                species,
                rotation,
                health,
                fall_distance,
                variant,
                Arc::new(root.clone()),
            ),
        })
    }

    fn to_nbt(&self, chunk: ChunkPos) -> io::Result<Compound> {
        check_position(self.position, chunk)?;
        check_motion(self.velocity)?;
        if !self.animal.rotation.into_iter().all(f32::is_finite)
            || !self.animal.health.is_finite()
            || self.animal.health <= 0.0
            || !self.animal.fall_distance.is_finite()
        {
            return Err(invalid("animal state cannot round trip"));
        }
        let mut root = (*self.animal.nbt).clone();
        root.insert(
            "id",
            match self.animal.species {
                AnimalSpecies::Pig => "minecraft:pig",
                AnimalSpecies::Cow => "minecraft:cow",
            },
        );
        root.insert("UUID", uuid_tag(self.uuid));
        root.insert("Pos", Nbt::List(List::Double(self.position.to_vec())));
        root.insert("Motion", Nbt::List(List::Double(self.velocity.to_vec())));
        root.insert(
            "Rotation",
            Nbt::List(List::Float(self.animal.rotation.to_vec())),
        );
        root.insert("Health", Nbt::Float(self.animal.health));
        if self.on_ground || root.get("OnGround").is_some() {
            root.insert("OnGround", Nbt::Byte(i8::from(self.on_ground)));
        }
        if self.animal.fall_distance != 0.0 || root.get("fall_distance").is_some() {
            root.insert("fall_distance", Nbt::Double(self.animal.fall_distance));
        }
        Ok(root)
    }
}

/// One entity-region document. Entity types not implemented by the engine remain opaque in `rest`.
#[derive(Debug, Clone, Default)]
pub struct EntityChunk {
    /// Active item entities, in their saved order.
    pub items: Vec<SavedItem>,
    pub(crate) animals: Vec<SavedAnimal>,
    rest: Arc<Compound>,
}

impl EntityChunk {
    pub(crate) fn opaque_creature_count(&self, types: &std::collections::HashSet<String>) -> usize {
        let Some(Nbt::List(List::Compound(entries))) = self.rest.get("Entities") else {
            return 0;
        };
        entries
            .iter()
            .filter(|entity| {
                matches!(entity.get("id"), Some(Nbt::String(id)) if types.contains(id)
                    || (!id.contains(':') && types.contains(&format!("minecraft:{id}"))))
                    && !persistent(entity)
            })
            .count()
    }

    /// Checks the document. Entries the engine cannot activate are kept as opaque NBT.
    pub fn read(mut root: Compound, pos: ChunkPos, codec: &ItemCodec) -> io::Result<Self> {
        harpas_anvil::check_chunk(&root)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if root.get("Position") != Some(&Nbt::IntArray(vec![pos.x, pos.z])) {
            return Err(invalid(
                "entity chunk Position differs from the requested chunk",
            ));
        }
        let entries = match root.remove("Entities") {
            Some(Nbt::List(List::Compound(entries))) => entries,
            None | Some(Nbt::List(List::Empty)) => Vec::new(),
            _ => return Err(invalid("entity chunk Entities must be a compound list")),
        };
        let mut items = Vec::new();
        let mut animals = Vec::new();
        let mut opaque = Vec::new();
        let mut uuids = std::collections::HashSet::new();
        for entry in entries {
            let result = match entry.get("id") {
                Some(Nbt::String(id)) if id == "minecraft:item" || id == "item" => {
                    SavedItem::read(&entry, pos, codec).map(|item| {
                        let uuid = item.uuid;
                        items.push(item);
                        uuid
                    })
                }
                Some(Nbt::String(id))
                    if let Some(species) = AnimalSpecies::from_id(id)
                        && !matches!(entry.get("Age"), Some(Nbt::Int(age)) if *age < 0) =>
                {
                    SavedAnimal::read(&entry, pos, species).map(|animal| {
                        let uuid = animal.uuid;
                        animals.push(animal);
                        uuid
                    })
                }
                _ => {
                    opaque.push(entry);
                    continue;
                }
            };
            match result {
                Ok(uuid) if !uuids.insert(uuid) => return Err(invalid("duplicate entity UUID")),
                Ok(_) => {}
                Err(error) => {
                    log::warn!(
                        "Keeping an inactive entity in chunk {}, {}: {error}",
                        pos.x,
                        pos.z
                    );
                    opaque.push(entry);
                }
            }
        }
        root.insert("Entities", Nbt::List(List::Compound(opaque)));
        Ok(Self {
            items,
            animals,
            rest: Arc::new(root),
        })
    }

    /// Encodes a snapshot on a save worker, retaining opaque entities and document fields.
    pub fn to_nbt(&self, pos: ChunkPos, codec: &ItemCodec) -> io::Result<Compound> {
        let mut root = (*self.rest).clone();
        if root.get("DataVersion").is_none() {
            root.insert("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION));
        }
        root.insert("Position", Nbt::IntArray(vec![pos.x, pos.z]));
        let mut entries = match root.remove("Entities") {
            Some(Nbt::List(List::Compound(entries))) => entries,
            _ => Vec::new(),
        };
        let mut uuids = std::collections::HashSet::new();
        for item in &self.items {
            if !uuids.insert(item.uuid) {
                return Err(invalid("duplicate entity UUID"));
            }
            entries.push(item.to_nbt(pos, codec)?);
        }
        for animal in &self.animals {
            if !uuids.insert(animal.uuid) {
                return Err(invalid("duplicate entity UUID"));
            }
            entries.push(animal.to_nbt(pos)?);
        }
        root.insert("Entities", Nbt::List(List::Compound(entries)));
        Ok(root)
    }
}

pub(crate) fn persistent(entity: &Compound) -> bool {
    nbt_bool(entity, "PersistenceRequired")
}

pub(crate) fn nbt_bool(entity: &Compound, key: &str) -> bool {
    matches!(entity.get(key), Some(Nbt::Byte(value)) if *value != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_items_round_trip_and_invalid_snapshots_fail_before_encoding() {
        let pos = ChunkPos { x: 0, z: 0 };
        let codec = ItemCodec::default();
        let mut source = EntityChunk::default();
        source.items.push(SavedItem {
            uuid: 1,
            position: [1.0, 64.0, 1.0],
            velocity: [0.0; 3],
            on_ground: false,
            stack: codec
                .read(&Compound::from_iter([("id", "minecraft:stone")]))
                .unwrap(),
            age: i32::from(i16::MIN),
            pickup_delay: i32::from(i16::MAX),
            health: 5.0,
            rest: Arc::default(),
        });
        let mut root = source.to_nbt(pos, &codec).unwrap();
        let Some(Nbt::List(List::Compound(mut entities))) = root.remove("Entities") else {
            panic!("missing entities");
        };
        entities[0].insert("id", "item");
        entities[0].insert("OnGround", Nbt::Byte(-1));
        root.insert("Entities", List::Compound(entities));
        let loaded = EntityChunk::read(root, pos, &codec).unwrap();
        assert_eq!(loaded.items.len(), 1);
        assert!(loaded.items[0].on_ground);
        assert_eq!(loaded.items[0].pickup_delay, i32::from(i16::MAX));
        assert!(source.to_nbt(ChunkPos { x: 1, z: 0 }, &codec).is_err());
        source.items.push(source.items[0].clone());
        assert!(source.to_nbt(pos, &codec).is_err());
        source.items.pop();
        source.items[0].age = 32_768;
        assert!(source.to_nbt(pos, &codec).is_err());
        source.items[0].age = 0;
        source.items[0].velocity[0] = 11.0;
        assert!(source.to_nbt(pos, &codec).is_err());
    }
}
