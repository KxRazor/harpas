//! Interior geometry reconstructed from observed vanilla placement on supplied terrain.

use harpas_core::{block::block_states, random::RandomSource};

use super::monument_blocks::MonumentBlocks;
use super::{BoundingBox, MonumentLayout, MonumentRoom, MonumentRoomKind, MonumentRoomPiece};
use crate::GeneratedChunk;

const DOWN: usize = 0;
const UP: usize = 1;
const NORTH: usize = 2;
const SOUTH: usize = 3;
const WEST: usize = 4;
const EAST: usize = 5;

impl MonumentLayout {
    /// Places the internal rooms, wings and penthouse in construction order, after the exterior.
    ///
    /// The caller supplies the chunk's decoration stream. Only intersecting child bounds consume it;
    /// sponge columns still draw outside the target chunk once their room intersects. This pass does
    /// not spawn elders, schedule fluid ticks or change the single saved building record. Worker
    /// integration and the complete FEATURES schedule are separate work.
    pub fn place_rooms_in_chunk(
        &self,
        chunk: &mut GeneratedChunk,
        sea_level: i32,
        random: &mut impl RandomSource,
    ) {
        let clip = BoundingBox {
            min: [chunk.chunk_x() * 16, chunk.min_y(), chunk.chunk_z() * 16],
            max: [
                chunk.chunk_x() * 16 + 15,
                chunk.min_y() + chunk.height() - 1,
                chunk.chunk_z() * 16 + 15,
            ],
        };
        for piece in &self.pieces {
            if !piece.bounding_box.intersects(&clip) {
                continue;
            }
            let mut blocks =
                MonumentBlocks::new(piece.bounding_box, self.orientation, chunk, sea_level);
            blocks.room(self, piece, random);
        }
    }
}

