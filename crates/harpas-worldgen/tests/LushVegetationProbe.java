// Calls the owner's vanilla features on controlled floors; no game code or generated fixtures are stored.
// Arguments: output, server jar, comma-separated seeds, custom configurations. Also checks survival and shapes.
import java.io.*;
import java.util.*;
import java.nio.file.*;
import com.google.gson.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.*;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.ConfiguredFeature;

public final class LushVegetationProbe {
    static final String[] FEATURES = {"clay_with_dripleaves", "clay_pool_with_dripleaves", "lush_caves_clay", "dripleaf"};
    static int y(int test) {
        return switch (test) { case 24 -> -64; case 25 -> -63; case 26 -> -60;
            case 27 -> 317; case 28 -> 319; case 29 -> 320; default -> 64; };
    }
    static BlockState initial(int test, int dx, int dy, int dz) {
        if (test == 33) return initial(0, dx, -dy, dz);
        int ground = test == 13 ? Math.floorMod(dx * 3 + dz * 7, 7) - 4 : -1;
        if (test == 14) ground = -4;
        if (test == 15) ground = 2;
        if (test == 16) ground = -7;
        if (dy <= ground) {
            if (test == 5 || test == 8 && dy == -1 && Math.floorMod(dx + dz, 4) == 0
                    || test == 9 && dy < -1 || test == 10 && dx == 0) return Blocks.AIR.defaultBlockState();
            if (test == 11) return Blocks.GLASS.defaultBlockState();
            if (test == 12 && dy == ground) return Blocks.OAK_SLAB.defaultBlockState().setValue(BlockStateProperties.SLAB_TYPE, SlabType.TOP);
            if (test == 17 && dy < -2) return Blocks.WATER.defaultBlockState();
            if (test == 18 && dy < -2) return Blocks.LAVA.defaultBlockState();
            return (switch (test) { case 1, 6, 7, 19, 20, 21, 22, 23 -> Blocks.CLAY;
                case 2 -> Blocks.MOSS_BLOCK; case 3, 30, 31 -> Blocks.DIRT;
                case 4 -> Blocks.GRASS_BLOCK; default -> Blocks.STONE; }).defaultBlockState();
        }
        if (dy == 0 || dy == 1 && test == 7) {
            if (test == 6 || test == 7 || test == 30) return Blocks.WATER.defaultBlockState();
            if (test == 19 || test == 31) return Blocks.WATER.defaultBlockState().setValue(BlockStateProperties.LEVEL, 1);
            if (test == 20) return Blocks.LAVA.defaultBlockState();
            if (test == 21) return Blocks.SMALL_DRIPLEAF.defaultBlockState();
            if (test == 22) return Blocks.BIG_DRIPLEAF.defaultBlockState();
            if (test == 23) return Blocks.BIG_DRIPLEAF_STEM.defaultBlockState();
        }
        if (test == 29 && dy >= 0 || test == 32 && dy == 1) return Blocks.STONE.defaultBlockState();
        return Blocks.AIR.defaultBlockState();
    }
    static WorldgenReferenceProbe.TestRegion world(RegistryAccess access, PalettedContainerFactory factory) throws Exception {
        var height = LevelHeightAccessor.create(-64, 384);
        var world = WorldgenReferenceProbe.TestRegion.create(access, height, factory,
            new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null), 0);
        for (int cx = 0; cx <= 1; cx++) for (int cz = -1; cz <= 0; cz++)
            ((ProtoChunk)world.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
        return world;
    }
    static int[] blocks(WorldgenReferenceProbe.TestRegion world, int y) {
        var values = new int[25 * 25 * 17];
        var pos = new BlockPos.MutableBlockPos();
        int i = 0;
        for (int dy = -9; dy <= 7; dy++) for (int dz = -12; dz <= 12; dz++) for (int dx = -12; dx <= 12; dx++)
            values[i++] = Block.getId(world.getBlockState(pos.set(15 + dx, y + dy, -1 + dz)));
        return values;
    }
    static void behavior(PrintWriter out, RegistryAccess access, PalettedContainerFactory factory) throws Exception {
        var world = world(access, factory);
        var pos = new BlockPos(15, 64, -1);
        var plants = new ArrayList<BlockState>(List.of(Blocks.SMALL_DRIPLEAF.defaultBlockState(),
            Blocks.SMALL_DRIPLEAF.defaultBlockState().setValue(BlockStateProperties.DOUBLE_BLOCK_HALF, DoubleBlockHalf.UPPER),
            Blocks.BIG_DRIPLEAF.defaultBlockState(), Blocks.BIG_DRIPLEAF_STEM.defaultBlockState()));
        for (var plant : List.copyOf(plants)) {
            var wet = plant.setValue(BlockStateProperties.WATERLOGGED, true).setValue(BlockStateProperties.HORIZONTAL_FACING, Direction.EAST);
            if (wet.is(Blocks.BIG_DRIPLEAF)) wet = wet.setValue(BlockStateProperties.TILT, Tilt.FULL);
            plants.add(wet);
        }
        for (var neighbor : Block.BLOCK_STATE_REGISTRY) {
            out.print("behavior " + Block.getId(neighbor));
            for (var plant : plants) {
                int mask = 0;
                for (int context = 0; context < 4; context++) {
                    world.setBlock(pos, context == 1 ? Blocks.WATER.defaultBlockState() : plant, 2);
                    world.setBlock(pos.below(), context == 2 ? Blocks.CLAY.defaultBlockState() : neighbor, 2);
                    world.setBlock(pos.above(), context == 2 ? neighbor : context == 3 ? Blocks.BIG_DRIPLEAF.defaultBlockState() : Blocks.AIR.defaultBlockState(), 2);
                    if (plant.canSurvive(world, pos)) mask |= 1 << context;
                }
                out.print(" " + mask);
                for (var direction : Direction.values()) {
                    world.setBlock(pos, plant, 2);
                    world.setBlock(pos.below(), Blocks.CLAY.defaultBlockState(), 2);
                    world.setBlock(pos.above(), Blocks.AIR.defaultBlockState(), 2);
                    world.setBlock(pos.relative(direction), neighbor, 2);
                    out.print(" " + Block.getId(plant.updateShape(world, world, pos, direction,
                        pos.relative(direction), neighbor, new XoroshiroRandomSource(0))));
                    world.setBlock(pos.relative(direction), Blocks.AIR.defaultBlockState(), 2);
                }
            }
            out.println();
        }
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.BLOCK, Registries.BLOCK, "block");
        PlacementProbe.bindTags(args[1], BuiltInRegistries.FLUID, Registries.FLUID, "fluid");
        var lookup = VanillaRegistries.createLookup();
        WorldgenReferenceProbe.loadConfiguredFeaturesFromDataPack(lookup);
        var biomes = new MappedRegistry<net.minecraft.world.level.biome.Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(h -> biomes.register(h.key(), h.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes,
            WorldgenReferenceProbe.configuredFeatureRegistry(lookup)));
        var factory = PalettedContainerFactory.create(access);
        var generator = new NoiseBasedChunkGenerator(MultiNoiseBiomeSource.createFromPreset(
            lookup.getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD)), lookup.getOrThrow(NoiseGeneratorSettings.OVERWORLD));
        var features = new LinkedHashMap<String, ConfiguredFeature<?, ?>>();
        for (String name : FEATURES) features.put(name, lookup.lookupOrThrow(Registries.CONFIGURED_FEATURE).getOrThrow(
            ResourceKey.create(Registries.CONFIGURED_FEATURE, Identifier.parse("minecraft:" + name))).value());
        var ops = RegistryOps.create(com.mojang.serialization.JsonOps.INSTANCE, lookup);
        for (var entry : JsonParser.parseString(Files.readString(Path.of(args[3]))).getAsJsonObject().entrySet())
            features.put(entry.getKey(), ConfiguredFeature.DIRECT_CODEC.parse(ops, entry.getValue()).getOrThrow());
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (String s : args[2].split(",")) for (boolean legacy : new boolean[]{false, true})
                for (var entry : features.entrySet()) for (int test = 0; test < 34; test++) {
                    String name = entry.getKey();
                    long seed = Long.parseLong(s);
                    var world = world(access, factory);
                    world.attachGenerator(generator);
                    int y = y(test);
                    for (int dy = -9; dy <= 7; dy++) for (int dz = -12; dz <= 12; dz++) for (int dx = -12; dx <= 12; dx++) {
                        var pos = new BlockPos(15 + dx, y + dy, -1 + dz);
                        if (pos.getY() >= -64 && pos.getY() < 320) world.setBlock(pos, initial(test, dx, dy, dz), 2);
                    }
                    int[] before = blocks(world, y);
                    var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                    var feature = entry.getValue();
                    for (int pass = 0; pass < 2; pass++) {
                        boolean result = feature.place(world, generator, random, new BlockPos(15, y, -1));
                        out.println("case " + seed + " " + legacy + " " + name + " " + test + " " + pass + " " + result + " " + random.nextInt());
                        int[] after = blocks(world, y);
                        for (int i = 0; i < after.length; i++) if (before[i] != after[i]) out.println("block " + i + " " + after[i]);
                        for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG")) {
                            long hash = 0xcbf29ce484222325L;
                            for (int z = -16; z < 16; z++) for (int x = 0; x < 32; x++) hash = (hash ^ world.getHeight(kind, x, z)) * 0x100000001b3L;
                            out.printf("height %016x%n", hash);
                        }
                    }
                }
            behavior(out, access, factory);
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
