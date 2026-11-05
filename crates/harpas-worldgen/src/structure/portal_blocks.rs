//! Ruined portal blocks, observed through vanilla's piece and processor APIs.
//!
//! A portal is placed as a whole by the chunk containing the center of its template. The sparse
//! result includes the netherrack spread outside the piece bounds and can be shared by chunk workers.

use std::collections::{HashMap, HashSet};

use harpas_core::block::block_states;
use harpas_core::math::position_seed;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};

use super::shipwreck::{contains, property, set, shape_at, water};
use super::{BoundingBox, StructurePiece, StructureStart, TemplatePiece};
use crate::{
    GeneratedChunk, HeightmapType, Mirror, StructureTemplates, WorldgenData, WorldgenDataError,
};

/// The immutable result of one complete ruined portal placement, including terrain outside its bounds.
#[derive(Debug, Default)]
pub struct RuinedPortalBlocks {
    blocks: HashMap<[i32; 3], u32>,
    entities: HashMap<[i32; 3], Compound>,
}

impl RuinedPortalBlocks {
    /// Whether this pass replaces an existing block entity, including a freshly placed container.
    pub fn removes_block_entity(&self, entity: &Compound) -> bool {
        let (Some(Nbt::Int(x)), Some(Nbt::Int(y)), Some(Nbt::Int(z)), Some(Nbt::String(kind))) = (
            entity.get("x"),
            entity.get("y"),
            entity.get("z"),
            entity.get("id"),
        ) else {
            return false;
        };
        self.entities.contains_key(&[*x, *y, *z])
            || self.blocks.get(&[*x, *y, *z]).is_some_and(|&state| {
                let name = name(state);
                name != kind
                    && !(kind == "minecraft:brushable_block"
                        && matches!(
                            name,
                            "minecraft:suspicious_sand" | "minecraft:suspicious_gravel"
                        ))
                    && !(kind == "minecraft:sign" && name.ends_with("_sign"))
                    && !(kind == "minecraft:bed" && name.ends_with("_bed"))
            })
    }

    /// Applies only the target chunk's part. Other workers can apply the same result independently.
    pub fn place_in_chunk(&self, chunk: &mut GeneratedChunk) -> Vec<Compound> {
        for (&pos, &state) in &self.blocks {
            if contains(chunk, pos) {
                set(chunk, pos, state);
            }
        }
        self.entities
            .iter()
            .filter(|(pos, _)| contains(chunk, **pos))
            .map(|(_, entity)| entity.clone())
            .collect()
    }
}

/// Compiled block tag membership used by the portal's processors.
pub struct RuinedPortalGenerator {
    protected: Vec<bool>,
    stairs: Vec<bool>,
    slabs: Vec<bool>,
    walls: Vec<bool>,
}

impl RuinedPortalGenerator {
    /// Resolves the owner's vanilla block tags once, before workers begin.
    pub fn new(data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        Ok(Self {
            protected: data.block_tag_states("minecraft:features_cannot_replace")?,
            stairs: data.block_tag_states("minecraft:stairs")?,
            slabs: data.block_tag_states("minecraft:slabs")?,
            walls: data.block_tag_states("minecraft:walls")?,
        })
    }