impl MonumentBlocks<'_> {
    fn room(
        &mut self,
        layout: &MonumentLayout,
        piece: &MonumentRoomPiece,
        random: &mut impl RandomSource,
    ) {
        use MonumentRoomKind::*;
        let (p, b, d, l) = (self.rough, self.bricks, self.dark, self.lamp);
        match piece.kind {
            Wing => {
                self.inner_wing(piece.design.unwrap_or(0));
                return;
            }
            Penthouse => {
                self.penthouse();
                return;
            }
            Core => {
                self.core();
                return;
            }
            Entry => {
                self.entry(&layout.rooms[&piece.room.unwrap()]);
                return;
            }
            _ => {}
        }
        let anchor = piece.room.unwrap();
        let room = &layout.rooms[&anchor];
        let (width, height, depth) = match piece.kind {
            DoubleXY => (2, 2, 1),
            DoubleYZ => (1, 2, 2),
            DoubleX => (2, 1, 1),
            DoubleZ => (1, 1, 2),
            DoubleY => (1, 2, 1),
            _ => (1, 1, 1),
        };
        // Every occupied bottom cell has its own floor opening; a missing cell above caps the roof.
        for x in 0..width {
            for z in 0..depth {
                let cell = anchor + x as usize + 5 * z as usize;
                if anchor / 25 > 0 {
                    self.floor(x * 8, z * 8, layout.rooms[&cell].openings[DOWN]);
                }
                let top = cell + 25 * (height as usize - 1);
                if layout.rooms[&top].connections[UP].is_none() {
                    let min = [
                        if x == 0 { 1 } else { 8 },
                        height * 4,
                        if z == 0 { 1 } else { 8 },
                    ];
                    let max = [
                        if width == 2 && x == 0 { 7 } else { x * 8 + 6 },
                        height * 4,
                        if depth == 2 && z == 0 { 7 } else { z * 8 + 6 },
                    ];
                    self.cap(min, max);
                }
            }
        }
        match piece.kind {
            SimpleTop => {
                let sponge = block_states()
                    .default_state("minecraft:wet_sponge")
                    .unwrap();
                for x in 1..=6 {
                    for z in 1..=6 {
                        if random.next_int_bounded(3) != 0 {
                            let bottom = if random.next_int_bounded(4) == 0 {
                                2
                            } else {
                                3
                            };
                            self.fill([x, bottom, z], [x, 3, z], sponge);
                        }
                    }
                }
                self.dark_room();
            }
            Simple => self.simple(room, piece.design.unwrap_or(0), random),
            DoubleX => {
                self.bands(15, 7, 3);
                self.fill([5, 1, 0], [10, 1, 4], b);
                self.fill([6, 2, 0], [9, 2, 3], p);
                self.fill([5, 3, 0], [10, 3, 4], b);
                for x in [6, 9] {
                    self.block([x, 2, 3], l);
                }
                self.openings(layout, anchor, [2, 1, 1]);
            }
            DoubleZ => {
                self.bands(7, 15, 3);
                for x in [1, 6] {
                    for y in [1, 3] {
                        for z in [1, 13] {
                            self.fill([x, y, z], [x, y, z + 1], b);
                        }
                    }
                }
                for x in [2, 5] {
                    for z in [6, 9] {
                        self.fill([x, 1, z], [x, 3, z], b);
                    }
                }
                for z in [6, 9] {
                    self.fill([3, 2, z], [4, 2, z], b);
                }
                for x in [2, 5] {
                    self.fill([x, 2, 7], [x, 2, 8], b);
                    for z in [5, 10] {
                        self.block([x, 2, z], l);
                        self.block([x, 3, z], b);
                    }
                }
                self.openings(layout, anchor, [1, 1, 2]);
            }
            DoubleXY => {
                self.bands(15, 7, 7);
                for x in [2, 13] {
                    self.fill([x, 1, 3], [x, 7, 4], b);
                }
                for x in [3, 11] {
                    for z in [2, 5] {
                        self.fill([x, 1, z], [x + 1, 7, z], b);
                    }
                }
                for x in [5, 10] {
                    self.fill([x, 1, 3], [x, 3, 4], b);
                }
                self.fill([5, 7, 2], [10, 7, 5], b);
                for x in [5, 10] {
                    for z in [2, 5] {
                        self.fill([x, 5, z], [x, 7, z], b);
                        self.block([x, 4, z], l);
                    }
                }
                for x in [6, 9] {
                    for z in [2, 5] {
                        self.block([x, 6, z], b);
                    }
                }
                self.fill([5, 4, 3], [6, 4, 4], b);
                self.fill([9, 4, 3], [10, 4, 4], b);
                self.openings(layout, anchor, [2, 2, 1]);
            }
            DoubleYZ => {
                self.bands(7, 15, 7);
                for y in 1..=7 {
                    self.fill([3, y, 7], [4, y, 8], if y == 2 || y == 6 { l } else { d });
                }
                self.openings(layout, anchor, [1, 2, 2]);
                for z in [0, 8] {
                    let upper = &layout.rooms[&(anchor + 25 + (z / 8 * 5) as usize)];
                    for (side, a, c, x) in [(WEST, 1, 2, 1), (EAST, 5, 6, 6)] {
                        if upper.openings[side] {
                            self.fill([a, 4, z + 2], [c, 4, z + 5], b);
                            for dz in [2, 5] {
                                self.fill([x, 1, z + dz], [x, 3, z + dz], b);
                            }
                        }
                    }
                }
            }
            DoubleY => {
                self.ring(7, 7, 4, b);
                for x in [2, 5] {
                    for z in [1, 5] {
                        self.fill([x, 4, z], [x, 4, z + 1], b);
                    }
                }
                for x in [1, 6] {
                    for z in [2, 5] {
                        self.block([x, 4, z], b);
                    }
                }
                for y in [0, 4] {
                    let cell = &layout.rooms[&(anchor + (y / 4 * 25) as usize)];
                    for side in [SOUTH, NORTH, WEST, EAST] {
                        self.tall_wall(cell.openings[side], side, y);
                    }
                }
            }
            _ => unreachable!(),
        }
    }

    fn cap(&mut self, min: [i32; 3], max: [i32; 3]) {
        let bounds = self.clipped_box(min, max);
        for y in bounds.min[1]..=bounds.max[1] {
            for x in bounds.min[0]..=bounds.max[0] {
                for z in bounds.min[2]..=bounds.max[2] {
                    let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
                    if self.chunk.block_state(lx, y, lz) == self.water {
                        self.chunk.set_block_state(lx, y, lz, self.rough);
                    }
                }
            }
        }
    }

    fn floor(&mut self, x: i32, z: i32, open: bool) {
        let (p, b) = (self.rough, self.bricks);
        if !open {
            self.fill([x, 0, z], [x + 7, 0, z + 7], p);
            return;
        }
        for (a, c) in [
            ([0, 0], [2, 7]),
            ([5, 0], [7, 7]),
            ([3, 0], [4, 2]),
            ([3, 5], [4, 7]),
        ] {
            self.fill([x + a[0], 0, z + a[1]], [x + c[0], 0, z + c[1]], p);
        }
        for (a, c) in [
            ([3, 2], [4, 2]),
            ([3, 5], [4, 5]),
            ([2, 3], [2, 4]),
            ([5, 3], [5, 4]),
        ] {
            self.fill([x + a[0], 0, z + a[1]], [x + c[0], 0, z + c[1]], b);
        }
    }

    fn ring(&mut self, x: i32, z: i32, y: i32, state: u32) {
        self.fill([0, y, 0], [0, y, z], state);
        self.fill([x, y, 0], [x, y, z], state);
        self.fill([1, y, 0], [x - 1, y, 0], state);
        self.fill([1, y, z], [x - 1, y, z], state);
    }
    fn bands(&mut self, x: i32, z: i32, height: i32) {
        for y in 1..=height {
            self.ring(
                x,
                z,
                y,
                if y == 2 || y == 6 {
                    self.rough
                } else {
                    self.bricks
                },
            );
        }
    }
    fn dark_room(&mut self) {
        for y in 1..=3 {
            self.ring(7, 7, y, if y == 2 { self.dark } else { self.bricks });
        }
        for side in [SOUTH, NORTH, WEST, EAST] {
            let (a, c) = door(side, 0, 0, 0);
            self.fill(a, c, self.dark);
        }
    }

    fn openings(&mut self, layout: &MonumentLayout, anchor: usize, [w, h, d]: [i32; 3]) {
        for y in 0..h {
            for z in 0..d {
                for x in 0..w {
                    let cell = &layout.rooms[&(anchor + (x + 5 * z + 25 * y) as usize)];
                    for side in [SOUTH, NORTH, WEST, EAST] {
                        if (side == SOUTH && z != 0)
                            || (side == NORTH && z != d - 1)
                            || (side == WEST && x != 0)
                            || (side == EAST && x != w - 1)
                        {
                            continue;
                        }
                        if cell.openings[side] {
                            let (a, c) = door(side, x * 8, y * 4, z * 8);
                            self.clear(a, c);
                        }
                    }
                }
            }
        }
    }

    fn entry(&mut self, room: &MonumentRoom) {
        let b = self.bricks;
        for y in 1..=3 {
            self.fill([0, y, 0], [y - 1, y, 7], b);
            self.fill([8 - y, y, 0], [7, y, 7], b);
        }
        self.fill([0, 1, 7], [7, 3, 7], b);
        self.fill([1, 1, 0], [2, 3, 0], b);
        self.fill([5, 1, 0], [6, 3, 0], b);
        for side in [NORTH, WEST, EAST] {
            if room.openings[side] {
                let (mut a, mut c) = door(side, 0, 0, 0);
                if side == WEST {
                    c[0] = 1;
                } else if side == EAST {
                    a[0] = 6;
                }
                self.clear(a, c);
            }
        }
    }

    fn simple(&mut self, room: &MonumentRoom, design: i32, random: &mut impl RandomSource) {
        // The boolean draw precedes the opening checks, including when a vertical passage prevents a pillar.
        let pillar = design != 0
            && random.next_bool()
            && !room.openings[DOWN]
            && !room.openings[UP]
            && room.openings.iter().filter(|&&v| v).count() > 1;
        let (p, b, l) = (self.rough, self.bricks, self.lamp);
        match design {
            0 => {
                for x in [0, 5] {
                    for z in [0, 5] {
                        for y in [1, 3] {
                            self.fill([x, y, z], [x + 2, y, z + 2], b);
                        }
                        let edge_x = if x == 0 { 0 } else { 7 };
                        let edge_z = if z == 0 { 0 } else { 7 };
                        self.fill([edge_x, 2, z], [edge_x, 2, z + 2], p);
                        self.fill([x, 2, edge_z], [x + 2, 2, edge_z], p);
                        self.block([x + 1, 2, z + 1], l);
                    }
                }
                for side in [SOUTH, NORTH, WEST, EAST] {
                    let (mut a, mut c) = door(side, 0, 0, 0);
                    if room.openings[side] {
                        self.fill([a[0], 3, a[2]], [c[0], 3, c[2]], b);
                    } else {
                        // Closed walls have a two-block-deep sill and lintel, but a one-block band.
                        self.fill([a[0], 2, a[2]], c, p);
                        match side {
                            SOUTH => c[2] = 1,
                            NORTH => a[2] = 6,
                            WEST => c[0] = 1,
                            EAST => a[0] = 6,
                            _ => unreachable!(),
                        }
                        for y in [1, 3] {
                            self.fill([a[0], y, a[2]], [c[0], y, c[2]], b);
                        }
                    }
                }
            }
            1 => {
                for x in [2, 5] {
                    for z in [2, 5] {
                        self.fill([x, 1, z], [x, 3, z], b);
                        self.block([x, 2, z], l);
                    }
                }
                for x in [0, 7] {
                    for z in [0, 7] {
                        let ix = if x == 0 { 1 } else { 6 };
                        let iz = if z == 0 { 1 } else { 6 };
                        self.fill([x.min(ix), 1, z], [x.max(ix), 3, z], b);
                        self.fill([x, 1, iz], [x, 3, iz], b);
                        self.block([ix, 2, z], p);
                        self.block([x, 2, iz], p);
                    }
                }
                for side in [SOUTH, NORTH, WEST, EAST] {
                    if !room.openings[side] {
                        let (mut a, mut c) = door(side, 0, 0, 0);
                        if side == SOUTH || side == NORTH {
                            a[0] = 1;
                            c[0] = 6;
                        } else {
                            a[2] = 1;
                            c[2] = 6;
                        }
                        for y in 1..=3 {
                            self.fill([a[0], y, a[2]], [c[0], y, c[2]], if y == 2 { p } else { b });
                        }
                    }
                }
            }
            _ => {
                self.dark_room();
                for side in [SOUTH, NORTH, WEST, EAST] {
                    if room.openings[side] {
                        let (a, c) = door(side, 0, 0, 0);
                        self.clear(a, c);
                    }
                }
            }
        }
        if pillar {
            for y in 1..=3 {
                self.fill([3, y, 3], [4, y, 4], if y == 2 { p } else { b });
            }
        }
    }

    fn tall_wall(&mut self, open: bool, side: usize, y: i32) {
        let (mut a, mut c) = door(side, 0, y, 0);
        let b = self.bricks;
        if open {
            if side == SOUTH || side == NORTH {
                for x in [2, 5] {
                    self.fill([x, y + 1, a[2]], [x, y + 3, a[2]], b);
                }
            } else {
                for z in [2, 5] {
                    self.fill([a[0], y + 1, z], [a[0], y + 3, z], b);
                }
            }
            a[1] = y + 3;
            c[1] = y + 3;
            self.fill(a, c, b);
        } else {
            if side == SOUTH || side == NORTH {
                a[0] = 0;
                c[0] = 7;
            } else {
                a[2] = 0;
                c[2] = 7;
            }
            c[1] = y + 3;
            self.fill(a, c, b);
            a[1] = y + 2;
            c[1] = y + 2;
            self.fill(a, c, self.rough);
        }
    }

    fn core(&mut self) {
        let (p, b, d, l) = (self.rough, self.bricks, self.dark, self.lamp);
        self.cap([1, 8, 0], [14, 8, 14]);
        self.ring(15, 15, 7, b);
        for y in 1..=6 {
            let state = if y == 2 || y == 6 { p } else { b };
            for x in [0, 15] {
                for (a, c) in [(0, 1), (6, 9), (14, 15)] {
                    self.fill([x, y, a], [x, y, c], state);
                }
            }
            for (a, c) in [(1, 1), (6, 9), (14, 14)] {
                self.fill([a, y, 0], [c, y, 0], state);
            }
            self.fill([1, y, 15], [14, y, 15], state);
        }
        self.fill([6, 3, 6], [9, 6, 9], d);
        self.fill(
            [7, 4, 7],
            [8, 5, 8],
            block_states()
                .default_state("minecraft:gold_block")
                .unwrap(),
        );
        for x in [6, 9] {
            for z in [6, 9] {
                for y in [3, 6] {
                    self.block([x, y, z], l);
                }
            }
        }
        for x in [5, 10] {
            for z in [6, 9] {
                self.fill([x, 1, z], [x, 2, z], b);
            }
        }
        for x in [6, 9] {
            for z in [5, 10] {
                self.fill([x, 1, z], [x, 2, z], b);
            }
        }
        for x in [5, 10] {
            for z in [5, 10] {
                self.fill([x, 2, z], [x, 6, z], b);
            }
        }
        for x in [5, 10] {
            for z in [1, 9] {
                self.fill([x, 7, z], [x, 7, z + 5], b);
            }
        }
        for x in [1, 9] {
            for z in [5, 10] {
                self.fill([x, 7, z], [x + 5, 7, z], b);
            }
        }
        for x in [2, 13] {
            for z in [2, 12] {
                self.fill([x, 1, z], [x, 1, z + 1], b);
                self.block(
                    [if x == 2 { 3 } else { 12 }, 1, if z == 2 { 2 } else { 13 }],
                    b,
                );
            }
        }
    }

    fn inner_wing(&mut self, design: i32) {
        let (b, d, l) = (self.bricks, self.dark, self.lamp);
        if design == 0 {
            for i in 0..4 {
                self.fill([10 - i, 3 - i, 20 - i], [12 + i, 3 - i, 20], b);
            }
            self.fill([7, 0, 6], [15, 0, 16], b);
            for x in [6, 16] {
                self.fill([x, 0, 6], [x, 3, 20], b);
            }
            for x in [7, 15] {
                self.fill([x, 1, 7], [x, 1, 20], b);
            }
            self.fill([7, 1, 6], [9, 3, 6], b);
            self.fill([13, 1, 6], [15, 3, 6], b);
            self.fill([8, 1, 7], [9, 1, 7], b);
            self.fill([13, 1, 7], [14, 1, 7], b);
            self.fill([9, 0, 5], [13, 0, 5], b);
            self.fill([10, 0, 7], [12, 0, 7], d);
            for x in [8, 14] {
                self.fill([x, 0, 10], [x, 0, 12], d);
            }
            for x in [6, 16] {
                for z in [9, 12, 15, 18] {
                    self.block([x, 3, z], l);
                }
            }
            for x in [10, 12] {
                for z in [10, 12] {
                    self.block([x, 0, z], l);
                }
            }
            for x in [8, 14] {
                self.block([x, 3, 6], l);
            }
            for x in [4, 18] {
                for z in [4, 18] {
                    for y in 0..=2 {
                        self.block([x, y, z], if y == 1 { l } else { b });
                    }
                }
            }
            for x in [9, 13] {
                self.block([x, 7, 20], b);
            }
            self.fill([6, 0, 21], [7, 4, 21], b);
            self.fill([15, 0, 21], [16, 4, 21], b);
        } else {
            self.fill([9, 3, 18], [13, 3, 20], b);
            for x in [9, 13] {
                self.fill([x, 0, 18], [x, 2, 18], b);
                for y in 4..=6 {
                    self.block([x, y, 20], if y == 5 { l } else { b });
                }
            }
            self.fill([7, 3, 7], [15, 3, 14], b);
            for x in [10, 12] {
                for z in [10, 12] {
                    self.fill([x, 0, z], [x, 6, z], b);
                    for y in [0, 4] {
                        self.block([x, y, z], l);
                    }
                }
            }
            for x in [8, 14] {
                for z in [7, 14] {
                    self.fill([x, 0, z], [x, 2, z], b);
                }
                self.fill([x, 3, 8], [x, 3, 13], d);
            }
        }
    }

    fn penthouse(&mut self) {
        let (p, b, d, l) = (self.rough, self.bricks, self.dark, self.lamp);
        self.fill([2, -1, 2], [11, -1, 11], b);
        for (a, c) in [
            ([0, -1, 0], [1, -1, 11]),
            ([12, -1, 0], [13, -1, 11]),
            ([2, -1, 0], [11, -1, 1]),
            ([2, -1, 12], [11, -1, 13]),
        ] {
            self.fill(a, c, p);
        }
        self.ring(13, 13, 0, b);
        for i in [2, 5, 8, 11] {
            for pos in [[0, 0, i], [13, 0, i], [i, 0, 0]] {
                self.block(pos, l);
            }
        }
        self.fill([2, 0, 3], [4, 0, 9], b);
        self.fill([9, 0, 3], [11, 0, 9], b);
        self.fill([4, 0, 9], [9, 0, 11], b);
        for pos in [[5, 0, 8], [8, 0, 8], [10, 0, 10], [3, 0, 10]] {
            self.block(pos, b);
        }
        for x in [3, 10] {
            self.fill([x, 0, 3], [x, 0, 7], d);
            for z in [2, 5, 8] {
                self.fill([x, 0, z], [x, 2, z], b);
            }
        }
        self.fill([6, 0, 10], [7, 0, 10], d);
        for x in [5, 8] {
            self.fill([x, 0, 10], [x, 2, 10], b);
        }
        self.fill([6, -1, 7], [7, -1, 8], d);
        self.clear([6, -1, 3], [7, -1, 4]);
    }
}

