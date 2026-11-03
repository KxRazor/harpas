// Test driver only: writes the owner-provided server's overworld climate parameter points, in order.
import java.io.PrintWriter;
import java.util.function.Consumer;
import com.mojang.datafixers.util.Pair;
import net.minecraft.resources.ResourceKey;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.biome.OverworldBiomeBuilder;

public final class BiomeParameterProbe {
    public static void main(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        try (var out = new PrintWriter(args[0])) {
            Consumer<Pair<Climate.ParameterPoint, ResourceKey<Biome>>> write = entry -> {
                var point = entry.getFirst();
                var line = new StringBuilder();
                for (var parameter : new Climate.Parameter[] {point.temperature(), point.humidity(),
                        point.continentalness(), point.erosion(), point.depth(), point.weirdness()}) {
                    line.append(parameter.min()).append(' ').append(parameter.max()).append(' ');
                }
                line.append(point.offset()).append(' ').append(point.offset()).append(' ');
                out.println(line.append(entry.getSecond().identifier()));
            };
            var addBiomes = OverworldBiomeBuilder.class.getDeclaredMethod("addBiomes", Consumer.class);
            addBiomes.setAccessible(true);
            addBiomes.invoke(new OverworldBiomeBuilder(), write);
        }
    }
}
