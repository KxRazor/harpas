// Test driver only: WorldgenRandom uses BitRandomSource sampling even with a Xoroshiro source.
import java.io.PrintWriter;
import net.minecraft.world.level.levelgen.*;
public final class WorldgenRandomProbe {
    public static void main(String[] args) throws Exception {
        try (var out = new PrintWriter(args[0])) {
            for (boolean legacy : new boolean[] {true, false}) for (long seed : new long[] {0, 42, -1, Long.MIN_VALUE}) {
                var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                out.println(random.setDecorationSeed(seed, -32, 48));
                random.setFeatureSeed(seed, 0, 3);
                for (int i = 0; i < 128; i++) {
                    out.println(random.nextInt());
                    for (int bound : new int[] {1, 16, 37, 1500000000}) out.println(random.nextInt(bound));
                    out.println(random.nextLong());
                    out.println(random.nextBoolean() ? 1 : 0);
                    out.println(Float.floatToRawIntBits(random.nextFloat()));
                    out.println(Double.doubleToRawLongBits(random.nextDouble()));
                    out.println(Double.doubleToRawLongBits(random.nextGaussian()));
                }
                random.nextGaussian(); random.setSeed(seed);
                out.println(Double.doubleToRawLongBits(random.nextGaussian()));
                out.println(random.nextLong());
            }
        }
    }
}
