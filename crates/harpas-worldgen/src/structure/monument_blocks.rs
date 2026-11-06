//! Monument exterior reconstructed from vanilla placement observed on supplied terrain.

use harpas_core::block::block_states;
use harpas_core::random::{WorldgenRandom, XoroshiroRandom};

use super::{BoundingBox, Direction, MonumentLayout, StructureStart};
use crate::GeneratedChunk;

/// `minecraft:monument`'s position among `SURFACE_STRUCTURES` in 26.1, observed from the reference
/// server. Vanilla seeds the decoration stream with this index and the step's ordinal.
const MONUMENT_FEATURE_INDEX: i32 = 6;
/// The ordinal of `GenerationStep.Decoration.SURFACE_STRUCTURES` in 26.1.
const SURFACE_STRUCTURES_STEP: i32 = 4;

impl StructureStart {
    /// Places the monument's exterior, entrance, roof and foundations in one chunk.
    ///
    /// Uses the saved building's origin and orientation. The five-block clearing outside the saved
    /// bounds is clipped to the target chunk; vanilla invokes this pass only for chunks intersecting
    /// those bounds, so callers that mimic it do the same. Call `MonumentLayout::place_rooms_in_chunk`
    /// afterwards. No random draws, entities or saved-record changes occur in this pass.
    pub fn place_monument_exterior_in_chunk(&self, chunk: &mut GeneratedChunk, sea_level: i32) {
        if self.structure != "minecraft:monument" {
            return;
        }
        let Some(piece) = self
            .pieces
            .first()
            .filter(|p| p.kind == "minecraft:omb" && p.orientation.is_some())
        else {
            return;
        };
        let mut exterior = MonumentBlocks::new(
            piece.bounding_box,
            piece.orientation.unwrap(),
            chunk,
            sea_level,
        );
        exterior.build();
    }

    /// Places the monument's exterior and internal rooms in one chunk with vanilla's decoration
    /// stream.
    ///
    /// Call this for every chunk that intersects the saved building's bounds, as vanilla does; the
    /// five-block exterior clearing is clipped to the target chunk and does not reach chunks outside
    /// those bounds. The stream is seeded like vanilla's `applyBiomeDecoration` for
    /// `minecraft:monument` (index 6, step 4). The saved record is not changed and no elder
    /// guardians are spawned; they follow in M4.
    pub fn place_monument_in_chunk(&self, chunk: &mut GeneratedChunk, seed: i64, sea_level: i32) {
        let Some(piece) = self.pieces.first() else {
            return;
        };
        if self.structure != "minecraft:monument"
            || piece.kind != "minecraft:omb"
            || piece.orientation.is_none()
        {
            return;
        }
        self.place_monument_exterior_in_chunk(chunk, sea_level);
        let Some(layout) = MonumentLayout::from_start(seed, self) else {
            return;
        };
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration =
            random.set_decoration_seed(seed, chunk.chunk_x() * 16, chunk.chunk_z() * 16);
        random.set_feature_seed(decoration, MONUMENT_FEATURE_INDEX, SURFACE_STRUCTURES_STEP);
        layout.place_rooms_in_chunk(chunk, sea_level, &mut random);
    }
}

pub(super) struct MonumentBlocks<'a> {
    bounds: BoundingBox,
    direction: Direction,
    pub(super) chunk: &'a mut GeneratedChunk,
    sea_level: i32,
    pub(super) rough: u32,
    pub(super) bricks: u32,
    pub(super) dark: u32,
    pub(super) lamp: u32,
    pub(super) water: u32,
}

