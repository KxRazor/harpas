//! Differential checks of `harpas_core::math`, `harpas_core::random` and `harpas_core::noise` against a Java
//! runtime (`javac`/`java`).
//!
//! Uses only standard Java SE classes (`java.util.Random`, `java.lang.Math`, `java.lang.StrictMath`,
//! `java.security.MessageDigest`), never Mojang's files or code.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use harpas_core::math;
use harpas_core::noise::{
    BlendedNoise, ImprovedNoise, NormalNoise, PerlinNoise, PerlinSimplexNoise, SimplexNoise,
};
use harpas_core::random::{
    LegacyRandom, RandomSequence, RandomSource, Seed128, WorldgenRandom, XoroshiroRandom,
};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-core-java-parity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("temporary folder");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mix(hash: &mut u64, word: u64) {
    *hash = (*hash ^ word).wrapping_mul(FNV_PRIME);
}

fn canonical_f32_bits(value: f32) -> u32 {
    if value.is_nan() {
        0x7fc0_0000
    } else {
        value.to_bits()
    }
}

fn canonical_f64_bits(value: f64) -> u64 {
    if value.is_nan() {
        0x7ff8_0000_0000_0000
    } else {
        value.to_bits()
    }
}

const JAVA_PROBE: &str = r#"
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Locale;
import java.util.Random;

public final class Probe {
    private static final long FNV_OFFSET = 0xcbf29ce484222325L;
    private static final long FNV_PRIME = 0x00000100000001b3L;
    private static final long GOLDEN_64 = 0x9e3779b97f4a7c15L;
    private static final long SILVER_64 = 0x6a09e667f3bcc909L;

    private static long mix(long h, long word) {
        return (h ^ word) * FNV_PRIME;
    }

    private static void emit(String name, long h) {
        System.out.printf(Locale.ROOT, "%s=%016x%n", name, h);
    }

    private static int mthFloor(float v) {
        int i = (int) v;
        return v < (float) i ? i - 1 : i;
    }

    private static int mthFloor(double v) {
        int i = (int) v;
        return v < (double) i ? i - 1 : i;
    }

    private static long mthLfloor(double v) {
        long i = (long) v;
        return v < (double) i ? i - 1L : i;
    }

    private static int mthCeil(float v) {
        int i = (int) v;
        return v > (float) i ? i + 1 : i;
    }

    private static int mthCeil(double v) {
        int i = (int) v;
        return v > (double) i ? i + 1 : i;
    }

    private static float wrapDegrees(float deg) {
        float f = deg % 360.0f;
        if (f >= 180.0f) f -= 360.0f;
        if (f < -180.0f) f += 360.0f;
        return f;
    }

    private static double wrapDegrees(double deg) {
        double d = deg % 360.0;
        if (d >= 180.0) d -= 360.0;
        if (d < -180.0) d += 360.0;
        return d;
    }

    private static long positionSeed(int x, int y, int z) {
        long l = (long) (x * 3129871) ^ (long) z * 116129781L ^ (long) y;
        l = l * l * 42317861L + l * 11L;
        return l >> 16;
    }

    private static long mixStafford13(long z) {
        z = (z ^ (z >>> 30)) * 0xbf58476d1ce4e5b9L;
        z = (z ^ (z >>> 27)) * 0x94d049bb133111ebL;
        return z ^ (z >>> 31);
    }

    private static long[] md5Words(String s) throws Exception {
        byte[] b = MessageDigest.getInstance("MD5").digest(s.getBytes(StandardCharsets.UTF_8));
        long lo = 0L, hi = 0L;
        for (int i = 0; i < 8; i++) lo = (lo << 8) | (b[i] & 0xffL);
        for (int i = 8; i < 16; i++) hi = (hi << 8) | (b[i] & 0xffL);
        return new long[] { lo, hi };
    }

    private static final class Xoro {
        long lo, hi;

        Xoro(long lo, long hi) {
            if ((lo | hi) == 0L) {
                this.lo = GOLDEN_64;
                this.hi = SILVER_64;
            } else {
                this.lo = lo;
                this.hi = hi;
            }
        }

        static Xoro fromSeed(long seed) {
            long l = seed ^ SILVER_64;
            long m = l + GOLDEN_64;
            return new Xoro(mixStafford13(l), mixStafford13(m));
        }

        long nextLong() {
            long s0 = lo, s1 = hi;
            long r = Long.rotateLeft(s0 + s1, 17) + s0;
            s1 ^= s0;
            lo = Long.rotateLeft(s0, 49) ^ s1 ^ (s1 << 21);
            hi = Long.rotateLeft(s1, 28);
            return r;
        }

        int nextInt() {
            return (int) nextLong();
        }

        int nextInt(int bound) {
            long l = Integer.toUnsignedLong(nextInt());
            long m = l * (long) bound;
            long n = m & 0xffffffffL;
            if (n < (long) bound) {
                int t = Integer.remainderUnsigned(~bound + 1, bound);
                while (n < (long) t) {
                    l = Integer.toUnsignedLong(nextInt());
                    m = l * (long) bound;
                    n = m & 0xffffffffL;
                }
            }
            return (int) (m >> 32);
        }

        boolean nextBoolean() {
            return (nextLong() & 1L) != 0L;
        }

        float nextFloat() {
            return (float) (nextLong() >>> 40) * 5.9604645e-8f;
        }

        double nextDouble() {
            return (double) (nextLong() >>> 11) * 0x1.0p-53;
        }
    }

    private interface Rng {
        int nextInt();
        int nextInt(int bound);
        long nextLong();
        double nextDouble();
        PosRng forkPositional();
        default void consumeCount(int count) {
            for (int i = 0; i < count; i++) nextInt();
        }
    }

    private interface PosRng {
        Rng fromHashOf(String name) throws Exception;
    }

    private static final class LegacyRng implements Rng {
        private final Random inner;

        LegacyRng(long seed) {
            this.inner = new Random(seed);
        }

        @Override public int nextInt() { return inner.nextInt(); }
        @Override public int nextInt(int bound) { return inner.nextInt(bound); }
        @Override public long nextLong() { return inner.nextLong(); }
        @Override public double nextDouble() { return inner.nextDouble(); }
        @Override public PosRng forkPositional() {
            long forkSeed = nextLong();
            return name -> new LegacyRng(((long) name.hashCode()) ^ forkSeed);
        }
    }

    private static final class XoroRng implements Rng {
        private final Xoro inner;

        XoroRng(Xoro inner) {
            this.inner = inner;
        }

        static XoroRng fromSeed(long seed) {
            return new XoroRng(Xoro.fromSeed(seed));
        }

        @Override public int nextInt() { return inner.nextInt(); }
        @Override public int nextInt(int bound) { return inner.nextInt(bound); }
        @Override public long nextLong() { return inner.nextLong(); }
        @Override public double nextDouble() { return inner.nextDouble(); }
        @Override public PosRng forkPositional() {
            long fLo = inner.nextLong();
            long fHi = inner.nextLong();
            return name -> {
                long[] md5 = md5Words(name);
                return new XoroRng(new Xoro(md5[0] ^ fLo, md5[1] ^ fHi));
            };
        }
    }

    private static final int[][] GRAD = {
        { 1, 1, 0 }, { -1, 1, 0 }, { 1, -1, 0 }, { -1, -1, 0 },
        { 1, 0, 1 }, { -1, 0, 1 }, { 1, 0, -1 }, { -1, 0, -1 },
        { 0, 1, 1 }, { 0, -1, 1 }, { 0, 1, -1 }, { 0, -1, -1 },
        { 1, 1, 0 }, { 0, -1, 1 }, { -1, 1, 0 }, { 0, -1, -1 }
    };