fn door(side: usize, x: i32, y: i32, z: i32) -> ([i32; 3], [i32; 3]) {
    let (a, c) = match side {
        SOUTH => ([3, 1, 0], [4, 2, 0]),
        NORTH => ([3, 1, 7], [4, 2, 7]),
        WEST => ([0, 1, 3], [0, 2, 4]),
        EAST => ([7, 1, 3], [7, 2, 4]),
        _ => unreachable!(),
    };
    (
        [a[0] + x, a[1] + y, a[2] + z],
        [c[0] + x, c[1] + y, c[2] + z],
    )
}

#[cfg(test)]
mod tests {
    use super::super::Direction;
    use super::*;
    use harpas_core::random::LegacyRandom;

    fn single_room(kind: MonumentRoomKind, origin: [i32; 3]) -> MonumentLayout {
        let room = MonumentRoom {
            index: 25,
            connections: [None; 6],
            openings: [false; 6],
        };
        MonumentLayout {
            rooms: [(25, room)].into(),
            core_room: 25,
            orientation: Direction::South,
            pieces: vec![MonumentRoomPiece {
                kind,
                room: Some(25),
                design: Some(1),
                bounding_box: BoundingBox {
                    min: origin,
                    max: [origin[0] + 7, origin[1] + 3, origin[2] + 7],
                },
            }],
        }
    }

