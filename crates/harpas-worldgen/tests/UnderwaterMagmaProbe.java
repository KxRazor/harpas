// Calls vanilla's underwater magma feature on controlled floors, including search and world boundaries.
// Arguments: output file, server jar. All expected blocks and heightmaps are generated locally.
import java.io.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BlockStateProperties;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.UnderwaterMagmaConfiguration;

public final class UnderwaterMagmaProbe {
    static int floor(int test) {
        return switch (test) { case 28 -> -64; case 29 -> -63; case 30 -> 318; case 31 -> 319; case 32 -> -65; default -> 44; };
    }
    static int distance(int test) {
        return switch (test) { case 5 -> 0; case 6 -> 5; case 7, 30, 31 -> 1; case 8 -> 7; case 9 -> 8; default -> 4; };
    }
    static UnderwaterMagmaConfiguration config(int test) {
        return new UnderwaterMagmaConfiguration(test == 3 ? 0 : test == 4 ? 1 : test == 8 || test == 9 ? 8 : test == 34 ? 512 : 5,
            test == 10 ? 0 : test == 11 ? 2 : 1, test == 1 ? 0 : test == 2 || test == 35 ? 1 : 0.5f);
    }
    static BlockState wetSlab() {
        return Blocks.OAK_SLAB.defaultBlockState().setValue(BlockStateProperties.WATERLOGGED, true);
    }
    static void prepare(WorldgenReferenceProbe.TestRegion region, int test) {
        int floor = floor(test), top = floor + distance(test);
        for (int y = Math.max(-64, floor - 4); y <= Math.min(319, top + 4); y++)
            for (int z = -5; z <= 3; z++) for (int x = 11; x <= 19; x++) {
                int surface = floor + (test == 26 ? Math.floorMod(x + z, 3) - 1 : 0);
                BlockState state = (y <= surface ? Blocks.STONE : Blocks.WATER).defaultBlockState();
                if (y <= surface) {
                    state = switch (test) {
                        case 22 -> wetSlab(); case 23 -> Blocks.LAVA.defaultBlockState();
                        case 24 -> Blocks.SHORT_GRASS.defaultBlockState(); case 25 -> Blocks.MAGMA_BLOCK.defaultBlockState();
                        default -> state;
                    };
                    if (test == 20 && Math.floorMod(x * 3 + z * 7, 5) == 0) state = Blocks.WATER.defaultBlockState();
                    if (test == 21 && y == floor - 1) state = Blocks.CAVE_AIR.defaultBlockState();
                    if (test == 35 && x == 16) state = Blocks.AIR.defaultBlockState();
                } else if (test == 16) state = state.setValue(BlockStateProperties.LEVEL, 5);
                if (test == 27 || test == 34) state = Blocks.WATER.defaultBlockState();
                region.setBlock(new BlockPos(x, y, z), state, 2);
            }
        BlockState origin = switch (test) {
            case 12 -> Blocks.AIR.defaultBlockState(); case 13 -> Blocks.LAVA.defaultBlockState();
            case 14 -> wetSlab(); case 15 -> Blocks.KELP.defaultBlockState();
            case 17 -> Blocks.BUBBLE_COLUMN.defaultBlockState(); default -> null;
        };
        if (origin != null) region.setBlock(new BlockPos(15, top, -1), origin, 2);
        if (test == 18 || test == 19) region.setBlock(new BlockPos(15, top - 2, -1),
            (test == 18 ? Blocks.AIR : Blocks.KELP).defaultBlockState(), 2);
    }
    static int[] blocks(WorldgenReferenceProbe.TestRegion region, int test) {
        var values = new ArrayList<Integer>();
        for (int y = Math.max(-64, floor(test) - 4); y <= Math.min(319, floor(test) + distance(test) + 4); y++)
            for (int z = -5; z <= 3; z++) for (int x = 11; x <= 19; x++)
                values.add(Block.getId(region.getBlockState(new BlockPos(x, y, z))));
        return values.stream().mapToInt(Integer::intValue).toArray();
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        var biomes = new MappedRegistry<net.minecraft.world.level.biome.Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (long seed : new long[]{0, 42, -4172144997902289642L}) for (boolean legacy : new boolean[]{false, true})
                for (int test = 0; test < 36; test++) {
                    var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
                    var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                    for (int cx = 0; cx <= 1; cx++) for (int cz = -1; cz <= 0; cz++)
                        ((ProtoChunk)region.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                    prepare(region, test);
                    int[] before = blocks(region, test);
                    var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                    boolean placed = Feature.UNDERWATER_MAGMA.place(config(test), region, null, random,
                        new BlockPos(15, floor(test) + distance(test), -1));
                    out.println("case " + seed + " " + legacy + " " + test + " " + placed + " " + random.nextInt());
                    int[] after = blocks(region, test);
                    for (int i = 0; i < after.length; i++) if (before[i] != after[i]) out.println("block " + i + " " + after[i]);
                    for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG"))
                        for (int z = -5; z <= 3; z++) for (int x = 11; x <= 19; x++)
                            out.println("height " + region.getHeight(kind, x, z));
                }
            // Isolate the destination and each of its five closing neighbors over every block state.
            var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
            var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, 42);
            prepare(region, 0);
            var center = new BlockPos(15, 44, -1);
            var offsets = new int[][]{{0, 0, 0}, {0, -1, 0}, {0, 0, -1}, {1, 0, 0}, {0, 0, 1}, {-1, 0, 0}};
            for (int role = 0; role < offsets.length; role++) {
                var offset = offsets[role];
                var at = center.offset(offset[0], offset[1], offset[2]);
                for (var state : Block.BLOCK_STATE_REGISTRY) {
                    region.setBlock(center, Blocks.STONE.defaultBlockState(), 2);
                    region.setBlock(at, state, 2);
                    boolean placed = Feature.UNDERWATER_MAGMA.place(new UnderwaterMagmaConfiguration(5, 0, 1), region, null,
                        new WorldgenRandom(new XoroshiroRandomSource(42)), center.above(4));
                    out.println("material " + role + " " + Block.getId(state) + " " + placed);
                    region.setBlock(at, Blocks.STONE.defaultBlockState(), 2);
                }
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