    private static double dot3(int[] g, double x, double y, double z) {
        return g[0] * x + g[1] * y + g[2] * z;
    }

    private static double smoothstep(double x) {
        return x * x * x * (x * (x * 6.0 - 15.0) + 10.0);
    }

    private static double lerp(double d, double a, double b) {
        return a + d * (b - a);
    }

    private static double lerp2(double dx, double dy, double v00, double v10, double v01, double v11) {
        return lerp(dy, lerp(dx, v00, v10), lerp(dx, v01, v11));
    }

    private static double lerp3(
        double dx, double dy, double dz,
        double v000, double v100, double v010, double v110,
        double v001, double v101, double v011, double v111
    ) {
        return lerp(dz, lerp2(dx, dy, v000, v100, v010, v110), lerp2(dx, dy, v001, v101, v011, v111));
    }

    private static double clampedLerp(double a, double b, double d) {
        if (d < 0.0) return a;
        if (d > 1.0) return b;
        return lerp(d, a, b);
    }

    private static double perlinWrap(double v) {
        return v - (double) mthLfloor(v / 3.3554432E7 + 0.5) * 3.3554432E7;
    }

    private static final class Improved {
        final byte[] p = new byte[256];
        final double xo, yo, zo;

        Improved(Rng rng) {
            xo = rng.nextDouble() * 256.0;
            yo = rng.nextDouble() * 256.0;
            zo = rng.nextDouble() * 256.0;
            for (int i = 0; i < 256; i++) p[i] = (byte) i;
            for (int i = 0; i < 256; i++) {
                int j = rng.nextInt(256 - i);
                byte tmp = p[i];
                p[i] = p[i + j];
                p[i + j] = tmp;
            }
        }

        int perm(int i) {
            return p[i & 255] & 255;
        }

        double noise(double x, double y, double z, double yScale, double yMax) {
            double sx = x + xo;
            double sy = y + yo;
            double sz = z + zo;
            int gx = mthFloor(sx);
            int gy = mthFloor(sy);
            int gz = mthFloor(sz);
            double lx = sx - (double) gx;
            double ly = sy - (double) gy;
            double lz = sz - (double) gz;
            double fudge;
            if (yScale != 0.0) {
                double clamped = yMax >= 0.0 && yMax < ly ? yMax : ly;
                fudge = (double) mthFloor(clamped / yScale + (double) 1.0E-7F) * yScale;
            } else {
                fudge = 0.0;
            }
            int i = perm(gx);
            int j = perm(gx + 1);
            int k = perm(i + gy);
            int l = perm(i + gy + 1);
            int m = perm(j + gy);
            int n = perm(j + gy + 1);
            double wy = ly - fudge;
            double d000 = dot3(GRAD[perm(k + gz) & 15], lx, wy, lz);
            double d100 = dot3(GRAD[perm(m + gz) & 15], lx - 1.0, wy, lz);
            double d010 = dot3(GRAD[perm(l + gz) & 15], lx, wy - 1.0, lz);
            double d110 = dot3(GRAD[perm(n + gz) & 15], lx - 1.0, wy - 1.0, lz);
            double d001 = dot3(GRAD[perm(k + gz + 1) & 15], lx, wy, lz - 1.0);
            double d101 = dot3(GRAD[perm(m + gz + 1) & 15], lx - 1.0, wy, lz - 1.0);
            double d011 = dot3(GRAD[perm(l + gz + 1) & 15], lx, wy - 1.0, lz - 1.0);
            double d111 = dot3(GRAD[perm(n + gz + 1) & 15], lx - 1.0, wy - 1.0, lz - 1.0);
            return lerp3(smoothstep(lx), smoothstep(ly), smoothstep(lz), d000, d100, d010, d110, d001, d101, d011, d111);
        }
    }

    private static final class Perlin {
        final Improved[] levels;
        final double[] amplitudes;
        final double lowestFreqValueFactor;
        final double lowestFreqInputFactor;
        final double maxValue;

        Perlin(Improved[] levels, int firstOctave, double[] amplitudes) {
            this.levels = levels;
            this.amplitudes = amplitudes;
            int n = amplitudes.length;
            this.lowestFreqInputFactor = Math.pow(2.0, firstOctave);
            this.lowestFreqValueFactor = Math.pow(2.0, n - 1) / (Math.pow(2.0, n) - 1.0);
            this.maxValue = edgeValue(2.0);
        }

        static Perlin createModern(Rng rng, int firstOctave, double[] amplitudes) throws Exception {
            PosRng pos = rng.forkPositional();
            Improved[] levels = new Improved[amplitudes.length];
            for (int i = 0; i < amplitudes.length; i++) {
                if (amplitudes[i] != 0.0) {
                    int oct = firstOctave + i;
                    levels[i] = new Improved(pos.fromHashOf("octave_" + oct));
                }
            }
            return new Perlin(levels, firstOctave, amplitudes);
        }

        static Perlin createLegacy(Rng rng, int firstOctave, double[] amplitudes) {
            int n = amplitudes.length;
            int zeroIdx = -firstOctave;
            Improved[] levels = new Improved[n];
            Improved zero = new Improved(rng);
            if (zeroIdx >= 0 && zeroIdx < n && amplitudes[zeroIdx] != 0.0) {
                levels[zeroIdx] = zero;
            }
            for (int k = zeroIdx - 1; k >= 0; k--) {
                if (k < n && amplitudes[k] != 0.0) {
                    levels[k] = new Improved(rng);
                } else {
                    rng.consumeCount(262);
                }
            }
            return new Perlin(levels, firstOctave, amplitudes);
        }

        double edgeValue(double x) {
            double total = 0.0;
            double vf = lowestFreqValueFactor;
            for (int i = 0; i < levels.length; i++) {
                if (levels[i] != null) {
                    total += amplitudes[i] * x * vf;
                }
                vf /= 2.0;
            }
            return total;
        }

        double maxBrokenValue(double yScale) {
            return edgeValue(yScale + 2.0);
        }

        Improved getOctaveNoise(int i) {
            return levels[levels.length - 1 - i];
        }

        double getValue(double x, double y, double z, double yScale, double yMax, boolean useOrigin) {
            double total = 0.0;
            double inf = lowestFreqInputFactor;
            double vf = lowestFreqValueFactor;
            for (int i = 0; i < levels.length; i++) {
                Improved lv = levels[i];
                if (lv != null) {
                    double s = lv.noise(
                        perlinWrap(x * inf),
                        useOrigin ? -lv.yo : perlinWrap(y * inf),
                        perlinWrap(z * inf),
                        yScale * inf,
                        yMax * inf
                    );
                    total += amplitudes[i] * s * vf;
                }
                inf *= 2.0;
                vf /= 2.0;
            }
            return total;
        }
    }

    private static final class Normal {
        static final double INPUT_FACTOR = 1.0181268882175227;
        final Perlin first, second;
        final double valueFactor, maxValue;

        Normal(Perlin first, Perlin second, double[] amplitudes) {
            this.first = first;
            this.second = second;
            int min = Integer.MAX_VALUE;
            int max = Integer.MIN_VALUE;
            for (int i = 0; i < amplitudes.length; i++) {
                if (amplitudes[i] != 0.0) {
                    min = Math.min(min, i);
                    max = Math.max(max, i);
                }
            }
            int span = min <= max ? (max - min) : 0;
            double expected = 0.1 * (1.0 + 1.0 / (double) (span + 1));
            this.valueFactor = (1.0 / 6.0) / expected;
            this.maxValue = (first.maxValue + second.maxValue) * this.valueFactor;
        }