    /// Runs a complete portal pass against carved or saved terrain. No shared start is mutated.
    pub fn prepare(
        &self,
        start: &StructureStart,
        seed: i64,
        templates: &StructureTemplates,
        min_y: i32,
        height: i32,
        block_at: &mut impl FnMut(i32, i32, i32) -> u32,
    ) -> RuinedPortalBlocks {
        let Some(index) = portal_index(&start.structure) else {
            return RuinedPortalBlocks::default();
        };
        let mut world = PlacementWorld {
            result: RuinedPortalBlocks::default(),
            block_at,
            min_y,
            max_y: min_y + height,
        };
        for piece in &start.pieces {
            if piece.kind != "minecraft:rupo" {
                continue;
            }
            let Some(template) = &piece.template else {
                continue;
            };
            let Some(blocks) = templates.get(&template.template) else {
                continue;
            };
            let bounds = piece.bounding_box;
            let [cx, cy, cz] = bounds.center();
            // Vanilla's owning chunk must contain the piece center, including its Y coordinate.
            if cy < min_y || cy >= min_y + height {
                continue;
            }
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
            let decoration = random.set_decoration_seed(seed, (cx >> 4) * 16, (cz >> 4) * 16);
            random.set_feature_seed(decoration, index, 4);
            let props = Properties::of(piece);
            let mut processed = Vec::new();
            for block in blocks.blocks(template.position) {
                let pos @ [x, y, z] = position(template, block.pos);
                let block_name = name(block.state);
                if block_name == "minecraft:structure_block"
                    || (!props.air_pocket && block_states().is_air(block.state))
                {
                    continue;
                }
                let mut state = block.state;
                let mut rule = LegacyRandom::new(position_seed(x, y, z));
                if block_name == "minecraft:gold_block" && rule.next_float() < 0.3 {
                    state = id("air");
                } else if block_name == "minecraft:lava" {
                    if props.cold {
                        state = id("netherrack");
                    } else if props.ocean || rule.next_float() < 0.2 {
                        state = id("magma_block");
                    }
                } else if block_name == "minecraft:netherrack"
                    && !props.cold
                    && rule.next_float() < 0.07
                {
                    state = id("magma_block");
                }
                state = self.age(
                    state,
                    props.mossiness,
                    &mut LegacyRandom::new(position_seed(x, y, z)),
                );
                let old = world.read(pos);
                if self.protected[old as usize] {
                    continue;
                }
                if name(old) == "minecraft:lava" && !full_cube(state) {
                    state = id("lava");
                }
                if props.blackstone {
                    state = blackstone(state);
                }
                processed.push((pos, transform_state(state, template), block.nbt.as_ref()));
            }
            let mut placed = Vec::new();
            for (pos, mut state, nbt) in processed {
                if pos[1] < min_y || pos[1] >= min_y + height {
                    continue;
                }
                if water(world.read(pos)) && can_waterlog(state) {
                    state = property(state, "waterlogged", "true");
                }
                world.write(pos, state);
                placed.push(pos);
                if let Some(nbt) = nbt
                    && nbt.get("id") == Some(&Nbt::from(name(state)))
                {
                    let mut entity = nbt.clone();
                    for (key, value) in ["x", "y", "z"].into_iter().zip(pos) {
                        entity.insert(key, value);
                    }
                    entity.insert("components", Compound::new());
                    if name(state) == "minecraft:chest" {
                        let loot = random.next_long();
                        if entity.get("LootTable").is_some() && loot != 0 {
                            entity.insert("LootTableSeed", loot);
                        }
                    }
                    world.result.entities.insert(pos, entity);
                }
            }
            finish(&mut world, &placed);
            // TemplateStructurePiece resolves the template's jigsaw final_state after shape updates.
            for block in blocks.blocks(template.position) {
                if name(block.state) == "minecraft:jigsaw" {
                    let pos = position(template, block.pos);
                    if let Some(Nbt::String(final_state)) =
                        block.nbt.as_ref().and_then(|n| n.get("final_state"))
                        && let Some(state) = block_states().default_state(final_state)
                    {
                        world.write(pos, state);
                    }
                }
            }
            spread(&mut world, bounds, &props, &self.protected, &mut random);
            for x in bounds.min[0] + 1..bounds.max[0] {
                for z in bounds.min[2] + 1..bounds.max[2] {
                    if name(world.read([x, bounds.min[1], z])) == "minecraft:netherrack" {
                        drip(
                            &mut world,
                            [x, bounds.min[1] - 1, z],
                            props.cold,
                            &mut random,
                        );
                    }
                }
            }
            if props.vines || props.overgrown {
                for z in bounds.min[2]..=bounds.max[2] {
                    for y in bounds.min[1]..=bounds.max[1] {
                        for x in bounds.min[0]..=bounds.max[0] {
                            let pos = [x, y, z];
                            if props.vines {
                                vines(&mut world, pos, &mut random);
                            }
                            if props.overgrown {
                                leaves(&mut world, pos, &mut random);
                            }
                        }
                    }
                }
            }
        }
        world.result
    }

