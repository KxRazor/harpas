// Black-box calls to vanilla's desert well feature on sand, unsupported terrain, chunk edges and world limits.
// Arguments: output file, server jar. Expected blocks and block entity NBT are produced only on this machine.
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
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.FeatureConfiguration;

public final class DesertWellFeatureProbe {
    static int y(int test) {
        return test >= 22 && test <= 25 ? test - 85 : test == 26 ? 318 : test == 27 || test == 28 ? 319 : 64;
    }

    static int originY(int test, int y) {
        return y + (test == 1 || test == 20 || test == 21 ? 8 : test == 2 || test == 28 ? 1 : test == 3 ? -1 : 0);
    }

    static void prepare(WorldgenReferenceProbe.TestRegion region, int test, int y) {
        if (test == 10) return;
        for (int z = -4; z <= 2; z++) for (int x = 12; x <= 18; x++) for (int py = y - 3; py <= y; py++)
            region.setBlock(new BlockPos(x, py, z), Blocks.SAND.defaultBlockState(), 2);
        Block[] supports = {Blocks.RED_SAND, Blocks.SANDSTONE, Blocks.DIRT, Blocks.WATER, Blocks.SUSPICIOUS_SAND, Blocks.GRAVEL};
        if (test >= 4 && test <= 9) region.setBlock(new BlockPos(15, y, -1), supports[test - 4].defaultBlockState(), 2);
        if (test >= 11 && test <= 16) {
            int x = test == 16 ? 12 : 13;
            region.setBlock(new BlockPos(x, y - 1, -3), (test == 13 ? Blocks.CAVE_AIR : test == 14 ? Blocks.SAND : Blocks.AIR).defaultBlockState(), 2);
            region.setBlock(new BlockPos(x, y - 2, -3), (test == 11 ? Blocks.SAND : test == 13 ? Blocks.VOID_AIR : test == 15 ? Blocks.WATER : Blocks.AIR).defaultBlockState(), 2);
        }
        if (test >= 17 && test <= 21) {
            Block[] above = {Blocks.STONE, Blocks.SHORT_GRASS, Blocks.WATER, Blocks.CAVE_AIR, Blocks.VOID_AIR};
            region.setBlock(new BlockPos(15, y + 1, -1), above[test - 17].defaultBlockState(), 2);
        }
        if (test == 29) {
            region.setBlock(new BlockPos(17, y + 1, -1), Blocks.CHEST.defaultBlockState(), 2);
            region.setBlock(new BlockPos(18, y + 2, -1), Blocks.CHEST.defaultBlockState(), 2);
            region.setBlock(new BlockPos(14, y + 2, -2), Blocks.OAK_LOG.defaultBlockState(), 2);
        }
        if (test == 30) for (int py = y - 3; py < y; py++)
            region.setBlock(new BlockPos(15, py, -1), Blocks.AIR.defaultBlockState(), 2);
        if (test == 31) for (int z = -3; z <= 1; z++) for (int x = 13; x <= 17; x++)
            region.setBlock(new BlockPos(x, y - 1, z), (Math.floorMod(x + z, 2) == 0 ? Blocks.AIR : Blocks.WATER).defaultBlockState(), 2);
    }

    static int[] blocks(WorldgenReferenceProbe.TestRegion region, int y) {
        var values = new ArrayList<Integer>();
        for (int py = Math.max(-64, y - 4); py < Math.min(320, y + 10); py++)
            for (int z = -4; z <= 2; z++) for (int x = 12; x <= 18; x++)
                values.add(Block.getId(region.getBlockState(new BlockPos(x, py, z))));
        return values.stream().mapToInt(Integer::intValue).toArray();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        var biomes = new MappedRegistry<net.minecraft.world.level.biome.Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(
            holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        var feature = new ConfiguredFeature<>(Feature.DESERT_WELL, FeatureConfiguration.NONE);
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (long seed : new long[] {0, 1, 3, 42, -4172144997902289642L}) for (boolean legacy : new boolean[] {false, true}) {
                for (int test = 0; test < 32; test++) {
                    int y = y(test);
                    var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
                    var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                    for (int cx = 0; cx <= 1; cx++) for (int cz = -1; cz <= 0; cz++)
                        ((ProtoChunk) region.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                    prepare(region, test, y);
                    int[] before = blocks(region, y);
                    var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                    boolean placed = feature.place(region, null, random, new BlockPos(15, originY(test, y), -1));
                    out.println("case " + seed + " " + legacy + " " + test + " " + placed + " " + random.nextInt());
                    int[] after = blocks(region, y);
                    for (int i = 0; i < after.length; i++) if (before[i] != after[i]) out.println("block " + i + " " + after[i]);
                    for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG"))
                        for (int z = -4; z <= 2; z++) for (int x = 12; x <= 18; x++)
                            out.println("height " + region.getHeight(kind, x, z));
                    for (var chunk : region.chunks.values()) for (var at : chunk.getBlockEntitiesPos()) {
                        var bytes = new ByteArrayOutputStream();
                        net.minecraft.nbt.NbtIo.writeAnyTag(chunk.getBlockEntityNbtForSaving(at, access), new DataOutputStream(bytes));
                        out.println("entity " + HexFormat.of().formatHex(bytes.toByteArray()));
                    }
                }
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