        double getValue(double x, double y, double z) {
            double x2 = x * INPUT_FACTOR;
            double y2 = y * INPUT_FACTOR;
            double z2 = z * INPUT_FACTOR;
            return (first.getValue(x, y, z, 0.0, 0.0, false) + second.getValue(x2, y2, z2, 0.0, 0.0, false)) * valueFactor;
        }
    }

    private static final class Blended {
        final Perlin minLimit, maxLimit, main;
        final double xzMul, yMul, xzFactor, yFactor, smearScaleMultiplier, maxValue;

        Blended(Rng rng, double xzScale, double yScale, double xzFactor, double yFactor, double smearScaleMultiplier) {
            double[] ones16 = new double[16];
            java.util.Arrays.fill(ones16, 1.0);
            double[] ones8 = new double[8];
            java.util.Arrays.fill(ones8, 1.0);
            this.minLimit = Perlin.createLegacy(rng, -15, ones16);
            this.maxLimit = Perlin.createLegacy(rng, -15, ones16);
            this.main = Perlin.createLegacy(rng, -7, ones8);
            this.xzMul = 684.412 * xzScale;
            this.yMul = 684.412 * yScale;
            this.xzFactor = xzFactor;
            this.yFactor = yFactor;
            this.smearScaleMultiplier = smearScaleMultiplier;
            this.maxValue = this.minLimit.maxBrokenValue(this.yMul);
        }

        double sample(double x, double y, double z) {
            double lx = x * xzMul;
            double ly = y * yMul;
            double lz = z * xzMul;
            double mx = lx / xzFactor;
            double my = ly / yFactor;
            double mz = lz / xzFactor;
            double smearLimit = yMul * smearScaleMultiplier;
            double smearMain = smearLimit / yFactor;

            double mainVal = 0.0;
            double scale = 1.0;
            for (int i = 0; i < 8; i++) {
                Improved oct = main.getOctaveNoise(i);
                if (oct != null) {
                    mainVal += oct.noise(perlinWrap(mx * scale), perlinWrap(my * scale), perlinWrap(mz * scale), smearMain * scale, my * scale) / scale;
                }
                scale /= 2.0;
            }

            double blend = (mainVal / 10.0 + 1.0) / 2.0;
            boolean skipMax = blend <= 0.0;
            boolean skipMin = blend >= 1.0;
            double minVal = 0.0;
            double maxVal = 0.0;
            scale = 1.0;
            for (int i = 0; i < 16; i++) {
                double wx = perlinWrap(lx * scale);
                double wy = perlinWrap(ly * scale);
                double wz = perlinWrap(lz * scale);
                double ySmear = smearLimit * scale;
                if (!skipMin) {
                    Improved oct = minLimit.getOctaveNoise(i);
                    if (oct != null) {
                        minVal += oct.noise(wx, wy, wz, ySmear, ly * scale) / scale;
                    }
                }
                if (!skipMax) {
                    Improved oct = maxLimit.getOctaveNoise(i);
                    if (oct != null) {
                        maxVal += oct.noise(wx, wy, wz, ySmear, ly * scale) / scale;
                    }
                }
                scale /= 2.0;
            }
            return clampedLerp(minVal / 512.0, maxVal / 512.0, blend) / 128.0;
        }
    }

    private static final class Simplex {
        static final double SQRT_3 = Math.sqrt(3.0);
        static final double F2 = 0.5 * (SQRT_3 - 1.0);
        static final double G2 = (3.0 - SQRT_3) / 6.0;
        static final double F3 = 1.0 / 3.0;
        static final double G3 = 1.0 / 6.0;

        final int[] p = new int[512];
        final double xo, yo, zo;

        Simplex(Rng rng) {
            xo = rng.nextDouble() * 256.0;
            yo = rng.nextDouble() * 256.0;
            zo = rng.nextDouble() * 256.0;
            for (int i = 0; i < 256; i++) p[i] = i;
            for (int i = 0; i < 256; i++) {
                int j = rng.nextInt(256 - i);
                int tmp = p[i];
                p[i] = p[i + j];
                p[i + j] = tmp;
            }
            for (int i = 0; i < 256; i++) p[i + 256] = p[i];
        }

        static double corner(int gi, double x, double y, double z, double falloff) {
            double t = falloff - x * x - y * y - z * z;
            if (t < 0.0) return 0.0;
            t *= t;
            return t * t * dot3(GRAD[gi], x, y, z);
        }

        double getValue2D(double x, double y) {
            double s = (x + y) * F2;
            int i = mthFloor(x + s);
            int j = mthFloor(y + s);
            double t = (double) (i + j) * G2;
            double x0 = x - ((double) i - t);
            double y0 = y - ((double) j - t);
            int i1, j1;
            if (x0 > y0) { i1 = 1; j1 = 0; } else { i1 = 0; j1 = 1; }
            double x1 = x0 - (double) i1 + G2;
            double y1 = y0 - (double) j1 + G2;
            double x2 = x0 - 1.0 + 2.0 * G2;
            double y2 = y0 - 1.0 + 2.0 * G2;
            int ii = i & 255;
            int jj = j & 255;
            int gi0 = p[ii + p[jj]] % 12;
            int gi1 = p[ii + i1 + p[jj + j1]] % 12;
            int gi2 = p[ii + 1 + p[jj + 1]] % 12;
            double n0 = corner(gi0, x0, y0, 0.0, 0.5);
            double n1 = corner(gi1, x1, y1, 0.0, 0.5);
            double n2 = corner(gi2, x2, y2, 0.0, 0.5);
            return 70.0 * (n0 + n1 + n2);
        }

        double getValue3D(double x, double y, double z) {
            double s = (x + y + z) * F3;
            int i = mthFloor(x + s);
            int j = mthFloor(y + s);
            int k = mthFloor(z + s);
            double t = (double) (i + j + k) * G3;
            double x0 = x - ((double) i - t);
            double y0 = y - ((double) j - t);
            double z0 = z - ((double) k - t);
            int i1, j1, k1, i2, j2, k2;
            if (x0 >= y0) {
                if (y0 >= z0) { i1 = 1; j1 = 0; k1 = 0; i2 = 1; j2 = 1; k2 = 0; }
                else if (x0 >= z0) { i1 = 1; j1 = 0; k1 = 0; i2 = 1; j2 = 0; k2 = 1; }
                else { i1 = 0; j1 = 0; k1 = 1; i2 = 1; j2 = 0; k2 = 1; }
            } else {
                if (y0 < z0) { i1 = 0; j1 = 0; k1 = 1; i2 = 0; j2 = 1; k2 = 1; }
                else if (x0 < z0) { i1 = 0; j1 = 1; k1 = 0; i2 = 0; j2 = 1; k2 = 1; }
                else { i1 = 0; j1 = 1; k1 = 0; i2 = 1; j2 = 1; k2 = 0; }
            }
            double x1 = x0 - (double) i1 + G3;
            double y1 = y0 - (double) j1 + G3;
            double z1 = z0 - (double) k1 + G3;
            double x2 = x0 - (double) i2 + 2.0 * G3;
            double y2 = y0 - (double) j2 + 2.0 * G3;
            double z2 = z0 - (double) k2 + 2.0 * G3;
            double x3 = x0 - 1.0 + 3.0 * G3;
            double y3 = y0 - 1.0 + 3.0 * G3;
            double z3 = z0 - 1.0 + 3.0 * G3;
            int ii = i & 255;
            int jj = j & 255;
            int kk = k & 255;
            int gi0 = p[ii + p[jj + p[kk]]] % 12;
            int gi1 = p[ii + i1 + p[jj + j1 + p[kk + k1]]] % 12;
            int gi2 = p[ii + i2 + p[jj + j2 + p[kk + k2]]] % 12;
            int gi3 = p[ii + 1 + p[jj + 1 + p[kk + 1]]] % 12;
            double n0 = corner(gi0, x0, y0, z0, 0.6);
            double n1 = corner(gi1, x1, y1, z1, 0.6);
            double n2 = corner(gi2, x2, y2, z2, 0.6);
            double n3 = corner(gi3, x3, y3, z3, 0.6);
            return 32.0 * (n0 + n1 + n2 + n3);
        }
    }

