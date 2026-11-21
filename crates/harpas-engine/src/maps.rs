//! Treasure maps are found and saved on a worker before a chest can publish their item stacks.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use harpas_anvil::MapFiles;
use harpas_core::item::items;
use harpas_core::math::sin_f64;
use harpas_core::nbt::{Compound, Nbt};

use crate::Game;
use crate::block::BlockPos;
use crate::inventory::ItemStack;
use crate::item_data::{ItemCodec, invalid};
use crate::player::{Output, Position};
use crate::world::{ChunkSource, ChunkStorage};

const SIDE: usize = 128;
const SCALE: i8 = 1;
const MAX_LOADED: usize = 128;
const MAP_ID: &str = "minecraft:map_id";
const DECORATIONS: &str = "minecraft:map_decorations";

/// One of the decorations used by a buried treasure map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapIconKind {
    /// The player is inside the image.
    Player,
    /// The player is just outside the image.
    PlayerOffMap,
    /// The player is far from the mapped area.
    PlayerOffLimits,
    /// Where to dig for the treasure.
    RedX,
}

/// A map decoration in the client's signed map coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapIcon {
    /// Which sprite to display.
    pub kind: MapIconKind,
    /// Horizontal position in half pixels, relative to the center.
    pub x: i8,
    /// Vertical position in half pixels, relative to the center.
    pub z: i8,
    /// A rotation from 0 to 15 in sixteenths of a turn.
    pub direction: u8,
}

/// The saved image and settings of a 26.1 map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapData {
    center: [i32; 2],
    scale: i8,
    locked: bool,
    tracking: bool,
    unlimited: bool,
    colors: Arc<[u8]>,
}

impl MapData {
    /// Renders the initial coastline preview for a buried treasure map.
    pub fn treasure_preview(target: [i32; 2], mut water_at: impl FnMut(i32, i32) -> bool) -> Self {
        let span = (SIDE as i32) << SCALE;
        let center = target.map(|block| (block + 64).div_euclid(span) * span + span / 2 - 64);
        let water: Vec<_> = (0..SIDE)
            .flat_map(|z| (0..SIDE).map(move |x| (x, z)))
            .map(|(x, z)| {
                let block = |i: usize, center: i32| center + (i as i32 - SIDE as i32 / 2) * 2;
                water_at(block(x, center[0]), block(z, center[1]))
            })
            .collect();
        let mut colors = vec![0; SIDE * SIDE];
        for z in 1..SIDE - 1 {
            for x in 1..SIDE - 1 {
                let i = z * SIDE + x;
                let wet = [
                    i - SIDE - 1,
                    i - SIDE,
                    i - SIDE + 1,
                    i - 1,
                    i + 1,
                    i + SIDE - 1,
                    i + SIDE,
                    i + SIDE + 1,
                ]
                .into_iter()
                .filter(|&neighbor| water[neighbor])
                .count();
                // MapColor packs the palette ID and its two brightness bits into one byte.
                const ORANGE: u8 = 15 * 4;
                const BROWN: u8 = 26 * 4;
                colors[i] = match (water[i], wet) {
                    (false, 0) => 0,
                    (false, 1..=3) => BROWN + 3,
                    (false, _) => BROWN + 1,
                    (true, 8) if z % 2 == 1 => 0,
                    (true, 8) => {
                        let wave = (x as i32 + (sin_f64(z as f64) * 7.0) as i32) / 8 % 5;
                        ORANGE + [0, 1, 2, 1, 0][wave as usize]
                    }
                    (true, 6..=7) => ORANGE + 1,
                    (true, 2..=5) => ORANGE,
                    (true, _) => ORANGE + 3,
                };
            }
        }
        Self {
            center,
            scale: SCALE,
            locked: false,
            tracking: true,
            unlimited: true,
            colors: colors.into(),
        }
    }

