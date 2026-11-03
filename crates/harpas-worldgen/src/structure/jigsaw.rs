//! Jigsaw structures: vanilla's `JigsawStructure.findGenerationPoint` and `JigsawPlacement`, which grow a structure
//! by connecting templates from template pools at their jigsaw blocks.

use std::collections::{BTreeMap, HashMap, VecDeque};

use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};

use super::start::{BoundingBox, JigsawJunction, JigsawPiece, StructurePiece};
use crate::data::{
    JigsawDef, PoolAliasDef, PoolElementDef, Projection, StartHeightDef, WorldgenData,
    WorldgenDataError,
};
use crate::generator::{HeightmapType, NoiseBasedChunkGenerator};
use crate::template::{
    Facing, JigsawBlock, JointType, Rotation, StructureTemplate, StructureTemplates,
};

/// The pool with no elements, which needs no warning when used (`Pools.EMPTY`).
const EMPTY_POOL: &str = "minecraft:empty";

/// Where a jigsaw structure's pieces may still go: a box, with the pieces already placed in it taken out. Vanilla
/// keeps this as a `VoxelShape`; for these boxes, a piece fits exactly when it is inside the outer box and
/// overlaps none of the taken pieces.
struct FreeSpace {
    /// The lowest corner, and one past the highest corner.
    outer: ([i32; 3], [i32; 3]),
    taken: Vec<BoundingBox>,
}

impl FreeSpace {
    fn fits(&self, b: &BoundingBox) -> bool {
        (0..3).all(|axis| b.min[axis] >= self.outer.0[axis] && b.max[axis] < self.outer.1[axis])
            && !self.taken.iter().any(|taken| {
                (0..3).all(|axis| taken.max[axis] >= b.min[axis] && taken.min[axis] <= b.max[axis])
            })
    }
}

/// A placed piece while the structure grows (`PoolElementStructurePiece`).
struct Piece<'d> {
    element: &'d PoolElementDef,
    position: [i32; 3],
    rotation: Rotation,
    bounding_box: BoundingBox,
    ground_level_delta: i32,
    junctions: Vec<JigsawJunction>,
}

/// A piece whose jigsaws are still to be tried (`JigsawPlacement.PieceState`).
struct PieceState {
    piece: usize,
    free: usize,
    depth: i32,
}

/// `SequencedPriorityIterator`: the highest placement priority first, then in the order added.
#[derive(Default)]
struct Placing {
    queues: BTreeMap<i32, VecDeque<PieceState>>,
}

impl Placing {
    fn add(&mut self, state: PieceState, priority: i32) {
        self.queues.entry(priority).or_default().push_back(state);
    }

    fn next(&mut self) -> Option<PieceState> {
        self.queues
            .values_mut()
            .rev()
            .find(|queue| !queue.is_empty())?
            .pop_front()
    }
}

/// What jigsaw placement reads from the world and the data.
struct Context<'d> {
    generator: &'d NoiseBasedChunkGenerator,
    data: &'d WorldgenData,
    templates: &'d StructureTemplates,
    max_sizes: HashMap<&'d str, i32>,
    /// The pools this start renames (`PoolAliasLookup`).
    aliases: HashMap<&'d str, &'d str>,
}