    private static final class PerlinSimplex {
        final Simplex[] levels;
        final double highestFreqValueFactor;
        final double highestFreqInputFactor;

        PerlinSimplex(Rng rng, int[] octaves) {
            java.util.TreeSet<Integer> set = new java.util.TreeSet<>();
            for (int o : octaves) set.add(o);
            int low = -set.first();
            int high = set.last();
            int total = low + high + 1;
            Simplex first = new Simplex(rng);
            int zeroIdx = high;
            this.levels = new Simplex[total];
            if (zeroIdx >= 0 && zeroIdx < total && set.contains(0)) {
                this.levels[zeroIdx] = first;
            }
            for (int idx = zeroIdx + 1; idx < total; idx++) {
                if (idx >= 0) {
                    int oct = zeroIdx - idx;
                    if (set.contains(oct)) {
                        this.levels[idx] = new Simplex(rng);
                    } else {
                        rng.consumeCount(262);
                    }
                }
            }
            if (high > 0) {
                long posSeed = (long) (first.getValue3D(first.xo, first.yo, first.zo) * (double) Long.MAX_VALUE);
                Rng hiRng = new LegacyRng(posSeed);
                for (int idx = zeroIdx - 1; idx >= 0; idx--) {
                    int oct = zeroIdx - idx;
                    if (idx < total && set.contains(oct)) {
                        this.levels[idx] = new Simplex(hiRng);
                    } else {
                        hiRng.consumeCount(262);
                    }
                }
            }
            this.highestFreqInputFactor = Math.pow(2.0, high);
            this.highestFreqValueFactor = 1.0 / (Math.pow(2.0, total) - 1.0);
        }

        double getValue(double x, double y, boolean useOffsets) {
            double total = 0.0;
            double inf = highestFreqInputFactor;
            double vf = highestFreqValueFactor;
            for (Simplex s : levels) {
                if (s != null) {
                    total += s.getValue2D(
                        x * inf + (useOffsets ? s.xo : 0.0),
                        y * inf + (useOffsets ? s.yo : 0.0)
                    ) * vf;
                }
                inf /= 2.0;
                vf *= 2.0;
            }
            return total;
        }
    }

