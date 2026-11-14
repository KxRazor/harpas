// Calls vanilla's large dripstone on controlled caves; records real writes, heightmaps and random tails.
// Arguments: output, server jar, JSON configurations, seeds (comma-separated).
import java.io.*;
import java.lang.reflect.Proxy;
import java.nio.file.*;
import java.util.*;
import com.google.gson.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.LargeDripstoneConfiguration;

public final class LargeDripstoneProbe {
    static int y(int test) { return test == 22 ? -62 : test == 23 ? 317 : 64; }
    static int floor(int test) {
        return switch(test) { case 7, 8 -> 62; case 9 -> 34; case 10 -> 35;
            case 22 -> -64; case 23 -> 314; default -> 40; };
    }
    static int ceiling(int test) {
        return switch(test) { case 7 -> 66; case 8 -> 67; case 9 -> 94; case 10 -> 93;
            case 22 -> -59; case 23 -> 319; default -> 80; };
    }
    static BlockState initial(int test, int x, int y, int z) {
        if (x == 15 && y == y(test) && z == -1) {
            if (test == 17) return Blocks.STONE.defaultBlockState();
            if (test == 18) return Blocks.CAVE_AIR.defaultBlockState();
            if (test == 19) return Blocks.OAK_SLAB.defaultBlockState().setValue(
                net.minecraft.world.level.block.state.properties.BlockStateProperties.WATERLOGGED, true);
        }
        int shift = test == 15 ? Math.floorMod(Math.abs(x - 15) + Math.abs(z + 1), 7) : 0;
        int f = floor(test) - shift, c = ceiling(test) + shift;
        int depth = test == 14 ? 0 : 12;
        if (test != 1 && y >= f - depth && y <= f && (test != 16 || Math.abs(x - 15) <= 3))
            return (test == 3 ? Blocks.DIRT : test == 11 ? Blocks.LAVA : Blocks.STONE).defaultBlockState();
        if (test != 2 && y >= c && y <= c + depth && (test != 16 || Math.abs(x - 15) <= 3))
            return (test == 4 ? Blocks.DIRT : test == 12 ? Blocks.LAVA : Blocks.STONE).defaultBlockState();
        if ((test == 13 || test == 24 || test == 25) && y == 68 && (x != 15 || z != -1))
            return (test == 24 ? Blocks.DRIPSTONE_BLOCK : test == 25 ? Blocks.STONE : Blocks.OBSIDIAN).defaultBlockState();
        if (y > f && y < c) {
            if (test == 5 || test == 20 && y <= 52) return Blocks.WATER.defaultBlockState();
            if (test == 6 || test == 21 && y <= 52) return Blocks.LAVA.defaultBlockState();
        }
        return Blocks.AIR.defaultBlockState();
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.BLOCK, Registries.BLOCK, "block");
        var lookup = VanillaRegistries.createLookup();
        var biomes = new MappedRegistry<net.minecraft.world.level.biome.Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(h -> biomes.register(h.key(), h.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        var configs = JsonParser.parseString(Files.readString(Path.of(args[2]))).getAsJsonArray();
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            // Compare the profile before its integer truncation too, independently of terrain and randomness.
            var profile = DripstoneUtils.class.getDeclaredMethod("getDripstoneHeight", double.class, double.class, double.class, double.class);
            profile.setAccessible(true);
            for (int radius : new int[]{1, 2, 4, 19, 60}) for (double scale : new double[]{0, 0.4, 1, 2, 20})
                for (double bluntness : new double[]{0.1, 0.7, 1, 10}) for (int d = 0; d <= radius * 4; d++)
                    out.println("profile " + profile.invoke(null, d / 4.0, (double)radius, scale, bluntness));
            for (String s : args[3].split(",")) for (boolean legacy : new boolean[]{false, true})
                for (int config = 0; config < configs.size(); config++) for (int test = 0; test < 26; test++) {
                    long seed = Long.parseLong(s);
                    var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory,
                        new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null), seed);
                    for (int cz = -2; cz <= 1; cz++) for (int cx = -1; cx <= 2; cx++) {
                        var chunk = (ProtoChunk)region.getChunk(cx, cz);
                        for (int z = cz * 16; z < cz * 16 + 16; z++) for (int x = cx * 16; x < cx * 16 + 16; x++)
                            for (int y = Math.max(-64, floor(test) - 12); y <= Math.min(319, ceiling(test) + 12); y++) {
                                var state = initial(test, x, y, z);
                                if (!state.isAir()) chunk.setBlockState(new BlockPos(x, y, z), state, 0);
                            }
                        Heightmap.primeHeightmaps(chunk, EnumSet.allOf(Heightmap.Types.class));
                        chunk.setPersistedStatus(ChunkStatus.CARVERS);
                    }
                    var writes = new HashSet<BlockPos>();
                    var world = (WorldGenLevel)Proxy.newProxyInstance(LargeDripstoneProbe.class.getClassLoader(), new Class[]{WorldGenLevel.class}, (p, method, a) -> {
                        if (method.getName().equals("setBlock")) writes.add(((BlockPos)a[0]).immutable());
                        return method.invoke(region, a);
                    });
                    var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                    var cfg = LargeDripstoneConfiguration.CODEC.parse(com.mojang.serialization.JsonOps.INSTANCE, configs.get(config)).getOrThrow();
                    boolean placed = Feature.LARGE_DRIPSTONE.place(cfg, world, null, random, new BlockPos(15, y(test), -1));
                    out.println("case " + seed + " " + legacy + " " + config + " " + test + " " + placed + " " + random.nextInt());
                    var sorted = writes.stream().sorted(Comparator.comparingInt((BlockPos at) -> at.getX()).thenComparingInt(at -> at.getY()).thenComparingInt(at -> at.getZ())).toList();
                    for (var at : sorted) {
                        var state = region.getBlockState(at);
                        if (!state.equals(initial(test, at.getX(), at.getY(), at.getZ())))
                            out.println("block " + at.getX() + " " + at.getY() + " " + at.getZ() + " " + Block.getId(state));
                    }
                    for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG")) {
                        long hash = 0xcbf29ce484222325L;
                        for (int z = -32; z < 32; z++) for (int x = -16; x < 48; x++)
                            hash = (hash ^ region.getHeight(kind, x, z)) * 0x100000001b3L;
                        out.printf("height %016x%n", hash);
                    }
                }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