impl<'d> Context<'d> {
    /// The template at a location, or an empty one as vanilla makes when there is none.
    fn template(&self, location: &str) -> &'d StructureTemplate {
        static EMPTY: StructureTemplate = StructureTemplate::empty();
        self.templates.get(location).unwrap_or(&EMPTY)
    }

    /// `StructurePoolElement.getBoundingBox`.
    fn bounding_box(
        &self,
        element: &PoolElementDef,
        position: [i32; 3],
        rotation: Rotation,
    ) -> Result<BoundingBox, WorldgenDataError> {
        Ok(match element {
            PoolElementDef::Single { location, .. } => {
                self.template(location).bounding_box(position, rotation)
            }
            PoolElementDef::Feature { .. } => BoundingBox {
                min: position,
                max: position,
            },
            PoolElementDef::List { elements, .. } => {
                let mut boxes = elements
                    .iter()
                    .filter(|element| !matches!(element, PoolElementDef::Empty))
                    .map(|element| self.bounding_box(element, position, rotation));
                let mut result =
                    boxes
                        .next()
                        .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                            context: "list_pool_element".to_string(),
                            message: "list has no elements with a bounding box".to_string(),
                        })??;
                for b in boxes {
                    let b = b?;
                    for axis in 0..3 {
                        result.min[axis] = result.min[axis].min(b.min[axis]);
                        result.max[axis] = result.max[axis].max(b.max[axis]);
                    }
                }
                result
            }
            PoolElementDef::Empty => unreachable!("empty elements are never placed"),
        })
    }

    /// `StructurePoolElement.getShuffledJigsawBlocks`.
    fn shuffled_jigsaws(
        &self,
        element: &PoolElementDef,
        position: [i32; 3],
        rotation: Rotation,
        random: &mut impl RandomSource,
    ) -> Result<Vec<JigsawBlock>, WorldgenDataError> {
        Ok(match element {
            PoolElementDef::Single { location, .. } => {
                let mut jigsaws = self.template(location).jigsaws(position, rotation);
                shuffle(&mut jigsaws, random);
                jigsaws.sort_by_key(|jigsaw| std::cmp::Reverse(jigsaw.selection_priority));
                jigsaws
            }
            PoolElementDef::Feature { .. } => vec![JigsawBlock {
                pos: position,
                front: Facing::Down,
                top: Facing::South,
                name: "minecraft:bottom".to_string(),
                pool: EMPTY_POOL.to_string(),
                target: "minecraft:empty".to_string(),
                joint: JointType::Rollable,
                placement_priority: 0,
                selection_priority: 0,
            }],
            PoolElementDef::List { elements, .. } => {
                self.shuffled_jigsaws(&elements[0], position, rotation, random)?
            }
            PoolElementDef::Empty => Vec::new(),
        })
    }

    /// `StructureTemplatePool.getMaxSize`: the tallest element of a pool, or 0 for a missing pool.
    fn max_size(&mut self, pool: &'d str) -> Result<i32, WorldgenDataError> {
        if let Some(&size) = self.max_sizes.get(pool) {
            return Ok(size);
        }
        let mut size = 0;
        if let Some(definition) = self.data.template_pools.get(pool) {
            for element in definition.templates() {
                if !matches!(element, PoolElementDef::Empty) {
                    let b = self.bounding_box(element, [0, 0, 0], Rotation::None)?;
                    size = size.max(b.max[1] - b.min[1] + 1);
                }
            }
        }
        self.max_sizes.insert(pool, size);
        Ok(size)
    }

    /// The pool a jigsaw's pool name stands for in this start.
    fn alias<'n>(&self, pool: &'n str) -> &'n str
    where
        'd: 'n,
    {
        self.aliases.get(pool).copied().unwrap_or(pool)
    }

    /// A pool that exists and is usable: not empty unless it is the empty pool.
    fn usable_pool(&self, name: &str) -> Option<&'d crate::data::TemplatePoolDef> {
        let pool = self.data.template_pools.get(name)?;
        (!pool.elements.is_empty() || name == EMPTY_POOL).then_some(pool)
    }
}

/// `PoolAliasLookup.create`: resolves the renamings with a random generator for the start position.
fn resolve_aliases(
    bindings: &[PoolAliasDef],
    level_seed: i64,
    [x, y, z]: [i32; 3],
) -> HashMap<&str, &str> {
    let mut aliases = HashMap::new();
    if bindings.is_empty() {
        return aliases;
    }
    let mut random = LegacyRandom::new(level_seed).fork_positional().at(x, y, z);
    for binding in bindings {
        resolve_alias(binding, &mut random, &mut aliases);
    }
    aliases
}

/// `PoolAliasBinding.forEachResolved`.
fn resolve_alias<'d>(
    binding: &'d PoolAliasDef,
    random: &mut impl RandomSource,
    aliases: &mut HashMap<&'d str, &'d str>,
) {
    match binding {
        PoolAliasDef::Direct { alias, target } => {
            aliases.insert(alias, target);
        }
        PoolAliasDef::Random { alias, targets } => {
            aliases.insert(alias, weighted(targets, random));
        }
        PoolAliasDef::RandomGroup { groups } => {
            for binding in weighted(groups, random) {
                resolve_alias(binding, random, aliases);
            }
        }
    }
}

