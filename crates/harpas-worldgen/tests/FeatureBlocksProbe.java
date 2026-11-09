// Parity probe: generates 3x3 chunks through carvers and runs vanilla's own applyBiomeDecoration on the center,
// with every placed feature that the Rust side does not place replaced by a no-op at the same index, so each
// feature keeps its order and seed. The chunks are at the carvers status, as in a world, so block changes update
// the final heightmaps. Writes the blocks that changed in the 3x3 chunks.
//
// Arguments: the output file, the server jar, the seed, a file listing the placed features to keep, then the
// chunks as x,z. With "final" before the chunks, each chunk is instead the center of 5x5 chunks generated
// through carvers, and its 3x3 chunks decorate in the order Harpas's chunk workers use (west to east, then north
// to south within each column), so the output lists the blocks that changed in the final center chunk.
import java.io.*;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.carver.CarverConfiguration;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.FeatureConfiguration;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import net.minecraft.world.level.levelgen.structure.Structure;

public final class FeatureBlocksProbe {
    static long key(int x, int z) { return ((long) x << 32) ^ (z & 0xffffffffL); }

    static void finalChunk(PrintWriter out, DataOutputStream discard, long seed, RegistryAccess access,
            PalettedContainerFactory factory, LevelHeightAccessor height, Registry<Biome> biomes,
            NoiseBasedChunkGenerator generator, RandomState random, int cx, int cz) throws Exception {
        var chunks = new HashMap<Long, ProtoChunk>();
        for (int dx = -2; dx <= 2; dx++) {
            for (int dz = -2; dz <= 2; dz++) {
                var at = new ChunkPos(cx + dx, cz + dz);
                var proto = new ProtoChunk(at, UpgradeData.EMPTY, height, factory, null);
                var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                var manager = new StructureManager(region, new WorldOptions(seed, false, false), null);
                ChunkParityProbe.writeChunk(discard, seed, biomes, factory, generator, random, region, manager, proto);
                proto.setPersistedStatus(ChunkStatus.CARVERS);
                chunks.put(key(at.x(), at.z()), proto);
            }
        }
        var center = chunks.get(key(cx, cz));
        var pos = new BlockPos.MutableBlockPos();
        int[] before = new int[16 * 16 * 384];
        int index = 0;
        for (int y = -64; y < 320; y++)
            for (int z = 0; z < 16; z++)
                for (int x = 0; x < 16; x++)
                    before[index++] = Block.getId(center.getBlockState(pos.set(cx * 16 + x, y, cz * 16 + z)));
        for (int dx = -1; dx <= 1; dx++) {
            for (int dz = -1; dz <= 1; dz++) {
                var target = chunks.get(key(cx + dx, cz + dz));
                var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, target, seed); region.attachGenerator(generator);
                region.chunks.putAll(chunks);
                region.biomes = new BiomeManager(
                    (qx, qy, qz) -> region.getChunk(qx >> 2, qz >> 2).getNoiseBiome(qx, qy, qz),
                    BiomeManager.obfuscateSeed(seed));
                var manager = new StructureManager(region, new WorldOptions(seed, false, false), null);
                generator.applyBiomeDecoration(region, target, manager);
                target.setPersistedStatus(ChunkStatus.FEATURES);
            }
        }
        out.println("chunk " + cx + " " + cz);
        index = 0;
        for (int y = -64; y < 320; y++) {
            for (int z = 0; z < 16; z++) {
                for (int x = 0; x < 16; x++, index++) {
                    int id = Block.getId(center.getBlockState(pos.set(cx * 16 + x, y, cz * 16 + z)));
                    if (before[index] != id) out.println((cx * 16 + x) + " " + y + " " + (cz * 16 + z) + " " + id);
                }
            }
        }
    }

    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.BLOCK, Registries.BLOCK, "block");
        PlacementProbe.bindTags(args[1], BuiltInRegistries.FLUID, Registries.FLUID, "fluid");
        long seed = Long.parseLong(args[2]);
        var supported = new HashSet<>(Files.readAllLines(Path.of(args[3])));
        var lookup = VanillaRegistries.createLookup();
        WorldgenReferenceProbe.loadConfiguredFeaturesFromDataPack(lookup);
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
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes, structures, placed,
            WorldgenReferenceProbe.configuredFeatureRegistry(lookup)));
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
        var kept = new ArrayList<FeatureSorter.StepFeatureData>();
        for (var step : steps) {
            var features = new ArrayList<PlacedFeature>();
            for (var feature : step.features()) features.add(supported.contains(names.get(feature)) ? feature : noOp);
            kept.add(new FeatureSorter.StepFeatureData(features, step.indexMapping()));
        }
        field.set(generator, (java.util.function.Supplier<List<FeatureSorter.StepFeatureData>>) () -> kept);

        var discard = new DataOutputStream(OutputStream.nullOutputStream());
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            if (args.length > 4 && args[4].equals("final")) {
                for (int argument = 5; argument < args.length; argument++) {
                    var coordinates = args[argument].split(",");
                    finalChunk(out, discard, seed, access, factory, height, biomes, generator, random,
                        Integer.parseInt(coordinates[0]), Integer.parseInt(coordinates[1]));
                }
                net.minecraft.util.Util.shutdownExecutors();
                return;
            }
            for (int argument = 4; argument < args.length; argument++) {
                var coordinates = args[argument].split(",");
                int cx = Integer.parseInt(coordinates[0]), cz = Integer.parseInt(coordinates[1]);
                var chunks = new HashMap<Long, ProtoChunk>();
                for (int dx = -1; dx <= 1; dx++) {
                    for (int dz = -1; dz <= 1; dz++) {
                        var at = new ChunkPos(cx + dx, cz + dz);
                        var proto = new ProtoChunk(at, UpgradeData.EMPTY, height, factory, null);
                        var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                        var manager = new StructureManager(region, new WorldOptions(seed, false, false), null);
                        ChunkParityProbe.writeChunk(discard, seed, biomes, factory, generator, random, region, manager, proto);
                        proto.setPersistedStatus(ChunkStatus.CARVERS);
                        chunks.put(key(at.x(), at.z()), proto);
                    }
                }
                var target = chunks.get(key(cx, cz));
                var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, target, seed); region.attachGenerator(generator);
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
                                int id = Block.getId(chunks.get(key(x >> 4, z >> 4)).getBlockState(pos.set(x, y, z)));
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