    pub(crate) fn from_nbt(data: &Compound) -> io::Result<Self> {
        if data.get("dimension") != Some(&Nbt::from("minecraft:overworld")) {
            return Err(invalid("treasure map data must be in the Overworld"));
        }
        let (Some(Nbt::Int(x)), Some(Nbt::Int(z)), Some(Nbt::ByteArray(colors))) =
            (data.get("xCenter"), data.get("zCenter"), data.get("colors"))
        else {
            return Err(invalid("map data is missing a center or image"));
        };
        if colors.len() != SIDE * SIDE {
            return Err(invalid("map image must be 128 × 128"));
        }
        let scale = match data.get("scale") {
            None => 0,
            Some(Nbt::Byte(scale @ 0..=4)) => *scale,
            _ => return Err(invalid("invalid map scale")),
        };
        Ok(Self {
            center: [*x, *z],
            scale,
            locked: data.get("locked") == Some(&Nbt::Byte(1)),
            tracking: data.get("trackingPosition") != Some(&Nbt::Byte(0)),
            unlimited: data.get("unlimitedTracking") == Some(&Nbt::Byte(1)),
            colors: colors.iter().map(|&color| color as u8).collect(),
        })
    }

    pub(crate) fn to_nbt(&self) -> Compound {
        Compound::from_iter([
            ("dimension", Nbt::from("minecraft:overworld")),
            ("xCenter", Nbt::Int(self.center[0])),
            ("zCenter", Nbt::Int(self.center[1])),
            ("scale", Nbt::Byte(self.scale)),
            ("unlimitedTracking", Nbt::Byte(i8::from(self.unlimited))),
            (
                "colors",
                Nbt::ByteArray(self.colors.iter().map(|&color| color as i8).collect()),
            ),
        ])
    }

    fn relative(&self, x: f64, z: f64) -> [f32; 2] {
        let scale = (1 << self.scale) as f32;
        [
            (x - f64::from(self.center[0])) as f32 / scale,
            (z - f64::from(self.center[1])) as f32 / scale,
        ]
    }

    fn icons(&self, position: Position, target: (f64, f64)) -> Vec<MapIcon> {
        let mut icons = Vec::with_capacity(2);
        let [x, z] = self.relative(position.x, position.z);
        if self.tracking {
            let distance = x.abs().max(z.abs());
            let (kind, direction) = if distance <= 63.0 {
                let yaw = f64::from(position.yaw);
                let adjusted = if yaw < 0.0 { yaw - 8.0 } else { yaw + 8.0 };
                (
                    Some(MapIconKind::Player),
                    (adjusted * 16.0 / 360.0) as i32 as u8 & 15,
                )
            } else if distance < 320.0 {
                (Some(MapIconKind::PlayerOffMap), 0)
            } else {
                (self.unlimited.then_some(MapIconKind::PlayerOffLimits), 0)
            };
            if let Some(kind) = kind {
                icons.push(icon(kind, x, z, direction));
            }
        }
        let [x, z] = self.relative(target.0, target.1);
        icons.push(icon(MapIconKind::RedX, x, z, 8));
        icons
    }
}

fn icon(kind: MapIconKind, x: f32, z: f32, direction: u8) -> MapIcon {
    let coordinate = |n: f32| {
        if n <= -63.0 {
            -128
        } else if n >= 63.0 {
            127
        } else {
            (f64::from(n * 2.0) + 0.5) as i8
        }
    };
    MapIcon {
        kind,
        x: coordinate(x),
        z: coordinate(z),
        direction,
    }
}

fn target(codec: &ItemCodec, stack: &ItemStack) -> Option<(f64, f64)> {
    let Nbt::Compound(decorations) = codec.saved_component(stack, DECORATIONS)? else {
        return None;
    };
    let Some(Nbt::Compound(marker)) = decorations.get("+") else {
        return None;
    };
    let (Some(Nbt::Double(x)), Some(Nbt::Double(z))) = (marker.get("x"), marker.get("z")) else {
        return None;
    };
    (marker.get("type") == Some(&Nbt::from("minecraft:red_x"))).then_some((*x, *z))
}

#[derive(Clone, Copy)]
pub(crate) struct TreasureMap {
    pub(crate) id: i32,
    pub(crate) target: [i32; 2],
}