/// `WeightedList.getRandomOrThrow`.
fn weighted<'a, T>(entries: &'a [(T, i32)], random: &mut impl RandomSource) -> &'a T {
    let total = entries.iter().map(|(_, weight)| weight).sum();
    let mut selection = random.next_int_bounded(total);
    for (entry, weight) in entries {
        selection -= weight;
        if selection < 0 {
            return entry;
        }
    }
    unreachable!("a selection below the total weight")
}

/// `Util.shuffle`.
fn shuffle<T>(values: &mut [T], random: &mut impl RandomSource) {
    for size in (2..=values.len()).rev() {
        let other = random.next_int_bounded(size as i32) as usize;
        values.swap(size - 1, other);
    }
}

const fn step(facing: Facing) -> [i32; 3] {
    match facing {
        Facing::Down => [0, -1, 0],
        Facing::Up => [0, 1, 0],
        Facing::North => [0, 0, -1],
        Facing::South => [0, 0, 1],
        Facing::West => [-1, 0, 0],
        Facing::East => [1, 0, 0],
    }
}

const fn opposite(facing: Facing) -> Facing {
    match facing {
        Facing::Down => Facing::Up,
        Facing::Up => Facing::Down,
        Facing::North => Facing::South,
        Facing::South => Facing::North,
        Facing::West => Facing::East,
        Facing::East => Facing::West,
    }
}

/// `JigsawBlock.canAttach`.
fn can_attach(source: &JigsawBlock, target: &JigsawBlock) -> bool {
    source.front == opposite(target.front)
        && (source.joint == JointType::Rollable || source.top == target.top)
        && source.target == target.name
}

fn inside(b: &BoundingBox, position: [i32; 3]) -> bool {
    (0..3).all(|axis| position[axis] >= b.min[axis] && position[axis] <= b.max[axis])
}

fn heightmap(name: &str) -> Option<HeightmapType> {
    Some(match name {
        "WORLD_SURFACE_WG" => HeightmapType::WorldSurfaceWg,
        "WORLD_SURFACE" => HeightmapType::WorldSurface,
        "OCEAN_FLOOR_WG" => HeightmapType::OceanFloorWg,
        "OCEAN_FLOOR" => HeightmapType::OceanFloor,
        "MOTION_BLOCKING" => HeightmapType::MotionBlocking,
        "MOTION_BLOCKING_NO_LEAVES" => HeightmapType::MotionBlockingNoLeaves,
        _ => return None,
    })
}

/// Whether Harpas can lay out a jigsaw structure: one whose start height and heightmap it knows.
pub(super) fn is_supported(jigsaw: &JigsawDef) -> bool {
    jigsaw.start_height.is_some()
        && jigsaw
            .project_start_to_heightmap
            .as_deref()
            .is_none_or(|name| heightmap(name).is_some())
}

/// The first piece of a jigsaw structure, placed but without children yet.
pub(super) struct Start<'d> {
    context: Context<'d>,
    jigsaw: &'d JigsawDef,
    center: Piece<'d>,
    /// Where the biome is checked (`GenerationStub.position`).
    pub(super) position: [i32; 3],
}