    public static void main(String[] args) throws Exception {
        int[] i32s = {
            Integer.MIN_VALUE, Integer.MIN_VALUE + 1, -1_000_000_000, -100, -7, -4, -1,
            0, 1, 4, 7, 100, 1_000_000_000, Integer.MAX_VALUE - 1, Integer.MAX_VALUE
        };
        long h = FNV_OFFSET;
        for (int x : i32s) {
            for (int y : i32s) {
                if (y == 0) continue;
                h = mix(h, x / y);
                h = mix(h, x % y);
                h = mix(h, Math.floorDiv(x, y));
                h = mix(h, Math.floorMod(x, y));
            }
        }
        emit("int_div_mod", h);

        long[] i64s = {
            Long.MIN_VALUE, Long.MIN_VALUE + 1, -5_000_000_000L, -7L, -4L, -1L,
            0L, 1L, 4L, 7L, 5_000_000_000L, Long.MAX_VALUE - 1, Long.MAX_VALUE
        };
        h = FNV_OFFSET;
        for (long x : i64s) {
            for (long y : i64s) {
                if (y == 0L) continue;
                h = mix(h, x / y);
                h = mix(h, x % y);
                h = mix(h, Math.floorDiv(x, y));
                h = mix(h, Math.floorMod(x, y));
            }
            for (int yi : new int[] { -16, -7, -1, 1, 7, 16 }) {
                h = mix(h, Math.floorMod(x, yi));
            }
        }
        emit("long_div_mod", h);

        int[] shifts = { Integer.MIN_VALUE, -65, -33, -32, -31, -1, 0, 1, 31, 32, 33, 63, 64, 65, Integer.MAX_VALUE };
        h = FNV_OFFSET;
        for (int x : i32s) {
            for (int s : shifts) {
                h = mix(h, x << s);
                h = mix(h, x >> s);
                h = mix(h, x >>> s);
            }
        }
        for (long x : i64s) {
            for (int s : shifts) {
                h = mix(h, x << s);
                h = mix(h, x >> s);
                h = mix(h, x >>> s);
            }
        }
        emit("shifts", h);

        int[] f32Bits = {
            0x00000000, 0x80000000, 0x7f800000, 0xff800000, 0x7fc00000, 0x00000001, 0x80000001,
            0x3effffff, 0x3f000000, 0x3f000001, 0xbeffffff, 0xbf000000, 0xbf000001,
            0x3fc00000, 0xbfc00000, 0x40200000, 0xc0200000,
            0x4affffff, 0x4b000000, 0xcaffffff, 0xcb000000,
            0x4f000000, 0xcf000000, 0x50000000, 0xd0000000
        };
        h = FNV_OFFSET;
        for (int b : f32Bits) {
            float v = Float.intBitsToFloat(b);
            h = mix(h, Math.round(v));
            h = mix(h, mthFloor(v));
            h = mix(h, mthCeil(v));
            h = mix(h, Float.floatToIntBits(wrapDegrees(v)));
        }
        Random sweep = new Random(2026L);
        for (int i = 0; i < 2000; i++) {
            float v = (sweep.nextFloat() - 0.5f) * (1 << (i % 34));
            h = mix(h, Math.round(v));
            h = mix(h, mthFloor(v));
            h = mix(h, mthCeil(v));
            h = mix(h, Float.floatToIntBits(v - (float) mthFloor(v)));
            h = mix(h, Float.floatToIntBits(wrapDegrees(v)));
        }
        emit("float_math", h);

        long[] f64Bits = {
            0x0000000000000000L, 0x8000000000000000L, 0x7ff0000000000000L, 0xfff0000000000000L,
            0x7ff8000000000000L, 0x0000000000000001L, 0x8000000000000001L,
            0x3fdfffffffffffffL, 0x3fe0000000000000L, 0x3fe0000000000001L,
            0xbfdfffffffffffffL, 0xbfe0000000000000L, 0xbfe0000000000001L,
            0x3ff8000000000000L, 0xbff8000000000000L, 0x4004000000000000L, 0xc004000000000000L,
            0x432fffffffffffffL, 0xc32fffffffffffffL, 0x43e0000000000000L, 0xc3e0000000000000L
        };
        h = FNV_OFFSET;
        for (long b : f64Bits) {
            double v = Double.longBitsToDouble(b);
            h = mix(h, Math.round(v));
            h = mix(h, mthFloor(v));
            h = mix(h, mthLfloor(v));
            h = mix(h, mthCeil(v));
            h = mix(h, Double.doubleToLongBits(wrapDegrees(v)));
        }
        for (int i = 0; i < 2000; i++) {
            double v = (sweep.nextDouble() - 0.5) * Math.scalb(1.0, i % 66);
            h = mix(h, Math.round(v));
            h = mix(h, mthFloor(v));
            h = mix(h, mthLfloor(v));
            h = mix(h, mthCeil(v));
            h = mix(h, Double.doubleToLongBits(v - (double) mthLfloor(v)));
            h = mix(h, Double.doubleToLongBits(wrapDegrees(v)));
        }
        emit("double_math", h);

        float[] sinTable = new float[65536];
        for (int i = 0; i < 65536; i++) {
            sinTable[i] = (float) Math.sin((double) i * Math.PI * 2.0 / 65536.0);
        }
        h = FNV_OFFSET;
        for (int i = -1000; i <= 1000; i++) {
            float rad = i * 0.1234567f;
            float s = sinTable[(int) (rad * 10430.378f) & 0xffff];
            float c = sinTable[(int) (rad * 10430.378f + 16384.0f) & 0xffff];
            h = mix(h, Float.floatToIntBits(s));
            h = mix(h, Float.floatToIntBits(c));
        }
        for (int x : new int[] { -100_000, -17, 0, 17, 100_000 }) {
            for (int y : new int[] { -64, 0, 64, 319 }) {
                for (int z : new int[] { -100_000, -17, 0, 17, 100_000 }) {
                    h = mix(h, positionSeed(x, y, z));
                }
            }
        }
        for (String s : new String[] { "", "minecraft:overworld", "minecraft:ore_diamond", "ñandú", "\uD83D\uDE00" }) {
            h = mix(h, s.hashCode());
        }
        emit("trig_pos_hash", h);

        long[] seeds = { 0L, 1L, -1L, Long.MIN_VALUE, Long.MAX_VALUE, 0x5deece66dL, 987654321012345L };
        int[] bounds = { 1, 2, 3, 10, 16, 97, 1 << 20, 1_500_000_000, Integer.MAX_VALUE };
        h = FNV_OFFSET;
        for (long seed : seeds) {
            Random r = new Random(seed);
            for (int i = 0; i < 50; i++) {
                h = mix(h, r.nextInt());
                h = mix(h, r.nextLong());
                h = mix(h, r.nextBoolean() ? 1L : 0L);
                h = mix(h, Float.floatToIntBits(r.nextFloat()));
                h = mix(h, Double.doubleToLongBits(r.nextDouble()));
                for (int b : bounds) {
                    h = mix(h, r.nextInt(b));
                }
            }
            for (int i = 0; i < 200; i++) {
                r.nextGaussian();
            }
            h = mix(h, r.nextLong());
        }
        emit("legacy_rng", h);

        h = FNV_OFFSET;
        for (long seed : seeds) {
            Xoro x = Xoro.fromSeed(seed);
            for (int i = 0; i < 50; i++) {
                h = mix(h, x.nextInt());
                h = mix(h, x.nextLong());
                h = mix(h, x.nextBoolean() ? 1L : 0L);
                h = mix(h, Float.floatToIntBits(x.nextFloat()));
                h = mix(h, Double.doubleToLongBits(x.nextDouble()));
                for (int b : bounds) {
                    h = mix(h, x.nextInt(b));
                }
            }
            long fLo = x.nextLong(), fHi = x.nextLong();
            long pos = positionSeed(128, -32, -512);
            Xoro at = new Xoro(pos ^ fLo, fHi);
            h = mix(h, at.nextLong());

            long[] md5 = md5Words("minecraft:chests/simple_dungeon");
            Xoro named = new Xoro(md5[0] ^ fLo, md5[1] ^ fHi);
            h = mix(h, named.nextLong());

            long uLo = (seed ^ SILVER_64) ^ md5[0];
            long uHi = ((seed ^ SILVER_64) + GOLDEN_64) ^ md5[1];
            Xoro seq = new Xoro(mixStafford13(uLo), mixStafford13(uHi));
            h = mix(h, seq.nextLong());
        }
        emit("xoroshiro_rng", h);

        double[] coords = {
            -40_000_000.75, -16_777_216.0, -512.375, -1.0000001, -0.5, 0.0,
            0.125, 0.9999999, 17.625, 1024.875, 16_777_216.0, 50_000_000.25
        };
        h = FNV_OFFSET;
        for (long seed : new long[] { 0L, 2026L, -987654321012345L }) {
            Improved impX = new Improved(XoroRng.fromSeed(seed));
            Improved impL = new Improved(new LegacyRng(seed));
            for (double x : coords) {
                for (double y : new double[] { -64.25, 0.0, 0.375, 127.875 }) {
                    for (double z : new double[] { -33_554_432.5, -12.75, 0.0, 99.125 }) {
                        h = mix(h, Double.doubleToLongBits(impX.noise(x, y, z, 0.0, 0.0)));
                        h = mix(h, Double.doubleToLongBits(impX.noise(x, y, z, 0.25, y * 0.25)));
                        h = mix(h, Double.doubleToLongBits(impL.noise(x, y, z, 0.125, -1.0)));
                    }
                }
            }

            double[][] ampCases = {
                { 1.0, 2.0, 1.0, 0.0, 0.0, 0.0 },
                { 0.0, 1.0, 0.0, 1.5, 0.5 },
                { 1.0 }
            };
            int[] firstOcts = { -7, -5, 0 };
            for (int c = 0; c < ampCases.length; c++) {
                Perlin pm = Perlin.createModern(XoroRng.fromSeed(seed), firstOcts[c], ampCases[c]);
                Perlin pl = Perlin.createLegacy(new LegacyRng(seed), firstOcts[c], ampCases[c]);
                h = mix(h, Double.doubleToLongBits(pm.maxValue));
                h = mix(h, Double.doubleToLongBits(pm.maxBrokenValue(85.5515)));
                h = mix(h, Double.doubleToLongBits(pl.maxValue));
                for (int i = -5; i <= 5; i++) {
                    double px = i * 37.25;
                    double py = i * 11.5;
                    double pz = -i * 53.75;
                    h = mix(h, Double.doubleToLongBits(pm.getValue(px, py, pz, 0.0, 0.0, false)));
                    h = mix(h, Double.doubleToLongBits(pm.getValue(px, py, pz, 0.5, py, true)));
                    h = mix(h, Double.doubleToLongBits(pl.getValue(px, py, pz, 0.0, 0.0, false)));
                    h = mix(h, Double.doubleToLongBits(pl.getValue(px * 1_000_000.0, py, pz * 1_000_000.0, 0.25, py, false)));
                }
            }
        }
        emit("improved_perlin_noise", h);

        h = FNV_OFFSET;
        double[][] normalAmps = {
            { 1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0 },
            { 1.0, 2.0, 1.0, 0.0, 0.0, 0.0 },
            { 1.0, 1.0, 1.0, 0.0 },
            { 0.0, 1.0, 0.0 },
            { 0.0, 0.0 }
        };
        int[] normalFirst = { -9, -7, -3, -4, -2 };
        for (long seed : new long[] { 1L, -42L, 8675309L }) {
            for (int c = 0; c < normalAmps.length; c++) {
                XoroRng xr = XoroRng.fromSeed(seed);
                Normal nm = new Normal(
                    Perlin.createModern(xr, normalFirst[c], normalAmps[c]),
                    Perlin.createModern(xr, normalFirst[c], normalAmps[c]),
                    normalAmps[c]
                );
                LegacyRng lr = new LegacyRng(seed);
                Normal nl = new Normal(
                    Perlin.createLegacy(lr, normalFirst[c], normalAmps[c]),
                    Perlin.createLegacy(lr, normalFirst[c], normalAmps[c]),
                    normalAmps[c]
                );
                h = mix(h, Double.doubleToLongBits(nm.maxValue));
                h = mix(h, Double.doubleToLongBits(nl.maxValue));
                for (int i = -8; i <= 8; i++) {
                    double nx = i * 64.125;
                    double ny = -64.0 + i * 24.0;
                    double nz = -i * 91.375;
                    h = mix(h, Double.doubleToLongBits(nm.getValue(nx, ny, nz)));
                    h = mix(h, Double.doubleToLongBits(nl.getValue(nx, ny, nz)));
                }
            }
        }
        emit("normal_noise", h);

        h = FNV_OFFSET;
        for (long seed : new long[] { 0L, 2026L, -7777777777777L }) {
            Blended bx = new Blended(XoroRng.fromSeed(seed), 0.25, 0.125, 80.0, 160.0, 8.0);
            Blended bl = new Blended(new LegacyRng(seed), 0.25, 0.25, 80.0, 80.0, 4.0);
            h = mix(h, Double.doubleToLongBits(bx.maxValue));
            h = mix(h, Double.doubleToLongBits(bl.maxValue));
            for (int ix = -4; ix <= 4; ix++) {
                for (int iy = -4; iy <= 4; iy++) {
                    for (int iz = -4; iz <= 4; iz++) {
                        double x = ix * 16.0;
                        double y = iy * 32.0;
                        double z = iz * 16.0;
                        h = mix(h, Double.doubleToLongBits(bx.sample(x, y, z)));
                        h = mix(h, Double.doubleToLongBits(bl.sample(x, y, z)));
                    }
                }
            }
        }
        emit("blended_noise", h);

        h = FNV_OFFSET;
        int[][] simplexOctaves = {
            { 0 },
            { -3, -2, -1, 0 },
            { -4, -2, 0 },
            { -3, -1, 0, 2 }
        };
        for (long seed : new long[] { 2345L, 0L, -123456789L }) {
            Simplex sx = new Simplex(XoroRng.fromSeed(seed));
            Simplex sl = new Simplex(new LegacyRng(seed));
            for (int ix = -6; ix <= 6; ix++) {
                for (int iy = -6; iy <= 6; iy++) {
                    double x = ix * 0.37;
                    double y = iy * 0.43;
                    h = mix(h, Double.doubleToLongBits(sx.getValue2D(x, y)));
                    h = mix(h, Double.doubleToLongBits(sl.getValue2D(x, y)));
                    for (int iz = -3; iz <= 3; iz++) {
                        double z = iz * 0.29;
                        h = mix(h, Double.doubleToLongBits(sx.getValue3D(x, y, z)));
                        h = mix(h, Double.doubleToLongBits(sl.getValue3D(x, y, z)));
                    }
                }
            }
            for (int[] octs : simplexOctaves) {
                PerlinSimplex psx = new PerlinSimplex(XoroRng.fromSeed(seed), octs);
                PerlinSimplex psl = new PerlinSimplex(new LegacyRng(seed), octs);
                for (int i = -10; i <= 10; i++) {
                    double x = i * 19.625;
                    double y = -i * 23.125;
                    h = mix(h, Double.doubleToLongBits(psx.getValue(x, y, false)));
                    h = mix(h, Double.doubleToLongBits(psx.getValue(x, y, true)));
                    h = mix(h, Double.doubleToLongBits(psl.getValue(x, y, false)));
                    h = mix(h, Double.doubleToLongBits(psl.getValue(x, y, true)));
                }
            }
        }
        emit("simplex_noise", h);
    }
}
"#;