    fn age(&self, state: u32, mossiness: f32, random: &mut impl RandomSource) -> u32 {
        let name = name(state);
        if matches!(
            name,
            "minecraft:stone" | "minecraft:stone_bricks" | "minecraft:chiseled_stone_bricks"
        ) {
            if random.next_float() >= 0.5 {
                return state;
            }
            // Both candidate stairs draw their orientation before the material and alternative are chosen.
            let stone = random_stairs("stone_brick_stairs", random);
            let mossy = random_stairs("mossy_stone_brick_stairs", random);
            let moss = random.next_float() < mossiness;
            let i = random.next_int_bounded(2);
            match (moss, i) {
                (true, 0) => id("mossy_stone_bricks"),
                (true, _) => mossy,
                (false, 0) => id("cracked_stone_bricks"),
                (false, _) => stone,
            }
        } else if self.stairs[state as usize] {
            if random.next_float() >= 0.5 {
                return state;
            }
            let moss = random.next_float() < mossiness;
            let i = random.next_int_bounded(2);
            match (moss, i) {
                (true, 0) => copy_properties(state, id("mossy_stone_brick_stairs")),
                (true, _) => id("mossy_stone_brick_slab"),
                (false, 0) => id("stone_slab"),
                (false, _) => id("stone_brick_slab"),
            }
        } else if self.slabs[state as usize] {
            if random.next_float() < mossiness {
                property(
                    id("mossy_stone_brick_slab"),
                    "type",
                    block_states()
                        .properties(state)
                        .unwrap()
                        .1
                        .iter()
                        .find(|p| p.0 == "type")
                        .unwrap()
                        .1,
                )
            } else {
                state
            }
        } else if self.walls[state as usize] {
            if random.next_float() < mossiness {
                id("mossy_stone_brick_wall")
            } else {
                state
            }
        } else if name == "minecraft:obsidian" && random.next_float() < 0.15 {
            id("crying_obsidian")
        } else {
            state
        }
    }
}

impl StructureStart {
    /// Ruined portal terrain reads and writes extend beyond vanilla's saved piece bounds.
    pub fn ruined_portal_terrain_bounds(&self) -> Option<BoundingBox> {
        portal_index(&self.structure)?;
        self.pieces
            .iter()
            .filter(|p| p.kind == "minecraft:rupo")
            .map(|p| {
                let mut bounds = p.bounding_box;
                let [x, _, z] = bounds.center();
                bounds.min[0] = bounds.min[0].min(x - 14) - 2;
                bounds.max[0] = bounds.max[0].max(x + 14) + 2;
                bounds.min[2] = bounds.min[2].min(z - 14) - 2;
                bounds.max[2] = bounds.max[2].max(z + 14) + 2;
                bounds
            })
            .reduce(|mut a, b| {
                a.encapsulate(&b);
                a
            })
    }
}

fn portal_index(structure: &str) -> Option<i32> {
    Some(match structure {
        "minecraft:ruined_portal" => 10,
        "minecraft:ruined_portal_desert" => 11,
        "minecraft:ruined_portal_jungle" => 12,
        "minecraft:ruined_portal_mountain" => 13,
        "minecraft:ruined_portal_ocean" => 15,
        "minecraft:ruined_portal_swamp" => 16,
        _ => return None,
    })
}

struct Properties {
    air_pocket: bool,
    cold: bool,
    mossiness: f32,
    overgrown: bool,
    vines: bool,
    blackstone: bool,
    ocean: bool,
    surface: bool,
}
impl Properties {
    fn of(piece: &StructurePiece) -> Self {
        let props = match piece.save_data.get("Properties") {
            Some(Nbt::Compound(p)) => Some(p),
            _ => None,
        };
        let flag = |key| props.and_then(|p| p.get(key)) == Some(&Nbt::Byte(1));
        let placement = piece.save_data.get("VerticalPlacement");
        Self {
            air_pocket: flag("air_pocket"),
            cold: flag("cold"),
            overgrown: flag("overgrown"),
            vines: flag("vines"),
            blackstone: flag("replace_with_blackstone"),
            mossiness: match props.and_then(|p| p.get("mossiness")) {
                Some(Nbt::Float(v)) => *v,
                _ => 0.2,
            },
            ocean: placement == Some(&Nbt::from("on_ocean_floor")),
            surface: matches!(placement, Some(Nbt::String(s)) if s == "on_land_surface" || s == "on_ocean_floor"),
        }
    }
}

