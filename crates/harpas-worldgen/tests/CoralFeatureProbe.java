// Calls vanilla's three reef features with both random sources, obstacles, chunk edges and world limits.
// Arguments: output file, server jar. Expected blocks and live heightmaps are generated locally.
import java.io.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.SimpleRandomFeatureConfiguration;

public final class CoralFeatureProbe {
    static int y(int test) {
        return switch (test) { case 20 -> -64; case 21 -> -63; case 22 -> 318; case 23 -> 319; case 24 -> 320; case 25 -> -65; default -> 48; };
    }

    static void prepare(WorldgenReferenceProbe.TestRegion region, int test, int y) {
        BlockState fill = (test == 1 ? Blocks.AIR : test == 2 ? Blocks.LAVA : Blocks.WATER).defaultBlockState();
        if (test == 3) fill = fill.setValue(net.minecraft.world.level.block.state.properties.BlockStateProperties.LEVEL, 5);
        if (test == 4) fill = Blocks.OAK_SLAB.defaultBlockState().setValue(net.minecraft.world.level.block.state.properties.BlockStateProperties.WATERLOGGED, true);
        for (int py = Math.max(-64, y - 4); py < Math.min(320, y + 12); py++)
            for (int z = -10; z <= 8; z++) for (int x = 6; x <= 24; x++) {
                var state = fill;
                if (test == 16 && py == y + 3) state = Blocks.STONE.defaultBlockState();
                if (test == 17 && Math.floorMod(x * 3 + py * 5 + z * 7, 5) == 0) state = Blocks.STONE.defaultBlockState();
                if (test == 18 && py >= y + 2) state = Blocks.AIR.defaultBlockState();
                if (test == 19 && Math.floorMod(x * 3 + py * 5 + z * 7, 11) == 0) state = Blocks.BRAIN_CORAL.defaultBlockState();
                region.setBlock(new BlockPos(x, py, z), state, 2);
            }
        Block[] origins = {Blocks.TUBE_CORAL, Blocks.TUBE_CORAL_FAN, Blocks.TUBE_CORAL_WALL_FAN, Blocks.TUBE_CORAL_BLOCK,
            Blocks.DEAD_TUBE_CORAL, Blocks.KELP, Blocks.SEAGRASS, Blocks.STONE};
        if (test >= 5 && test <= 12) region.setBlock(new BlockPos(15, y, -1), origins[test - 5].defaultBlockState(), 2);
        if (test >= 13 && test <= 15) region.setBlock(new BlockPos(15, y + 1, -1),
            (test == 13 ? Blocks.AIR : test == 14 ? Blocks.TUBE_CORAL : Blocks.SEAGRASS).defaultBlockState(), 2);
    }

    static int[] blocks(WorldgenReferenceProbe.TestRegion region, int y) {
        var values = new ArrayList<Integer>();
        for (int py = Math.max(-64, y - 4); py < Math.min(320, y + 12); py++)
            for (int z = -10; z <= 8; z++) for (int x = 6; x <= 24; x++)
                values.add(Block.getId(region.getBlockState(new BlockPos(x, py, z))));
        return values.stream().mapToInt(Integer::intValue).toArray();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.BLOCK, Registries.BLOCK, "block");
        var lookup = VanillaRegistries.createLookup();
        WorldgenReferenceProbe.loadConfiguredFeaturesFromDataPack(lookup);
        var biomes = new MappedRegistry<net.minecraft.world.level.biome.Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        var vegetation = lookup.getOrThrow(ResourceKey.create(Registries.CONFIGURED_FEATURE, Identifier.parse("minecraft:warm_ocean_vegetation"))).value();
        var features = ((SimpleRandomFeatureConfiguration) vegetation.config()).features.stream()
            .map(holder -> holder.value().feature().value()).toList();
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (long seed : new long[] {0, 42, -4172144997902289642L}) for (boolean legacy : new boolean[] {false, true})
                for (int shape = 0; shape < features.size(); shape++) for (int test = 0; test < 26; test++) {
                    int y = y(test);
                    var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
                    var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                    for (int cx = 0; cx <= 1; cx++) for (int cz = -1; cz <= 0; cz++)
                        ((ProtoChunk) region.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                    prepare(region, test, y);
                    int[] before = blocks(region, y);
                    var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                    boolean placed = features.get(shape).place(region, null, random, new BlockPos(15, y, -1));
                    out.println("case " + seed + " " + legacy + " " + shape + " " + test + " " + placed + " " + random.nextInt());
                    int[] after = blocks(region, y);
                    for (int i = 0; i < after.length; i++) if (before[i] != after[i]) out.println("block " + i + " " + after[i]);
                    for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG"))
                        for (int z = -10; z <= 8; z++) for (int x = 6; x <= 24; x++)
                            out.println("height " + region.getHeight(kind, x, z));
                }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
