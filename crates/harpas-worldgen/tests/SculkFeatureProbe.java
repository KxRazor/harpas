// Test driver only: calls vanilla's own sculk patch and sculk vein features in test caves and writes what they change.
// Arguments: <output file>. No game code or data is copied into the repository.
//
// Each case decorates chunk (0, 0) of a region holding it and its eight neighbors, filled with one of the terrains
// below, with one feature at one origin. The output holds whether it placed, the random's next int afterwards and
// every block of the region below Y 0 that differs from the terrain.
import java.io.*;
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

public final class SculkFeatureProbe {
    // Kept in step with sculk_feature_parity.rs.
    static BlockState terrain(int scenario, int x, int y, int z) {
        if (y >= 0) return Blocks.AIR.defaultBlockState();
        if (y < -59) return Blocks.BEDROCK.defaultBlockState();
        boolean cave = x >= -12 && x <= 28 && z >= -12 && z <= 28 && y >= -40 && y <= -30;
        if (cave) {
            if (scenario == 1 && y <= -39) return Blocks.WATER.defaultBlockState();
            if (scenario == 3 && Math.floorMod(x, 4) == 0 && Math.floorMod(z, 4) == 0) return Blocks.DEEPSLATE.defaultBlockState();
            if (scenario == 5 && y == -40 && Math.floorMod(x * 7 + z * 3, 11) == 0) return Blocks.SCULK_SENSOR.defaultBlockState();
            return Blocks.AIR.defaultBlockState();
        }
        if (scenario == 2 && y == -41) {
            Block[] floors = {Blocks.DEEPSLATE, Blocks.TUFF, Blocks.DIRT, Blocks.COBBLESTONE, Blocks.STONE, Blocks.GRAVEL};
            return floors[Math.floorMod(x + 2 * z, floors.length)].defaultBlockState();
        }
        if (scenario == 5 && y == -41 && Math.floorMod(x + z, 5) == 0) return Blocks.SCULK.defaultBlockState();
        return Blocks.DEEPSLATE.defaultBlockState();
    }

    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var generator = new NoiseBasedChunkGenerator(
            net.minecraft.world.level.biome.MultiNoiseBiomeSource.createFromPreset(registries.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
                .getOrThrow(net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists.OVERWORLD)),
            registries.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD));
        var features = registries.lookupOrThrow(Registries.CONFIGURED_FEATURE);
        var height = LevelHeightAccessor.create(-64, 384);
        var factory = PalettedContainerFactory.create(registries);
        try (var out = new ProbeOutput(args[0])) {
            for (String name : new String[] {"sculk_patch_deep_dark", "sculk_patch_ancient_city", "sculk_vein"}) {
                ConfiguredFeature<?, ?> feature = features.getOrThrow(net.minecraft.resources.ResourceKey.create(
                    Registries.CONFIGURED_FEATURE, Identifier.withDefaultNamespace(name))).value();
                for (int scenario = 0; scenario < 6; scenario++)
                    for (int[] origin : new int[][] {{8, -40, 8}, {3, -40, 12}, {8, -35, 8}, {8, -41, 8}})
                        for (long seed : new long[] {0, -4172144997902289642L}) for (boolean legacy : new boolean[] {false, true}) {
                            var target = new ProtoChunk(new ChunkPos(0, 0), UpgradeData.EMPTY, height, factory, null);
                            var region = WorldgenReferenceProbe.TestRegion.create(registries, height, factory, target, seed);
                            var pos = new BlockPos.MutableBlockPos();
                            for (int cx = -1; cx <= 1; cx++) for (int cz = -1; cz <= 1; cz++) {
                                var chunk = (ProtoChunk) region.getChunk(cx, cz);
                                for (int x = cx * 16; x < cx * 16 + 16; x++) for (int z = cz * 16; z < cz * 16 + 16; z++)
                                    for (int y = -64; y < 0; y++) {
                                        var state = terrain(scenario, x, y, z);
                                        if (!state.isAir()) chunk.setBlockState(pos.set(x, y, z), state, 0);
                                    }
                                chunk.setPersistedStatus(ChunkStatus.CARVERS);
                            }
                            var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                            boolean placed = feature.place(region, generator, random, new BlockPos(origin[0], origin[1], origin[2]));
                            out.println("case " + name + " " + scenario + " " + origin[0] + " " + origin[1] + " " + origin[2] + " "
                                + seed + " " + legacy + " " + placed + " " + random.nextInt());
                            for (int y = -64; y < 0; y++) for (int z = -16; z < 32; z++) for (int x = -16; x < 32; x++) {
                                var state = region.getBlockState(pos.set(x, y, z));
                                if (state != terrain(scenario, x, y, z))
                                    out.println("block " + x + " " + y + " " + z + " " + Block.getId(state));
                            }
                        }
            }
        }
        server.resources().close();
        net.minecraft.util.Util.shutdownExecutors();
    }
}
