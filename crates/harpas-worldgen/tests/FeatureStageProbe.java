// Parity probe: writes the global order of the placed features in each of the 11 generation steps, as vanilla's
// FeatureSorter merges the Overworld biomes' lists, for the Rust index to compare.
import java.io.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.chunk.ChunkGenerator;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;

public final class FeatureStageProbe {
    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        var biomeSource = MultiNoiseBiomeSource.createFromPreset(
            lookup.getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        var generator = new NoiseBasedChunkGenerator(biomeSource, lookup.getOrThrow(NoiseGeneratorSettings.OVERWORLD));
        var names = new IdentityHashMap<PlacedFeature, String>();
        lookup.lookupOrThrow(Registries.PLACED_FEATURE).listElements()
            .forEach(holder -> names.put(holder.value(), holder.key().identifier().toString()));
        var field = ChunkGenerator.class.getDeclaredField("featuresPerStep");
        field.setAccessible(true);
        @SuppressWarnings("unchecked")
        var steps = ((java.util.function.Supplier<List<FeatureSorter.StepFeatureData>>) field.get(generator)).get();
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (var step : steps) {
                var line = new StringJoiner(" ", "step ", "");
                for (var feature : step.features()) line.add(names.get(feature));
                out.println(line);
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