    #[test]
    fn ceilings_replace_only_source_water_and_floor_passages_stay_open() {
        let mut layout = single_room(MonumentRoomKind::Simple, [0, 0, 0]);
        layout.rooms.get_mut(&25).unwrap().openings[DOWN] = true;
        let states = block_states();
        let water = states.default_state("minecraft:water").unwrap();
        let flowing = states.state("minecraft:water", [("level", "7")]).unwrap().0;
        let ice = states.default_state("minecraft:ice").unwrap();
        let rough = states.default_state("minecraft:prismarine").unwrap();
        let mut chunk = GeneratedChunk::new(0, 0, -16, 48, 1);
        for (x, state) in [(1, water), (2, flowing), (3, ice)] {
            chunk.set_block_state(x, 4, 1, state);
        }
        chunk.set_block_state(3, 0, 3, ice);
        layout.place_rooms_in_chunk(&mut chunk, 63, &mut LegacyRandom::new(123));
        assert_eq!(chunk.block_state(1, 4, 1), rough);
        assert_eq!(chunk.block_state(2, 4, 1), flowing);
        assert_eq!(chunk.block_state(3, 4, 1), ice);
        assert_eq!(chunk.block_state(3, 0, 3), ice);
        assert_eq!(chunk.block_state(0, 0, 0), rough);
    }

