//! Checks terrain, aquifers, ore veins and selected surface operations against an owner-provided server.

mod reference;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use harpas_core::random::{LegacyRandom, RandomSource};
use harpas_worldgen::VerticalAnchor;
use harpas_worldgen::carver::{
    CanyonPath, CarverConfig, CarverFloatProvider, CarverHeightProvider, CarvingEllipsoid,
    CarvingEnvironment, CarvingMask, CavePart, CaveRoom, CaveTunnel, canyon_should_skip,
    carve_ellipsoid, cave_should_skip, plan_canyon_path, plan_cave_parts,
};
use harpas_worldgen::{
    GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator, OreVeinifier, WorldgenData,
};

use reference::Reference;

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

struct ProbeAquifer {
    air: u32,
    water: u32,
    lava: u32,
    scheduled: bool,
}

impl CarvingEnvironment for ProbeAquifer {
    fn compute_substance(&mut self, x: i32, _y: i32, z: i32) -> Option<u32> {
        self.scheduled = (x + z) & 1 == 0;
        match x & 3 {
            0 => None,
            1 => Some(self.water),
            _ => Some(self.air),
        }
    }
    fn should_schedule_fluid_update(&self) -> bool {
        self.scheduled
    }
    fn has_fluid(&self, state: u32) -> bool {
        state == self.water || state == self.lava
    }
    fn top_material(&mut self, _: &GeneratedChunk, _: i32, _: i32, _: i32, _: bool) -> Option<u32> {
        panic!("synthetic terrain has no surface blocks")
    }
}

