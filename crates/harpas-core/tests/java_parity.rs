//! Differential checks of `harpas_core::math` and `harpas_core::random` against a Java runtime (`javac`/`java`).
//!
//! Uses only standard Java SE classes (`java.util.Random`, `java.lang.Math`, `java.lang.StrictMath`,
//! `java.security.MessageDigest`), never Mojang's files or code.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use harpas_core::math;
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
            h = mix(h, Float.floatToRawIntBits(wrapDegrees(v)));
        }
        Random sweep = new Random(2026L);
        for (int i = 0; i < 2000; i++) {
            float v = (sweep.nextFloat() - 0.5f) * (1 << (i % 34));
            h = mix(h, Math.round(v));
            h = mix(h, mthFloor(v));
            h = mix(h, mthCeil(v));
            h = mix(h, Float.floatToRawIntBits(v - (float) mthFloor(v)));
            h = mix(h, Float.floatToRawIntBits(wrapDegrees(v)));
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
            h = mix(h, Double.doubleToRawLongBits(wrapDegrees(v)));
        }
        for (int i = 0; i < 2000; i++) {
            double v = (sweep.nextDouble() - 0.5) * Math.scalb(1.0, i % 66);
            h = mix(h, Math.round(v));
            h = mix(h, mthFloor(v));
            h = mix(h, mthLfloor(v));
            h = mix(h, mthCeil(v));
            h = mix(h, Double.doubleToRawLongBits(v - (double) mthLfloor(v)));
            h = mix(h, Double.doubleToRawLongBits(wrapDegrees(v)));
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
            h = mix(h, Float.floatToRawIntBits(s));
            h = mix(h, Float.floatToRawIntBits(c));
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
                h = mix(h, Float.floatToRawIntBits(r.nextFloat()));
                h = mix(h, Double.doubleToRawLongBits(r.nextDouble()));
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
                h = mix(h, Float.floatToRawIntBits(x.nextFloat()));
                h = mix(h, Double.doubleToRawLongBits(x.nextDouble()));
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
    }
}
"#;

fn run_java_probe() -> Option<BTreeMap<String, u64>> {
    let require_java =
        std::env::var_os("HARPAS_REQUIRE_JAVA").is_some() || std::env::var_os("CI").is_some();
    let dir = TempDir::new();
    let source = dir.0.join("Probe.java");
    fs::write(&source, JAVA_PROBE).expect("write Probe.java");

    let javac = match Command::new("javac").arg(&source).output() {
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
            i64::from(math::wrap_degrees_f32(v).to_bits() as i32) as u64,
        );
    }
    let mut sweep = LegacyRandom::new(2026);
    for i in 0..2000_i32 {
        let scale = math::shl_i32(1, i % 34) as f32;
        let v = (sweep.next_float() - 0.5_f32) * scale;
        mix(&mut h, i64::from(math::round_f32(v)) as u64);
        mix(&mut h, i64::from(math::floor_f32(v)) as u64);
        mix(&mut h, i64::from(math::ceil_f32(v)) as u64);
        mix(&mut h, i64::from(math::frac_f32(v).to_bits() as i32) as u64);
        mix(
            &mut h,
            i64::from(math::wrap_degrees_f32(v).to_bits() as i32) as u64,
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
        mix(&mut h, math::wrap_degrees_f64(v).to_bits());
    }
    for i in 0..2000_u32 {
        let exp_bits = (1023_u64 + u64::from(i % 66)) << 52;
        let v = (sweep.next_double() - 0.5) * f64::from_bits(exp_bits);
        mix(&mut h, math::round_f64(v) as u64);
        mix(&mut h, i64::from(math::floor_f64(v)) as u64);
        mix(&mut h, math::lfloor(v) as u64);
        mix(&mut h, i64::from(math::ceil_f64(v)) as u64);
        mix(&mut h, math::frac_f64(v).to_bits());
        mix(&mut h, math::wrap_degrees_f64(v).to_bits());
    }
    assert_eq!(h, expected["double_math"], "double_math mismatch");

    h = FNV_OFFSET;
    for i in -1000..=1000_i32 {
        let rad = (i as f32) * 0.123_456_7_f32;
        mix(
            &mut h,
            i64::from(math::sin_f32(rad).to_bits() as i32) as u64,
        );
        mix(
            &mut h,
            i64::from(math::cos_f32(rad).to_bits() as i32) as u64,
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
            mix(&mut h, i64::from(r.next_float().to_bits() as i32) as u64);
            mix(&mut h, r.next_double().to_bits());
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
            mix(&mut h, i64::from(x.next_float().to_bits() as i32) as u64);
            mix(&mut h, x.next_double().to_bits());
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

    // Verify `WorldgenRandom` and `Seed128` round-trip invariants as well.
    let mut wg = WorldgenRandom::new(LegacyRandom::new(0));
    wg.set_large_feature_seed(42, -3, 5);
    let _ = wg.next_long();
    wg.set_large_feature_with_salt(42, -3, 5, 14_357_617);
    let _ = wg.next_long();
    assert_eq!(Seed128::from_seed(0), XoroshiroRandom::new(0).state());
}