    #[test]
    fn an_intersecting_sponge_room_draws_for_every_column_before_clipping() {
        let layout = single_room(MonumentRoomKind::SimpleTop, [13, 0, 0]);
        let mut random = LegacyRandom::new(123);
        let mut expected = random.clone();
        let mut chunk = GeneratedChunk::new(0, 0, -16, 48, 1);
        let mut whole = GeneratedChunk::new(0, 0, -16, 48, 1);
        single_room(MonumentRoomKind::SimpleTop, [0, 0, 0]).place_rooms_in_chunk(
            &mut whole,
            63,
            &mut expected,
        );
        let sponge = block_states()
            .default_state("minecraft:wet_sponge")
            .unwrap();
        layout.place_rooms_in_chunk(&mut chunk, 63, &mut random);
        let mut found = 0;
        for x in 13..16 {
            for z in 0..8 {
                for y in 0..=4 {
                    let state = chunk.block_state(x, y, z);
                    assert_eq!(state, whole.block_state(x - 13, y, z));
                    found += i32::from(state == sponge);
                }
            }
        }
        assert!(found > 0);
        assert_eq!(random.next_long(), expected.next_long());
        let mut untouched = LegacyRandom::new(123);
        let mut outside = GeneratedChunk::new(-1, 0, -16, 48, 1);
        layout.place_rooms_in_chunk(&mut outside, 63, &mut untouched);
        assert_eq!(untouched.next_long(), LegacyRandom::new(123).next_long());
    }