fn compare_seed(data: &WorldgenData, seed: i64, output: &Path) {
    let generator = NoiseBasedChunkGenerator::overworld(seed, data).expect("compile generator");
    let mut lines = BufReader::new(File::open(output).unwrap()).lines();
    let mut check = |expected: String| {
        let actual = lines.next().expect("missing Java result").unwrap();
        assert_eq!(expected, actual, "worldgen mismatch for seed {seed}");
    };
    let mut carver_names: Vec<_> = data.configured_carvers.keys().collect();
    carver_names.sort();
    for name in carver_names {
        let carver = CarverConfig::compile(name, data).unwrap();
        let mut random = LegacyRandom::new(seed);
        let hash = (0..1000).fold(OFFSET, |hash, _| {
            (hash ^ u64::from(carver.starts(&mut random))).wrapping_mul(PRIME)
        });
        check(format!(
            "q {name} {hash:016x} {} true true false",
            random.next_int()
        ));
    }
    let float_providers = [
        CarverFloatProvider::Constant(3.5),
        CarverFloatProvider::Uniform {
            min: -1.0,
            max: 1.4,
        },
        CarverFloatProvider::Trapezoid {
            min: 0.0,
            max: 6.0,
            plateau: 2.0,
        },
        CarverFloatProvider::Trapezoid {
            min: -2.0,
            max: 4.0,
            plateau: 0.0,
        },
        CarverFloatProvider::Trapezoid {
            min: 1.0,
            max: 3.0,
            plateau: 2.0,
        },
    ];
    for (index, provider) in float_providers.into_iter().enumerate() {
        let mut random = LegacyRandom::new(seed);
        let hash = (0..100).fold(OFFSET, |hash, _| {
            (hash ^ u64::from(provider.sample(&mut random).to_bits())).wrapping_mul(PRIME)
        });
        check(format!("f {index} {hash:016x} {}", random.next_int()));
    }
    let height_providers = [
        CarverHeightProvider::Uniform {
            min: VerticalAnchor::AboveBottom(8),
            max: VerticalAnchor::Absolute(47),
        },
        CarverHeightProvider::Uniform {
            min: VerticalAnchor::AboveBottom(0),
            max: VerticalAnchor::BelowTop(0),
        },
        CarverHeightProvider::Uniform {
            min: VerticalAnchor::Absolute(12),
            max: VerticalAnchor::Absolute(12),
        },
    ];
    for (index, provider) in height_providers.into_iter().enumerate() {
        let mut random = LegacyRandom::new(seed);
        let hash = (0..100).fold(OFFSET, |hash, _| {
            (hash ^ u64::from(provider.sample(&mut random, -64, 384) as u32)).wrapping_mul(PRIME)
        });
        check(format!("v {index} {hash:016x} {}", random.next_int()));
    }
    let (mut cave_hash, mut canyon_hash) = (OFFSET, OFFSET);
    for (floor, width) in [(-1.0, 0.5), (-0.4, 1.0), (0.0, 1.5)] {
        for xi in -5..=5 {
            for yi in -5..=5 {
                for zi in -5..=5 {
                    let point = [
                        f64::from(xi) / 5.0,
                        f64::from(yi) / 5.0,
                        f64::from(zi) / 5.0,
                    ];
                    cave_hash =
                        (cave_hash ^ u64::from(cave_should_skip(point, floor))).wrapping_mul(PRIME);
                    canyon_hash = (canyon_hash ^ u64::from(canyon_should_skip(point, width)))
                        .wrapping_mul(PRIME);
                }
            }
        }
    }
    check(format!("k {cave_hash:016x} {canyon_hash:016x}"));
    let mut trig_hash = OFFSET;
    let trig_scale = 65_536.0 / (2.0 * std::f64::consts::PI);
    for i in 0..65_536 {
        let angle = (f64::from(i) + 0.25) / trig_scale;
        for value in [
            harpas_core::math::sin_f64(angle),
            harpas_core::math::cos_f64(angle),
        ] {
            trig_hash = (trig_hash ^ u64::from(value.to_bits())).wrapping_mul(PRIME);
        }
    }
    check(format!("j {trig_hash:016x}"));
    for index in 0..5 {
        let room = CaveRoom {
            center: [-10.0, 32.0, 20.0],
            width: [0.0, 0.5, 2.3, 6.0, 10.0][index],
            y_scale: [0.5, 1.0, 1.25, 2.0, 0.1][index],
            floor_level: -0.5,
        };
        let ellipsoid = room.ellipsoid();
        let hash = [
            ellipsoid.center[0],
            ellipsoid.center[1],
            ellipsoid.center[2],
            ellipsoid.horizontal_radius,
            ellipsoid.vertical_radius,
        ]
        .into_iter()
        .fold(OFFSET, |hash, value| {
            (hash ^ value.to_bits()).wrapping_mul(PRIME)
        });
        check(format!("m {index} 1 {hash:016x}"));
    }
    let tunnels = [
        CaveTunnel {
            seed: 42,
            center: [8.0, 64.0, 8.0],
            radius_scale: [1.0, 1.0],
            thickness: 2.0,
            yaw: 0.0,
            pitch: 0.0,
            first_step: 0,
            end_step: 64,
            y_scale: 1.0,
        },
        CaveTunnel {
            seed,
            center: [8.0, 32.0, 8.0],
            radius_scale: [0.7, 1.3],
            thickness: 0.8,
            yaw: 1.2,
            pitch: 0.15,
            first_step: 0,
            end_step: 72,
            y_scale: 0.75,
        },
        CaveTunnel {
            seed: -1,
            center: [-10.0, -40.0, 16.0],
            radius_scale: [1.2, 0.8],
            thickness: 3.0,
            yaw: -0.8,
            pitch: -0.2,
            first_step: 0,
            end_step: 112,
            y_scale: 1.0,
        },
        CaveTunnel {
            seed: 0,
            center: [50.0, 20.0, 50.0],
            radius_scale: [1.0, 1.0],
            thickness: 0.7,
            yaw: 2.2,
            pitch: 0.03,
            first_step: 0,
            end_step: 48,
            y_scale: 1.0,
        },
    ];
    for (index, tunnel) in tunnels.into_iter().enumerate() {
        let mut count = 0;
        let mut hash = OFFSET;
        tunnel.for_each_ellipsoid(0, 0, |ellipsoid| {
            count += 1;
            for value in [
                ellipsoid.center[0],
                ellipsoid.center[1],
                ellipsoid.center[2],
                ellipsoid.horizontal_radius,
                ellipsoid.vertical_radius,
            ] {
                hash = (hash ^ value.to_bits()).wrapping_mul(PRIME);
            }
        });
        check(format!("w {index} {count} {hash:016x}"));
    }
    for index in 0..64_i32 {
        let tunnel = CaveTunnel {
            seed: seed.wrapping_add(i64::from(index) * 104_729),
            center: [
                f64::from((index % 5) * 8 - 8),
                f64::from((index % 7) * 16 - 48),
                f64::from((index % 4) * 8 - 8),
            ],
            radius_scale: [
                [0.6, 1.0, 1.4][index as usize % 3],
                [0.7, 1.0, 1.3][index as usize % 3],
            ],
            thickness: [0.5, 0.99, 1.0, 1.5, 2.0, 3.0][index as usize % 6],
            yaw: index as f32 * 0.1,
            pitch: (index % 9 - 4) as f32 * 0.05,
            first_step: 0,
            end_step: [16, 32, 64, 112][index as usize % 4],
            y_scale: [0.5, 1.0, 1.25][index as usize % 3],
        };
        let mut count = 0;
        let mut hash = OFFSET;
        tunnel.for_each_ellipsoid(0, 0, |ellipsoid| {
            count += 1;
            for value in [
                ellipsoid.center[0],
                ellipsoid.center[1],
                ellipsoid.center[2],
                ellipsoid.horizontal_radius,
                ellipsoid.vertical_radius,
            ] {
                hash = (hash ^ value.to_bits()).wrapping_mul(PRIME);
            }
        });
        check(format!("w {} {count} {hash:016x}", index + 4));
    }
    for name in ["minecraft:cave", "minecraft:cave_extra_underground"] {
        let config = CarverConfig::compile(name, data).unwrap();
        for index in 0..64_i32 {
            let mut random = LegacyRandom::new(seed.wrapping_add(i64::from(index) * 104_729));
            let mut count = 0_u64;
            let mut hash = OFFSET;
            let mut add = |value: u64| {
                hash = (hash ^ value).wrapping_mul(PRIME);
            };
            plan_cave_parts(
                &config,
                &mut random,
                index % 9 - 4,
                index % 7 - 3,
                -64,
                384,
                |part| {
                    count += 1;
                    match part {
                        CavePart::Room(room) => {
                            add(1);
                            for v in room.center {
                                add(v.to_bits());
                            }
                            add(u64::from(room.width.to_bits()));
                            add(room.y_scale.to_bits());
                        }
                        CavePart::Tunnel { path, .. } => {
                            add(2);
                            add(path.seed as u64);
                            for v in path.center {
                                add(v.to_bits());
                            }
                            for v in path.radius_scale {
                                add(v.to_bits());
                            }
                            add(u64::from(path.thickness.to_bits()));
                            add(u64::from(path.yaw.to_bits()));
                            add(u64::from(path.pitch.to_bits()));
                            add(path.first_step as u64);
                            add(path.end_step as u64);
                            add(path.y_scale.to_bits());
                        }
                    }
                },
            )
            .unwrap();
            check(format!(
                "r {name} {index} {count} {hash:016x} {}",
                random.next_int()
            ));
        }
    }
    let config = CarverConfig::compile("minecraft:canyon", data).unwrap();
    let harpas_worldgen::carver::CarverShape::Canyon(mut shape) = config.shape else {
        panic!("expected canyon shape");
    };
    for smoothness in [1, 2, 3, 10] {
        shape.width_smoothness = smoothness;
        let mut random = LegacyRandom::new(seed);
        let widths = shape.width_factors(&mut random, 384);
        let hash = widths.iter().fold(OFFSET, |hash, width| {
            (hash ^ u64::from(width.to_bits())).wrapping_mul(PRIME)
        });
        check(format!(
            "b {smoothness} {} {hash:016x} {}",
            widths.len(),
            random.next_int()
        ));
    }
    for index in 0..32_i32 {
        shape.width_smoothness = [1, 2, 3, 10][index as usize % 4];
        shape.vertical_radius_default_factor = [1.0, 1.2][index as usize % 2];
        shape.vertical_radius_center_factor = [0.0, 0.8][index as usize % 2];
        let path = CanyonPath {
            seed: seed.wrapping_add(i64::from(index) * 104_729),
            center: [
                f64::from((index % 5) * 8 - 8),
                f64::from((index % 7) * 16 - 48),
                f64::from((index % 4) * 8 - 8),
            ],
            thickness: [0.5, 1.0, 2.0, 3.0][index as usize % 4],
            yaw: index as f32 * 0.1,
            pitch: (index % 9 - 4) as f32 * 0.05,
            first_step: 0,
            end_step: [16, 32, 64, 112][index as usize % 4],
            y_scale: [0.5, 1.0, 1.25][index as usize % 3],
        };
        let mut count = 0;
        let mut hash = OFFSET;
        path.for_each_ellipsoid(&shape, 0, 0, 384, |ellipsoid, widths| {
            count += 1;
            for value in [
                ellipsoid.center[0],
                ellipsoid.center[1],
                ellipsoid.center[2],
                ellipsoid.horizontal_radius,
                ellipsoid.vertical_radius,
            ] {
                hash = (hash ^ value.to_bits()).wrapping_mul(PRIME);
            }
            for layer in [-63, -32, 0, 64, 150, 318] {
                let skip = canyon_should_skip([0.75, 0.5, 0.25], widths[(layer + 64 - 1) as usize]);
                hash = (hash ^ u64::from(skip)).wrapping_mul(PRIME);
            }
        });
        check(format!("n {index} {count} {hash:016x}"));
    }
    let config = CarverConfig::compile("minecraft:canyon", data).unwrap();
    let harpas_worldgen::carver::CarverShape::Canyon(shape) = config.shape else {
        unreachable!();
    };
    for index in 0..64_i32 {
        let mut random = LegacyRandom::new(seed.wrapping_add(i64::from(index) * 104_729));
        let path =
            plan_canyon_path(&config, &mut random, index % 9 - 4, index % 7 - 3, -64, 384).unwrap();
        let mut count = 0;
        let mut hash = OFFSET;
        path.for_each_ellipsoid(&shape, 0, 0, 384, |ellipsoid, widths| {
            count += 1;
            for value in [
                ellipsoid.center[0],
                ellipsoid.center[1],
                ellipsoid.center[2],
                ellipsoid.horizontal_radius,
                ellipsoid.vertical_radius,
            ] {
                hash = (hash ^ value.to_bits()).wrapping_mul(PRIME);
            }
            for layer in [-63, -32, 0, 64, 150, 318] {
                let skip = canyon_should_skip([0.75, 0.5, 0.25], widths[(layer + 64 - 1) as usize]);
                hash = (hash ^ u64::from(skip)).wrapping_mul(PRIME);
            }
        });
        check(format!(
            "g {index} {count} {hash:016x} {}",
            random.next_int()
        ));
    }
    {
        let states = harpas_core::block::block_states();
        let stone = states.default_state("minecraft:stone").unwrap();
        let bedrock = states.default_state("minecraft:bedrock").unwrap();
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, data.biomes.len());
        for x in 0..16_usize {
            for z in 0..16_usize {
                for y in 5..=15_i32 {
                    let state = if (x as i32 * 3 + z as i32 + y) % 7 == 0 {
                        bedrock
                    } else {
                        stone
                    };
                    chunk.set_block_state(x, y, z, state);
                }
            }
        }
        let mut config = CarverConfig::compile("minecraft:cave", data).unwrap();
        config.lava_level = VerticalAnchor::Absolute(10);
        config.replaceable.fill(false);
        config.replaceable[stone as usize] = true;
        let mut mask = CarvingMask::new(-64, 384);
        mask.insert(8, 11, 8);
        let mut aquifer = ProbeAquifer {
            air: states.default_state("minecraft:air").unwrap(),
            water: states.default_state("minecraft:water").unwrap(),
            lava: states.default_state("minecraft:lava").unwrap(),
            scheduled: false,
        };
        for x in [8.5, 9.5] {
            let ellipsoid = CarvingEllipsoid {
                center: [x, 10.5, 8.5],
                horizontal_radius: 3.2,
                vertical_radius: 2.7,
            };
            carve_ellipsoid(
                &mut chunk,
                &mut mask,
                &config,
                ellipsoid,
                &mut aquifer,
                |point, _| cave_should_skip(point, -0.4),
            );
        }
        let mut block_hash = OFFSET;
        for x in 0..16 {
            for z in 0..16 {
                for y in 5..=15 {
                    block_hash =
                        (block_hash ^ u64::from(chunk.block_state(x, y, z))).wrapping_mul(PRIME);
                }
            }
        }
        let mask_hash = mask.to_raw_data().into_iter().fold(OFFSET, |hash, word| {
            (hash ^ word as u64).wrapping_mul(PRIME)
        });
        let mut post_hash = OFFSET;
        for &(x, y, z) in chunk.fluid_postprocessing() {
            let section = (y >> 4) - (chunk.min_y() >> 4);
            let value = u64::from(x) | (u64::from((y & 15) as u32) << 4) | (u64::from(z) << 8);
            post_hash = (post_hash ^ section as u64).wrapping_mul(PRIME);
            post_hash = (post_hash ^ value).wrapping_mul(PRIME);
        }
        check(format!(
            "d {block_hash:016x} {mask_hash:016x} {} {post_hash:016x}",
            chunk.fluid_postprocessing().len()
        ));
    }
    let surface = generator.surface_system();
    for x in (-32..=32).step_by(7) {
        for z in (-32..=32).step_by(9) {
            let depth = surface.get_surface_depth(x, z);
            for y in (-64..320).step_by(13) {
                check(format!(
                    "s {x} {y} {z} {depth} {}",
                    surface.get_band(x, y, z)
                ));
            }
        }
    }
    let ore = OreVeinifier::new(generator.graph().ore_veins_random());
    for toggle in [-0.8, -0.6, -0.4, 0.4, 0.6, 0.8] {
        for ridged in [-0.1, 0.1] {
            for gap in [-0.5, 0.0] {
                for x in (-8..=8).step_by(4) {
                    for z in (-8..=8).step_by(4) {
                        for y in (-60..=50).step_by(5) {
                            let state = ore.compute_with_samplers(x, y, z, toggle, |is_gap| {
                                if is_gap { gap } else { ridged }
                            });
                            check(format!(
                                "o {toggle:.1} {ridged:.1} {gap:.1} {x} {y} {z} {}",
                                state.map(i64::from).unwrap_or(-1)
                            ));
                        }
                    }
                }
            }
        }
    }
    let mut chunks = Vec::new();
    for cx in [-1, 0, 1] {
        for cz in [-1, 0, 1] {
            chunks.push((cx, cz));
        }
    }
    if seed == 42 {
        for cx in [-120, -119] {
            for cz in [-120, -119] {
                chunks.push((cx, cz));
            }
        }
        for cx in [360, 361] {
            for cz in [-32, -31] {
                chunks.push((cx, cz));
            }
        }
    }
    for (cx, cz) in chunks {
        let mut noise = generator.create_noise_chunk(cx, cz);
        let mut aquifer = generator.create_aquifer(cx, cz);
        for x in (cx * 16 - 20..=cx * 16 + 20).step_by(8) {
            for z in (cz * 16 - 20..=cz * 16 + 20).step_by(8) {
                check(format!(
                    "p {cx} {cz} {x} {z} {}",
                    noise.preliminary_surface_level(x, z)
                ));
            }
        }
        for x in (cx * 16..cx * 16 + 16).step_by(5) {
            for z in (cz * 16..cz * 16 + 16).step_by(5) {
                for y in (-64..320).step_by(7) {
                    for density in [-1.0, -0.1, 0.0, 0.1] {
                        let state = aquifer.compute_substance(&mut noise, x, y, z, density);
                        check(format!(
                            "a {x} {y} {z} {density:.1} {} {}",
                            state.map(i64::from).unwrap_or(-1),
                            aquifer.should_schedule_fluid_update()
                        ));
                    }
                }
            }
        }
        let mut terrain = GeneratedChunk::new(cx, cz, -64, 384, data.biomes.len());
        let mut noise = generator.create_noise_chunk(cx, cz);
        let mut aquifer = generator.create_aquifer(cx, cz);
        generator.populate_biomes(&mut terrain, &mut noise);
        generator.fill_from_noise_with_aquifer(&mut terrain, &mut noise, &mut aquifer);
        let mut hash = OFFSET;
        for cell_x in 0..4 {
            for cell_z in 0..4 {
                for cell_y in (0..48).rev() {
                    for iy in (0..8).rev() {
                        let y = -64 + cell_y * 8 + iy;
                        for ix in 0..4 {
                            for iz in 0..4 {
                                let state =
                                    terrain.block_state(cell_x * 4 + ix, y, cell_z * 4 + iz);
                                hash = (hash ^ u64::from(state)).wrapping_mul(PRIME);
                            }
                        }
                    }
                }
            }
        }
        let mut fluid_hash = OFFSET;
        for &(x, y, z) in terrain.fluid_postprocessing() {
            for value in [u64::from(x), u64::from(y as u32), u64::from(z)] {
                fluid_hash = (fluid_hash ^ value).wrapping_mul(PRIME);
            }
        }
        check(format!(
            "t {cx} {cz} {hash:016x} {} {fluid_hash:016x}",
            terrain.fluid_postprocessing().len()
        ));
        generator.build_surface(&mut terrain, &mut noise, data);
        let mut surface_hash = OFFSET;
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..320 {
                    surface_hash = (surface_hash ^ u64::from(terrain.block_state(x, y, z)))
                        .wrapping_mul(PRIME);
                }
            }
        }
        check(format!("u {cx} {cz} {surface_hash:016x}"));
        for kind in HeightmapType::ALL {
            let hash = terrain
                .heightmaps()
                .pack(kind)
                .into_iter()
                .fold(OFFSET, |hash, word| {
                    (hash ^ word as u64).wrapping_mul(PRIME)
                });
            check(format!("h {cx} {cz} {} {hash:016x}", kind.nbt_key()));
        }
        for x in [0, 7, 15] {
            for z in [0, 7, 15] {
                for y in [-60, -1, 30, 63, 128, 300] {
                    for fluid in [false, true] {
                        let state = generator.top_material(
                            &terrain,
                            &mut noise,
                            data,
                            [cx * 16 + x, y, cz * 16 + z],
                            fluid,
                        );
                        check(format!(
                            "q2 {cx} {cz} {x} {y} {z} {fluid} {}",
                            state.map(i64::from).unwrap_or(-1)
                        ));
                    }
                }
            }
        }
        let ellipsoids = [
            [8.1, 10.1, 8.1, 2.6, 1.5],
            [-1.3, 30.5, 8.5, 2.4, 4.0],
            [16.9, 42.7, 15.9, 3.5, 2.3],
            [8.5, -64.0, 8.5, 2.0, 2.0],
            [8.5, 312.0, 8.5, 1.0, 2.0],
            [30.0, 10.0, 8.0, 2.0, 2.0],
        ];
        for (index, [x, y, z, horizontal_radius, vertical_radius]) in
            ellipsoids.into_iter().enumerate()
        {
            let ellipsoid = CarvingEllipsoid {
                center: [f64::from(cx * 16) + x, y, f64::from(cz * 16) + z],
                horizontal_radius,
                vertical_radius,
            };
            let mut count = 0_u64;
            let mut hash = OFFSET;
            ellipsoid.for_each_candidate(cx, cz, -64, 384, |_, block_y, _, [dx, dy, dz]| {
                count += 1;
                for value in [
                    dx.to_bits(),
                    dy.to_bits(),
                    dz.to_bits(),
                    block_y as i64 as u64,
                ] {
                    hash = (hash ^ value).wrapping_mul(PRIME);
                }
            });
            check(format!("e {cx} {cz} {index} {count} {hash:016x}"));
        }
        let mut mask = CarvingMask::new(-64, 384);
        for (kind, name) in [
            "minecraft:cave",
            "minecraft:cave_extra_underground",
            "minecraft:canyon",
        ]
        .into_iter()
        .enumerate()
        {
            let config = CarverConfig::compile(name, data).unwrap();
            for (offset, (dx, dz)) in [(0, 0), (1, 0), (-1, 1)].into_iter().enumerate() {
                let mut rng = LegacyRandom::new(
                    seed.wrapping_add(kind as i64 * 104_729 + offset as i64 * 8_191),
                );
                generator
                    .carve_from_source(
                        &mut terrain,
                        &mut noise,
                        &mut aquifer,
                        data,
                        &mut mask,
                        &config,
                        &mut rng,
                        [cx + dx, cz + dz],
                    )
                    .unwrap();
            }
        }
        let mut carved_hash = OFFSET;
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..320 {
                    carved_hash =
                        (carved_hash ^ u64::from(terrain.block_state(x, y, z))).wrapping_mul(PRIME);
                }
            }
        }
        let carved_mask_hash = mask.to_raw_data().into_iter().fold(OFFSET, |hash, word| {
            (hash ^ word as u64).wrapping_mul(PRIME)
        });
        let mut post_hash = OFFSET;
        for section in 0..24 {
            for &(x, y, z) in terrain.fluid_postprocessing() {
                if (y >> 4) - (terrain.min_y() >> 4) != section {
                    continue;
                }
                let value = u64::from(x) | (u64::from((y & 15) as u32) << 4) | (u64::from(z) << 8);
                post_hash = (post_hash ^ section as u64).wrapping_mul(PRIME);
                post_hash = (post_hash ^ value).wrapping_mul(PRIME);
            }
        }
        check(format!(
            "a2 {cx} {cz} {carved_hash:016x} {carved_mask_hash:016x} {} {post_hash:016x}",
            terrain.fluid_postprocessing().len()
        ));
        let mask = generator
            .apply_carvers(&mut terrain, &mut noise, &mut aquifer, data)
            .unwrap();
        let mut auto_hash = OFFSET;
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..320 {
                    auto_hash =
                        (auto_hash ^ u64::from(terrain.block_state(x, y, z))).wrapping_mul(PRIME);
                }
            }
        }
        let auto_mask_hash = mask.to_raw_data().into_iter().fold(OFFSET, |hash, word| {
            (hash ^ word as u64).wrapping_mul(PRIME)
        });
        let mut post_hash = OFFSET;
        for section in 0..24 {
            for &(x, y, z) in terrain.fluid_postprocessing() {
                if (y >> 4) - (terrain.min_y() >> 4) != section {
                    continue;
                }
                let value = u64::from(x) | (u64::from((y & 15) as u32) << 4) | (u64::from(z) << 8);
                post_hash = (post_hash ^ section as u64).wrapping_mul(PRIME);
                post_hash = (post_hash ^ value).wrapping_mul(PRIME);
            }
        }
        check(format!(
            "a3 {cx} {cz} {auto_hash:016x} {auto_mask_hash:016x} {} {post_hash:016x}",
            terrain.fluid_postprocessing().len()
        ));
        for kind in HeightmapType::ALL {
            let hash = terrain
                .heightmaps()
                .pack(kind)
                .into_iter()
                .fold(OFFSET, |hash, word| {
                    (hash ^ word as u64).wrapping_mul(PRIME)
                });
            check(format!("h2 {cx} {cz} {} {hash:016x}", kind.nbt_key()));
        }
    }
    assert!(lines.next().is_none(), "unexpected extra Java results");
}

#[test]
fn terrain_aquifers_veins_and_surface_samples_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping vanilla worldgen parity: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    reference.compile(&[(
        "WorldgenReferenceProbe.java",
        include_str!("WorldgenReferenceProbe.java"),
    )]);
    for &seed in reference::sampled_seeds(&[123_456_789, 42, -1]) {
        let output = reference.temp.0.join("results.txt");
        reference.java(
            "WorldgenReferenceProbe",
            [output.display().to_string(), seed.to_string()],
        );
        compare_seed(&data, seed, &output);
    }
}