/// `JigsawStructure.findGenerationPoint` and the first half of `JigsawPlacement.addPieces`.
#[allow(clippy::too_many_arguments)]
pub(super) fn start<'d>(
    generator: &'d NoiseBasedChunkGenerator,
    data: &'d WorldgenData,
    templates: &'d StructureTemplates,
    jigsaw: &'d JigsawDef,
    level_seed: i64,
    random: &mut WorldgenRandom<LegacyRandom>,
    chunk_x: i32,
    chunk_z: i32,
) -> Result<Option<Start<'d>>, WorldgenDataError> {
    let height = 384;
    let start_y = match jigsaw.start_height.expect("a supported start height") {
        StartHeightDef::Constant(anchor) => anchor.resolve(generator.min_y(), height),
        StartHeightDef::Uniform {
            min_inclusive,
            max_inclusive,
        } => {
            let min = min_inclusive.resolve(generator.min_y(), height);
            let max = max_inclusive.resolve(generator.min_y(), height);
            if min > max {
                min
            } else {
                random.next_int_bounded(max - min + 1) + min
            }
        }
    };
    let position = [chunk_x * 16, start_y, chunk_z * 16];
    let context = Context {
        generator,
        data,
        templates,
        max_sizes: HashMap::new(),
        aliases: resolve_aliases(&jigsaw.pool_aliases, level_seed, position),
    };
    let rotation = Rotation::ALL[random.next_int_bounded(4) as usize];
    let pool = data
        .template_pools
        .get(context.alias(&jigsaw.start_pool))
        .or_else(|| data.template_pools.get(&jigsaw.start_pool))
        .ok_or_else(|| WorldgenDataError::InvalidDefinition {
            context: jigsaw.start_pool.clone(),
            message: "unknown template pool".to_string(),
        })?;
    let templates_list = pool.templates();
    let element = if templates_list.is_empty() {
        &PoolElementDef::Empty
    } else {
        templates_list[random.next_int_bounded(templates_list.len() as i32) as usize]
    };
    if matches!(element, PoolElementDef::Empty) {
        return Ok(None);
    }
    let anchor = match &jigsaw.start_jigsaw_name {
        Some(name) => match context
            .shuffled_jigsaws(element, position, rotation, random)?
            .into_iter()
            .find(|jigsaw| &jigsaw.name == name)
        {
            Some(jigsaw) => jigsaw.pos,
            None => return Ok(None),
        },
        None => position,
    };
    let local_anchor = [
        anchor[0] - position[0],
        anchor[1] - position[1],
        anchor[2] - position[2],
    ];
    let adjusted = [
        position[0] - local_anchor[0],
        position[1] - local_anchor[1],
        position[2] - local_anchor[2],
    ];
    let mut bounding_box = context.bounding_box(element, adjusted, rotation)?;
    let center_x = (bounding_box.max[0] + bounding_box.min[0]) / 2;
    let center_z = (bounding_box.max[2] + bounding_box.min[2]) / 2;
    let bottom_y = match jigsaw
        .project_start_to_heightmap
        .as_deref()
        .and_then(heightmap)
    {
        Some(kind) => position[1] + generator.base_height(center_x, center_z, kind),
        None => adjusted[1],
    };
    let ground_level_delta = 1;
    let dy = bottom_y - (bounding_box.min[1] + ground_level_delta);
    bounding_box.min[1] += dy;
    bounding_box.max[1] += dy;
    let (padding_bottom, padding_top) = jigsaw.dimension_padding;
    let max_y = generator.min_y() + height - 1;
    if (padding_bottom, padding_top) != (0, 0)
        && (bounding_box.min[1] < generator.min_y() + padding_bottom
            || bounding_box.max[1] > max_y - padding_top)
    {
        return Ok(None);
    }
    let center = Piece {
        element,
        position: [adjusted[0], adjusted[1] + dy, adjusted[2]],
        rotation,
        bounding_box,
        ground_level_delta,
        junctions: Vec::new(),
    };
    Ok(Some(Start {
        context,
        jigsaw,
        center,
        position: [center_x, bottom_y + local_anchor[1], center_z],
    }))
}

impl<'d> Start<'d> {
    /// The second half of `JigsawPlacement.addPieces`: grows the structure from its first piece.
    pub(super) fn pieces(
        mut self,
        random: &mut WorldgenRandom<LegacyRandom>,
    ) -> Result<Vec<StructurePiece>, WorldgenDataError> {
        let mut pieces = vec![self.center];
        if self.jigsaw.max_depth > 0 {
            let [center_x, center_y, center_z] = self.position;
            let (horizontal, vertical) = self.jigsaw.max_distance_from_center;
            let (padding_bottom, padding_top) = self.jigsaw.dimension_padding;
            let min_y = self.context.generator.min_y();
            let max_y = min_y + 384 - 1;
            let outer = (
                [
                    center_x - horizontal,
                    (center_y - vertical).max(min_y + padding_bottom),
                    center_z - horizontal,
                ],
                [
                    center_x + horizontal + 1,
                    (center_y + vertical + 1).min(max_y + 1 - padding_top),
                    center_z + horizontal + 1,
                ],
            );
            let mut placer = Placer {
                context: &mut self.context,
                jigsaw: self.jigsaw,
                random,
                pieces: &mut pieces,
                free: vec![FreeSpace {
                    outer,
                    taken: vec![],
                }],
                placing: Placing::default(),
            };
            let center_box = placer.pieces[0].bounding_box;
            placer.free[0].taken.push(center_box);
            placer.try_placing_children(0, 0, 0)?;
            while let Some(state) = placer.placing.next() {
                placer.try_placing_children(state.piece, state.free, state.depth)?;
            }
        }
        Ok(pieces
            .into_iter()
            .map(|piece| StructurePiece {
                kind: "minecraft:jigsaw",
                bounding_box: piece.bounding_box,
                orientation: None,
                gen_depth: 0,
                junctions: piece.junctions,
                jigsaw: Some(JigsawPiece {
                    element: piece.element.clone(),
                    position: piece.position,
                    rotation: piece.rotation,
                    ground_level_delta: piece.ground_level_delta,
                }),
            })
            .collect())
    }
}

