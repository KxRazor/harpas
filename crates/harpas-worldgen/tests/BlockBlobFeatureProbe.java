// Black-box calls to vanilla's block_blob feature. Expected blocks and random tails are generated locally.
// Arguments: output file, server jar. No implementation or game data is extracted into the repository.
import java.io.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.blockpredicates.BlockPredicate;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.BlockBlobConfiguration;

public final class BlockBlobFeatureProbe {
    static int y(int test) {
        return test == 24 ? -61 : test == 25 ? -60 : test == 26 ? 319 : test == 27 ? 320 : 64;
    }

    static void prepare(WorldgenReferenceProbe.TestRegion region, int test, int y) {
        if (test == 31) return;
        for (int z = -8; z <= 6; z++) for (int x = 8; x <= 22; x++) {
            int top = y - 2 - Math.floorMod(x + z, 3);
            region.setBlock(new BlockPos(x, top, z), Blocks.DIRT.defaultBlockState(), 2);
            region.setBlock(new BlockPos(x, top + 1, z), Blocks.WATER.defaultBlockState(), 2);
        }
        Block[] supports = {Blocks.DIRT, Blocks.GRASS_BLOCK, Blocks.PODZOL, Blocks.COARSE_DIRT,
            Blocks.ROOTED_DIRT, Blocks.MUD, Blocks.MUDDY_MANGROVE_ROOTS, Blocks.MOSS_BLOCK,
            Blocks.PALE_MOSS_BLOCK, Blocks.MYCELIUM, Blocks.STONE, Blocks.SAND, Blocks.GRAVEL,
            Blocks.WATER, Blocks.LAVA, Blocks.SNOW, Blocks.CAVE_AIR, Blocks.VOID_AIR, Blocks.AIR, Blocks.BEDROCK};
        region.setBlock(new BlockPos(15, y - 1, -1), (test < 20 ? supports[test] : Blocks.DIRT).defaultBlockState(), 2);
        region.setBlock(new BlockPos(16, y - 1, -1), Blocks.SAND.defaultBlockState(), 2);
        if (test == 20) region.setBlock(new BlockPos(15, y + 4, -1), Blocks.STONE.defaultBlockState(), 2);
        if (test == 21 || test == 22) region.setBlock(new BlockPos(15, y, -1),
            (test == 21 ? Blocks.WATER : Blocks.BEDROCK).defaultBlockState(), 2);
        if (test == 23) region.setBlock(new BlockPos(15, y + 1, -1), Blocks.CHEST.defaultBlockState(), 2);
        if (test == 28) region.setBlock(new BlockPos(15, y - 1, -1), Blocks.AIR.defaultBlockState(), 2);
        if (test == 29) region.setBlock(new BlockPos(15, y + 4, -1), Blocks.SAND.defaultBlockState(), 2);
        if (test == 30) region.setBlock(new BlockPos(15, y + 4, -1), Blocks.WATER.defaultBlockState(), 2);
    }

    static int[] blocks(WorldgenReferenceProbe.TestRegion region, int y) {
        var values = new ArrayList<Integer>();
        for (int py = Math.max(-64, y - 8); py < Math.min(320, y + 10); py++)
            for (int z = -8; z <= 6; z++) for (int x = 8; x <= 22; x++)
                values.add(Block.getId(region.getBlockState(new BlockPos(x, py, z))));
        return values.stream().mapToInt(Integer::intValue).toArray();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.BLOCK, Registries.BLOCK, "block");
        var lookup = VanillaRegistries.createLookup();
        var biomes = new MappedRegistry<net.minecraft.world.level.biome.Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(
            holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        var vanilla = lookup.lookupOrThrow(Registries.CONFIGURED_FEATURE).listElements()
            .filter(holder -> holder.key().identifier().toString().equals("minecraft:forest_rock")).findFirst().orElseThrow().value();
        var configurations = new ArrayList<ConfiguredFeature<?, ?>>();
        configurations.add(vanilla);
        for (var predicate : List.of(BlockPredicate.alwaysTrue(),
                BlockPredicate.matchesBlocks(new Vec3i(1, 0, 0), Blocks.SAND),
                BlockPredicate.not(BlockPredicate.alwaysTrue()), BlockPredicate.matchesBlocks(Blocks.SAND))) {
            configurations.add(new ConfiguredFeature<>(Feature.BLOCK_BLOB,
                new BlockBlobConfiguration(Blocks.OAK_LOG.defaultBlockState().setValue(RotatedPillarBlock.AXIS, Direction.Axis.X), predicate)));
        }
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (long seed : new long[] {0, 42, -4172144997902289642L}) for (boolean legacy : new boolean[] {false, true}) {
                for (int config = 0; config < configurations.size(); config++) for (int test = 0; test < 32; test++) {
                    int y = y(test);
                    var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
                    var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                    for (int cx = -1; cx <= 1; cx++) for (int cz = -2; cz <= 0; cz++)
                        ((ProtoChunk) region.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                    prepare(region, test, y);
                    int[] before = blocks(region, y);
                    var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                    boolean placed = configurations.get(config).place(region, null, random,
                        new BlockPos(15, y + (test == 20 || test == 29 || test == 30 ? 8 : 0), -1));
                    out.println("case " + seed + " " + legacy + " " + config + " " + test + " " + placed + " " + random.nextInt());
                    int[] after = blocks(region, y);
                    for (int i = 0; i < after.length; i++) if (before[i] != after[i]) out.println(i + " " + after[i]);
                }
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
