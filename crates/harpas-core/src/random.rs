//! Deterministic random number generators identical to vanilla's
//!
//! Vanilla world generation, structure placement, loot and gameplay consume two pseudorandom families
//! (`docs/engine-core.md#7-determinism`):
//! - [`LegacyRandom`]: Java's 48-bit linear congruential generator (`java.util.Random` / `LegacyRandomSource`);
//! - [`XoroshiroRandom`]: Blackman and Vigna's `xoroshiro128++` 1.0 (`XoroshiroRandomSource`), seeded through
//!   Stafford's Mix13 and MD5 ([`Seed128`]).
//!
//! [`WorldgenRandom`] layers vanilla's chunk, feature, carver, region-structure and slime-chunk seeding formulas
//! on top of any [`RandomSource`].

use md5::{Digest, Md5};

use crate::math::{java_string_hash, ln, position_seed};

/// Odd 64-bit fractional constant of the golden ratio `(√5 − 1) / 2 · 2^64` (`-7046029254386353131_i64`).
pub const GOLDEN_RATIO_64: u64 = 0x9e37_79b9_7f4a_7c15;

/// Odd 64-bit fractional constant of the silver ratio `(√2 − 1) · 2^64` (`7640891576956012809_i64`).
pub const SILVER_RATIO_64: u64 = 0x6a09_e667_f3bc_c909;

const LEGACY_MULTIPLIER: u64 = 0x0005_deec_e66d;
const LEGACY_ADDEND: u64 = 0xb;
const LEGACY_MASK: u64 = (1_u64 << 48) - 1;
const FLOAT_UNIT: f32 = f32::from_bits(0x3380_0000); // 2^-24
const DOUBLE_UNIT: f64 = f64::from_bits(0x3ca0_0000_0000_0000); // 2^-53

