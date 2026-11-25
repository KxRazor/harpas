//! Supported entities in vanilla's per-chunk entity regions. Other entities and unrelated NBT survive saves.

use std::io;
use std::sync::Arc;

use harpas_core::nbt::{Compound, List, Nbt};

use crate::entity::Pig;
use crate::inventory::ItemStack;
use crate::item_data::{ItemCodec, invalid};
use crate::player::PigVariant;
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
    let [x, y, z] = vector(root, "Pos")?;
    if x.abs() > 30_000_512.0
        || y.abs() > 20_000_000.0
        || z.abs() > 30_000_512.0
        || ChunkPos::at(x, z) != chunk
    {
        return Err(invalid(
            "entity position is outside its saved chunk or world limits",
        ));
    }
    Ok([x, y, z])
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
            on_ground: root.get("OnGround") == Some(&Nbt::Byte(1)),
            stack,
            age: short("Age", 0)?,
            pickup_delay: short("PickupDelay", 0)?,
            health: short("Health", 5)? as f32,
            rest: Arc::new(rest),
        })
    }

    fn to_nbt(&self, codec: &ItemCodec) -> io::Result<Compound> {
        let mut root = (*self.rest).clone();
        root.insert("id", "minecraft:item");
        root.insert("UUID", uuid_tag(self.uuid));
        root.insert("Pos", Nbt::List(List::Double(self.position.to_vec())));
        root.insert("Motion", Nbt::List(List::Double(self.velocity.to_vec())));
        root.insert("OnGround", Nbt::Byte(i8::from(self.on_ground)));
        root.insert("Item", Nbt::Compound(codec.write(&self.stack)?));
        root.insert("Age", Nbt::Short(self.age as i16));
        root.insert("PickupDelay", Nbt::Short(self.pickup_delay as i16));
        root.insert("Health", Nbt::Short(self.health as i16));
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
pub(crate) struct SavedPig {
    pub(crate) uuid: u128,
    pub(crate) position: [f64; 3],
    pub(crate) velocity: [f64; 3],
    pub(crate) on_ground: bool,
    pub(crate) pig: Pig,
}

impl SavedPig {
    fn read(root: &Compound, chunk: ChunkPos) -> io::Result<Self> {
        match root.get("Age") {
            Some(Nbt::Int(age)) if *age >= 0 => {}
            None => {}
            _ => return Err(invalid("expected an adult pig")),
        }
        let rotation = match root.get("Rotation") {
            Some(Nbt::List(List::Float(v))) if v.len() == 2 && v.iter().all(|v| v.is_finite()) => {
                [v[0], v[1]]
            }
            _ => return Err(invalid("pig Rotation must contain two finite floats")),
        };
        let health = match root.get("Health") {
            Some(Nbt::Float(value)) if value.is_finite() && *value > 0.0 => *value,
            _ => return Err(invalid("expected a living pig")),
        };
        let fall_distance = match root.get("fall_distance") {
            Some(Nbt::Double(value)) if value.is_finite() => *value,
            None => 0.0,
            _ => return Err(invalid("pig fall_distance must be a finite double")),
        };
        let variant = match root.get("variant") {
            Some(Nbt::String(name)) => match name.as_str() {
                "minecraft:cold" | "cold" => PigVariant::Cold,
                "minecraft:temperate" | "temperate" => PigVariant::Temperate,
                "minecraft:warm" | "warm" => PigVariant::Warm,
                _ => return Err(invalid("unsupported pig variant")),
            },
            None => PigVariant::Temperate,
            _ => return Err(invalid("pig variant must be a string")),
        };
        Ok(Self {
            uuid: read_uuid(root)?,
            position: position(root, chunk)?,
            velocity: velocity(root)?,
            on_ground: root.get("OnGround") == Some(&Nbt::Byte(1)),
            pig: Pig::new(
                rotation,
                health,
                fall_distance,
                variant,
                Arc::new(root.clone()),
            ),
        })
    }

    fn to_nbt(&self) -> Compound {
        let mut root = (*self.pig.nbt).clone();
        root.insert("UUID", uuid_tag(self.uuid));
        root.insert("Pos", Nbt::List(List::Double(self.position.to_vec())));
        root.insert("Motion", Nbt::List(List::Double(self.velocity.to_vec())));
        root.insert(
            "Rotation",
            Nbt::List(List::Float(self.pig.rotation.to_vec())),
        );
        root.insert("Health", Nbt::Float(self.pig.health));
        if self.on_ground || root.get("OnGround").is_some() {
            root.insert("OnGround", Nbt::Byte(i8::from(self.on_ground)));
        }
        if self.pig.fall_distance != 0.0 || root.get("fall_distance").is_some() {
            root.insert("fall_distance", Nbt::Double(self.pig.fall_distance));
        }
        root
    }
}

/// One entity-region document. Entity types not implemented by the engine remain opaque in `rest`.
#[derive(Debug, Clone, Default)]
pub struct EntityChunk {
    /// Active item entities, in their saved order.
    pub items: Vec<SavedItem>,
    pub(crate) pigs: Vec<SavedPig>,
    rest: Arc<Compound>,
}

impl EntityChunk {
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
        let mut pigs = Vec::new();
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
                    if (id == "minecraft:pig" || id == "pig")
                        && !matches!(entry.get("Age"), Some(Nbt::Int(age)) if *age < 0) =>
                {
                    SavedPig::read(&entry, pos).map(|pig| {
                        let uuid = pig.uuid;
                        pigs.push(pig);
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
            pigs,
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
        for item in &self.items {
            entries.push(item.to_nbt(codec)?);
        }
        entries.extend(self.pigs.iter().map(SavedPig::to_nbt));
        root.insert("Entities", Nbt::List(List::Compound(entries)));
        Ok(root)
    }
}
