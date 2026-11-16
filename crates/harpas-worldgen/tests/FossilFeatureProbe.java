// Test driver only: calls vanilla's own fossil features on generated test terrain and writes what they change.
// Arguments: <output file>. No game code or data is copied into the repository.
//
// Each case decorates chunk (0, 0) of a region holding it and its eight neighbors, filled with one of the terrains
// below, with a fossil feature at one origin. The output holds whether it placed, the random's next int afterwards
// and every block of the region that differs from the terrain. The templates come from the server's template
// manager, which the feature reads through the level.
import java.io.*;
import java.nio.file.Path;
import net.minecraft.core.*;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.ConfiguredFeature;

public final class FossilFeatureProbe {
    // Kept in step with fossil_feature_parity.rs.
    static BlockState terrain(int scenario, int x, int y, int z) {
        if (y < -59) return Blocks.BEDROCK.defaultBlockState();
        BlockState rock = (y < 0 ? Blocks.DEEPSLATE : Blocks.STONE).defaultBlockState();
        return switch (scenario) {
            // Ocean: a floor at 40 under water.
            case 0 -> y <= 40 ? rock : y <= 62 ? Blocks.WATER.defaultBlockState() : Blocks.AIR.defaultBlockState();
            // Land at 70.
            case 1 -> y <= 70 ? rock : Blocks.AIR.defaultBlockState();
            // Land with a cave under half of the region.
            case 2 -> y > 70 ? Blocks.AIR.defaultBlockState() : y >= 35 && y <= 58 && z < 8 ? Blocks.CAVE_AIR.defaultBlockState() : rock;
            // Land near the bottom of the world.
            case 3 -> y <= -45 ? rock : Blocks.AIR.defaultBlockState();
            // A sloping sea floor.
            case 4 -> y <= 40 + Math.floorMod(x + z, 25) ? rock : y <= 62 ? Blocks.WATER.defaultBlockState() : Blocks.AIR.defaultBlockState();
            // Land with lava under part of the region.
            default -> y > 70 ? Blocks.AIR.defaultBlockState() : y >= 20 && y <= 45 && x < 8 ? Blocks.LAVA.defaultBlockState() : rock;
        };
    }

    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var storage = net.minecraft.world.level.storage.LevelStorageSource.createDefault(Path.of("fossil-worlds")).createAccess("test");
        var manager = new net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager(
            server.resources(), storage, net.minecraft.util.datafix.DataFixers.getDataFixer(), registries.lookupOrThrow(Registries.BLOCK));
        var generator = new NoiseBasedChunkGenerator(
            net.minecraft.world.level.biome.MultiNoiseBiomeSource.createFromPreset(registries.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
                .getOrThrow(net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists.OVERWORLD)),
            registries.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD));
        var unsafeClass = Class.forName("sun.misc.Unsafe");
        var unsafeField = unsafeClass.getDeclaredField("theUnsafe"); unsafeField.setAccessible(true);
        var unsafe = unsafeField.get(null);
        var allocate = unsafeClass.getMethod("allocateInstance", Class.class);
        // FossilFeature reads its templates from the server's template manager.
        var fakeServer = allocate.invoke(unsafe, net.minecraft.server.dedicated.DedicatedServer.class);
        WorldgenReferenceProbe.TestRegion.set(net.minecraft.server.MinecraftServer.class, "structureTemplateManager", fakeServer, manager);
        var features = registries.lookupOrThrow(Registries.CONFIGURED_FEATURE);
        var height = LevelHeightAccessor.create(-64, 384);
        var factory = PalettedContainerFactory.create(registries);
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (String name : new String[] {"fossil_coal", "fossil_diamonds"}) {
                ConfiguredFeature<?, ?> feature = features.getOrThrow(net.minecraft.resources.ResourceKey.create(
                    Registries.CONFIGURED_FEATURE, Identifier.withDefaultNamespace(name))).value();
                for (int scenario = 0; scenario < 6; scenario++) for (int[] origin : new int[][] {{3, 12}, {8, 8}, {14, 1}})
                    for (int y : new int[] {100, -20}) for (long seed : new long[] {0, -4172144997902289642L})
                        for (boolean legacy : new boolean[] {false, true}) {
                            var target = new ProtoChunk(new ChunkPos(0, 0), UpgradeData.EMPTY, height, factory, null);
                            var region = WorldgenReferenceProbe.TestRegion.create(registries, height, factory, target, seed);
                            region.attachGenerator(generator);
                            WorldgenReferenceProbe.TestRegion.set(net.minecraft.server.level.ServerLevel.class, "server", region.getLevel(), fakeServer);
                            var pos = new BlockPos.MutableBlockPos();
                            for (int cx = -1; cx <= 1; cx++) for (int cz = -1; cz <= 1; cz++) {
                                var chunk = (ProtoChunk) region.getChunk(cx, cz);
                                for (int x = cx * 16; x < cx * 16 + 16; x++) for (int z = cz * 16; z < cz * 16 + 16; z++)
                                    for (int by = -64; by < 320; by++) {
                                        var state = terrain(scenario, x, by, z);
                                        if (!state.isAir()) chunk.setBlockState(pos.set(x, by, z), state, 0);
                                    }
                                chunk.setPersistedStatus(ChunkStatus.CARVERS);
                            }
                            var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                            boolean placed = feature.place(region, generator, random, new BlockPos(origin[0], y, origin[1]));
                            out.println("case " + name + " " + scenario + " " + origin[0] + " " + origin[1] + " " + y + " " + seed + " "
                                + legacy + " " + placed + " " + random.nextInt());
                            for (int by = -64; by < 320; by++) for (int z = -16; z < 32; z++) for (int x = -16; x < 32; x++) {
                                var state = region.getBlockState(pos.set(x, by, z));
                                if (state != terrain(scenario, x, by, z))
                                    out.println("block " + x + " " + by + " " + z + " " + Block.getId(state));
                            }
                        }
            }
        }
        server.resources().close();
        net.minecraft.util.Util.shutdownExecutors();
    }
}