/// `JigsawPlacement.Placer`.
struct Placer<'p, 'd> {
    context: &'p mut Context<'d>,
    jigsaw: &'d JigsawDef,
    random: &'p mut WorldgenRandom<LegacyRandom>,
    pieces: &'p mut Vec<Piece<'d>>,
    free: Vec<FreeSpace>,
    placing: Placing,
}

impl<'d> Placer<'_, 'd> {
    /// `Placer.tryPlacingChildren`.
    fn try_placing_children(
        &mut self,
        source_index: usize,
        context_free: usize,
        depth: i32,
    ) -> Result<(), WorldgenDataError> {
        let source = &self.pieces[source_index];
        let source_element = source.element;
        let source_rigid = projection(source_element) == Projection::Rigid;
        let source_projection = projection(source_element);
        let source_box = source.bounding_box;
        let source_ground_level_delta = source.ground_level_delta;
        let source_box_y = source_box.min[1];
        let mut source_free = None;
        let source_jigsaws = self.context.shuffled_jigsaws(
            source_element,
            source.position,
            source.rotation,
            self.random,
        )?;
        'jigsaws: for source_jigsaw in &source_jigsaws {
            let direction = step(source_jigsaw.front);
            let target_jigsaw_pos = [
                source_jigsaw.pos[0] + direction[0],
                source_jigsaw.pos[1] + direction[1],
                source_jigsaw.pos[2] + direction[2],
            ];
            let source_jigsaw_local_y = source_jigsaw.pos[1] - source_box_y;
            let mut source_jigsaw_base_height = None;
            let Some(target_pool) = self
                .context
                .usable_pool(self.context.alias(&source_jigsaw.pool))
            else {
                continue;
            };
            let Some(fallback) = self.context.usable_pool(&target_pool.fallback) else {
                continue;
            };
            let children_free = if inside(&source_box, target_jigsaw_pos) {
                *source_free.get_or_insert_with(|| {
                    self.free.push(FreeSpace {
                        outer: (source_box.min, source_box.max.map(|value| value + 1)),
                        taken: vec![],
                    });
                    self.free.len() - 1
                })
            } else {
                context_free
            };
            let mut target_elements = Vec::new();
            if depth != self.jigsaw.max_depth {
                target_elements.extend(target_pool.shuffled_templates(self.random));
            }
            target_elements.extend(fallback.shuffled_templates(self.random));
            let placement_priority = source_jigsaw.placement_priority;
            for target_element in target_elements {
                if matches!(target_element, PoolElementDef::Empty) {
                    break;
                }
                let mut rotations = Rotation::ALL;
                shuffle(&mut rotations, self.random);
                for target_rotation in rotations {
                    let target_jigsaws = self.context.shuffled_jigsaws(
                        target_element,
                        [0, 0, 0],
                        target_rotation,
                        self.random,
                    )?;
                    let hack_box =
                        self.context
                            .bounding_box(target_element, [0, 0, 0], target_rotation)?;
                    let hack_y_span = hack_box.max[1] - hack_box.min[1] + 1;
                    let mut expand_to = 0;
                    if self.jigsaw.use_expansion_hack && hack_y_span <= 16 {
                        for target_jigsaw in &target_jigsaws {
                            let s = step(target_jigsaw.front);
                            let front = [
                                target_jigsaw.pos[0] + s[0],
                                target_jigsaw.pos[1] + s[1],
                                target_jigsaw.pos[2] + s[2],
                            ];
                            if !inside(&hack_box, front) {
                                continue;
                            }
                            let pool = self.context.alias(&target_jigsaw.pool);
                            let Some((name, child)) =
                                self.context.data.template_pools.get_key_value(pool)
                            else {
                                continue;
                            };
                            let child_size = self.context.max_size(name)?;
                            let fallback_size = match self
                                .context
                                .data
                                .template_pools
                                .get_key_value(&child.fallback)
                            {
                                Some((name, _)) => self.context.max_size(name)?,
                                None => 0,
                            };
                            expand_to = expand_to.max(child_size.max(fallback_size));
                        }
                    }
                    for target_jigsaw in &target_jigsaws {
                        if !can_attach(source_jigsaw, target_jigsaw) {
                            continue;
                        }
                        let local = target_jigsaw.pos;
                        let raw_position = [
                            target_jigsaw_pos[0] - local[0],
                            target_jigsaw_pos[1] - local[1],
                            target_jigsaw_pos[2] - local[2],
                        ];
                        let raw_box = self.context.bounding_box(
                            target_element,
                            raw_position,
                            target_rotation,
                        )?;
                        let target_projection = projection(target_element);
                        let target_rigid = target_projection == Projection::Rigid;
                        let target_jigsaw_local_y = local[1];
                        let delta_y = source_jigsaw_local_y - target_jigsaw_local_y + direction[1];
                        let mut base_height = || {
                            *source_jigsaw_base_height.get_or_insert_with(|| {
                                self.context.generator.base_height(
                                    source_jigsaw.pos[0],
                                    source_jigsaw.pos[2],
                                    HeightmapType::WorldSurfaceWg,
                                )
                            })
                        };
                        let target_box_y = if source_rigid && target_rigid {
                            source_box_y + delta_y
                        } else {
                            base_height() - target_jigsaw_local_y
                        };
                        let y_offset = target_box_y - raw_box.min[1];
                        let mut target_box = raw_box;
                        target_box.min[1] += y_offset;
                        target_box.max[1] += y_offset;
                        let target_position =
                            [raw_position[0], raw_position[1] + y_offset, raw_position[2]];
                        if expand_to > 0 {
                            let new_size =
                                (expand_to + 1).max(target_box.max[1] - target_box.min[1]);
                            target_box.max[1] = target_box.max[1].max(target_box.min[1] + new_size);
                        }
                        if !self.free[children_free].fits(&target_box) {
                            continue;
                        }
                        self.free[children_free].taken.push(target_box);
                        let target_ground_level_delta = if target_rigid {
                            source_ground_level_delta - delta_y
                        } else {
                            1
                        };
                        let junction_y = if source_rigid {
                            source_box_y + source_jigsaw_local_y
                        } else if target_rigid {
                            target_box_y + target_jigsaw_local_y
                        } else {
                            base_height() + delta_y / 2
                        };
                        self.pieces[source_index].junctions.push(JigsawJunction {
                            source: [
                                target_jigsaw_pos[0],
                                junction_y - source_jigsaw_local_y + source_ground_level_delta,
                                target_jigsaw_pos[2],
                            ],
                            delta_y,
                            destination_rigid: target_rigid,
                        });
                        self.pieces.push(Piece {
                            element: target_element,
                            position: target_position,
                            rotation: target_rotation,
                            bounding_box: target_box,
                            ground_level_delta: target_ground_level_delta,
                            junctions: vec![JigsawJunction {
                                source: [
                                    source_jigsaw.pos[0],
                                    junction_y - target_jigsaw_local_y + target_ground_level_delta,
                                    source_jigsaw.pos[2],
                                ],
                                delta_y: -delta_y,
                                destination_rigid: source_projection == Projection::Rigid,
                            }],
                        });
                        if depth < self.jigsaw.max_depth {
                            self.placing.add(
                                PieceState {
                                    piece: self.pieces.len() - 1,
                                    free: children_free,
                                    depth: depth + 1,
                                },
                                placement_priority,
                            );
                        }
                        continue 'jigsaws;
                    }
                }
            }
        }
        Ok(())
    }
}