/// David Stafford's `Mix13` 64-bit bijective avalanche mixer, used by `SplitMix64` and vanilla's `RandomSupport`.
pub const fn mix_stafford_13(mut seed: u64) -> u64 {
    seed = (seed ^ (seed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    seed = (seed ^ (seed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    seed ^ (seed >> 31)
}

/// A 128-bit seed pair `(lo, hi)` for [`XoroshiroRandom`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seed128 {
    /// Low 64 bits (`seedLo`).
    pub lo: u64,
    /// High 64 bits (`seedHi`).
    pub hi: u64,
}

impl Seed128 {
    /// Creates a 128-bit seed from explicit halves.
    pub const fn new(lo: u64, hi: u64) -> Self {
        Self { lo, hi }
    }

    /// Expands a 64-bit seed using the silver and golden ratio constants without applying [`mix_stafford_13`].
    pub const fn upgrade_unmixed(seed: i64) -> Self {
        let lo = (seed as u64) ^ SILVER_RATIO_64;
        let hi = lo.wrapping_add(GOLDEN_RATIO_64);
        Self { lo, hi }
    }

    /// Expands a 64-bit seed and mixes both halves with [`mix_stafford_13`], as vanilla's `upgradeSeedTo128Bit`.
    pub const fn from_seed(seed: i64) -> Self {
        Self::upgrade_unmixed(seed).mixed()
    }

    /// Mixes both 64-bit halves with [`mix_stafford_13`].
    pub const fn mixed(self) -> Self {
        Self {
            lo: mix_stafford_13(self.lo),
            hi: mix_stafford_13(self.hi),
        }
    }

    /// Bitwise XOR of both halves.
    pub const fn xor(self, other: Self) -> Self {
        Self {
            lo: self.lo ^ other.lo,
            hi: self.hi ^ other.hi,
        }
    }

    /// Hashes `input` as UTF-8 with MD5 into two big-endian 64-bit words, unmixed (`RandomSupport.seedFromHashOf`).
    pub fn from_md5(input: &str) -> Self {
        let digest = Md5::digest(input.as_bytes());
        let lo = u64::from_be_bytes(digest[0..8].try_into().expect("8-byte low half"));
        let hi = u64::from_be_bytes(digest[8..16].try_into().expect("8-byte high half"));
        Self { lo, hi }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct MarsagliaGaussian {
    cached: Option<f64>,
}

impl MarsagliaGaussian {
    const fn new() -> Self {
        Self { cached: None }
    }

    fn reset(&mut self) {
        self.cached = None;
    }

    fn sample(&mut self, mut next_double: impl FnMut() -> f64) -> f64 {
        if let Some(value) = self.cached.take() {
            return value;
        }
        loop {
            let v1 = 2.0 * next_double() - 1.0;
            let v2 = 2.0 * next_double() - 1.0;
            let s = v1 * v1 + v2 * v2;
            if s < 1.0 && s != 0.0 {
                let multiplier = (-2.0 * ln(s) / s).sqrt();
                self.cached = Some(v2 * multiplier);
                return v1 * multiplier;
            }
        }
    }
}

/// Interface shared by vanilla's deterministic random generators (`RandomSource`).
pub trait RandomSource {
    /// Splits off an independent generator by drawing its seed from `self`.
    fn fork_source(&mut self) -> AnyRandom;

    /// Splits off a positional factory by drawing its seed from `self`.
    fn fork_positional_source(&mut self) -> PositionalRandom;

    /// Reseeds the generator and clears any cached Gaussian sample.
    fn set_seed(&mut self, seed: i64);

    /// Uniformly distributed 32-bit signed integer.
    fn next_int(&mut self) -> i32;

    /// Uniformly distributed integer in `0..bound`.
    ///
    /// # Panics
    ///
    /// Panics if `bound <= 0`.
    fn next_int_bounded(&mut self, bound: i32) -> i32;

    /// Uniformly distributed integer in `min..=max` (`betweenInclusive`).
    ///
    /// # Panics
    ///
    /// Panics if `min > max` or `max - min + 1` overflows `i32` into a non-positive bound, as in Java.
    fn next_int_between_inclusive(&mut self, min: i32, max: i32) -> i32 {
        assert!(min <= max, "min ({min}) must be <= max ({max})");
        self.next_int_bounded(max.wrapping_sub(min).wrapping_add(1))
            .wrapping_add(min)
    }

    /// Uniformly distributed 64-bit signed integer.
    fn next_long(&mut self) -> i64;

    /// Uniformly distributed boolean.
    fn next_bool(&mut self) -> bool;

    /// Uniformly distributed `f32` in `[0.0, 1.0)` with 24 bits of precision.
    fn next_float(&mut self) -> f32;

    /// Uniformly distributed `f64` in `[0.0, 1.0)` with 53 bits of precision.
    fn next_double(&mut self) -> f64;

    /// Standard normal (`μ = 0`, `σ = 1`) sample via Marsaglia's polar method.
    fn next_gaussian(&mut self) -> f64;

    /// Symmetric triangular distribution centered at `mode` with half-width `deviation`.
    fn triangle(&mut self, mode: f64, deviation: f64) -> f64 {
        let a = self.next_double();
        let b = self.next_double();
        mode + deviation * (a - b)
    }

    /// Advances the generator by `count` [`Self::next_int`] draws.
    fn consume_count(&mut self, count: u32) {
        for _ in 0..count {
            self.next_int();
        }
    }
}

/// Java's 48-bit linear congruential generator (`java.util.Random` / vanilla's `LegacyRandomSource`).
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyRandom {
    state: u64,
    gaussian: MarsagliaGaussian,
}

impl LegacyRandom {
    /// Creates a generator seeded with `(seed ^ 0x5DEECE66D) & ((1 << 48) - 1)`.
    pub const fn new(seed: i64) -> Self {
        Self {
            state: ((seed as u64) ^ LEGACY_MULTIPLIER) & LEGACY_MASK,
            gaussian: MarsagliaGaussian::new(),
        }
    }

    fn step_bits(state: &mut u64, bits: u32) -> i32 {
        *state = state
            .wrapping_mul(LEGACY_MULTIPLIER)
            .wrapping_add(LEGACY_ADDEND)
            & LEGACY_MASK;
        (*state >> (48 - bits)) as i32
    }

    fn sample_double(state: &mut u64) -> f64 {
        let hi = i64::from(Self::step_bits(state, 26)) << 27;
        let lo = i64::from(Self::step_bits(state, 27));
        ((hi + lo) as f64) * DOUBLE_UNIT
    }

    /// Draws the top `bits` (`1..=32`) of the next 48-bit LCG state.
    pub fn next_bits(&mut self, bits: u32) -> i32 {
        debug_assert!((1..=32).contains(&bits));
        Self::step_bits(&mut self.state, bits)
    }

    /// Splits off a new [`LegacyRandom`] seeded with [`Self::next_long`].
    pub fn fork(&mut self) -> Self {
        Self::new(self.next_long())
    }

    /// Splits off a [`LegacyPositionalRandom`] seeded with [`Self::next_long`].
    pub fn fork_positional(&mut self) -> LegacyPositionalRandom {
        LegacyPositionalRandom::new(self.next_long())
    }
}

impl RandomSource for LegacyRandom {
    fn fork_source(&mut self) -> AnyRandom {
        AnyRandom::Legacy(self.fork())
    }

    fn fork_positional_source(&mut self) -> PositionalRandom {
        PositionalRandom::Legacy(self.fork_positional())
    }

    fn set_seed(&mut self, seed: i64) {
        self.state = ((seed as u64) ^ LEGACY_MULTIPLIER) & LEGACY_MASK;
        self.gaussian.reset();
    }

    fn next_int(&mut self) -> i32 {
        self.next_bits(32)
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        if (bound & (bound - 1)) == 0 {
            return ((i64::from(bound) * i64::from(self.next_bits(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next_bits(31);
            let value = bits % bound;
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }

    fn next_long(&mut self) -> i64 {
        // Java sign-extends the second signed 32-bit `next(32)` before adding it to `first << 32`.
        let hi = i64::from(self.next_bits(32)) << 32;
        let lo = i64::from(self.next_bits(32));
        hi.wrapping_add(lo)
    }

    fn next_bool(&mut self) -> bool {
        self.next_bits(1) != 0
    }

    fn next_float(&mut self) -> f32 {
        (self.next_bits(24) as f32) * FLOAT_UNIT
    }

    fn next_double(&mut self) -> f64 {
        Self::sample_double(&mut self.state)
    }

    fn next_gaussian(&mut self) -> f64 {
        let state = &mut self.state;
        self.gaussian.sample(|| Self::sample_double(state))
    }
}

/// Positional factory backed by [`LegacyRandom`] (`LegacyPositionalRandomFactory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyPositionalRandom {
    seed: i64,
}

impl LegacyPositionalRandom {
    /// Creates a positional factory from a 64-bit seed.
    pub const fn new(seed: i64) -> Self {
        Self { seed }
    }

    /// Creates a [`LegacyRandom`] for block coordinates `(x, y, z)`.
    pub const fn at(&self, x: i32, y: i32, z: i32) -> LegacyRandom {
        LegacyRandom::new(position_seed(x, y, z) ^ self.seed)
    }

    /// Creates a [`LegacyRandom`] from Java's [`String::hashCode`](java_string_hash) of `name`.
    pub fn from_hash_of(&self, name: &str) -> LegacyRandom {
        LegacyRandom::new(i64::from(java_string_hash(name)) ^ self.seed)
    }

    /// Creates a [`LegacyRandom`] XORing `seed` with this factory's seed.
    pub const fn from_seed(&self, seed: i64) -> LegacyRandom {
        LegacyRandom::new(seed ^ self.seed)
    }
}

/// Blackman and Vigna's `xoroshiro128++` 1.0 generator (`XoroshiroRandomSource`).
#[derive(Debug, Clone, PartialEq)]
pub struct XoroshiroRandom {
    lo: u64,
    hi: u64,
    gaussian: MarsagliaGaussian,
}

impl XoroshiroRandom {
    /// Creates a generator by upgrading `seed` with [`Seed128::from_seed`].
    pub const fn new(seed: i64) -> Self {
        Self::from_seed_128(Seed128::from_seed(seed))
    }

    /// Creates a generator directly from a 128-bit state, substituting golden/silver ratio constants for `(0, 0)`.
    pub const fn from_seed_128(seed: Seed128) -> Self {
        let (lo, hi) = if (seed.lo | seed.hi) == 0 {
            (GOLDEN_RATIO_64, SILVER_RATIO_64)
        } else {
            (seed.lo, seed.hi)
        };
        Self {
            lo,
            hi,
            gaussian: MarsagliaGaussian::new(),
        }
    }

    /// Current 128-bit state `(lo, hi)`.
    pub const fn state(&self) -> Seed128 {
        Seed128 {
            lo: self.lo,
            hi: self.hi,
        }
    }

    fn step_u64(lo: &mut u64, hi: &mut u64) -> u64 {
        let s0 = *lo;
        let mut s1 = *hi;
        let result = s0.wrapping_add(s1).rotate_left(17).wrapping_add(s0);
        s1 ^= s0;
        *lo = s0.rotate_left(49) ^ s1 ^ (s1 << 21);
        *hi = s1.rotate_left(28);
        result
    }

    fn sample_double(lo: &mut u64, hi: &mut u64) -> f64 {
        ((Self::step_u64(lo, hi) >> (64 - 53)) as f64) * DOUBLE_UNIT
    }

    /// Next raw 64-bit `xoroshiro128++` word.
    pub fn next_u64(&mut self) -> u64 {
        Self::step_u64(&mut self.lo, &mut self.hi)
    }

    /// Top `bits` (`1..=64`) of the next 64-bit output word (`nextBits`).
    pub fn next_bits(&mut self, bits: u32) -> u64 {
        debug_assert!((1..=64).contains(&bits));
        self.next_u64() >> (64 - bits)
    }

    /// Splits off a new [`XoroshiroRandom`] from two consecutive 64-bit draws.
    pub fn fork(&mut self) -> Self {
        let lo = self.next_u64();
        let hi = self.next_u64();
        Self::from_seed_128(Seed128 { lo, hi })
    }

    /// Splits off a [`XoroshiroPositionalRandom`] from two consecutive 64-bit draws.
    pub fn fork_positional(&mut self) -> XoroshiroPositionalRandom {
        let lo = self.next_u64();
        let hi = self.next_u64();
        XoroshiroPositionalRandom::new(Seed128 { lo, hi })
    }
}

impl RandomSource for XoroshiroRandom {
    fn fork_source(&mut self) -> AnyRandom {
        AnyRandom::Xoroshiro(self.fork())
    }

    fn fork_positional_source(&mut self) -> PositionalRandom {
        PositionalRandom::Xoroshiro(self.fork_positional())
    }

    fn set_seed(&mut self, seed: i64) {
        let state = Self::new(seed);
        self.lo = state.lo;
        self.hi = state.hi;
        self.gaussian.reset();
    }

    fn next_int(&mut self) -> i32 {
        self.next_u64() as i32
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        let bound_u32 = bound as u32;
        let bound_u64 = u64::from(bound_u32);
        let mut sample = u64::from(self.next_int() as u32);
        let mut product = sample * bound_u64;
        let mut low = product & 0xffff_ffff;
        if low < bound_u64 {
            let threshold = u64::from(bound_u32.wrapping_neg() % bound_u32);
            while low < threshold {
                sample = u64::from(self.next_int() as u32);
                product = sample * bound_u64;
                low = product & 0xffff_ffff;
            }
        }
        (product >> 32) as i32
    }

    fn next_long(&mut self) -> i64 {
        self.next_u64() as i64
    }

    fn next_bool(&mut self) -> bool {
        (self.next_u64() & 1) != 0
    }

    fn next_float(&mut self) -> f32 {
        (self.next_bits(24) as f32) * FLOAT_UNIT
    }

    fn next_double(&mut self) -> f64 {
        Self::sample_double(&mut self.lo, &mut self.hi)
    }

    fn next_gaussian(&mut self) -> f64 {
        let (lo, hi) = (&mut self.lo, &mut self.hi);
        self.gaussian.sample(|| Self::sample_double(lo, hi))
    }
}

/// Positional factory backed by [`XoroshiroRandom`] (`XoroshiroPositionalRandomFactory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XoroshiroPositionalRandom {
    seed: Seed128,
}

impl XoroshiroPositionalRandom {
    /// Creates a positional factory from a 128-bit seed.
    pub const fn new(seed: Seed128) -> Self {
        Self { seed }
    }

    /// Creates a [`XoroshiroRandom`] for block coordinates `(x, y, z)`, XORing the coordinate hash into `lo`.
    pub const fn at(&self, x: i32, y: i32, z: i32) -> XoroshiroRandom {
        let pos = position_seed(x, y, z) as u64;
        XoroshiroRandom::from_seed_128(Seed128::new(pos ^ self.seed.lo, self.seed.hi))
    }

    /// Creates a [`XoroshiroRandom`] by XORing the MD5 digest of `name` with this factory's 128-bit seed.
    pub fn from_hash_of(&self, name: &str) -> XoroshiroRandom {
        XoroshiroRandom::from_seed_128(Seed128::from_md5(name).xor(self.seed))
    }

    /// Creates a [`XoroshiroRandom`] by XORing `seed` into both 64-bit halves.
    pub const fn from_seed(&self, seed: i64) -> XoroshiroRandom {
        let word = seed as u64;
        XoroshiroRandom::from_seed_128(Seed128::new(word ^ self.seed.lo, word ^ self.seed.hi))
    }
}

/// Either of vanilla's two [`RandomSource`] implementations, avoiding heap allocation when forking dynamically.
#[derive(Debug, Clone, PartialEq)]
pub enum AnyRandom {
    /// 48-bit Java LCG.
    Legacy(LegacyRandom),
    /// 128-bit `xoroshiro128++`.
    Xoroshiro(XoroshiroRandom),
}

impl RandomSource for AnyRandom {
    fn fork_source(&mut self) -> AnyRandom {
        match self {
            Self::Legacy(rng) => rng.fork_source(),
            Self::Xoroshiro(rng) => rng.fork_source(),
        }
    }

    fn fork_positional_source(&mut self) -> PositionalRandom {
        match self {
            Self::Legacy(rng) => rng.fork_positional_source(),
            Self::Xoroshiro(rng) => rng.fork_positional_source(),
        }
    }

    fn set_seed(&mut self, seed: i64) {
        match self {
            Self::Legacy(rng) => rng.set_seed(seed),
            Self::Xoroshiro(rng) => rng.set_seed(seed),
        }
    }

    fn next_int(&mut self) -> i32 {
        match self {
            Self::Legacy(rng) => rng.next_int(),
            Self::Xoroshiro(rng) => rng.next_int(),
        }
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        match self {
            Self::Legacy(rng) => rng.next_int_bounded(bound),
            Self::Xoroshiro(rng) => rng.next_int_bounded(bound),
        }
    }

    fn next_long(&mut self) -> i64 {
        match self {
            Self::Legacy(rng) => rng.next_long(),
            Self::Xoroshiro(rng) => rng.next_long(),
        }
    }

    fn next_bool(&mut self) -> bool {
        match self {
            Self::Legacy(rng) => rng.next_bool(),
            Self::Xoroshiro(rng) => rng.next_bool(),
        }
    }

    fn next_float(&mut self) -> f32 {
        match self {
            Self::Legacy(rng) => rng.next_float(),
            Self::Xoroshiro(rng) => rng.next_float(),
        }
    }

    fn next_double(&mut self) -> f64 {
        match self {
            Self::Legacy(rng) => rng.next_double(),
            Self::Xoroshiro(rng) => rng.next_double(),
        }
    }

    fn next_gaussian(&mut self) -> f64 {
        match self {
            Self::Legacy(rng) => rng.next_gaussian(),
            Self::Xoroshiro(rng) => rng.next_gaussian(),
        }
    }
}

/// Either of vanilla's two positional random factories (`PositionalRandomFactory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionalRandom {
    /// Factory producing [`LegacyRandom`] instances.
    Legacy(LegacyPositionalRandom),
    /// Factory producing [`XoroshiroRandom`] instances.
    Xoroshiro(XoroshiroPositionalRandom),
}

impl PositionalRandom {
    /// Creates a generator for block coordinates `(x, y, z)`.
    pub const fn at(&self, x: i32, y: i32, z: i32) -> AnyRandom {
        match self {
            Self::Legacy(factory) => AnyRandom::Legacy(factory.at(x, y, z)),
            Self::Xoroshiro(factory) => AnyRandom::Xoroshiro(factory.at(x, y, z)),
        }
    }

    /// Creates a generator hashed from `name`.
    pub fn from_hash_of(&self, name: &str) -> AnyRandom {
        match self {
            Self::Legacy(factory) => AnyRandom::Legacy(factory.from_hash_of(name)),
            Self::Xoroshiro(factory) => AnyRandom::Xoroshiro(factory.from_hash_of(name)),
        }
    }

    /// Creates a generator seeded from `seed`.
    pub const fn from_seed(&self, seed: i64) -> AnyRandom {
        match self {
            Self::Legacy(factory) => AnyRandom::Legacy(factory.from_seed(seed)),
            Self::Xoroshiro(factory) => AnyRandom::Xoroshiro(factory.from_seed(seed)),
        }
    }
}

/// Named world-seed random sequence (`RandomSequence`), persisted in `random_sequences.dat`.
#[derive(Debug, Clone, PartialEq)]
pub struct RandomSequence {
    source: XoroshiroRandom,
}

impl RandomSequence {
    /// Creates a sequence from `world_seed` and an optional resource location (such as `"minecraft:chests/simple_dungeon"`).
    pub fn new(world_seed: i64, location: Option<&str>) -> Self {
        let unmixed = Seed128::upgrade_unmixed(world_seed);
        let unmixed = match location {
            Some(name) => unmixed.xor(Seed128::from_md5(name)),
            None => unmixed,
        };
        Self {
            source: XoroshiroRandom::from_seed_128(unmixed.mixed()),
        }
    }

    /// Restores a sequence from its saved 128-bit state.
    pub const fn from_seed_128(seed: Seed128) -> Self {
        Self {
            source: XoroshiroRandom::from_seed_128(seed),
        }
    }

    /// Underlying [`XoroshiroRandom`] generator.
    pub fn random(&mut self) -> &mut XoroshiroRandom {
        &mut self.source
    }

    /// Current 128-bit state for saving.
    pub const fn state(&self) -> Seed128 {
        self.source.state()
    }
}

/// Wrapper adding vanilla's chunk, feature, carver and structure seeding formulas (`WorldgenRandom`).
#[derive(Debug, Clone, PartialEq)]
pub struct WorldgenRandom<R = LegacyRandom> {
    inner: R,
}

impl<R> WorldgenRandom<R> {
    /// Wraps an inner random generator.
    pub const fn new(inner: R) -> Self {
        Self { inner }
    }

    /// Unwraps the inner random generator.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl WorldgenRandom<LegacyRandom> {
    /// Creates a [`LegacyRandom`] seeded with vanilla's slime-chunk formula (vanilla uses `salt = 0x3ad8025f`).
    pub const fn seed_slime_chunk(
        chunk_x: i32,
        chunk_z: i32,
        level_seed: i64,
        salt: i64,
    ) -> LegacyRandom {
        let seed = level_seed
            .wrapping_add(chunk_x.wrapping_mul(chunk_x).wrapping_mul(0x004c_1906) as i64)
            .wrapping_add(chunk_x.wrapping_mul(0x005a_c0db) as i64)
            .wrapping_add((chunk_z.wrapping_mul(chunk_z) as i64).wrapping_mul(0x0043_07a7))
            .wrapping_add(chunk_z.wrapping_mul(0x0005_f24f) as i64)
            ^ salt;
        LegacyRandom::new(seed)
    }
}

impl<R: RandomSource> WorldgenRandom<R> {
    /// Seeds decoration generation for a chunk whose minimum block coordinates are `(min_block_x, min_block_z)`,
    /// returning the computed decoration seed.
    pub fn set_decoration_seed(
        &mut self,
        level_seed: i64,
        min_block_x: i32,
        min_block_z: i32,
    ) -> i64 {
        self.set_seed(level_seed);
        let a = self.next_long() | 1;
        let b = self.next_long() | 1;
        let decoration_seed = (i64::from(min_block_x).wrapping_mul(a)
            ^ i64::from(min_block_z).wrapping_mul(b))
            ^ level_seed;
        self.set_seed(decoration_seed);
        decoration_seed
    }

    /// Seeds an individual placed feature from `decoration_seed`, feature `index` and generation `step`.
    pub fn set_feature_seed(&mut self, decoration_seed: i64, index: i32, step: i32) {
        let seed = decoration_seed
            .wrapping_add(i64::from(index))
            .wrapping_add(i64::from(10_000_i32.wrapping_mul(step)));
        self.set_seed(seed);
    }

    /// Seeds large terrain features such as cave and canyon carvers at `(chunk_x, chunk_z)`.
    pub fn set_large_feature_seed(&mut self, level_seed: i64, chunk_x: i32, chunk_z: i32) {
        self.set_seed(level_seed);
        let a = self.next_long();
        let b = self.next_long();
        let seed =
            (i64::from(chunk_x).wrapping_mul(a) ^ i64::from(chunk_z).wrapping_mul(b)) ^ level_seed;
        self.set_seed(seed);
    }

    /// Seeds regional structure placement (`RandomSpreadStructurePlacement`) at `(region_x, region_z)` with `salt`.
    pub fn set_large_feature_with_salt(
        &mut self,
        level_seed: i64,
        region_x: i32,
        region_z: i32,
        salt: i32,
    ) {
        let seed = i64::from(region_x)
            .wrapping_mul(341_873_128_712)
            .wrapping_add(i64::from(region_z).wrapping_mul(132_897_987_541))
            .wrapping_add(level_seed)
            .wrapping_add(i64::from(salt));
        self.set_seed(seed);
    }
}

impl<R: RandomSource> RandomSource for WorldgenRandom<R> {
    fn fork_source(&mut self) -> AnyRandom {
        self.inner.fork_source()
    }

    fn fork_positional_source(&mut self) -> PositionalRandom {
        self.inner.fork_positional_source()
    }

    fn set_seed(&mut self, seed: i64) {
        self.inner.set_seed(seed);
    }

    fn next_int(&mut self) -> i32 {
        self.inner.next_int()
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        self.inner.next_int_bounded(bound)
    }

    fn next_long(&mut self) -> i64 {
        self.inner.next_long()
    }

    fn next_bool(&mut self) -> bool {
        self.inner.next_bool()
    }

    fn next_float(&mut self) -> f32 {
        self.inner.next_float()
    }

    fn next_double(&mut self) -> f64 {
        self.inner.next_double()
    }

    fn next_gaussian(&mut self) -> f64 {
        self.inner.next_gaussian()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_random_matches_java_util_random_sequence_and_edge_cases() {
        let mut rng = LegacyRandom::new(12_345);
        assert_eq!(rng.next_int(), 1_553_932_502);
        assert_eq!(rng.next_int(), -2_090_749_135);
        // Includes negative lower 32 bits in `nextLong()` sign-extension addition.
        assert_eq!(rng.next_long(), -1_236_052_134_575_208_584);
        assert_eq!(rng.next_int_bounded(16), 13);
        assert_eq!(rng.next_int_bounded(100), 84);
        // Large non-power-of-two bound exercises 32-bit signed overflow rejection without panicking.
        for _ in 0..32 {
            let v = rng.next_int_bounded(1_500_000_000);
            assert!((0..1_500_000_000).contains(&v));
        }
        assert!(rng.next_float() >= 0.0 && rng.next_float() < 1.0);
        assert!(rng.next_double() >= 0.0 && rng.next_double() < 1.0);

        // Marsaglia polar rejection consumes the exact same LCG draws as Java 25 over 200,000 samples.
        let mut gauss_rng = LegacyRandom::new(12_345);
        for _ in 0..200_000 {
            let _ = gauss_rng.next_gaussian();
        }
        assert_eq!(gauss_rng.next_long() as u64, 0xbcde_8b37_a6b5_40a2);

        // `set_seed` clears the cached second Gaussian sample.
        let mut a = LegacyRandom::new(99);
        let first = a.next_gaussian();
        a.set_seed(99);
        assert_eq!(a.next_gaussian().to_bits(), first.to_bits());
    }

    #[test]
    fn xoroshiro128_plus_plus_matches_reference_vectors_and_zero_fallback() {
        // Blackman & Vigna xoroshiro128++ 1.0 reference outputs from state (1, 2).
        let mut rng = XoroshiroRandom::from_seed_128(Seed128::new(1, 2));
        assert_eq!(rng.next_u64(), 0x0000_0000_0006_0001);
        assert_eq!(rng.next_u64(), 0x0002_60c0_0066_0007);
        assert_eq!(rng.next_u64(), 0x180a_cc04_7186_06d3);
        assert_eq!(rng.next_u64(), 0x9e22_6d35_036f_c4c7);

        // Zero state falls back to (GOLDEN_RATIO_64, SILVER_RATIO_64) rather than getting stuck at zero.
        let zero = XoroshiroRandom::from_seed_128(Seed128::new(0, 0));
        assert_eq!(zero.state(), Seed128::new(GOLDEN_RATIO_64, SILVER_RATIO_64));

        // Seed 0 upgraded via Silver/Golden ratios + Stafford Mix13.
        let mut seeded = XoroshiroRandom::new(0);
        assert_eq!(
            seeded.state(),
            Seed128::new(0x3564_b439_cd1e_1f16, 0x63cf_c62a_2b09_7592)
        );
        for _ in 0..64 {
            let v = seeded.next_int_bounded(1_500_000_000);
            assert!((0..1_500_000_000).contains(&v));
        }
    }

    #[test]
    fn md5_seeding_positional_factories_and_random_sequences_are_deterministic() {
        // RFC 1321 MD5("abc") = 900150983cd24fb0 d6963f7d28e17f72.
        assert_eq!(
            Seed128::from_md5("abc"),
            Seed128::new(0x9001_5098_3cd2_4fb0, 0xd696_3f7d_28e1_7f72)
        );

        let mut root = XoroshiroRandom::new(42);
        let factory = root.fork_positional();
        let mut at_a = factory.at(16, 64, -32);
        let mut at_b = factory.at(16, 64, -32);
        let mut at_c = factory.at(16, 65, -32);
        assert_eq!(at_a.next_long(), at_b.next_long());
        assert_ne!(at_b.next_long(), at_c.next_long());

        let mut by_name_a = factory.from_hash_of("minecraft:ore_diamond");
        let mut by_name_b = factory.from_hash_of("minecraft:ore_diamond");
        assert_eq!(by_name_a.next_long(), by_name_b.next_long());

        let mut seq_a = RandomSequence::new(123_456, Some("minecraft:chests/simple_dungeon"));
        let saved = seq_a.state();
        let draw_a = seq_a.random().next_long();
        let mut seq_b = RandomSequence::from_seed_128(saved);
        assert_eq!(seq_b.random().next_long(), draw_a);
    }

    #[test]
    fn worldgen_random_decoration_feature_carver_region_and_slime_seeds_work() {
        let mut slime_zero = WorldgenRandom::seed_slime_chunk(0, 0, 0, 0x3ad8_025f);
        assert_eq!(slime_zero.next_int_bounded(10), 7);

        // Large coordinates wrap in 32-bit int arithmetic as in Java without panicking.
        let mut distant_slime =
            WorldgenRandom::seed_slime_chunk(100_000, -100_000, -999, 0x3ad8_025f);
        assert_eq!(distant_slime.next_long(), -1_259_707_904_227_432_742);

        let mut wg = WorldgenRandom::new(XoroshiroRandom::new(0));
        let dec = wg.set_decoration_seed(8_675_309, 160, -320);
        wg.set_feature_seed(dec, 3, 7);
        let sample = wg.next_long();

        let mut wg2 = WorldgenRandom::new(XoroshiroRandom::new(0));
        assert_eq!(wg2.set_decoration_seed(8_675_309, 160, -320), dec);
        wg2.set_feature_seed(dec, 3, 7);
        assert_eq!(wg2.next_long(), sample);
    }

    #[test]
    #[should_panic(expected = "bound must be positive")]
    fn legacy_rejects_non_positive_bound() {
        let mut rng = LegacyRandom::new(1);
        rng.next_int_bounded(0);
    }

    #[test]
    #[should_panic(expected = "bound must be positive")]
    fn xoroshiro_rejects_non_positive_bound() {
        let mut rng = XoroshiroRandom::new(1);
        rng.next_int_bounded(-5);
    }
}
