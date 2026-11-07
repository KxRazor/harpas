// Parity probe: generates 3x3 chunks through carvers and runs vanilla's own applyBiomeDecoration on the center,
// with every placed feature but vein ores replaced by a no-op at the same index, so each ore keeps its order and
// seed. Writes the order of every step and the blocks that changed, for the Rust scheduler to compare.
import java.io.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.*;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.carver.CarverConfiguration;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.*;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import net.minecraft.world.level.levelgen.structure.Structure;
import net.minecraft.world.level.levelgen.structure.templatesystem.*;

public final class FeatureStageProbe {
    /** The built-in registries leave block tags unbound, so a tag test is replaced by its blocks from the jar. */
    static final class BlocksRule extends RuleTest {
        final Set<Block> blocks = new HashSet<>();
        @Override public boolean test(BlockState state, RandomSource random) { return blocks.contains(state.getBlock()); }
        @Override protected RuleTestType<?> getType() { return RuleTestType.ALWAYS_TRUE_TEST; }
    }

    static void bindRules(OreConfiguration config) throws Exception {
        var field = OreConfiguration.TargetBlockState.class.getDeclaredField("target");
        field.setAccessible(true);
        var tag = TagMatchTest.class.getDeclaredField("tag");
        tag.setAccessible(true);
        for (var target : config.targetStates) {
            if (!(target.target instanceof TagMatchTest)) continue;
            var key = (net.minecraft.tags.TagKey<?>) tag.get(target.target);
            var holders = new LinkedHashSet<Holder<Block>>();
            WorldgenReferenceProbe.addBlockTag(key.location().toString(), holders, new HashSet<>());
            var rule = new BlocksRule();
            for (var holder : holders) rule.blocks.add(holder.value());
            field.set(target, rule);
        }
    }

    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        long seed = Long.parseLong(args[1]);
        var lookup = VanillaRegistries.createLookup();
        var random = RandomState.create(lookup, NoiseGeneratorSettings.OVERWORLD, seed);
        var biomes = new MappedRegistry<Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(
            holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var structures = new MappedRegistry<Structure>(Registries.STRUCTURE, com.mojang.serialization.Lifecycle.stable());
        structures.freeze();
        var placed = new MappedRegistry<PlacedFeature>(Registries.PLACED_FEATURE, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.PLACED_FEATURE).listElements().forEach(
            holder -> placed.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        placed.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes, structures, placed));
        var factory = PalettedContainerFactory.create(access);
        var biomeSource = MultiNoiseBiomeSource.createFromPreset(
            lookup.getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        var height = LevelHeightAccessor.create(-64, 384);
        var generator = new NoiseBasedChunkGenerator(biomeSource, lookup.getOrThrow(NoiseGeneratorSettings.OVERWORLD));

        var carverBlocks = new LinkedHashSet<Holder<Block>>();
        WorldgenReferenceProbe.addBlockTag("minecraft:overworld_carver_replaceables", carverBlocks, new HashSet<>());
        var replaceable = HolderSet.direct(new ArrayList<>(carverBlocks));
        var replaceableField = CarverConfiguration.class.getDeclaredField("replaceable");
        replaceableField.setAccessible(true);
        for (var holder : lookup.lookupOrThrow(Registries.CONFIGURED_CARVER).listElements().toList()) {
            replaceableField.set(holder.value().config(), replaceable);
        }

        var names = new IdentityHashMap<PlacedFeature, String>();
        lookup.lookupOrThrow(Registries.PLACED_FEATURE).listElements()
            .forEach(holder -> names.put(holder.value(), holder.key().identifier().toString()));
        var noOp = new PlacedFeature(Holder.direct(new ConfiguredFeature<>(Feature.NO_OP, FeatureConfiguration.NONE)), List.of());
        var field = ChunkGenerator.class.getDeclaredField("featuresPerStep");
        field.setAccessible(true);
        @SuppressWarnings("unchecked")
        var steps = ((java.util.function.Supplier<List<FeatureSorter.StepFeatureData>>) field.get(generator)).get();

        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            var oresOnly = new ArrayList<FeatureSorter.StepFeatureData>();
            for (var step : steps) {
                var line = new StringJoiner(" ", "step ", "");
                var features = new ArrayList<PlacedFeature>();
                for (var feature : step.features()) {
                    line.add(names.get(feature));
                    var configured = feature.feature().value();
                    if (configured.feature() == Feature.ORE) {
                        bindRules((OreConfiguration) configured.config());
                        features.add(feature);
                    } else {
                        features.add(noOp);
                    }
                }
                out.println(line);
                oresOnly.add(new FeatureSorter.StepFeatureData(features, step.indexMapping()));
            }
            field.set(generator, (java.util.function.Supplier<List<FeatureSorter.StepFeatureData>>) () -> oresOnly);

            var discard = new DataOutputStream(OutputStream.nullOutputStream());
            for (int argument = 2; argument < args.length; argument++) {
                var coordinates = args[argument].split(",");
                int cx = Integer.parseInt(coordinates[0]), cz = Integer.parseInt(coordinates[1]);
                var center = new ChunkPos(cx, cz);
                var chunks = new HashMap<Long, ProtoChunk>();
                for (int dx = -1; dx <= 1; dx++) {
                    for (int dz = -1; dz <= 1; dz++) {
                        var at = new ChunkPos(cx + dx, cz + dz);
                        var proto = new ProtoChunk(at, UpgradeData.EMPTY, height, factory, null);
                        var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                        var manager = new StructureManager(region, new WorldOptions(seed, false, false), null);
                        ChunkParityProbe.writeChunk(discard, seed, biomes, factory, generator, random, region, manager, proto);
                        chunks.put(((long) at.x() << 32) ^ (at.z() & 0xffffffffL), proto);
                    }
                }
                var target = chunks.get(((long) cx << 32) ^ (cz & 0xffffffffL));
                var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, target, seed);
                region.chunks.putAll(chunks);
                region.biomes = new BiomeManager((qx, qy, qz) -> region.getChunk(qx >> 2, qz >> 2).getNoiseBiome(qx, qy, qz),
                    BiomeManager.obfuscateSeed(seed));

                var pos = new BlockPos.MutableBlockPos();
                int[] before = new int[48 * 48 * 384];
                for (int pass = 0; pass < 2; pass++) {
                    if (pass == 1) {
                        var manager = new StructureManager(region, new WorldOptions(seed, false, false), null);
                        generator.applyBiomeDecoration(region, target, manager);
                        out.println("chunk " + cx + " " + cz);
                    }
                    int index = 0;
                    for (int y = -64; y < 320; y++) {
                        for (int z = (cz - 1) * 16; z < (cz + 2) * 16; z++) {
                            for (int x = (cx - 1) * 16; x < (cx + 2) * 16; x++, index++) {
                                var chunk = region.getChunk(x >> 4, z >> 4);
                                int id = Block.getId(chunk.getBlockState(pos.set(x, y, z)));
                                if (pass == 0) before[index] = id;
                                else if (before[index] != id) out.println(x + " " + y + " " + z + " " + id);
                            }
                        }
                    }
                }
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