impl MonumentBlocks<'_> {
    pub(super) fn new(
        bounds: BoundingBox,
        direction: Direction,
        chunk: &mut GeneratedChunk,
        sea_level: i32,
    ) -> MonumentBlocks<'_> {
        let states = block_states();
        MonumentBlocks {
            bounds,
            direction,
            chunk,
            sea_level,
            rough: states.default_state("minecraft:prismarine").unwrap(),
            bricks: states.default_state("minecraft:prismarine_bricks").unwrap(),
            dark: states.default_state("minecraft:dark_prismarine").unwrap(),
            lamp: states.default_state("minecraft:sea_lantern").unwrap(),
            water: states.default_state("minecraft:water").unwrap(),
        }
    }
    fn intersects(&self, [x0, z0, x1, z1]: [i32; 4]) -> bool {
        let bounds =
            BoundingBox::from_corners(self.position([x0, 0, z0]), self.position([x1, 0, z1]));
        let (x, z) = (self.chunk.chunk_x() * 16, self.chunk.chunk_z() * 16);
        bounds.intersects_columns(x, z, x + 15, z + 15)
    }

    pub(super) fn position(&self, [x, y, z]: [i32; 3]) -> [i32; 3] {
        let b = self.bounds;
        let (x, z) = match self.direction {
            Direction::South => (b.min[0] + x, b.min[2] + z),
            Direction::North => (b.min[0] + x, b.max[2] - z),
            Direction::East => (b.min[0] + z, b.min[2] + x),
            Direction::West => (b.max[0] - z, b.min[2] + x),
        };
        [x, b.min[1] + y, z]
    }

    pub(super) fn clipped_box(&self, min: [i32; 3], max: [i32; 3]) -> BoundingBox {
        let mut b = BoundingBox::from_corners(self.position(min), self.position(max));
        let x = self.chunk.chunk_x() * 16;
        let z = self.chunk.chunk_z() * 16;
        b.min[0] = b.min[0].max(x);
        b.min[1] = b.min[1].max(self.chunk.min_y());
        b.min[2] = b.min[2].max(z);
        b.max[0] = b.max[0].min(x + 15);
        b.max[1] = b.max[1].min(self.chunk.min_y() + self.chunk.height() - 1);
        b.max[2] = b.max[2].min(z + 15);
        b
    }

    pub(super) fn fill(&mut self, min: [i32; 3], max: [i32; 3], state: u32) {
        let b = self.clipped_box(min, max);
        for y in b.min[1]..=b.max[1] {
            for x in b.min[0]..=b.max[0] {
                for z in b.min[2]..=b.max[2] {
                    self.chunk
                        .set_block_state((x & 15) as usize, y, (z & 15) as usize, state);
                }
            }
        }
    }

    pub(super) fn block(&mut self, pos: [i32; 3], state: u32) {
        self.fill(pos, pos, state);
    }

    pub(super) fn clear(&mut self, min: [i32; 3], max: [i32; 3]) {
        if min[1] > max[1] {
            return;
        }
        let b = self.clipped_box(min, max);
        for y in b.min[1]..=b.max[1] {
            for x in b.min[0]..=b.max[0] {
                for z in b.min[2]..=b.max[2] {
                    let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
                    let current = self.chunk.block_state(lx, y, lz);
                    let name = block_states().properties(current).unwrap().0;
                    if matches!(
                        name,
                        "minecraft:water"
                            | "minecraft:ice"
                            | "minecraft:packed_ice"
                            | "minecraft:blue_ice"
                    ) {
                        continue;
                    }
                    self.chunk.set_block_state(
                        lx,
                        y,
                        lz,
                        if y >= self.sea_level { 0 } else { self.water },
                    );
                }
            }
        }
    }

    fn build(&mut self) {
        self.clear(
            [0, 0, 0],
            [58, self.sea_level.max(64) - self.bounds.min[1], 58],
        );
        self.wing(0, false);
        self.wing(33, true);
        self.entrance();
        self.facade();
        self.roof();
        self.lower_walls();
        self.middle_walls();
        self.upper_walls();
        self.foundations();
        for i in 0..5 {
            self.clear([-1 - i, 2 * i, -1 - i], [-1 - i, 23, 58 + i]);
            self.clear([58 + i, 2 * i, -1 - i], [58 + i, 23, 58 + i]);
            self.clear([-i, 2 * i, -1 - i], [57 + i, 23, -1 - i]);
            self.clear([-i, 2 * i, 58 + i], [57 + i, 23, 58 + i]);
        }
    }

    fn wing(&mut self, offset: i32, mirrored: bool) {
        if !self.intersects([offset, 0, offset + 23, 20]) {
            return;
        }
        let (p, b) = (self.rough, self.bricks);
        self.fill([offset, 0, 0], [offset + 24, 0, 20], p);
        self.clear([offset, 1, 0], [offset + 24, 10, 20]);
        for i in 0..4 {
            for x in [i, 24 - i] {
                self.fill([offset + x, i + 1, i], [offset + x, i + 1, 20], b);
            }
            for x in [7 + i, 17 - i] {
                self.fill([offset + x, i + 5, 7 + i], [offset + x, i + 5, 20], b);
            }
            self.fill([offset + i + 1, i + 1, i], [offset + 23 - i, i + 1, i], b);
            self.fill(
                [offset + 8 + i, i + 5, 7 + i],
                [offset + 16 - i, i + 5, 7 + i],
                b,
            );
        }
        for (a, c) in [
            ([4, 4, 4], [6, 4, 20]),
            ([7, 4, 4], [17, 4, 6]),
            ([18, 4, 4], [20, 4, 20]),
            ([11, 8, 11], [13, 8, 20]),
        ] {
            self.fill([offset + a[0], a[1], a[2]], [offset + c[0], c[1], c[2]], p);
        }
        for z in [12, 15, 18] {
            self.block([offset + 12, 9, z], b);
        }
        let (a, c) = if mirrored { (19, 5) } else { (5, 19) };
        for z in (5..=20).step_by(3) {
            self.block([offset + a, 5, z], b);
        }
        for z in (7..=19).step_by(3) {
            self.block([offset + c, 5, z], b);
        }
        for x in [8, 11, 14, 17] {
            self.block([offset + if mirrored { 24 - x } else { x }, 5, 5], b);
        }
        self.block([offset + c, 5, 5], b);
        self.fill([offset + 11, 1, 12], [offset + 13, 7, 12], p);
        self.fill([offset + 12, 1, 11], [offset + 12, 7, 13], p);
    }

    fn entrance(&mut self) {
        if !self.intersects([22, 5, 35, 17]) {
            return;
        }
        self.clear([25, 0, 0], [32, 8, 20]);
        for z in [5, 9, 13, 17] {
            for mirror in [false, true] {
                let x = |x| if mirror { 57 - x } else { x };
                self.fill([x(24), 2, z], [x(24), 4, z], self.bricks);
                self.fill(
                    [x(22).min(x(23)), 4, z],
                    [x(22).max(x(23)), 4, z],
                    self.bricks,
                );
                self.block([x(25), 5, z], self.bricks);
                self.block([x(26), 6, z], self.bricks);
                self.block([x(26), 5, z], self.lamp);
            }
            self.fill([27, 6, z], [30, 6, z], self.rough);
        }
    }

    fn facade(&mut self) {
        if !self.intersects([15, 20, 42, 21]) {
            return;
        }
        let (p, b, d) = (self.rough, self.bricks, self.dark);
        self.clear([26, 1, 21], [31, 3, 21]);
        for (y, left, right) in [
            (0, 15, 42),
            (12, 21, 36),
            (11, 17, 40),
            (10, 16, 41),
            (6, 16, 41),
            (5, 17, 40),
            (4, 21, 36),
            (3, 22, 26),
            (3, 31, 35),
            (2, 23, 25),
            (2, 32, 34),
        ] {
            self.fill([left, y, 21], [right, y, 21], p);
        }
        self.fill([15, 7, 21], [42, 9, 21], p);
        self.fill([28, 4, 20], [29, 4, 21], b);
        for (x, y) in [(27, 3), (30, 3), (26, 2), (31, 2), (25, 1), (32, 1)] {
            self.block([x, y, 21], b);
        }
        for (start_y, count) in [(6, 7), (9, 4), (12, 1)] {
            for i in 0..count {
                self.block([28 - i, start_y + i, 21], d);
                self.block([29 + i, start_y + i, 21], d);
            }
        }
        for x in [18, 20, 22, 35, 37, 39] {
            self.fill([x, 8, 21], [x, 9, 21], d);
        }
        self.clear([15, 13, 21], [42, 15, 21]);
        for (left, right, top) in [
            (15, 15, 6),
            (16, 16, 5),
            (17, 20, 4),
            (21, 21, 3),
            (22, 22, 2),
            (23, 24, 1),
        ] {
            self.clear([left, 1, 21], [right, top, 21]);
            self.clear([57 - right, 1, 21], [57 - left, top, 21]);
        }
    }

    fn roof(&mut self) {
        if !self.intersects([21, 21, 36, 36]) {
            return;
        }
        let (p, b) = (self.rough, self.bricks);
        self.fill([21, 0, 22], [36, 0, 36], p);
        self.clear([21, 1, 22], [36, 23, 36]);
        for i in 0..4 {
            self.fill([21 + i, 13 + i, 21 + i], [36 - i, 13 + i, 21 + i], b);
            self.fill([21 + i, 13 + i, 36 - i], [36 - i, 13 + i, 36 - i], b);
            self.fill([21 + i, 13 + i, 22 + i], [21 + i, 13 + i, 35 - i], b);
            self.fill([36 - i, 13 + i, 22 + i], [36 - i, 13 + i, 35 - i], b);
        }
        self.fill([25, 16, 25], [32, 16, 32], p);
        for x in [25, 32] {
            for z in [25, 32] {
                self.fill([x, 17, z], [x, 19, z], b);
            }
        }
        for (x, z, cx, cz) in [
            (26, 26, 27, 27),
            (26, 31, 27, 30),
            (31, 31, 30, 30),
            (31, 26, 30, 27),
        ] {
            self.block([x, 20, z], b);
            self.block([cx, 21, cz], b);
            self.block([cx, 20, cz], self.lamp);
        }
        self.fill([28, 21, 27], [29, 21, 27], p);
        self.fill([27, 21, 28], [27, 21, 29], p);
        self.fill([28, 21, 30], [29, 21, 30], p);
        self.fill([30, 21, 28], [30, 21, 29], p);
    }

    fn lower_walls(&mut self) {
        let (p, b) = (self.rough, self.bricks);
        for mirror in [false, true] {
            if !self.intersects(if mirror {
                [51, 21, 58, 58]
            } else {
                [0, 21, 6, 58]
            }) {
                continue;
            }
            let x = |x| if mirror { 57 - x } else { x };
            self.fill([x(0).min(x(6)), 0, 21], [x(0).max(x(6)), 0, 57], p);
            self.clear([x(0).min(x(6)), 1, 21], [x(0).max(x(6)), 7, 57]);
            self.fill([x(4).min(x(6)), 4, 21], [x(4).max(x(6)), 4, 53], p);
            for i in 0..4 {
                self.fill([x(i), i + 1, 21], [x(i), i + 1, 57 - i], b);
            }
            for z in (23..=50).step_by(3) {
                self.block([x(5), 5, z], b);
            }
            self.block([x(5), 5, 52], b);
            self.fill([x(4).min(x(6)), 1, 52], [x(4).max(x(6)), 3, 52], p);
            self.fill([x(5), 1, 51], [x(5), 3, 53], p);
        }
        if !self.intersects([0, 51, 57, 57]) {
            return;
        }
        self.fill([7, 0, 51], [50, 0, 57], p);
        self.clear([7, 1, 51], [50, 10, 57]);
        for i in 0..4 {
            self.fill([1 + i, 1 + i, 57 - i], [56 - i, 1 + i, 57 - i], b);
        }
    }

    fn middle_walls(&mut self) {
        let (p, b) = (self.rough, self.bricks);
        for mirror in [false, true] {
            if !self.intersects(if mirror {
                [44, 21, 50, 54]
            } else {
                [7, 21, 13, 50]
            }) {
                continue;
            }
            let x = |x| if mirror { 57 - x } else { x };
            self.fill([x(7).min(x(13)), 0, 21], [x(7).max(x(13)), 0, 50], p);
            self.clear([x(7).min(x(13)), 1, 21], [x(7).max(x(13)), 10, 50]);
            self.fill([x(11).min(x(13)), 8, 21], [x(11).max(x(13)), 8, 53], p);
            for i in 0..4 {
                self.fill([x(7 + i), 5 + i, 21], [x(7 + i), 5 + i, 54], b);
            }
            for z in (21..=45).step_by(3) {
                self.block([x(12), 9, z], b);
            }
        }
        if !self.intersects([8, 44, 49, 54]) {
            return;
        }
        self.fill([14, 0, 44], [43, 0, 50], p);
        self.clear([14, 1, 44], [43, 10, 50]);
        for x in (12..=45).step_by(3) {
            for z in [45, 52] {
                self.block([x, 9, z], b);
            }
            if [12, 18, 24, 33, 39, 45].contains(&x) {
                for (y, z) in [
                    (9, 47),
                    (9, 50),
                    (10, 45),
                    (10, 46),
                    (10, 51),
                    (10, 52),
                    (11, 47),
                    (11, 50),
                    (12, 48),
                    (12, 49),
                ] {
                    self.block([x, y, z], b);
                }
            }
        }
        for i in 0..3 {
            self.fill([8 + i, 5 + i, 54], [49 - i, 5 + i, 54], p);
        }
        self.fill([11, 8, 54], [46, 8, 54], b);
        self.fill([14, 8, 44], [43, 8, 53], p);
    }

    fn upper_walls(&mut self) {
        let (p, b) = (self.rough, self.bricks);
        for mirror in [false, true] {
            if !self.intersects(if mirror {
                [37, 21, 43, 43]
            } else {
                [14, 21, 20, 43]
            }) {
                continue;
            }
            let x = |x| if mirror { 57 - x } else { x };
            self.fill([x(14).min(x(20)), 0, 21], [x(14).max(x(20)), 0, 43], p);
            self.clear([x(14).min(x(20)), 1, 22], [x(14).max(x(20)), 14, 43]);
            self.fill([x(18).min(x(20)), 12, 22], [x(18).max(x(20)), 12, 39], p);
            self.fill([x(18).min(x(20)), 12, 21], [x(18).max(x(20)), 12, 21], b);
            for i in 0..4 {
                self.fill([x(14 + i), 9 + i, 21], [x(14 + i), 9 + i, 43 - i], b);
            }
            for z in (23..=38).step_by(3) {
                self.block([x(19), 13, z], b);
            }
        }
        if !self.intersects([15, 37, 42, 43]) {
            return;
        }
        self.fill([21, 0, 37], [36, 0, 43], p);
        self.clear([21, 1, 37], [36, 14, 43]);
        self.fill([21, 12, 37], [36, 12, 39], p);
        for i in 0..4 {
            self.fill([15 + i, 9 + i, 43 - i], [42 - i, 9 + i, 43 - i], b);
        }
        for x in (21..=36).step_by(3) {
            self.block([x, 13, 38], b);
        }
    }

    fn foundations(&mut self) {
        for gx in 0..7 {
            for gz in 0..7 {
                if (gx != 0 && gx != 6 && gz != 0 && gz != 6) || (gx == 3 && gz == 0) {
                    continue;
                }
                for dx in 0..4 {
                    for dz in 0..4 {
                        let [x, y, z] = self.position([gx * 9 + dx, 0, gz * 9 + dz]);
                        if (x >> 4, z >> 4) != (self.chunk.chunk_x(), self.chunk.chunk_z()) {
                            continue;
                        }
                        let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
                        self.chunk.set_block_state(lx, y, lz, self.bricks);
                        // The top block and downward column are clipped independently. A building
                        // immediately above the world can still have a foundation starting inside it.
                        if y - 1 < self.chunk.min_y()
                            || y > self.chunk.min_y() + self.chunk.height()
                        {
                            continue;
                        }
                        for y in (self.chunk.min_y() + 2..y).rev() {
                            let current = self.chunk.block_state(lx, y, lz);
                            let name = block_states().properties(current).unwrap().0;
                            if !block_states().is_air(current)
                                && !matches!(
                                    name,
                                    "minecraft:water"
                                        | "minecraft:lava"
                                        | "minecraft:seagrass"
                                        | "minecraft:tall_seagrass"
                                        | "minecraft:bubble_column"
                                        | "minecraft:glow_lichen"
                                )
                            {
                                break;
                            }
                            self.chunk.set_block_state(lx, y, lz, self.bricks);
                        }
                    }
                }
            }
        }
    }
}
