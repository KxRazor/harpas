//! Item entities in vanilla's per-chunk entity regions. Unimplemented entities and unrelated NBT survive saves.

use std::io;
use std::sync::Arc;

use harpas_core::nbt::{Compound, List, Nbt};

use crate::inventory::ItemStack;
use crate::item_data::{ItemCodec, invalid};
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

fn vector(root: &Compound, name: &str) -> io::Result<[f64; 3]> {
    match root.get(name) {
        Some(Nbt::List(List::Double(v))) if v.len() == 3 && v.iter().all(|v| v.is_finite()) => {
            Ok([v[0], v[1], v[2]])
        }
        _ => Err(invalid(format!(
            "item entity {name} must contain three finite doubles"
        ))),
    }
}

impl SavedItem {
    fn read(root: &Compound, codec: &ItemCodec) -> io::Result<Self> {
        let uuid = match root.get("UUID") {
            Some(Nbt::IntArray(v)) if v.len() == 4 => v
                .iter()
                .fold(0_u128, |uuid, &word| (uuid << 32) | u128::from(word as u32)),
            _ => return Err(invalid("item entity has no four-int UUID")),
        };
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
            position: vector(root, "Pos")?,
            // Vanilla discards individual velocity components above ten blocks per tick on load.
            velocity: vector(root, "Motion")?.map(|v| if v.abs() > 10.0 { 0.0 } else { v }),
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
        root.insert(
            "UUID",
            Nbt::IntArray(
                (0..4)
                    .rev()
                    .map(|i| (self.uuid >> (i * 32)) as i32)
                    .collect(),
            ),
        );
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

/// One entity-region document. Entity types not implemented by the engine remain opaque in `rest`.
#[derive(Debug, Clone, Default)]
pub struct EntityChunk {
    /// Active item entities, in their saved order.
    pub items: Vec<SavedItem>,
    rest: Arc<Compound>,
}

impl EntityChunk {
    /// Checks the document. Items whose data cannot be decoded are kept as opaque NBT.
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
        let mut opaque = Vec::new();
        let mut uuids = std::collections::HashSet::new();
        for entry in entries {
            if matches!(entry.get("id"), Some(Nbt::String(id)) if id == "minecraft:item" || id == "item")
            {
                let result = SavedItem::read(&entry, codec).and_then(|item| {
                    let [x, y, z] = item.position;
                    if x.abs() > 30_000_512.0
                        || y.abs() > 20_000_000.0
                        || z.abs() > 30_000_512.0
                        || ChunkPos::at(x, z) != pos
                    {
                        return Err(invalid(
                            "item entity position is outside its saved chunk or world limits",
                        ));
                    }
                    Ok(item)
                });
                match result {
                    Ok(item) => {
                        if !uuids.insert(item.uuid) {
                            return Err(invalid("duplicate item entity UUID"));
                        }
                        items.push(item);
                        continue;
                    }
                    Err(error) => log::warn!(
                        "Keeping an inactive item in entity chunk {}, {}: {error}",
                        pos.x,
                        pos.z
                    ),
                }
            }
            opaque.push(entry);
        }
        root.insert("Entities", Nbt::List(List::Compound(opaque)));
        Ok(Self {
            items,
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
        root.insert("Entities", Nbt::List(List::Compound(entries)));
        Ok(root)
    }
}