impl TreasureMap {
    pub(crate) fn apply(self, stack: &mut ItemStack, codec: &ItemCodec) -> io::Result<()> {
        stack.item = items().id("minecraft:filled_map").unwrap();
        codec.set_saved_component(stack, MAP_ID, &Nbt::Int(self.id))?;
        let marker = Compound::from_iter([
            ("type", Nbt::from("minecraft:red_x")),
            ("x", Nbt::Double(f64::from(self.target[0]))),
            ("z", Nbt::Double(f64::from(self.target[1]))),
            ("rotation", Nbt::Float(180.0)),
        ]);
        let decorations = Compound::from_iter([("+", Nbt::Compound(marker))]);
        codec.set_saved_component(stack, DECORATIONS, &Nbt::Compound(decorations))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct MapChest {
    pub(crate) pos: BlockPos,
    pub(crate) seed: i64,
}

#[derive(Clone, Copy)]
pub(crate) enum PendingChestAction {
    Open(BlockPos),
    Break(BlockPos),
}

impl Game {
    pub(crate) fn retry_map_chests(&mut self) {
        if let Some(world) = &mut self.world {
            world.maps.poll();
        }
        for index in 0..self.players.len() {
            let player = &mut self.players[index];
            let Some(action) = player.pending_map_chest.take() else {
                continue;
            };
            let (PendingChestAction::Open(pos) | PendingChestAction::Break(pos)) = action;
            let range = player.block_interaction_range() + 1.0;
            if player.disconnecting
                || !player.data_ready
                || player.awaiting_teleport.is_some()
                || pos.distance_squared(player.eye_position()) >= range * range
                || harpas_core::block::block_states().name(self.block_at(pos))
                    != Some(crate::chest::CHEST)
            {
                continue;
            }
            match action {
                PendingChestAction::Open(pos) => self.open_chest(index, pos),
                PendingChestAction::Break(pos) => {
                    self.destroy_block(index, pos);
                }
            }
        }
    }

    pub(crate) fn send_held_maps(&mut self) {
        let Some(world) = &mut self.world else { return };
        let codec = world.item_codec();
        for player in &mut self.players {
            if player.disconnecting || !player.data_ready {
                continue;
            }
            let mut held = [None; 2];
            for (hand, offhand) in [false, true].into_iter().enumerate() {
                let stack = &player.inventory.slots[player.inventory.hand_slot(offhand)];
                if stack.name() != Some("minecraft:filled_map") {
                    continue;
                }
                let Some(Nbt::Int(id)) = codec.saved_component(stack, MAP_ID) else {
                    continue;
                };
                let Some(target) = target(&codec, stack) else {
                    continue;
                };
                if held.contains(&Some(id)) {
                    continue;
                }
                held[hand] = Some(id);
                let Some(data) = world.maps.get(id) else {
                    continue;
                };
                let icons = data.icons(player.player.position, target);
                if player.sent_maps.get(&id) == Some(&icons) {
                    continue;
                }
                let first = player.sent_maps.insert(id, icons.clone()).is_none();
                player.send(Output::MapUpdate {
                    id,
                    scale: data.scale,
                    locked: data.locked,
                    icons,
                    colors: first.then(|| Arc::clone(&data.colors)),
                });
            }
            player.sent_maps.retain(|id, _| held.contains(&Some(*id)));
        }
    }
}

enum Request {
    Find(MapChest),
    Read(i32),
}

enum Reply {
    Found(MapChest, io::Result<Option<(TreasureMap, MapData)>>),
    Read(i32, io::Result<Option<MapData>>),
}

pub(crate) struct Maps {
    requests: Option<SyncSender<Request>>,
    replies: Option<mpsc::Receiver<Reply>>,
    worker: Option<JoinHandle<()>>,
    searching: HashSet<MapChest>,
    ready: HashMap<MapChest, (Instant, io::Result<Option<TreasureMap>>)>,
    reading: HashSet<i32>,
    loaded: HashMap<i32, Option<MapData>>,
    load_order: VecDeque<i32>,
}

impl Maps {
    pub(crate) fn start(
        world: &Path,
        source: Option<(Arc<dyn ChunkSource>, Arc<ChunkStorage>)>,
    ) -> io::Result<Self> {
        let mut files = MapFiles::new(world);
        let (requests, jobs) = mpsc::sync_channel(16);
        let (done, replies) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("harpas-maps".to_owned())
            .spawn(move || {
                for job in jobs {
                    let reply = match job {
                        Request::Find(chest) => Reply::Found(
                            chest,
                            (|| {
                                let Some((source, storage)) = &source else {
                                    return Ok(None);
                                };
                                let Some((data, target)) =
                                    source.treasure_map(chest.pos, storage)?
                                else {
                                    return Ok(None);
                                };
                                let id = files.create(data.to_nbt())?;
                                Ok(Some((TreasureMap { id, target }, data)))
                            })(),
                        ),
                        Request::Read(id) => Reply::Read(
                            id,
                            files
                                .read(id)
                                .and_then(|data| data.as_ref().map(MapData::from_nbt).transpose()),
                        ),
                    };
                    if done.send(reply).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests: Some(requests),
            replies: Some(replies),
            worker: Some(worker),
            searching: HashSet::new(),
            ready: HashMap::new(),
            reading: HashSet::new(),
            loaded: HashMap::new(),
            load_order: VecDeque::new(),
        })
    }

    fn request(&self, request: Request) -> io::Result<()> {
        match self.requests.as_ref().unwrap().try_send(request) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(pending()),
            Err(TrySendError::Disconnected(_)) => Err(io::Error::other("map worker stopped")),
        }
    }

    pub(crate) fn poll(&mut self) {
        self.ready
            .retain(|_, (time, _)| time.elapsed() < Duration::from_secs(60));
        while let Ok(reply) = self.replies.as_ref().unwrap().try_recv() {
            match reply {
                Reply::Found(chest, result) => {
                    self.searching.remove(&chest);
                    let result = result.map(|found| {
                        found.map(|(map, data)| {
                            self.cache(map.id, Some(data));
                            map
                        })
                    });
                    self.ready.insert(chest, (Instant::now(), result));
                }
                Reply::Read(id, result) => {
                    self.reading.remove(&id);
                    if let Err(error) = &result {
                        log::warn!("Cannot read map {id}: {error}");
                    }
                    self.cache(id, result.unwrap_or(None));
                }
            }
        }
    }

    fn cache(&mut self, id: i32, data: Option<MapData>) {
        if self.loaded.insert(id, data).is_some() {
            return;
        }
        self.load_order.push_back(id);
        if self.load_order.len() > MAX_LOADED {
            let oldest = self.load_order.pop_front().unwrap();
            self.loaded.remove(&oldest);
        }
    }

    pub(crate) fn prepare(&mut self, chest: MapChest) -> io::Result<()> {
        self.poll();
        if let Some((_, Err(_))) = self.ready.get(&chest) {
            return self.ready.remove(&chest).unwrap().1.map(|_| ());
        }
        if self.ready.contains_key(&chest) {
            return Ok(());
        }
        if !self.searching.contains(&chest) {
            self.request(Request::Find(chest))?;
            self.searching.insert(chest);
        }
        Err(pending())
    }

    pub(crate) fn prepared(&self, chest: MapChest) -> Option<TreasureMap> {
        self.ready
            .get(&chest)
            .and_then(|(_, result)| *result.as_ref().ok()?)
    }

    pub(crate) fn consume(&mut self, chest: MapChest) {
        self.ready.remove(&chest);
    }

    fn get(&mut self, id: i32) -> Option<&MapData> {
        if !self.loaded.contains_key(&id)
            && !self.reading.contains(&id)
            && self.request(Request::Read(id)).is_ok()
        {
            self.reading.insert(id);
        }
        self.loaded.get(&id).and_then(Option::as_ref)
    }
}

impl Drop for Maps {
    fn drop(&mut self) {
        self.requests.take();
        self.replies.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn pending() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "treasure map search is pending")
}