    #[test]
    fn closed_simple_walls_have_deep_sills_and_lintels() {
        let mut layout = single_room(MonumentRoomKind::Simple, [0, 0, 0]);
        layout.pieces[0].design = Some(0);
        let mut chunk = GeneratedChunk::new(0, 0, -16, 48, 1);
        layout.place_rooms_in_chunk(&mut chunk, 63, &mut LegacyRandom::new(123));
        let bricks = block_states()
            .default_state("minecraft:prismarine_bricks")
            .unwrap();
        let rough = block_states()
            .default_state("minecraft:prismarine")
            .unwrap();
        for y in [1, 3] {
            for (x, z) in [(3, 1), (3, 6), (1, 3), (6, 3)] {
                assert_eq!(chunk.block_state(x, y, z), bricks);
            }
        }
        assert_eq!(chunk.block_state(3, 2, 0), rough);
        assert_eq!(chunk.block_state(3, 2, 1), 0);
    }

    #[test]
    fn simple_room_draws_its_boolean_even_when_a_vertical_opening_prevents_the_pillar() {
        let mut layout = single_room(MonumentRoomKind::Simple, [0, 0, 0]);
        let room = layout.rooms.get_mut(&25).unwrap();
        room.openings[UP] = true;
        room.openings[NORTH] = true;
        let mut random = LegacyRandom::new(123);
        let mut expected = random.clone();
        assert!(expected.next_bool());
        let mut chunk = GeneratedChunk::new(0, 0, -16, 48, 1);
        layout.place_rooms_in_chunk(&mut chunk, 63, &mut random);
        assert_eq!(random.next_long(), expected.next_long());
        assert_eq!(chunk.block_state(3, 1, 3), 0);
    }
}