fn projection(element: &PoolElementDef) -> Projection {
    match element {
        PoolElementDef::Single { projection, .. }
        | PoolElementDef::Feature { projection, .. }
        | PoolElementDef::List { projection, .. } => *projection,
        PoolElementDef::Empty => Projection::Rigid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(piece: usize) -> PieceState {
        PieceState {
            piece,
            free: 0,
            depth: 0,
        }
    }

    #[test]
    fn higher_placement_priorities_go_first_and_ties_keep_their_order() {
        let mut placing = Placing::default();
        placing.add(state(1), 0);
        placing.add(state(2), 5);
        placing.add(state(3), 0);
        placing.add(state(4), 5);
        let order: Vec<_> = std::iter::from_fn(|| placing.next())
            .map(|s| s.piece)
            .collect();
        assert_eq!(order, [2, 4, 1, 3]);
    }

    #[test]
    fn a_piece_fits_inside_the_box_and_beside_taken_pieces() {
        let free = FreeSpace {
            outer: ([0, 0, 0], [10, 10, 10]),
            taken: vec![BoundingBox {
                min: [0, 0, 0],
                max: [4, 4, 4],
            }],
        };
        let at = |min, max| BoundingBox { min, max };
        assert!(free.fits(&at([5, 0, 0], [9, 4, 4])));
        assert!(!free.fits(&at([4, 0, 0], [9, 4, 4])));
        assert!(!free.fits(&at([5, 0, 0], [10, 4, 4])));
        assert!(!free.fits(&at([-1, 5, 5], [3, 9, 9])));
    }

    #[test]
    fn weighted_choices_follow_the_cumulative_weights() {
        let entries = [("a", 1), ("b", 3)];
        let mut random = LegacyRandom::new(42);
        let mut expected = LegacyRandom::new(42);
        for _ in 0..20 {
            let selection = expected.next_int_bounded(4);
            let chosen = *weighted(&entries, &mut random);
            assert_eq!(chosen, if selection < 1 { "a" } else { "b" });
        }
    }

    #[test]
    fn aliases_resolve_groups_and_keep_unknown_pools() {
        let bindings = vec![
            PoolAliasDef::Direct {
                alias: "x".to_string(),
                target: "y".to_string(),
            },
            PoolAliasDef::RandomGroup {
                groups: vec![(
                    vec![PoolAliasDef::Direct {
                        alias: "p".to_string(),
                        target: "q".to_string(),
                    }],
                    1,
                )],
            },
        ];
        let aliases = resolve_aliases(&bindings, 0, [0, 0, 0]);
        assert_eq!(aliases.get("x"), Some(&"y"));
        assert_eq!(aliases.get("p"), Some(&"q"));
        assert!(resolve_aliases(&[], 0, [0, 0, 0]).is_empty());
    }

    #[test]
    fn jigsaws_attach_face_to_face_with_matching_names() {
        let jigsaw = |front, top, name: &str, target: &str, joint| JigsawBlock {
            pos: [0, 0, 0],
            front,
            top,
            name: name.to_string(),
            pool: EMPTY_POOL.to_string(),
            target: target.to_string(),
            joint,
            placement_priority: 0,
            selection_priority: 0,
        };
        let source = jigsaw(Facing::North, Facing::Up, "a", "b", JointType::Aligned);
        assert!(can_attach(
            &source,
            &jigsaw(Facing::South, Facing::Up, "b", "", JointType::Aligned)
        ));
        assert!(!can_attach(
            &source,
            &jigsaw(Facing::North, Facing::Up, "b", "", JointType::Aligned)
        ));
        assert!(!can_attach(
            &source,
            &jigsaw(Facing::South, Facing::Up, "c", "", JointType::Aligned)
        ));
        let down = jigsaw(Facing::Down, Facing::North, "a", "b", JointType::Aligned);
        let up = jigsaw(Facing::Up, Facing::East, "b", "", JointType::Aligned);
        assert!(!can_attach(&down, &up));
        let rollable = jigsaw(Facing::Down, Facing::North, "a", "b", JointType::Rollable);
        assert!(can_attach(&rollable, &up));
    }
}