fn run_java_probe() -> Option<BTreeMap<String, u64>> {
    let require_java =
        std::env::var_os("HARPAS_REQUIRE_JAVA").is_some() || std::env::var_os("CI").is_some();
    let dir = TempDir::new();
    let source = dir.0.join("Probe.java");
    fs::write(&source, JAVA_PROBE).expect("write Probe.java");

    let javac = match Command::new("javac")
        .args(["-encoding", "UTF-8"])
        .arg(&source)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !require_java => {
            eprintln!("Skipping Java parity check: `javac` is not on PATH");
            return None;
        }
        Err(err) => panic!("failed to start `javac`: {err}"),
    };
    assert!(
        javac.status.success(),
        "javac failed: {}",
        String::from_utf8_lossy(&javac.stderr)
    );

    let java = Command::new("java")
        .arg("-cp")
        .arg(&dir.0)
        .arg("Probe")
        .output()
        .expect("run `java Probe`");
    assert!(
        java.status.success(),
        "java Probe failed: {}",
        String::from_utf8_lossy(&java.stderr)
    );

    let stdout = String::from_utf8(java.stdout).expect("UTF-8 probe output");
    let mut map = BTreeMap::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let (key, hex) = line.split_once('=').expect("key=hex line");
        let value = u64::from_str_radix(hex.trim(), 16).expect("16-digit hex digest");
        map.insert(key.to_owned(), value);
    }
    Some(map)
}