struct PlacementWorld<'a, F> {
    result: RuinedPortalBlocks,
    block_at: &'a mut F,
    min_y: i32,
    max_y: i32,
}
impl<F: FnMut(i32, i32, i32) -> u32> PlacementWorld<'_, F> {
    fn read(&mut self, pos: [i32; 3]) -> u32 {
        if pos[1] < self.min_y || pos[1] >= self.max_y {
            return 0;
        }
        self.result
            .blocks
            .get(&pos)
            .copied()
            .unwrap_or_else(|| (self.block_at)(pos[0], pos[1], pos[2]))
    }
    fn write(&mut self, pos: [i32; 3], state: u32) {
        if pos[1] < self.min_y || pos[1] >= self.max_y {
            return;
        }
        if name(self.read(pos)) != name(state) {
            self.result.entities.remove(&pos);
        }
        self.result.blocks.insert(pos, state);
    }
    fn height(&mut self, x: i32, z: i32, kind: HeightmapType) -> i32 {
        (self.min_y..self.max_y)
            .rev()
            .find(|&y| kind.matches(self.read([x, y, z])))
            .map_or(self.min_y, |y| y + 1)
    }
}

fn spread(
    world: &mut PlacementWorld<'_, impl FnMut(i32, i32, i32) -> u32>,
    bounds: BoundingBox,
    props: &Properties,
    protected: &[bool],
    random: &mut impl RandomSource,
) {
    let [cx, _, cz] = bounds.center();
    let average_span = (bounds.max[0] - bounds.min[0] + bounds.max[2] - bounds.min[2] + 2) / 2;
    let offset = random.next_int_bounded((8 - average_span / 2).max(1));
    // Observed radial falloff; the Manhattan radius is shifted by the template size and one draw.
    let chances = [
        1.0_f32, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.9, 0.9, 0.8, 0.7, 0.6, 0.4, 0.2,
    ];
    for x in cx - 14..=cx + 14 {
        for z in cz - 14..=cz + 14 {
            let distance = (x - cx).abs() + (z - cz).abs() + offset;
            if distance >= chances.len() as i32
                || random.next_double() >= f64::from(chances[distance as usize])
            {
                continue;
            }
            let surface = world.height(
                x,
                z,
                if props.ocean {
                    HeightmapType::OceanFloorWg
                } else {
                    HeightmapType::WorldSurfaceWg
                },
            ) - 1;
            let y = if props.surface {
                surface
            } else {
                surface.min(bounds.min[1])
            };
            if (y - bounds.min[1]).abs() > 3 {
                continue;
            }
            let pos = [x, y, z];
            let old = world.read(pos);
            if protected[old as usize]
                || block_states().is_air(old)
                || name(old) == "minecraft:obsidian"
                || name(old) == "minecraft:chest"
                || (name(old) == "minecraft:lava" && !props.ocean)
            {
                continue;
            }
            netherrack(world, pos, props.cold, random);
            if props.overgrown {
                leaves(world, pos, random);
            }
            drip(world, [x, y - 1, z], props.cold, random);
        }
    }
}
fn netherrack(
    world: &mut PlacementWorld<'_, impl FnMut(i32, i32, i32) -> u32>,
    pos: [i32; 3],
    cold: bool,
    random: &mut impl RandomSource,
) {
    let magma = !cold && random.next_float() < 0.07;
    world.write(pos, id(if magma { "magma_block" } else { "netherrack" }));
}
fn drip(
    world: &mut PlacementWorld<'_, impl FnMut(i32, i32, i32) -> u32>,
    mut pos: [i32; 3],
    cold: bool,
    random: &mut impl RandomSource,
) {
    // One guaranteed block, then up to eight more. The capped tail consumes no extra stop draw.
    netherrack(world, pos, cold, random);
    for _ in 0..8 {
        if random.next_float() >= 0.5 {
            break;
        }
        pos[1] -= 1;
        netherrack(world, pos, cold, random);
    }
}
fn leaves(
    world: &mut PlacementWorld<'_, impl FnMut(i32, i32, i32) -> u32>,
    pos: [i32; 3],
    random: &mut impl RandomSource,
) {
    if random.next_float() >= 0.5 || name(world.read(pos)) != "minecraft:netherrack" {
        return;
    }
    let above = [pos[0], pos[1] + 1, pos[2]];
    if block_states().is_air(world.read(above)) {
        world.write(above, property(id("jungle_leaves"), "persistent", "true"));
    }
}
fn vines(
    world: &mut PlacementWorld<'_, impl FnMut(i32, i32, i32) -> u32>,
    pos: [i32; 3],
    random: &mut impl RandomSource,
) {
    let state = world.read(pos);
    if block_states().is_air(state) || name(state) == "minecraft:vine" {
        return;
    }
    let (dx, dz, face) = [
        (0, -1, "south"),
        (1, 0, "west"),
        (0, 1, "north"),
        (-1, 0, "east"),
    ][random.next_int_bounded(4) as usize];
    let at = [pos[0] + dx, pos[1], pos[2] + dz];
    if block_states().is_air(world.read(at))
        && sturdy(
            state,
            match face {
                "north" => "south",
                "east" => "west",
                "south" => "north",
                _ => "east",
            },
        )
    {
        world.write(at, property(id("vine"), face, "true"));
    }
}
fn finish(world: &mut PlacementWorld<'_, impl FnMut(i32, i32, i32) -> u32>, placed: &[[i32; 3]]) {
    loop {
        let mut changed = false;
        for &pos in placed {
            let state = world.read(pos);
            if !can_waterlog(state) {
                continue;
            }
            if [[0, 1, 0], [0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]]
                .iter()
                .any(|d| {
                    let n = world.read(std::array::from_fn(|i| pos[i] + d[i]));
                    let (name, props) = block_states().properties(n).unwrap();
                    water(n) && (name != "minecraft:water" || props.contains(&("level", "0")))
                })
            {
                world.write(pos, property(state, "waterlogged", "true"));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut affected = placed.to_vec();
    let mut seen: HashSet<_> = placed.iter().copied().collect();
    for &pos in placed {
        for d in [
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [1, 0, 0],
            [0, 0, 1],
            [-1, 0, 0],
        ] {
            let at = std::array::from_fn(|i| pos[i] + d[i]);
            if seen.insert(at) {
                affected.push(at);
            }
        }
    }
    for &pos in &affected {
        if name(world.read(pos)).ends_with("_stairs") {
            let state = portal_shape(world, pos);
            world.write(pos, state);
        }
    }
    for &pos in &affected {
        let state = portal_shape(world, pos);
        world.write(pos, state);
    }
    // Existing vines can lose support when a replay clears an old portal block. Resolve hanging
    // faces from the top down so a vine does not retain a face from a vine that is about to drop.
    affected.sort_by_key(|pos| std::cmp::Reverse(pos[1]));
    for pos in affected {
        let state = world.read(pos);
        if name(state) != "minecraft:vine" {
            continue;
        }
        let mut updated = state;
        for (key, dx, dy, dz, face) in [
            ("north", 0, 0, -1, "south"),
            ("east", 1, 0, 0, "west"),
            ("south", 0, 0, 1, "north"),
            ("west", -1, 0, 0, "east"),
            ("up", 0, 1, 0, "down"),
        ] {
            if !block_states()
                .properties(state)
                .unwrap()
                .1
                .contains(&(key, "true"))
            {
                continue;
            }
            let neighbor = world.read([pos[0] + dx, pos[1] + dy, pos[2] + dz]);
            let supported = if key == "up" {
                full_cube(neighbor)
            } else {
                sturdy(neighbor, face)
            };
            let above = world.read([pos[0], pos[1] + 1, pos[2]]);
            let hangs = key != "up"
                && name(above) == "minecraft:vine"
                && block_states()
                    .properties(above)
                    .unwrap()
                    .1
                    .contains(&(key, "true"));
            if !supported && !hangs {
                updated = property(updated, key, "false");
            }
        }
        if !block_states()
            .properties(updated)
            .unwrap()
            .1
            .iter()
            .any(|(_, v)| *v == "true")
        {
            updated = id("air");
        }
        world.write(pos, updated);
    }
}
fn portal_shape(
    world: &mut PlacementWorld<'_, impl FnMut(i32, i32, i32) -> u32>,
    pos: [i32; 3],
) -> u32 {
    let mut state = shape_at(pos, &mut |p| world.read(p));
    if name(state) == "minecraft:iron_bars" {
        for (key, dx, dz) in [
            ("north", 0, -1),
            ("east", 1, 0),
            ("south", 0, 1),
            ("west", -1, 0),
        ] {
            let neighbor = world.read([pos[0] + dx, pos[1], pos[2] + dz]);
            let connects = name(neighbor) == "minecraft:iron_bars"
                || sturdy(
                    neighbor,
                    match key {
                        "north" => "south",
                        "east" => "west",
                        "south" => "north",
                        _ => "east",
                    },
                );
            state = property(state, key, if connects { "true" } else { "false" });
        }
    }
    if name(state).ends_with("_wall") {
        let above = world.read([pos[0], pos[1] + 1, pos[2]]);
        for (key, dx, dz) in [
            ("north", 0, -1),
            ("east", 1, 0),
            ("south", 0, 1),
            ("west", -1, 0),
        ] {
            let neighbor = world.read([pos[0] + dx, pos[1], pos[2] + dz]);
            let connects = name(neighbor).ends_with("_wall")
                || sturdy(
                    neighbor,
                    match key {
                        "north" => "south",
                        "east" => "west",
                        "south" => "north",
                        _ => "east",
                    },
                );
            state = property(
                state,
                key,
                if !connects {
                    "none"
                } else if full_cube(above)
                    || block_states()
                        .properties(above)
                        .unwrap()
                        .1
                        .contains(&("half", "bottom"))
                    || block_states()
                        .properties(above)
                        .unwrap()
                        .1
                        .contains(&("type", "bottom"))
                {
                    "tall"
                } else {
                    "low"
                },
            );
        }
    }
    state
}

fn random_stairs(name: &str, random: &mut impl RandomSource) -> u32 {
    let state = property(
        id(name),
        "facing",
        ["north", "east", "south", "west"][random.next_int_bounded(4) as usize],
    );
    property(
        state,
        "half",
        if random.next_int_bounded(2) == 0 {
            "top"
        } else {
            "bottom"
        },
    )
}
fn copy_properties(from: u32, to: u32) -> u32 {
    block_states()
        .properties(from)
        .unwrap()
        .1
        .into_iter()
        .fold(to, |s, (k, v)| property(s, k, v))
}
fn blackstone(state: u32) -> u32 {
    let replacement = match name(state) {
        "minecraft:cobblestone" | "minecraft:mossy_cobblestone" => "blackstone",
        "minecraft:cobblestone_stairs" | "minecraft:mossy_cobblestone_stairs" => {
            "blackstone_stairs"
        }
        "minecraft:cobblestone_slab" | "minecraft:mossy_cobblestone_slab" => "blackstone_slab",
        "minecraft:cobblestone_wall" | "minecraft:mossy_cobblestone_wall" => "blackstone_wall",
        "minecraft:stone" => "polished_blackstone",
        "minecraft:stone_slab" | "minecraft:smooth_stone_slab" => "polished_blackstone_slab",
        "minecraft:stone_stairs" => "polished_blackstone_stairs",
        "minecraft:iron_bars" => "iron_chain",
        "minecraft:stone_bricks" | "minecraft:mossy_stone_bricks" => "polished_blackstone_bricks",
        "minecraft:stone_brick_stairs" | "minecraft:mossy_stone_brick_stairs" => {
            "polished_blackstone_brick_stairs"
        }
        "minecraft:stone_brick_slab" | "minecraft:mossy_stone_brick_slab" => {
            "polished_blackstone_brick_slab"
        }
        "minecraft:stone_brick_wall" | "minecraft:mossy_stone_brick_wall" => {
            "polished_blackstone_brick_wall"
        }
        "minecraft:cracked_stone_bricks" => "cracked_polished_blackstone_bricks",
        "minecraft:chiseled_stone_bricks" => "chiseled_polished_blackstone",
        _ => return state,
    };
    copy_properties(state, id(replacement))
}
fn position(template: &TemplatePiece, pos: [i32; 3]) -> [i32; 3] {
    let rotated = template
        .rotation
        .transform_around(template.mirror.apply(pos), template.pivot);
    std::array::from_fn(|i| rotated[i] + template.position[i])
}
fn transform_state(state: u32, template: &TemplatePiece) -> u32 {
    let mut state = state;
    if template.mirror == Mirror::FrontBack {
        let (_, props) = block_states().properties(state).unwrap();
        for (k, v) in props {
            if k == "facing" && matches!(v, "east" | "west") {
                state = property(state, k, if v == "east" { "west" } else { "east" });
            }
        }
    }
    super::igloo::rotate_state(state, template.rotation)
}
fn can_waterlog(state: u32) -> bool {
    let (_, props) = block_states().properties(state).unwrap();
    props.contains(&("waterlogged", "false")) && !props.contains(&("type", "double"))
}
fn sturdy(state: u32, face: &str) -> bool {
    if full_cube(state) || name(state).ends_with("_leaves") {
        return true;
    }
    let (name, props) = block_states().properties(state).unwrap();
    if !name.ends_with("_stairs") {
        return false;
    }
    let facing = props.iter().find(|p| p.0 == "facing").unwrap().1;
    let shape = props.iter().find(|p| p.0 == "shape").unwrap().1;
    let direction = |d| match d {
        "north" => (0, -1),
        "east" => (1, 0),
        "south" => (0, 1),
        _ => (-1, 0),
    };
    let (dx, dz) = direction(facing);
    !shape.starts_with("outer") && facing == face
        || shape == "inner_left" && direction(face) == (dz, -dx)
        || shape == "inner_right" && direction(face) == (-dz, dx)
}

fn full_cube(state: u32) -> bool {
    let (name, props) = block_states().properties(state).unwrap();
    HeightmapType::OceanFloor.matches(state)
        && !name.ends_with("_stairs")
        && (!name.ends_with("_slab") || props.contains(&("type", "double")))
        && !name.ends_with("_wall")
        && !name.ends_with("_fence")
        && !name.ends_with("_leaves")
        && !name.ends_with("_trapdoor")
        && !name.ends_with("_door")
        && !matches!(
            name,
            "minecraft:chest"
                | "minecraft:iron_bars"
                | "minecraft:iron_chain"
                | "minecraft:lantern"
                | "minecraft:pointed_dripstone"
        )
}
fn id(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}
fn name(state: u32) -> &'static str {
    block_states().properties(state).unwrap().0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_replay_removes_only_vine_faces_that_lost_support() {
        let mut terrain = |_, _, _| id("air");
        let mut world = PlacementWorld {
            result: RuinedPortalBlocks::default(),
            block_at: &mut terrain,
            min_y: -64,
            max_y: 320,
        };
        let old_support = [0, 50, 0];
        world.write(old_support, id("stone"));
        world.write([1, 50, -1], id("stone"));
        world.write(
            [1, 50, 0],
            property(property(id("vine"), "west", "true"), "north", "true"),
        );
        world.write([-1, 50, 0], property(id("vine"), "east", "true"));
        // A fresh template pass clears this old support, next to two existing vines.
        world.write(old_support, id("air"));
        finish(&mut world, &[old_support]);
        assert_eq!(world.read([-1, 50, 0]), id("air"));
        assert_eq!(
            world.read([1, 50, 0]),
            property(id("vine"), "north", "true")
        );
    }
}
