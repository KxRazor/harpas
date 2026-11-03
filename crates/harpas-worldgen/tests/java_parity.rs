//! Differential checks of `harpas_worldgen` (`CubicSpline`, density functions, `NoiseChunk` cell interpolation,
//! `Climate.RTree`, and `BiomeManager` 3D Voronoi zoom) against a Java runtime (`javac`/`java`).
//!
//! Uses only standard Java SE classes (`java.util.*`, `java.lang.Math`, `java.security.MessageDigest`), never
//! Mojang's files or code.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use harpas_core::noise::NoiseParameters;
use harpas_worldgen::{
    BinaryOp, BiomeManager, CompiledDensityGraph, CubicSpline, DensityFunctionDef, MarkerKind,
    NodeId, NoiseChunk, NoiseRouterDef, NoiseSamplingSettings, Parameter, ParameterPoint, RTree,
    RarityValueMapper, TargetPoint, UnaryOp, quantize_coord,
};
use indexmap::IndexMap;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-worldgen-java-parity-{}-{}",
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
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

fn canonical_f64_bits(value: f64) -> u64 {
    if value.is_nan() {
        0x7ff8_0000_0000_0000
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

const JAVA_PROBE: &str = r#"
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;

public final class WorldgenProbe {
    private static final long FNV_OFFSET = 0xcbf29ce484222325L;
    private static final long FNV_PRIME = 0x00000100000001b3L;
    private static final long GOLDEN_64 = 0x9e3779b97f4a7c15L;
    private static final long SILVER_64 = 0x6a09e667f3bcc909L;

    private static long mix(long h, long word) {
        return (h ^ word) * FNV_PRIME;
    }

    private static long f32Bits(float v) {
        if (Float.isNaN(v)) return 0x7fc00000L;
        if (v == 0.0f) return 0L;
        return Integer.toUnsignedLong(Float.floatToRawIntBits(v));
    }

    private static long f64Bits(double v) {
        if (Double.isNaN(v)) return 0x7ff8000000000000L;
        if (v == 0.0) return 0L;
        return Double.doubleToRawLongBits(v);
    }

    private static void emit(String name, long h) {
        System.out.printf(Locale.ROOT, "%s=%016x%n", name, h);
    }

    private static long stafford13(long z) {
        z = (z ^ (z >>> 30)) * 0xbf58476d1ce4e5b9L;
        z = (z ^ (z >>> 27)) * 0x94d049bb133111ebL;
        return z ^ (z >>> 31);
    }

    static final class Xoroshiro {
        long lo, hi;
        Xoroshiro(long lo, long hi) {
            if ((lo | hi) == 0L) {
                lo = GOLDEN_64;
                hi = SILVER_64;
            }
            this.lo = lo;
            this.hi = hi;
        }
        static Xoroshiro fromSeed(long seed) {
            long l = seed ^ SILVER_64;
            long h = l + GOLDEN_64;
            return new Xoroshiro(stafford13(l), stafford13(h));
        }
        long nextLong() {
            long s0 = lo, s1 = hi;
            long res = Long.rotateLeft(s0 + s1, 17) + s0;
            s1 ^= s0;
            lo = Long.rotateLeft(s0, 49) ^ s1 ^ (s1 << 21);
            hi = Long.rotateLeft(s1, 28);
            return res;
        }
        int nextInt(int bound) {
            long r = Integer.toUnsignedLong((int) nextLong()) * bound;
            long low = r & 0xffffffffL;
            if (low < bound) {
                long threshold = Integer.toUnsignedLong(-bound) % bound;
                while (low < threshold) {
                    r = Integer.toUnsignedLong((int) nextLong()) * bound;
                    low = r & 0xffffffffL;
                }
            }
            return (int) (r >>> 32);
        }
        double nextDouble() {
            return (nextLong() >>> 11) * (1.0 / (1L << 53));
        }
        void consumeCount(int count) {
            for (int i = 0; i < count; i++) nextLong();
        }
        Xoroshiro forkPositionalFromHashOf(String name) throws Exception {
            long fLo = nextLong();
            long fHi = nextLong();
            MessageDigest md5 = MessageDigest.getInstance("MD5");
            byte[] d = md5.digest(name.getBytes(StandardCharsets.UTF_8));
            ByteBuffer bb = ByteBuffer.wrap(d).order(ByteOrder.BIG_ENDIAN);
            long hLo = bb.getLong();
            long hHi = bb.getLong();
            return new Xoroshiro(hLo ^ fLo, hHi ^ fHi);
        }
    }

    private static final int[][] GRAD = {
        {1, 1, 0}, {-1, 1, 0}, {1, -1, 0}, {-1, -1, 0},
        {1, 0, 1}, {-1, 0, 1}, {1, 0, -1}, {-1, 0, -1},
        {0, 1, 1}, {0, -1, 1}, {0, 1, -1}, {0, -1, -1},
        {1, 1, 0}, {0, -1, 1}, {-1, 1, 0}, {0, -1, -1}
    };

    static double smoothstep(double t) {
        return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    }

    static double lerp(double t, double a, double b) {
        return a + t * (b - a);
    }

    static double lerp2(double tx, double ty, double c00, double c10, double c01, double c11) {
        return lerp(ty, lerp(tx, c00, c10), lerp(tx, c01, c11));
    }

    static double lerp3(double tx, double ty, double tz,
                        double c000, double c100, double c010, double c110,
                        double c001, double c101, double c011, double c111) {
        return lerp(tz, lerp2(tx, ty, c000, c100, c010, c110), lerp2(tx, ty, c001, c101, c011, c111));
    }

    static final class Improved {
        final byte[] p = new byte[256];
        final double xo, yo, zo;
        Improved(Xoroshiro rng) {
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
        int perm(int idx) { return p[idx & 0xff] & 0xff; }
        static double gradDot(int hash, double x, double y, double z) {
            int[] g = GRAD[hash & 15];
            return g[0] * x + g[1] * y + g[2] * z;
        }
        double noise(double x, double y, double z) {
            double sx = x + xo, sy = y + yo, sz = z + zo;
            int ix = (int) Math.floor(sx), iy = (int) Math.floor(sy), iz = (int) Math.floor(sz);
            double fx = sx - ix, fy = sy - iy, fz = sz - iz;
            double ux = smoothstep(fx), uy = smoothstep(fy), uz = smoothstep(fz);
            int i = perm(ix) + iy, i0 = perm(i) + iz, i1 = perm(i + 1) + iz;
            int j = perm(ix + 1) + iy, j0 = perm(j) + iz, j1 = perm(j + 1) + iz;
            double c000 = gradDot(perm(i0), fx, fy, fz);
            double c100 = gradDot(perm(j0), fx - 1.0, fy, fz);
            double c010 = gradDot(perm(i1), fx, fy - 1.0, fz);
            double c110 = gradDot(perm(j1), fx - 1.0, fy - 1.0, fz);
            double c001 = gradDot(perm(i0 + 1), fx, fy, fz - 1.0);
            double c101 = gradDot(perm(j0 + 1), fx - 1.0, fy, fz - 1.0);
            double c011 = gradDot(perm(i1 + 1), fx, fy - 1.0, fz - 1.0);
            double c111 = gradDot(perm(j1 + 1), fx - 1.0, fy - 1.0, fz - 1.0);
            return lerp3(ux, uy, uz, c000, c100, c010, c110, c001, c101, c011, c111);
        }
    }

    static double wrap(double v) {
        return v - Math.floor(v / 33554432.0 + 0.5) * 33554432.0;
    }

    static final class Perlin {
        final Improved[] levels;
        final double[] amplitudes;
        final double lowestFreqInputFactor;
        final double lowestFreqValueFactor;
        Perlin(Xoroshiro rng, int firstOctave, double[] amplitudes) throws Exception {
            this.amplitudes = amplitudes;
            this.levels = new Improved[amplitudes.length];
            Xoroshiro fLo = new Xoroshiro(rng.lo, rng.hi);
            long posLo = fLo.nextLong();
            long posHi = fLo.nextLong();
            MessageDigest md5 = MessageDigest.getInstance("MD5");
            for (int i = 0; i < amplitudes.length; i++) {
                if (amplitudes[i] != 0.0) {
                    int octave = firstOctave + i;
                    String name = "octave_" + octave;
                    byte[] d = md5.digest(name.getBytes(StandardCharsets.UTF_8));
                    ByteBuffer bb = ByteBuffer.wrap(d).order(ByteOrder.BIG_ENDIAN);
                    Xoroshiro octRng = new Xoroshiro(bb.getLong() ^ posLo, bb.getLong() ^ posHi);
                    levels[i] = new Improved(octRng);
                }
            }
            rng.nextLong();
            rng.nextLong();
            lowestFreqInputFactor = Math.pow(2.0, firstOctave);
            lowestFreqValueFactor = Math.pow(2.0, amplitudes.length - 1) / (Math.pow(2.0, amplitudes.length) - 1.0);
        }
        double getValue(double x, double y, double z) {
            double total = 0.0;
            double inF = lowestFreqInputFactor;
            double valF = lowestFreqValueFactor;
            for (int i = 0; i < levels.length; i++) {
                if (levels[i] != null) {
                    total += amplitudes[i] * levels[i].noise(wrap(x * inF), wrap(y * inF), wrap(z * inF)) * valF;
                }
                inF *= 2.0;
                valF /= 2.0;
            }
            return total;
        }
    }

    static final class Normal {
        private static final double INPUT_FACTOR = 1.0181268882175227;
        final Perlin first, second;
        final double valueFactor;
        Normal(Xoroshiro rng, int firstOctave, double[] amplitudes) throws Exception {
            first = new Perlin(rng, firstOctave, amplitudes);
            second = new Perlin(rng, firstOctave, amplitudes);
            int minOct = Integer.MAX_VALUE, maxOct = Integer.MIN_VALUE;
            for (int i = 0; i < amplitudes.length; i++) {
                if (amplitudes[i] != 0.0) {
                    minOct = Math.min(minOct, i);
                    maxOct = Math.max(maxOct, i);
                }
            }
            int expected = maxOct - minOct;
            valueFactor = (1.0 / 6.0) / (0.1 * (1.0 + 1.0 / (double) (expected + 1)));
        }
        double getValue(double x, double y, double z) {
            return (first.getValue(x, y, z) + second.getValue(x * INPUT_FACTOR, y * INPUT_FACTOR, z * INPUT_FACTOR)) * valueFactor;
        }
    }

    // --- 1. CubicSpline ---
    interface Spline {
        float apply(int bx, int by, int bz);
        float minValue();
        float maxValue();
    }

    record ConstantSpline(float value) implements Spline {
        @Override public float apply(int bx, int by, int bz) { return value; }
        @Override public float minValue() { return value; }
        @Override public float maxValue() { return value; }
    }

    interface CoordFn {
        double compute(int bx, int by, int bz);
        double minValue();
        double maxValue();
    }

    static final class MultipointSpline implements Spline {
        final CoordFn coordinate;
        final float[] locations;
        final Spline[] values;
        final float[] derivatives;
        final float minValue;
        final float maxValue;

        MultipointSpline(CoordFn coordinate, float[] locations, Spline[] values, float[] derivatives) {
            this.coordinate = coordinate;
            this.locations = locations;
            this.values = values;
            this.derivatives = derivatives;
            float coordMin = (float) coordinate.minValue();
            float coordMax = (float) coordinate.maxValue();
            float minVal = Float.POSITIVE_INFINITY;
            float maxVal = Float.NEGATIVE_INFINITY;
            int last = locations.length - 1;
            if (coordMin < locations[0]) {
                float e1 = linearExtend(coordMin, locations, values[0].minValue(), derivatives, 0);
                float e2 = linearExtend(coordMin, locations, values[0].maxValue(), derivatives, 0);
                minVal = Math.min(minVal, Math.min(e1, e2));
                maxVal = Math.max(maxVal, Math.max(e1, e2));
            }
            if (coordMax > locations[last]) {
                float e1 = linearExtend(coordMax, locations, values[last].minValue(), derivatives, last);
                float e2 = linearExtend(coordMax, locations, values[last].maxValue(), derivatives, last);
                minVal = Math.min(minVal, Math.min(e1, e2));
                maxVal = Math.max(maxVal, Math.max(e1, e2));
            }
            for (Spline v : values) {
                minVal = Math.min(minVal, v.minValue());
                maxVal = Math.max(maxVal, v.maxValue());
            }
            for (int i = 0; i < last; i++) {
                float l0 = locations[i];
                float l1 = locations[i + 1];
                float span = l1 - l0;
                Spline v0 = values[i];
                Spline v1 = values[i + 1];
                float minV0 = v0.minValue();
                float maxV0 = v0.maxValue();
                float minV1 = v1.minValue();
                float maxV1 = v1.maxValue();
                float d0 = derivatives[i];
                float d1 = derivatives[i + 1];
                if (d0 != 0.0f || d1 != 0.0f) {
                    float p0 = d0 * span;
                    float p1 = d1 * span;
                    float minA = p0 - maxV1 + minV0;
                    float maxA = p0 - minV1 + maxV0;
                    float minB = -p1 + minV1 - maxV0;
                    float maxB = -p1 + maxV1 - minV0;
                    float minExt = Math.min(minA, minB);
                    float maxExt = Math.max(maxA, maxB);
                    minVal = Math.min(minVal, Math.min(minV0, minV1) + 0.25f * minExt);
                    maxVal = Math.max(maxVal, Math.max(maxV0, maxV1) + 0.25f * maxExt);
                }
            }
            this.minValue = minVal;
            this.maxValue = maxVal;
        }

        static float linearExtend(float x, float[] locs, float val, float[] ders, int idx) {
            float d = ders[idx];
            return d == 0.0f ? val : val + d * (x - locs[idx]);
        }

        static int findIntervalStart(float[] locs, float x) {
            int left = 0;
            int len = locs.length;
            while (len > 0) {
                int half = len / 2;
                int mid = left + half;
                if (x < locs[mid]) {
                    len = half;
                } else {
                    left = mid + 1;
                    len -= half + 1;
                }
            }
            return left - 1;
        }

        static float lerpF32(float t, float a, float b) {
            return a + t * (b - a);
        }

        @Override
        public float apply(int bx, int by, int bz) {
            float x = (float) coordinate.compute(bx, by, bz);
            int idx = findIntervalStart(locations, x);
            int last = locations.length - 1;
            if (idx < 0) {
                return linearExtend(x, locations, values[0].apply(bx, by, bz), derivatives, 0);
            }
            if (idx == last) {
                return linearExtend(x, locations, values[last].apply(bx, by, bz), derivatives, last);
            }
            float l0 = locations[idx];
            float l1 = locations[idx + 1];
            float t = (x - l0) / (l1 - l0);
            float v0 = values[idx].apply(bx, by, bz);
            float v1 = values[idx + 1].apply(bx, by, bz);
            float d0 = derivatives[idx];
            float d1 = derivatives[idx + 1];
            float a = d0 * (l1 - l0) - (v1 - v0);
            float b = -d1 * (l1 - l0) + (v1 - v0);
            return lerpF32(t, v0, v1) + t * (1.0f - t) * lerpF32(t, a, b);
        }

        @Override public float minValue() { return minValue; }
        @Override public float maxValue() { return maxValue; }
    }

    static double yClampedGradient(int by, int fromY, int toY, double fromVal, double toVal) {
        double delta = ((double) by - (double) fromY) / ((double) toY - (double) fromY);
        if (delta < 0.0) return fromVal;
        if (delta > 1.0) return toVal;
        return fromVal + delta * (toVal - fromVal);
    }

    private static long hashCubicSpline() {
        CoordFn coordX = new CoordFn() {
            @Override public double compute(int bx, int by, int bz) { return yClampedGradient(bx, -100, 100, -1.5, 1.5); }
            @Override public double minValue() { return -1.5; }
            @Override public double maxValue() { return 1.5; }
        };
        CoordFn coordY = new CoordFn() {
            @Override public double compute(int bx, int by, int bz) { return yClampedGradient(by, -64, 320, 1.5, -1.5); }
            @Override public double minValue() { return -1.5; }
            @Override public double maxValue() { return 1.5; }
        };

        Spline inner0 = new MultipointSpline(
            coordY,
            new float[] { -1.0f, -0.2f, 0.4f, 1.0f },
            new Spline[] {
                new ConstantSpline(-0.75f),
                new ConstantSpline(0.15f),
                new ConstantSpline(0.85f),
                new ConstantSpline(0.25f)
            },
            new float[] { 0.5f, 1.2f, -0.4f, 0.0f }
        );
        Spline inner1 = new MultipointSpline(
            coordY,
            new float[] { -0.8f, 0.0f, 0.9f },
            new Spline[] {
                new ConstantSpline(0.1f),
                new ConstantSpline(-0.3f),
                new ConstantSpline(1.1f)
            },
            new float[] { -0.25f, 0.0f, 0.75f }
        );
        MultipointSpline outer = new MultipointSpline(
            coordX,
            new float[] { -1.1f, -0.35f, 0.25f, 0.95f },
            new Spline[] {
                new ConstantSpline(-0.5f),
                inner0,
                inner1,
                new ConstantSpline(1.25f)
            },
            new float[] { 0.2f, -0.6f, 0.8f, -0.1f }
        );

        long h = FNV_OFFSET;
        h = mix(h, f32Bits(outer.minValue()));
        h = mix(h, f32Bits(outer.maxValue()));
        for (int bx = -120; bx <= 120; bx += 7) {
            for (int by = -80; by <= 336; by += 13) {
                double val = (double) outer.apply(bx, by, 0);
                h = mix(h, f64Bits(val));
            }
        }
        return h;
    }

    // --- 2. Density ops, FindTopSurface & NoiseChunk Interpolation ---
    private static double evalGraphDensity(Normal nBase, Normal nShift, int bx, int by, int bz) {
        // flat_cache(shift_a("minecraft:offset")) at quart-aligned (qx << 2, 0, qz << 2)
        int qbx = (bx >> 2) << 2;
        int qbz = (bz >> 2) << 2;
        double shiftX = nShift.getValue(qbx * 0.25, 0.0, qbz * 0.25) * 4.0;
        double shiftZ = nShift.getValue(qbz * 0.25, qbx * 0.25, 0.0) * 4.0;
        double shifted = nBase.getValue(bx * 0.5 + shiftX, by * 0.25, bz * 0.5 + shiftZ);
        double gy = yClampedGradient(by, -32, 32, 1.25, -1.25);

        double uAbs = Math.abs(shifted);
        double uSq = shifted * shifted;
        double uCube = shifted * shifted * shifted;
        double uHalf = shifted > 0.0 ? shifted : shifted * 0.5;
        double uQuarter = gy > 0.0 ? gy : gy * 0.25;
        double c = Math.max(-1.0, Math.min(1.0, shifted));
        double uSqueeze = c / 2.0 - c * c * c / 24.0;
        double uInvert = 1.0 / (uAbs + 0.5);

        double rarity = shifted < -0.5 ? 0.75 : (shifted < 0.0 ? 1.0 : (shifted < 0.5 ? 1.5 : 2.0));
        double weird = rarity * Math.abs(nBase.getValue(bx / rarity, by / rarity, bz / rarity));

        double sum = uSq + uCube + (uHalf * uQuarter) + uSqueeze + (-1.0 * uInvert) + weird;
        double branched = (sum >= -0.25 && sum < 0.75)
            ? Math.max(-1.5, Math.min(1.5, gy + sum))
            : Math.min(gy, sum * 0.5);
        return gy + branched * 0.5;
    }

    private static long hashDensityAndNoiseChunk() throws Exception {
        long worldSeed = 987654321L;
        Xoroshiro root1 = Xoroshiro.fromSeed(worldSeed);
        Normal nShift = new Normal(root1.forkPositionalFromHashOf("minecraft:offset"), -3, new double[] { 1.0, 1.0, 1.0, 0.0 });
        Xoroshiro root2 = Xoroshiro.fromSeed(worldSeed);
        Normal nBase = new Normal(root2.forkPositionalFromHashOf("minecraft:continentalness"), -9, new double[] { 1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0 });

        long h = FNV_OFFSET;
        // 1. Point evaluations and FindTopSurface across chunk (0, 0)
        for (int bx = 0; bx < 16; bx += 4) {
            for (int bz = 0; bz < 16; bz += 4) {
                for (int by = -32; by <= 32; by += 8) {
                    h = mix(h, f64Bits(evalGraphDensity(nBase, nShift, bx, by, bz)));
                }
                double upper = 24.0;
                int cellHeight = 8;
                int lowerBound = -32;
                int topY = (int) Math.floor(upper / (double) cellHeight) * cellHeight;
                double found = (double) lowerBound;
                if (topY > lowerBound) {
                    for (int y = topY; y >= lowerBound; y -= cellHeight) {
                        if (evalGraphDensity(nBase, nShift, bx, y, bz) > 0.0) {
                            found = (double) y;
                            break;
                        }
                    }
                }
                h = mix(h, f64Bits(found));
            }
        }

        // 2. NoiseChunk 4x8x4 Trilinear Interpolation over chunk (0, 0) with min_y = -32, height = 64
        int cellWidth = 4;
        int cellHeight = 8;
        int cellCountXZ = 4;
        int cellCountY = 8;
        int firstCellX = 0;
        int firstCellY = -32 / cellHeight;
        int firstCellZ = 0;

        double[][] slice0 = new double[cellCountXZ + 1][cellCountY + 1];
        double[][] slice1 = new double[cellCountXZ + 1][cellCountY + 1];

        for (int iz = 0; iz <= cellCountXZ; iz++) {
            int bz = (firstCellZ + iz) * cellWidth;
            for (int iy = 0; iy <= cellCountY; iy++) {
                int by = (firstCellY + iy) * cellHeight;
                slice0[iz][iy] = evalGraphDensity(nBase, nShift, firstCellX * cellWidth, by, bz);
            }
        }

        for (int dx = 0; dx < cellCountXZ; dx++) {
            int nextCellX = firstCellX + dx + 1;
            for (int iz = 0; iz <= cellCountXZ; iz++) {
                int bz = (firstCellZ + iz) * cellWidth;
                for (int iy = 0; iy <= cellCountY; iy++) {
                    int by = (firstCellY + iy) * cellHeight;
                    slice1[iz][iy] = evalGraphDensity(nBase, nShift, nextCellX * cellWidth, by, bz);
                }
            }

            for (int dy = cellCountY - 1; dy >= 0; dy--) {
                for (int dz = 0; dz < cellCountXZ; dz++) {
                    double v000 = slice0[dz][dy];
                    double v001 = slice0[dz + 1][dy];
                    double v100 = slice1[dz][dy];
                    double v101 = slice1[dz + 1][dy];
                    double v010 = slice0[dz][dy + 1];
                    double v011 = slice0[dz + 1][dy + 1];
                    double v110 = slice1[dz][dy + 1];
                    double v111 = slice1[dz + 1][dy + 1];

                    for (int iy = cellHeight - 1; iy >= 0; iy--) {
                        double deltaY = (double) iy / (double) cellHeight;
                        double n00 = lerp(deltaY, v000, v010);
                        double n10 = lerp(deltaY, v100, v110);
                        double n01 = lerp(deltaY, v001, v011);
                        double n11 = lerp(deltaY, v101, v111);
                        for (int ix = 0; ix < cellWidth; ix++) {
                            double deltaX = (double) ix / (double) cellWidth;
                            double n0 = lerp(deltaX, n00, n10);
                            double n1 = lerp(deltaX, n01, n11);
                            for (int iz = 0; iz < cellWidth; iz++) {
                                double deltaZ = (double) iz / (double) cellWidth;
                                double val = lerp(deltaZ, n0, n1);
                                int by = (firstCellY + dy) * cellHeight + iy;
                                double grad = yClampedGradient(by, -32, 32, -0.5, 0.5);
                                h = mix(h, f64Bits(val + grad));
                            }
                        }
                    }
                }
            }

            double[][] tmp = slice0;
            slice0 = slice1;
            slice1 = tmp;
        }
        return h;
    }

    // --- 4. Climate.RTree ---
    record Param(long min, long max) {
        static Param point(float v) {
            long q = (long) (v * 10000.0f);
            return new Param(q, q);
        }
        static Param span(float a, float b) {
            return new Param((long) (a * 10000.0f), (long) (b * 10000.0f));
        }
        static Param span(Param a, Param b) {
            return new Param(Math.min(a.min, b.min), Math.max(a.max, b.max));
        }
        long distance(long val) {
            long above = val - max;
            long below = min - val;
            if (above > 0) return above;
            return Math.max(below, 0L);
        }
    }

    static long distSq(Param[] space, long[] target) {
        long s = 0L;
        for (int i = 0; i < 7; i++) {
            long d = space[i].distance(target[i]);
            s += d * d;
        }
        return s;
    }

    static abstract class RNode {
        final Param[] space;
        RNode(Param[] space) { this.space = space; }
        abstract RLeaf search(long[] target, RLeaf bestLeaf, long bestDist);
    }

    static final class RLeaf extends RNode {
        final int value;
        RLeaf(Param[] space, int value) {
            super(space);
            this.value = value;
        }
        @Override
        RLeaf search(long[] target, RLeaf bestLeaf, long bestDist) {
            long d = distSq(this.space, target);
            return d < bestDist ? this : bestLeaf;
        }
    }

    static final class RSubTree extends RNode {
        final RNode[] children;
        RSubTree(List<RNode> children) {
            super(unionSpace(children));
            this.children = children.toArray(new RNode[0]);
        }
        @Override
        RLeaf search(long[] target, RLeaf bestLeaf, long bestDist) {
            for (RNode child : children) {
                if (bestDist <= 0L) {
                    return bestLeaf;
                }
                long bound = distSq(child.space, target);
                if (bestDist > bound) {
                    RLeaf candidate = child.search(target, bestLeaf, bestDist);
                    long candDist = bestLeaf == candidate ? bestDist : distSq(candidate.space, target);
                    if (candDist < bestDist) {
                        bestDist = candDist;
                        bestLeaf = candidate;
                    }
                }
            }
            return bestLeaf;
        }
    }

    static Param[] unionSpace(List<RNode> nodes) {
        Param[] res = Arrays.copyOf(nodes.get(0).space, 7);
        for (int i = 1; i < nodes.size(); i++) {
            Param[] s = nodes.get(i).space;
            for (int d = 0; d < 7; d++) {
                res[d] = Param.span(res[d], s[d]);
            }
        }
        return res;
    }

    static long cost(Param[] space) {
        long c = 0L;
        for (Param p : space) {
            c += Math.abs(p.max - p.min);
        }
        return c;
    }

    static void sortNodes(List<RNode> children, int startDim, boolean abs) {
        Comparator<RNode> cmp = comparator(startDim, abs);
        for (int i = 1; i < 7; i++) {
            cmp = cmp.thenComparing(comparator((startDim + i) % 7, abs));
        }
        children.sort(cmp);
    }

    static Comparator<RNode> comparator(int dim, boolean abs) {
        return (a, b) -> {
            long ma = (a.space[dim].min + a.space[dim].max) / 2L;
            long mb = (b.space[dim].min + b.space[dim].max) / 2L;
            long ka = abs ? Math.abs(ma) : ma;
            long kb = abs ? Math.abs(mb) : mb;
            return Long.compare(ka, kb);
        };
    }

    static List<List<RNode>> bucketize(List<RNode> nodes) {
        List<List<RNode>> buckets = new ArrayList<>();
        int total = nodes.size();
        int bucketSize = (int) Math.pow(6.0, Math.floor(Math.log((double) total - 0.01) / Math.log(6.0)));
        bucketSize = Math.max(1, bucketSize);
        for (int i = 0; i < total; i += bucketSize) {
            buckets.add(new ArrayList<>(nodes.subList(i, Math.min(total, i + bucketSize))));
        }
        return buckets;
    }

    static RNode buildRTree(List<RNode> children) {
        if (children.size() == 1) {
            return children.get(0);
        }
        if (children.size() <= 6) {
            children.sort(Comparator.comparingLong(n -> {
                long total = 0L;
                for (int d = 0; d < 7; d++) {
                    total += Math.abs((n.space[d].min + n.space[d].max) / 2L);
                }
                return total;
            }));
            return new RSubTree(children);
        }
        long bestCost = Long.MAX_VALUE;
        List<List<RNode>> bestBuckets = null;
        for (int dim = 0; dim < 7; dim++) {
            sortNodes(children, dim, false);
            List<List<RNode>> buckets = bucketize(children);
            long totalCost = 0L;
            for (List<RNode> b : buckets) {
                totalCost += cost(unionSpace(b));
            }
            if (bestCost > totalCost) {
                bestCost = totalCost;
                bestBuckets = buckets;
            }
        }
        List<RNode> built = new ArrayList<>(bestBuckets.size());
        for (List<RNode> b : bestBuckets) {
            built.add(buildRTree(b));
        }
        return new RSubTree(built);
    }

    private static long hashRTree() {
        List<RNode> leaves = new ArrayList<>();
        for (int i = 0; i < 80; i++) {
            float baseT = -1.0f + (i % 5) * 0.4f;
            float baseH = -1.0f + ((i / 5) % 4) * 0.5f;
            float baseC = -0.8f + ((i / 20) % 4) * 0.45f;
            float baseE = -0.9f + (i % 7) * 0.25f;
            float baseD = (i % 2 == 0) ? 0.0f : 1.0f;
            float baseW = -0.7f + (i % 6) * 0.25f;
            long offset = (long) (((i % 3) * 0.05f) * 10000.0f);
            Param[] space = new Param[] {
                Param.span(baseT, baseT + 0.35f),
                Param.span(baseH, baseH + 0.45f),
                Param.span(baseC, baseC + 0.4f),
                Param.span(baseE, baseE + 0.2f),
                Param.point(baseD),
                Param.span(baseW, baseW + 0.2f),
                new Param(offset, offset)
            };
            leaves.add(new RLeaf(space, i));
        }
        RNode root = buildRTree(leaves);
        RLeaf lastResult = null;
        long h = FNV_OFFSET;
        for (int q = 0; q < 500; q++) {
            float t = -1.1f + (q % 23) * 0.1f;
            float hum = -1.1f + ((q * 3) % 19) * 0.12f;
            float c = -1.0f + ((q * 7) % 17) * 0.13f;
            float e = -1.0f + ((q * 11) % 21) * 0.1f;
            float d = (q % 5 == 0) ? 0.85f : 0.0f;
            float w = -0.9f + ((q * 13) % 25) * 0.08f;
            long[] target = new long[] {
                (long) (t * 10000.0f),
                (long) (hum * 10000.0f),
                (long) (c * 10000.0f),
                (long) (e * 10000.0f),
                (long) (d * 10000.0f),
                (long) (w * 10000.0f),
                0L
            };
            long bestDist = lastResult == null ? Long.MAX_VALUE : distSq(lastResult.space, target);
            lastResult = root.search(target, lastResult, bestDist);
            h = mix(h, lastResult.value);
        }
        return h;
    }

    // --- 5. BiomeManager ---
    static long obfuscateSeed(long seed) throws Exception {
        ByteBuffer buf = ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN);
        buf.putLong(seed);
        MessageDigest sha = MessageDigest.getInstance("SHA-256");
        byte[] digest = sha.digest(buf.array());
        return ByteBuffer.wrap(digest, 0, 8).order(ByteOrder.LITTLE_ENDIAN).getLong();
    }

    static long lcgNext(long left, long right) {
        left *= left * 6364136223846793005L + 1442695040888963407L;
        return left + right;
    }

    static double getFiddle(long seed) {
        long mod = Math.floorMod(seed >> 24, 1024);
        double d = (double) mod / 1024.0;
        return (d - 0.5) * 0.9;
    }

    static double getFiddledDistance(long zoomSeed, int x, int y, int z, double fx, double fy, double fz) {
        long s = lcgNext(zoomSeed, x);
        s = lcgNext(s, y);
        s = lcgNext(s, z);
        s = lcgNext(s, x);
        s = lcgNext(s, y);
        s = lcgNext(s, z);
        double dx = fx + getFiddle(s);
        s = lcgNext(s, zoomSeed);
        double dy = fy + getFiddle(s);
        s = lcgNext(s, zoomSeed);
        double dz = fz + getFiddle(s);
        return dz * dz + dy * dy + dx * dx;
    }

    private static long hashBiomeManager() throws Exception {
        long h = FNV_OFFSET;
        for (long s = -32; s <= 32; s++) {
            h = mix(h, obfuscateSeed(s * 9876543210123L));
        }
        long zoomSeed = obfuscateSeed(20260329L);
        for (int bx = -16; bx < 16; bx += 3) {
            for (int by = -64; by < 64; by += 7) {
                for (int bz = -16; bz < 16; bz += 3) {
                    int ox = bx - 2;
                    int oy = by - 2;
                    int oz = bz - 2;
                    int px = ox >> 2;
                    int py = oy >> 2;
                    int pz = oz >> 2;
                    double fx = (double) (ox & 3) / 4.0;
                    double fy = (double) (oy & 3) / 4.0;
                    double fz = (double) (oz & 3) / 4.0;
                    int bestIdx = 0;
                    double bestDist = Double.POSITIVE_INFINITY;
                    for (int i = 0; i < 8; i++) {
                        boolean bX = (i & 4) == 0;
                        boolean bY = (i & 2) == 0;
                        boolean bZ = (i & 1) == 0;
                        int qx = bX ? px : px + 1;
                        int qy = bY ? py : py + 1;
                        int qz = bZ ? pz : pz + 1;
                        double rx = bX ? fx : fx - 1.0;
                        double ry = bY ? fy : fy - 1.0;
                        double rz = bZ ? fz : fz - 1.0;
                        double dist = getFiddledDistance(zoomSeed, qx, qy, qz, rx, ry, rz);
                        if (bestDist > dist) {
                            bestIdx = i;
                            bestDist = dist;
                        }
                    }
                    int fqx = (bestIdx & 4) == 0 ? px : px + 1;
                    int fqy = (bestIdx & 2) == 0 ? py : py + 1;
                    int fqz = (bestIdx & 1) == 0 ? pz : pz + 1;
                    int fakeBiome = ((fqx * 73856093) ^ (fqy * 19349663) ^ (fqz * 83492791)) & 0x7fffffff;
                    h = mix(h, Integer.toUnsignedLong(fakeBiome));
                }
            }
        }
        return h;
    }

    public static void main(String[] args) throws Exception {
        emit("cubic_spline", hashCubicSpline());
        emit("density_and_noise_chunk", hashDensityAndNoiseChunk());
        emit("rtree", hashRTree());
        emit("biome_manager", hashBiomeManager());
    }
}
"#;

fn run_java_probe() -> Option<BTreeMap<String, u64>> {
    let require_java = std::env::var_os("HARPAS_REQUIRE_JAVA").is_some();
    let dir = TempDir::new();
    let src_path = dir.0.join("WorldgenProbe.java");
    fs::write(&src_path, JAVA_PROBE).expect("write WorldgenProbe.java");

    let javac = match Command::new("javac").arg(&src_path).output() {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !require_java => return None,
        Err(err) => panic!("failed to run javac: {err}"),
    };
    assert!(
        javac.status.success(),
        "javac failed:\n{}",
        String::from_utf8_lossy(&javac.stderr)
    );

    let java = Command::new("java")
        .arg("-cp")
        .arg(&dir.0)
        .arg("WorldgenProbe")
        .output()
        .expect("run java WorldgenProbe");
    assert!(
        java.status.success(),
        "java failed:\n{}",
        String::from_utf8_lossy(&java.stderr)
    );

    let stdout = String::from_utf8(java.stdout).expect("utf-8 stdout");
    let mut out = BTreeMap::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let (k, v) = line.split_once('=').expect("key=value line");
        let parsed = u64::from_str_radix(v.trim(), 16).expect("hex u64");
        out.insert(k.trim().to_string(), parsed);
    }
    Some(out)
}

fn rust_hash_cubic_spline() -> u64 {
    let inner0 = CubicSpline::multipoint(
        NodeId(1),
        -1.5,
        1.5,
        vec![-1.0, -0.2, 0.4, 1.0],
        vec![
            CubicSpline::constant(-0.75),
            CubicSpline::constant(0.15),
            CubicSpline::constant(0.85),
            CubicSpline::constant(0.25),
        ],
        vec![0.5, 1.2, -0.4, 0.0],
    );
    let inner1 = CubicSpline::multipoint(
        NodeId(1),
        -1.5,
        1.5,
        vec![-0.8, 0.0, 0.9],
        vec![
            CubicSpline::constant(0.1),
            CubicSpline::constant(-0.3),
            CubicSpline::constant(1.1),
        ],
        vec![-0.25, 0.0, 0.75],
    );
    let outer = CubicSpline::multipoint(
        NodeId(0),
        -1.5,
        1.5,
        vec![-1.1, -0.35, 0.25, 0.95],
        vec![
            CubicSpline::constant(-0.5),
            inner0,
            inner1,
            CubicSpline::constant(1.25),
        ],
        vec![0.2, -0.6, 0.8, -0.1],
    );

    let y_grad = |v: i32, from_y: i32, to_y: i32, from_val: f64, to_val: f64| -> f64 {
        let delta = (f64::from(v) - f64::from(from_y)) / (f64::from(to_y) - f64::from(from_y));
        if delta < 0.0 {
            from_val
        } else if delta > 1.0 {
            to_val
        } else {
            from_val + delta * (to_val - from_val)
        }
    };

    let mut h = FNV_OFFSET;
    mix(&mut h, u64::from(canonical_f32_bits(outer.min_value())));
    mix(&mut h, u64::from(canonical_f32_bits(outer.max_value())));
    let mut bx = -120;
    while bx <= 120 {
        let mut by = -80;
        while by <= 336 {
            let val = f64::from(outer.apply_with(&mut |node| match node.0 {
                0 => y_grad(bx, -100, 100, -1.5, 1.5),
                1 => y_grad(by, -64, 320, 1.5, -1.5),
                _ => unreachable!(),
            }));
            mix(&mut h, canonical_f64_bits(val));
            by += 13;
        }
        bx += 7;
    }
    h
}

fn rust_hash_density_and_noise_chunk() -> u64 {
    let mut noises = IndexMap::new();
    noises.insert(
        "minecraft:offset".to_string(),
        NoiseParameters::new(-3, vec![1.0, 1.0, 1.0, 0.0]),
    );
    noises.insert(
        "minecraft:continentalness".to_string(),
        NoiseParameters::new(-9, vec![1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0]),
    );

    let mut dfs = IndexMap::new();
    dfs.insert(
        "minecraft:shift_x".to_string(),
        DensityFunctionDef::Marker {
            kind: MarkerKind::FlatCache,
            argument: Box::new(DensityFunctionDef::Marker {
                kind: MarkerKind::Cache2D,
                argument: Box::new(DensityFunctionDef::ShiftA {
                    argument: "minecraft:offset".to_string(),
                }),
            }),
        },
    );
    dfs.insert(
        "minecraft:shift_z".to_string(),
        DensityFunctionDef::Marker {
            kind: MarkerKind::FlatCache,
            argument: Box::new(DensityFunctionDef::Marker {
                kind: MarkerKind::Cache2D,
                argument: Box::new(DensityFunctionDef::ShiftB {
                    argument: "minecraft:offset".to_string(),
                }),
            }),
        },
    );

    let shifted = DensityFunctionDef::Marker {
        kind: MarkerKind::CacheOnce,
        argument: Box::new(DensityFunctionDef::ShiftedNoise {
            noise: "minecraft:continentalness".to_string(),
            xz_scale: 0.5,
            y_scale: 0.25,
            shift_x: Box::new(DensityFunctionDef::Reference(
                "minecraft:shift_x".to_string(),
            )),
            shift_y: Box::new(DensityFunctionDef::Constant(0.0)),
            shift_z: Box::new(DensityFunctionDef::Reference(
                "minecraft:shift_z".to_string(),
            )),
        }),
    };
    dfs.insert("minecraft:test_shifted".to_string(), shifted);

    let gy = DensityFunctionDef::YClampedGradient {
        from_y: -32,
        to_y: 32,
        from_value: 1.25,
        to_value: -1.25,
    };
    dfs.insert("minecraft:test_gy".to_string(), gy);

    let ref_shifted = || {
        Box::new(DensityFunctionDef::Reference(
            "minecraft:test_shifted".to_string(),
        ))
    };
    let ref_gy = || {
        Box::new(DensityFunctionDef::Reference(
            "minecraft:test_gy".to_string(),
        ))
    };

    let u_abs = DensityFunctionDef::Unary {
        op: UnaryOp::Abs,
        argument: ref_shifted(),
    };
    let u_sq = DensityFunctionDef::Unary {
        op: UnaryOp::Square,
        argument: ref_shifted(),
    };
    let u_cube = DensityFunctionDef::Unary {
        op: UnaryOp::Cube,
        argument: ref_shifted(),
    };
    let u_half = DensityFunctionDef::Unary {
        op: UnaryOp::HalfNegative,
        argument: ref_shifted(),
    };
    let u_quarter = DensityFunctionDef::Unary {
        op: UnaryOp::QuarterNegative,
        argument: ref_gy(),
    };
    let u_squeeze = DensityFunctionDef::Unary {
        op: UnaryOp::Squeeze,
        argument: ref_shifted(),
    };
    let u_invert = DensityFunctionDef::Unary {
        op: UnaryOp::Invert,
        argument: Box::new(DensityFunctionDef::Binary {
            op: BinaryOp::Add,
            argument1: Box::new(u_abs),
            argument2: Box::new(DensityFunctionDef::Constant(0.5)),
        }),
    };
    let weird = DensityFunctionDef::WeirdScaledSampler {
        input: ref_shifted(),
        noise: "minecraft:continentalness".to_string(),
        rarity_value_mapper: RarityValueMapper::Type1,
    };

    let add = |a: DensityFunctionDef, b: DensityFunctionDef| DensityFunctionDef::Binary {
        op: BinaryOp::Add,
        argument1: Box::new(a),
        argument2: Box::new(b),
    };
    let mul = |a: DensityFunctionDef, b: DensityFunctionDef| DensityFunctionDef::Binary {
        op: BinaryOp::Mul,
        argument1: Box::new(a),
        argument2: Box::new(b),
    };

    let sum = add(
        add(
            add(add(add(u_sq, u_cube), mul(u_half, u_quarter)), u_squeeze),
            mul(DensityFunctionDef::Constant(-1.0), u_invert),
        ),
        weird,
    );
    dfs.insert("minecraft:test_sum".to_string(), sum);
    let ref_sum = || {
        Box::new(DensityFunctionDef::Reference(
            "minecraft:test_sum".to_string(),
        ))
    };

    let branched = DensityFunctionDef::RangeChoice {
        input: ref_sum(),
        min_inclusive: -0.25,
        max_exclusive: 0.75,
        when_in_range: Box::new(DensityFunctionDef::Clamp {
            input: Box::new(add(*ref_gy(), *ref_sum())),
            min: -1.5,
            max: 1.5,
        }),
        when_out_of_range: Box::new(DensityFunctionDef::Binary {
            op: BinaryOp::Min,
            argument1: ref_gy(),
            argument2: Box::new(mul(*ref_sum(), DensityFunctionDef::Constant(0.5))),
        }),
    };

    let graph_density = add(*ref_gy(), mul(branched, DensityFunctionDef::Constant(0.5)));
    dfs.insert("minecraft:test_density".to_string(), graph_density);

    let prelim_surface = DensityFunctionDef::FindTopSurface {
        density: Box::new(DensityFunctionDef::Reference(
            "minecraft:test_density".to_string(),
        )),
        upper_bound: Box::new(DensityFunctionDef::Constant(24.0)),
        lower_bound: -32,
        cell_height: 8,
    };

    let final_density = add(
        DensityFunctionDef::Marker {
            kind: MarkerKind::Interpolated,
            argument: Box::new(DensityFunctionDef::Reference(
                "minecraft:test_density".to_string(),
            )),
        },
        DensityFunctionDef::YClampedGradient {
            from_y: -32,
            to_y: 32,
            from_value: -0.5,
            to_value: 0.5,
        },
    );

    let zero = DensityFunctionDef::Constant(0.0);
    let router_def = NoiseRouterDef {
        barrier_noise: zero.clone(),
        fluid_level_floodedness_noise: zero.clone(),
        fluid_level_spread_noise: zero.clone(),
        lava_noise: zero.clone(),
        temperature: zero.clone(),
        vegetation: zero.clone(),
        continents: DensityFunctionDef::Reference("minecraft:test_density".to_string()),
        erosion: zero.clone(),
        depth: zero.clone(),
        ridges: zero.clone(),
        preliminary_surface_level: prelim_surface,
        final_density,
        vein_toggle: zero.clone(),
        vein_ridged: zero.clone(),
        vein_gap: zero,
    };

    let (graph, router) =
        CompiledDensityGraph::compile_router(987_654_321, false, &router_def, &noises, &dfs)
            .expect("compile test router");
    let settings = NoiseSamplingSettings {
        min_y: -32,
        height: 64,
        size_horizontal: 1,
        size_vertical: 2,
    };
    let mut chunk = NoiseChunk::new(&graph, &router, settings, 0, 0);

    let mut h = FNV_OFFSET;
    for bx in (0..16).step_by(4) {
        for bz in (0..16).step_by(4) {
            for by in (-32..=32).step_by(8) {
                let d = chunk.eval_point(router.continents, bx, by, bz);
                mix(&mut h, canonical_f64_bits(d));
            }
            let surf = chunk.eval_point(router.preliminary_surface_level, bx, 0, bz);
            mix(&mut h, canonical_f64_bits(surf));
        }
    }

    chunk.initialize_for_first_cell_x();
    let cell_w = settings.cell_width() as usize;
    let cell_h = settings.cell_height() as usize;
    let count_xz = settings.cell_count_xz();
    let count_y = settings.cell_count_y();
    for dx in 0..count_xz {
        chunk.advance_cell_x(dx);
        for dy in (0..count_y).rev() {
            for dz in 0..count_xz {
                chunk.select_cell_yz(dy, dz);
                for iy in (0..cell_h).rev() {
                    let delta_y = (iy as f64) / (cell_h as f64);
                    chunk.update_for_y(iy, delta_y);
                    for ix in 0..cell_w {
                        let delta_x = (ix as f64) / (cell_w as f64);
                        chunk.update_for_x(ix, delta_x);
                        for iz in 0..cell_w {
                            let delta_z = (iz as f64) / (cell_w as f64);
                            chunk.update_for_z(iz, delta_z);
                            let val = chunk.eval_node(router.final_density);
                            mix(&mut h, canonical_f64_bits(val));
                        }
                    }
                }
            }
        }
        chunk.swap_slices();
    }
    chunk.stop_interpolation();
    h
}

fn rust_hash_rtree() -> u64 {
    let mut entries = Vec::with_capacity(80);
    for i in 0..80i32 {
        let base_t = -1.0_f32 + ((i % 5) as f32) * 0.4_f32;
        let base_h = -1.0_f32 + (((i / 5) % 4) as f32) * 0.5_f32;
        let base_c = -0.8_f32 + (((i / 20) % 4) as f32) * 0.45_f32;
        let base_e = -0.9_f32 + ((i % 7) as f32) * 0.25_f32;
        let base_d = if i % 2 == 0 { 0.0_f32 } else { 1.0_f32 };
        let base_w = -0.7_f32 + ((i % 6) as f32) * 0.25_f32;
        let offset = quantize_coord(((i % 3) as f32) * 0.05_f32);
        let point = ParameterPoint {
            temperature: Parameter::span(base_t, base_t + 0.35_f32),
            humidity: Parameter::span(base_h, base_h + 0.45_f32),
            continentalness: Parameter::span(base_c, base_c + 0.4_f32),
            erosion: Parameter::span(base_e, base_e + 0.2_f32),
            depth: Parameter::point(base_d),
            weirdness: Parameter::span(base_w, base_w + 0.2_f32),
            offset,
        };
        entries.push((point, i as u64));
    }
    let tree = RTree::new(entries);
    let mut last_result = None;
    let mut h = FNV_OFFSET;
    for q in 0..500i32 {
        let t = -1.1_f32 + ((q % 23) as f32) * 0.1_f32;
        let hum = -1.1_f32 + (((q * 3) % 19) as f32) * 0.12_f32;
        let c = -1.0_f32 + (((q * 7) % 17) as f32) * 0.13_f32;
        let e = -1.0_f32 + (((q * 11) % 21) as f32) * 0.1_f32;
        let d = if q % 5 == 0 { 0.85_f32 } else { 0.0_f32 };
        let w = -0.9_f32 + (((q * 13) % 25) as f32) * 0.08_f32;
        let target = TargetPoint::from_floats(t, hum, c, e, d, w);
        let chosen = *tree.search(target, &mut last_result);
        mix(&mut h, chosen);
    }
    h
}

fn rust_hash_biome_manager() -> u64 {
    let mut h = FNV_OFFSET;
    for s in -32..=32i64 {
        let obf = BiomeManager::obfuscate_seed(s.wrapping_mul(9_876_543_210_123));
        mix(&mut h, obf as u64);
    }
    let mgr = BiomeManager::from_world_seed(20_260_329);
    let mut bx = -16;
    while bx < 16 {
        let mut by = -64;
        while by < 64 {
            let mut bz = -16;
            while bz < 16 {
                let biome = mgr.get_biome(bx, by, bz, |qx, qy, qz| {
                    let hash = qx.wrapping_mul(73_856_093)
                        ^ qy.wrapping_mul(19_349_663)
                        ^ qz.wrapping_mul(83_492_791);
                    (hash & 0x7fff_ffff) as u32
                });
                mix(&mut h, u64::from(biome));
                bz += 3;
            }
            by += 7;
        }
        bx += 3;
    }
    h
}

#[test]
fn worldgen_density_chunk_spline_rtree_and_biome_zoom_match_java() {
    let Some(java_hashes) = run_java_probe() else {
        return;
    };
    assert_eq!(
        rust_hash_cubic_spline(),
        java_hashes["cubic_spline"],
        "CubicSpline mismatch vs Java"
    );
    assert_eq!(
        rust_hash_density_and_noise_chunk(),
        java_hashes["density_and_noise_chunk"],
        "DensityFunction / FindTopSurface / NoiseChunk interpolation mismatch vs Java"
    );
    assert_eq!(
        rust_hash_rtree(),
        java_hashes["rtree"],
        "Climate.RTree mismatch vs Java"
    );
    assert_eq!(
        rust_hash_biome_manager(),
        java_hashes["biome_manager"],
        "BiomeManager 3D Voronoi zoom mismatch vs Java"
    );
}
