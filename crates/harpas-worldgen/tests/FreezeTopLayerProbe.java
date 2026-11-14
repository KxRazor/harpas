// Calls vanilla's feature on mixed surface columns in every biome, at temperature and world boundaries.
// Expected block IDs, heightmaps, snow support and random tails are produced locally, never stored as fixtures.
import java.io.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BlockStateProperties;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.FeatureConfiguration;

public final class FreezeTopLayerProbe {
    static final int[] HEIGHTS = {-65, -64, 62, 79, 80, 127, 255, 318, 319};
    static BlockState material(int index) {
        return switch (index % 24) {
            case 0 -> Blocks.GRASS_BLOCK.defaultBlockState();
            case 1 -> Blocks.PODZOL.defaultBlockState();
            case 2 -> Blocks.MYCELIUM.defaultBlockState();
            case 3 -> Blocks.WATER.defaultBlockState();
            case 4 -> Blocks.WATER.defaultBlockState().setValue(BlockStateProperties.LEVEL, 1);
            case 5 -> Blocks.WATER.defaultBlockState().setValue(BlockStateProperties.LEVEL, 8);
            case 6 -> Blocks.BUBBLE_COLUMN.defaultBlockState();
            case 7 -> Blocks.OAK_SLAB.defaultBlockState().setValue(BlockStateProperties.WATERLOGGED, true);
            case 8 -> Blocks.OAK_LEAVES.defaultBlockState();
            case 9 -> Blocks.ICE.defaultBlockState();
            case 10 -> Blocks.PACKED_ICE.defaultBlockState();
            case 11 -> Blocks.BLUE_ICE.defaultBlockState();
            case 12 -> Blocks.HONEY_BLOCK.defaultBlockState();
            case 13 -> Blocks.SOUL_SAND.defaultBlockState();
            case 14 -> Blocks.MUD.defaultBlockState();
            case 15 -> Blocks.SNOW.defaultBlockState().setValue(BlockStateProperties.LAYERS, 8);
            case 16 -> Blocks.SNOW.defaultBlockState().setValue(BlockStateProperties.LAYERS, 7);
            case 17 -> Blocks.BARRIER.defaultBlockState();
            case 18 -> Blocks.GLASS.defaultBlockState();
            case 19 -> Blocks.OAK_SLAB.defaultBlockState();
            case 20 -> Blocks.SHORT_GRASS.defaultBlockState();
            case 21 -> Blocks.LAVA.defaultBlockState();
            case 22 -> Blocks.GLOWSTONE.defaultBlockState();
            default -> Blocks.STONE.defaultBlockState();
        };
    }
    static long hash(long hash, int value) { return (hash ^ Integer.toUnsignedLong(value)) * 0x100000001b3L; }
    static String snapshot(WorldgenReferenceProbe.TestRegion world, int ground) {
        long blocks = 0xcbf29ce484222325L, heights = blocks;
        var pos = new BlockPos.MutableBlockPos();
        for (int y = Math.max(-64, ground - 1); y <= Math.min(319, ground + 3); y++)
            for (int z = -2; z <= 15; z++) for (int x = 14; x <= 31; x++)
                blocks = hash(blocks, Block.getId(world.getBlockState(pos.set(x, y, z))));
        for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG"))
            for (int z = -2; z <= 15; z++) for (int x = 14; x <= 31; x++)
                heights = hash(heights, world.getHeight(kind, x, z));
        return Long.toUnsignedString(blocks) + " " + Long.toUnsignedString(heights);
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.BLOCK, Registries.BLOCK, "block");
        var lookup = VanillaRegistries.createLookup();
        var biomes = new MappedRegistry<Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(h -> biomes.register(h.key(), h.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (var biome : biomes.listElements().sorted(Comparator.comparing(h -> h.key().identifier().toString())).toList())
                for (int ground : HEIGHTS) {
                    var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
                    var world = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, 42);
                    world.biomes = new BiomeManager((x, y, z) -> biome, BiomeManager.obfuscateSeed(42));
                    for (int cx = 0; cx <= 1; cx++) for (int cz = -1; cz <= 0; cz++)
                        ((ProtoChunk)world.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                    for (int z = -2; z <= 15; z++) for (int x = 14; x <= 31; x++) {
                        int index = (x - 14) + 18 * (z + 2);
                        if (ground >= -64) world.setBlock(new BlockPos(x, ground, z), material(index), 2);
                        if (ground >= -64 && ground < 319 && index % 7 == 0)
                            world.setBlock(new BlockPos(x, ground + 1, z), Blocks.SHORT_GRASS.defaultBlockState(), 2);
                    }
                    var random = new WorldgenRandom(new XoroshiroRandomSource(42));
                    boolean result = Feature.FREEZE_TOP_LAYER.place(FeatureConfiguration.NONE, world, null, random, new BlockPos(15, -1000, -1));
                    out.println("case " + biome.key().identifier() + " " + ground + " " + result + " " + random.nextInt() + " " + snapshot(world, ground));
                    // A second pass exercises existing snow, snow over snow, and already frozen water.
                    result = Feature.FREEZE_TOP_LAYER.place(FeatureConfiguration.NONE, world, null, random, new BlockPos(15, 1000, -1));
                    out.println("replay " + result + " " + random.nextInt() + " " + snapshot(world, ground));
                }
            var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
            var world = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, 42);
            for (var state : Block.BLOCK_STATE_REGISTRY) {
                world.setBlock(new BlockPos(15, 63, -1), state, 2);
                out.println("support " + Block.getId(state) + " " + Blocks.SNOW.defaultBlockState().canSurvive(world, new BlockPos(15, 64, -1)));
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