#[test]
fn math_and_random_match_java_runtime_bit_for_bit() {
    let Some(expected) = run_java_probe() else {
        return;
    };

    let i32s = [
        i32::MIN,
        i32::MIN + 1,
        -1_000_000_000,
        -100,
        -7,
        -4,
        -1,
        0,
        1,
        4,
        7,
        100,
        1_000_000_000,
        i32::MAX - 1,
        i32::MAX,
    ];
    let mut h = FNV_OFFSET;
    for x in i32s {
        for y in i32s {
            if y == 0 {
                continue;
            }
            mix(&mut h, i64::from(math::div_i32(x, y)) as u64);
            mix(&mut h, i64::from(math::rem_i32(x, y)) as u64);
            mix(&mut h, i64::from(math::floor_div_i32(x, y)) as u64);
            mix(&mut h, i64::from(math::floor_mod_i32(x, y)) as u64);
        }
    }
    assert_eq!(h, expected["int_div_mod"], "int_div_mod mismatch");

    let i64s = [
        i64::MIN,
        i64::MIN + 1,
        -5_000_000_000,
        -7,
        -4,
        -1,
        0,
        1,
        4,
        7,
        5_000_000_000,
        i64::MAX - 1,
        i64::MAX,
    ];
    h = FNV_OFFSET;
    for x in i64s {
        for y in i64s {
            if y == 0 {
                continue;
            }
            mix(&mut h, math::div_i64(x, y) as u64);
            mix(&mut h, math::rem_i64(x, y) as u64);
            mix(&mut h, math::floor_div_i64(x, y) as u64);
            mix(&mut h, math::floor_mod_i64(x, y) as u64);
        }
        for yi in [-16, -7, -1, 1, 7, 16] {
            mix(&mut h, i64::from(math::floor_mod_i64_i32(x, yi)) as u64);
        }
    }
    assert_eq!(h, expected["long_div_mod"], "long_div_mod mismatch");

    let shifts = [
        i32::MIN,
        -65,
        -33,
        -32,
        -31,
        -1,
        0,
        1,
        31,
        32,
        33,
        63,
        64,
        65,
        i32::MAX,
    ];
    h = FNV_OFFSET;
    for x in i32s {
        for s in shifts {
            mix(&mut h, i64::from(math::shl_i32(x, s)) as u64);
            mix(&mut h, i64::from(math::shr_i32(x, s)) as u64);
            mix(&mut h, i64::from(math::ushr_i32(x, s)) as u64);
        }
    }
    for x in i64s {
        for s in shifts {
            mix(&mut h, math::shl_i64(x, s) as u64);
            mix(&mut h, math::shr_i64(x, s) as u64);
            mix(&mut h, math::ushr_i64(x, s) as u64);
        }
    }
    assert_eq!(h, expected["shifts"], "shifts mismatch");

    let f32_bits: [u32; 25] = [
        0x0000_0000,
        0x8000_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0000,
        0x0000_0001,
        0x8000_0001,
        0x3eff_ffff,
        0x3f00_0000,
        0x3f00_0001,
        0xbeff_ffff,
        0xbf00_0000,
        0xbf00_0001,
        0x3fc0_0000,
        0xbfc0_0000,
        0x4020_0000,
        0xc020_0000,
        0x4aff_ffff,
        0x4b00_0000,
        0xcaff_ffff,
        0xcb00_0000,
        0x4f00_0000,
        0xcf00_0000,
        0x5000_0000,
        0xd000_0000,
    ];
    h = FNV_OFFSET;
    for b in f32_bits {
        let v = f32::from_bits(b);
        mix(&mut h, i64::from(math::round_f32(v)) as u64);
        mix(&mut h, i64::from(math::floor_f32(v)) as u64);
        mix(&mut h, i64::from(math::ceil_f32(v)) as u64);
        mix(
            &mut h,
            i64::from(canonical_f32_bits(math::wrap_degrees_f32(v)) as i32) as u64,
        );
    }
    let mut sweep = LegacyRandom::new(2026);
    for i in 0..2000_i32 {
        let scale = math::shl_i32(1, i % 34) as f32;
        let v = (sweep.next_float() - 0.5_f32) * scale;
        mix(&mut h, i64::from(math::round_f32(v)) as u64);
        mix(&mut h, i64::from(math::floor_f32(v)) as u64);
        mix(&mut h, i64::from(math::ceil_f32(v)) as u64);
        mix(
            &mut h,
            i64::from(canonical_f32_bits(math::frac_f32(v)) as i32) as u64,
        );
        mix(
            &mut h,
            i64::from(canonical_f32_bits(math::wrap_degrees_f32(v)) as i32) as u64,
        );
    }
    assert_eq!(h, expected["float_math"], "float_math mismatch");

    let f64_bits: [u64; 21] = [
        0x0000_0000_0000_0000,
        0x8000_0000_0000_0000,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_0000_0000_0000,
        0x0000_0000_0000_0001,
        0x8000_0000_0000_0001,
        0x3fdf_ffff_ffff_ffff,
        0x3fe0_0000_0000_0000,
        0x3fe0_0000_0000_0001,
        0xbfdf_ffff_ffff_ffff,
        0xbfe0_0000_0000_0000,
        0xbfe0_0000_0000_0001,
        0x3ff8_0000_0000_0000,
        0xbff8_0000_0000_0000,
        0x4004_0000_0000_0000,
        0xc004_0000_0000_0000,
        0x432f_ffff_ffff_ffff,
        0xc32f_ffff_ffff_ffff,
        0x43e0_0000_0000_0000,
        0xc3e0_0000_0000_0000,
    ];
    h = FNV_OFFSET;
    for b in f64_bits {
        let v = f64::from_bits(b);
        mix(&mut h, math::round_f64(v) as u64);
        mix(&mut h, i64::from(math::floor_f64(v)) as u64);
        mix(&mut h, math::lfloor(v) as u64);
        mix(&mut h, i64::from(math::ceil_f64(v)) as u64);
        mix(&mut h, canonical_f64_bits(math::wrap_degrees_f64(v)));
    }
    for i in 0..2000_u32 {
        let exp_bits = (1023_u64 + u64::from(i % 66)) << 52;
        let v = (sweep.next_double() - 0.5) * f64::from_bits(exp_bits);
        mix(&mut h, math::round_f64(v) as u64);
        mix(&mut h, i64::from(math::floor_f64(v)) as u64);
        mix(&mut h, math::lfloor(v) as u64);
        mix(&mut h, i64::from(math::ceil_f64(v)) as u64);
        mix(&mut h, canonical_f64_bits(math::frac_f64(v)));
        mix(&mut h, canonical_f64_bits(math::wrap_degrees_f64(v)));
    }
    assert_eq!(h, expected["double_math"], "double_math mismatch");

    h = FNV_OFFSET;
    for i in -1000..=1000_i32 {
        let rad = (i as f32) * 0.123_456_7_f32;
        mix(
            &mut h,
            i64::from(canonical_f32_bits(math::sin_f32(rad)) as i32) as u64,
        );
        mix(
            &mut h,
            i64::from(canonical_f32_bits(math::cos_f32(rad)) as i32) as u64,
        );
    }
    for x in [-100_000, -17, 0, 17, 100_000] {
        for y in [-64, 0, 64, 319] {
            for z in [-100_000, -17, 0, 17, 100_000] {
                mix(&mut h, math::position_seed(x, y, z) as u64);
            }
        }
    }
    for s in [
        "",
        "minecraft:overworld",
        "minecraft:ore_diamond",
        "ñandú",
        "😀",
    ] {
        mix(&mut h, i64::from(math::java_string_hash(s)) as u64);
    }
    assert_eq!(h, expected["trig_pos_hash"], "trig_pos_hash mismatch");

    let seeds = [
        0_i64,
        1,
        -1,
        i64::MIN,
        i64::MAX,
        0x0005_deec_e66d,
        987_654_321_012_345,
    ];
    let bounds = [1, 2, 3, 10, 16, 97, 1 << 20, 1_500_000_000, i32::MAX];
    h = FNV_OFFSET;
    for seed in seeds {
        let mut r = LegacyRandom::new(seed);
        for _ in 0..50 {
            mix(&mut h, i64::from(r.next_int()) as u64);
            mix(&mut h, r.next_long() as u64);
            mix(&mut h, u64::from(r.next_bool()));
            mix(
                &mut h,
                i64::from(canonical_f32_bits(r.next_float()) as i32) as u64,
            );
            mix(&mut h, canonical_f64_bits(r.next_double()));
            for b in bounds {
                mix(&mut h, i64::from(r.next_int_bounded(b)) as u64);
            }
        }
        for _ in 0..200 {
            let _ = r.next_gaussian();
        }
        mix(&mut h, r.next_long() as u64);
    }
    assert_eq!(h, expected["legacy_rng"], "legacy_rng mismatch");

    h = FNV_OFFSET;
    for seed in seeds {
        let mut x = XoroshiroRandom::new(seed);
        for _ in 0..50 {
            mix(&mut h, i64::from(x.next_int()) as u64);
            mix(&mut h, x.next_long() as u64);
            mix(&mut h, u64::from(x.next_bool()));
            mix(
                &mut h,
                i64::from(canonical_f32_bits(x.next_float()) as i32) as u64,
            );
            mix(&mut h, canonical_f64_bits(x.next_double()));
            for b in bounds {
                mix(&mut h, i64::from(x.next_int_bounded(b)) as u64);
            }
        }
        let factory = x.fork_positional();
        let mut at = factory.at(128, -32, -512);
        mix(&mut h, at.next_long() as u64);

        let mut named = factory.from_hash_of("minecraft:chests/simple_dungeon");
        mix(&mut h, named.next_long() as u64);

        let mut seq = RandomSequence::new(seed, Some("minecraft:chests/simple_dungeon"));
        mix(&mut h, seq.random().next_long() as u64);
    }
    assert_eq!(h, expected["xoroshiro_rng"], "xoroshiro_rng mismatch");

    let coords = [
        -40_000_000.75,
        -16_777_216.0,
        -512.375,
        -1.000_000_1,
        -0.5,
        0.0,
        0.125,
        0.999_999_9,
        17.625,
        1024.875,
        16_777_216.0,
        50_000_000.25,
    ];
    h = FNV_OFFSET;
    for seed in [0_i64, 2026, -987_654_321_012_345] {
        let mut xoro = XoroshiroRandom::new(seed);
        let imp_x = ImprovedNoise::new(&mut xoro);
        let mut leg = LegacyRandom::new(seed);
        let imp_l = ImprovedNoise::new(&mut leg);
        for x in coords {
            for y in [-64.25, 0.0, 0.375, 127.875] {
                for z in [-33_554_432.5, -12.75, 0.0, 99.125] {
                    mix(&mut h, canonical_f64_bits(imp_x.noise(x, y, z)));
                    mix(
                        &mut h,
                        canonical_f64_bits(imp_x.noise_with_y_scale(x, y, z, 0.25, y * 0.25)),
                    );
                    mix(
                        &mut h,
                        canonical_f64_bits(imp_l.noise_with_y_scale(x, y, z, 0.125, -1.0)),
                    );
                }
            }
        }

        let amp_cases: [(&[f64], i32); 3] = [
            (&[1.0, 2.0, 1.0, 0.0, 0.0, 0.0], -7),
            (&[0.0, 1.0, 0.0, 1.5, 0.5], -5),
            (&[1.0], 0),
        ];
        for (amps, first_oct) in amp_cases {
            let mut xoro = XoroshiroRandom::new(seed);
            let pm = PerlinNoise::new(&mut xoro, first_oct, amps);
            let mut leg = LegacyRandom::new(seed);
            let pl = PerlinNoise::new_legacy(&mut leg, first_oct, amps);
            mix(&mut h, canonical_f64_bits(pm.max_value()));
            mix(&mut h, canonical_f64_bits(pm.max_broken_value(85.5515)));
            mix(&mut h, canonical_f64_bits(pl.max_value()));
            for i in -5..=5_i32 {
                let px = (i as f64) * 37.25;
                let py = (i as f64) * 11.5;
                let pz = (-i as f64) * 53.75;
                mix(&mut h, canonical_f64_bits(pm.get_value(px, py, pz)));
                mix(
                    &mut h,
                    canonical_f64_bits(pm.get_value_with_y_scale(px, py, pz, 0.5, py, true)),
                );
                mix(&mut h, canonical_f64_bits(pl.get_value(px, py, pz)));
                mix(
                    &mut h,
                    canonical_f64_bits(pl.get_value_with_y_scale(
                        px * 1_000_000.0,
                        py,
                        pz * 1_000_000.0,
                        0.25,
                        py,
                        false,
                    )),
                );
            }
        }
    }
    assert_eq!(
        h, expected["improved_perlin_noise"],
        "improved_perlin_noise mismatch"
    );

    h = FNV_OFFSET;
    let normal_cases: [(&[f64], i32); 5] = [
        (&[1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0], -9),
        (&[1.0, 2.0, 1.0, 0.0, 0.0, 0.0], -7),
        (&[1.0, 1.0, 1.0, 0.0], -3),
        (&[0.0, 1.0, 0.0], -4),
        (&[0.0, 0.0], -2),
    ];
    for seed in [1_i64, -42, 8_675_309] {
        for (amps, first_oct) in normal_cases {
            let mut xoro = XoroshiroRandom::new(seed);
            let nm = NormalNoise::new(&mut xoro, first_oct, amps);
            let mut leg = LegacyRandom::new(seed);
            let nl = NormalNoise::new_legacy(&mut leg, first_oct, amps);
            mix(&mut h, canonical_f64_bits(nm.max_value()));
            mix(&mut h, canonical_f64_bits(nl.max_value()));
            for i in -8..=8_i32 {
                let nx = (i as f64) * 64.125;
                let ny = -64.0 + (i as f64) * 24.0;
                let nz = (-i as f64) * 91.375;
                mix(&mut h, canonical_f64_bits(nm.get_value(nx, ny, nz)));
                mix(&mut h, canonical_f64_bits(nl.get_value(nx, ny, nz)));
            }
        }
    }
    assert_eq!(h, expected["normal_noise"], "normal_noise mismatch");

    h = FNV_OFFSET;
    for seed in [0_i64, 2026, -7_777_777_777_777] {
        let mut xoro = XoroshiroRandom::new(seed);
        let bx = BlendedNoise::new(&mut xoro, 0.25, 0.125, 80.0, 160.0, 8.0);
        let mut leg = LegacyRandom::new(seed);
        let bl = BlendedNoise::new(&mut leg, 0.25, 0.25, 80.0, 80.0, 4.0);
        mix(&mut h, canonical_f64_bits(bx.max_value()));
        mix(&mut h, canonical_f64_bits(bl.max_value()));
        for ix in -4..=4_i32 {
            for iy in -4..=4_i32 {
                for iz in -4..=4_i32 {
                    let x = (ix as f64) * 16.0;
                    let y = (iy as f64) * 32.0;
                    let z = (iz as f64) * 16.0;
                    mix(&mut h, canonical_f64_bits(bx.sample(x, y, z)));
                    mix(&mut h, canonical_f64_bits(bl.sample(x, y, z)));
                }
            }
        }
    }
    assert_eq!(h, expected["blended_noise"], "blended_noise mismatch");

    h = FNV_OFFSET;
    let simplex_octaves: [&[i32]; 4] = [&[0], &[-3, -2, -1, 0], &[-4, -2, 0], &[-3, -1, 0, 2]];
    for seed in [2345_i64, 0, -123_456_789] {
        let mut xoro = XoroshiroRandom::new(seed);
        let sx = SimplexNoise::new(&mut xoro);
        let mut leg = LegacyRandom::new(seed);
        let sl = SimplexNoise::new(&mut leg);
        for ix in -6..=6_i32 {
            for iy in -6..=6_i32 {
                let x = (ix as f64) * 0.37;
                let y = (iy as f64) * 0.43;
                mix(&mut h, canonical_f64_bits(sx.get_value_2d(x, y)));
                mix(&mut h, canonical_f64_bits(sl.get_value_2d(x, y)));
                for iz in -3..=3_i32 {
                    let z = (iz as f64) * 0.29;
                    mix(&mut h, canonical_f64_bits(sx.get_value_3d(x, y, z)));
                    mix(&mut h, canonical_f64_bits(sl.get_value_3d(x, y, z)));
                }
            }
        }
        for octs in simplex_octaves {
            let mut xoro = XoroshiroRandom::new(seed);
            let psx = PerlinSimplexNoise::new(&mut xoro, octs);
            let mut leg = LegacyRandom::new(seed);
            let psl = PerlinSimplexNoise::new(&mut leg, octs);
            for i in -10..=10_i32 {
                let x = (i as f64) * 19.625;
                let y = (-i as f64) * 23.125;
                mix(&mut h, canonical_f64_bits(psx.get_value(x, y, false)));
                mix(&mut h, canonical_f64_bits(psx.get_value(x, y, true)));
                mix(&mut h, canonical_f64_bits(psl.get_value(x, y, false)));
                mix(&mut h, canonical_f64_bits(psl.get_value(x, y, true)));
            }
        }
    }
    assert_eq!(h, expected["simplex_noise"], "simplex_noise mismatch");

    // Verify `WorldgenRandom` and `Seed128` round-trip invariants as well.
    let mut wg = WorldgenRandom::new(LegacyRandom::new(0));
    wg.set_large_feature_seed(42, -3, 5);
    let _ = wg.next_long();
    wg.set_large_feature_with_salt(42, -3, 5, 14_357_617);
    let _ = wg.next_long();
    assert_eq!(Seed128::from_seed(0), XoroshiroRandom::new(0).state());
}
