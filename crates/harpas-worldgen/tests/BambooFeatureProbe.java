// Calls vanilla's bamboo feature on uneven ground crossing chunk edges. Compares blocked and unsupported
// origins, stalks cut short at each height, podzol probabilities, and the world's upper and lower limits.
// Arguments: output file, server jar. All data and expected results come from the owner's local server.
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
import net.minecraft.world.level.levelgen.feature.configurations.ProbabilityFeatureConfiguration;

public final class BambooFeatureProbe {
    static void prepare(WorldgenReferenceProbe.TestRegion region, int test, int y) {
        Block[] ground = {Blocks.DIRT, Blocks.MUD, Blocks.SAND, Blocks.STONE,
            Blocks.GRASS_BLOCK, Blocks.MYCELIUM, Blocks.MOSS_BLOCK, Blocks.GRAVEL};
        for (int z = -6; z <= 4; z++) {
            for (int x = 10; x <= 20; x++) {
                int top = y - 1 - Math.floorMod(x + z, 3);
                int kind = Math.floorMod(x * 3 + z * 7, ground.length);
                region.setBlock(new BlockPos(x, top, z), ground[kind].defaultBlockState(), 2);
                if (kind == 0) region.setBlock(new BlockPos(x, top + 1, z), Blocks.SHORT_GRASS.defaultBlockState(), 2);
            }
        }
        var support = test == 18 ? Blocks.STONE : test == 19 ? Blocks.BAMBOO
            : test == 20 ? Blocks.BAMBOO_SAPLING : Blocks.DIRT;
        region.setBlock(new BlockPos(15, y - 1, -1), support.defaultBlockState(), 2);
        var at = test == 21 ? Blocks.WATER : test == 22 ? Blocks.SHORT_GRASS
            : test == 23 ? Blocks.CAVE_AIR : test == 24 ? Blocks.VOID_AIR : Blocks.AIR;
        region.setBlock(new BlockPos(15, y, -1), at.defaultBlockState(), 2);
        if (test < 17) region.setBlock(new BlockPos(15, y + test, -1), Blocks.STONE.defaultBlockState(), 2);
    }

    static int[] blocks(WorldgenReferenceProbe.TestRegion region, int y) {
        var values = new ArrayList<Integer>();
        for (int py = Math.max(-64, y - 3); py < Math.min(320, y + 17); py++)
            for (int z = -6; z <= 4; z++)
                for (int x = 10; x <= 20; x++)
                    values.add(Block.getId(region.getBlockState(new BlockPos(x, py, z))));
        return values.stream().mapToInt(Integer::intValue).toArray();
    }

    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
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
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (long seed : new long[] {0, 42, -4172144997902289642L}) {
                for (float probability : new float[] {0, 0.2f, 1}) {
                    var feature = new ConfiguredFeature<>(Feature.BAMBOO, new ProbabilityFeatureConfiguration(probability));
                    for (int test = 0; test < 27; test++) {
                        int y = test == 25 ? 319 : test == 26 ? -63 : 64;
                        var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
                        proto.setPersistedStatus(ChunkStatus.CARVERS);
                        var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                        for (int cx = -1; cx <= 1; cx++) for (int cz = -2; cz <= 0; cz++)
                            ((ProtoChunk) region.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                        prepare(region, test, y);
                        int[] before = blocks(region, y);
                        var random = new WorldgenRandom(new XoroshiroRandomSource(seed));
                        boolean placed = feature.place(region, null, random, new BlockPos(15, y, -1));
                        out.println("case " + seed + " " + probability + " " + test + " " + placed + " " + random.nextInt());
                        int[] after = blocks(region, y);
                        for (int i = 0; i < after.length; i++)
                            if (before[i] != after[i]) out.println(i + " " + after[i]);
                    }
                }
            }
            var proto = new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null);
            var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, 0);
            for (var state : Block.BLOCK_STATE_REGISTRY) {
                region.setBlock(new BlockPos(15, 63, -1), state, 2);
                out.println("support " + Block.getId(state) + " "
                    + Blocks.BAMBOO.defaultBlockState().canSurvive(region, new BlockPos(15, 64, -1)) + " "
                    + Blocks.BAMBOO_SAPLING.defaultBlockState().canSurvive(region, new BlockPos(15, 64, -1)));
            }
            var stalks = new ArrayList<>(Blocks.BAMBOO.getStateDefinition().getPossibleStates());
            stalks.add(Blocks.BAMBOO_SAPLING.defaultBlockState());
            var pos = new BlockPos(15, 64, -1);
            for (var state : stalks) for (var support : new Block[] {Blocks.AIR, Blocks.DIRT}) {
                for (var neighbor : new net.minecraft.world.level.block.state.BlockState[] {
                        Blocks.AIR.defaultBlockState(), Blocks.BAMBOO.defaultBlockState(),
                        Blocks.BAMBOO.defaultBlockState().setValue(BambooStalkBlock.AGE, 1),
                        Blocks.BAMBOO_SAPLING.defaultBlockState(), Blocks.STONE.defaultBlockState()}) {
                    for (var direction : Direction.values()) {
                        for (var clear : Direction.values()) region.setBlock(pos.relative(clear), Blocks.AIR.defaultBlockState(), 2);
                        region.setBlock(pos.below(), support.defaultBlockState(), 2);
                        region.setBlock(pos, state, 2);
                        region.setBlock(pos.relative(direction), neighbor, 2);
                        var updated = state.updateShape(region, region, pos, direction, pos.relative(direction), neighbor, new XoroshiroRandomSource(0));
                        out.println("shape " + Block.getId(state) + " " + Block.getId(support.defaultBlockState()) + " "
                            + Block.getId(neighbor) + " " + direction.ordinal() + " " + Block.getId(updated));
                    }
                }
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
