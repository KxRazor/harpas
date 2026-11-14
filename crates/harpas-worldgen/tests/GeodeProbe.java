// Calls vanilla's GeodeFeature on controlled terrain with data-pack and synthetic configurations.
// Arguments: output file, server jar, comma-separated seeds, configurations JSON.
import com.google.gson.JsonParser;
import java.io.*;
import java.nio.file.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.RegistryOps;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BlockStateProperties;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.*;

public final class GeodeProbe {
    static BlockPos origin(int test) {
        return new BlockPos(test == 14 ? -17 : test == 15 ? 29999980 : 15,
            switch (test) { case 8 -> -64; case 9 -> -60; case 10 -> 313; case 11 -> 320; default -> 64; }, -1);
    }
    static BlockState initial(int test, int dx, int dy, int dz) {
        int hash = Math.floorMod(dx * 3 + dy * 7 + dz * 11, 13);
        return switch (test) {
            case 1 -> Blocks.AIR.defaultBlockState();
            case 2 -> Blocks.WATER.defaultBlockState();
            case 3 -> Blocks.LAVA.defaultBlockState();
            case 4 -> Blocks.BEDROCK.defaultBlockState();
            case 5 -> (hash < 3 ? Blocks.BEDROCK : hash == 3 ? Blocks.CHEST : Blocks.STONE).defaultBlockState();
            case 6 -> (hash < 2 ? Blocks.LAVA : Blocks.STONE).defaultBlockState();
            case 7 -> (dx == dy && dy == dz ? Blocks.CAVE_AIR : Blocks.STONE).defaultBlockState();
            case 12 -> (hash < 3 ? Blocks.WATER : hash == 3 ? Blocks.AIR : Blocks.STONE).defaultBlockState();
            case 13 -> (dy > 4 ? Blocks.WATER : Blocks.STONE).defaultBlockState();
            default -> Blocks.STONE.defaultBlockState();
        };
    }
    static int[] blocks(WorldgenReferenceProbe.TestRegion world, BlockPos p) {
        int[] result = new int[35 * 35 * 35];
        int i = 0;
        for (int dy = -17; dy <= 17; dy++) for (int dz = -17; dz <= 17; dz++) for (int dx = -17; dx <= 17; dx++)
            result[i++] = Block.getId(world.getBlockState(p.offset(dx, dy, dz)));
        return result;
    }
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.BLOCK, Registries.BLOCK, "block");
        PlacementProbe.bindTags(args[1], BuiltInRegistries.FLUID, Registries.FLUID, "fluid");
        var lookup = VanillaRegistries.createLookup();
        var biomes = new MappedRegistry<net.minecraft.world.level.biome.Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(h -> biomes.register(h.key(), h.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        var features = new LinkedHashMap<String, ConfiguredFeature<?, ?>>();
        var ops = RegistryOps.create(com.mojang.serialization.JsonOps.INSTANCE, lookup);
        for (var entry : JsonParser.parseString(Files.readString(Path.of(args[3]))).getAsJsonObject().entrySet())
            features.put(entry.getKey(), ConfiguredFeature.DIRECT_CODEC.parse(ops, entry.getValue()).getOrThrow());
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (String s : args[2].split(",")) for (boolean legacy : new boolean[]{false, true})
                for (var entry : features.entrySet()) for (int test = 0; test < 16; test++) {
                    long seed = Long.parseLong(s);
                    var p = origin(test);
                    // Different seeds for the level noise and the decoration source expose accidental coupling.
                    var world = WorldgenReferenceProbe.TestRegion.create(access, height, factory,
                        new ProtoChunk(new ChunkPos(p.getX() >> 4, p.getZ() >> 4), UpgradeData.EMPTY, height, factory, null), seed ^ 0x123456789abcdefL);
                    for (int cx = (p.getX() - 18) >> 4; cx <= (p.getX() + 18) >> 4; cx++)
                        for (int cz = (p.getZ() - 18) >> 4; cz <= (p.getZ() + 18) >> 4; cz++)
                            ((ProtoChunk)world.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                    for (int dy = -18; dy <= 18; dy++) for (int dz = -18; dz <= 18; dz++) for (int dx = -18; dx <= 18; dx++) {
                        var at = p.offset(dx, dy, dz);
                        if (at.getY() >= -64 && at.getY() < 320) world.setBlock(at, initial(test, dx, dy, dz), 2);
                    }
                    int[] before = blocks(world, p);
                    var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                    for (int pass = 0; pass < 2; pass++) {
                        boolean placed = entry.getValue().place(world, null, random, p);
                        out.println("case " + seed + " " + legacy + " " + entry.getKey() + " " + test + " " + pass + " " + placed + " " + random.nextInt());
                        int[] after = blocks(world, p);
                        for (int i = 0; i < after.length; i++) if (before[i] != after[i]) out.println("block " + i + " " + after[i]);
                        for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG")) {
                            long hash = 0xcbf29ce484222325L;
                            for (int dz = -18; dz <= 18; dz++) for (int dx = -18; dx <= 18; dx++)
                                hash = (hash ^ world.getHeight(kind, p.getX() + dx, p.getZ() + dz)) * 0x100000001b3L;
                            out.printf("height %016x%n", hash);
                        }
                    }
                }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
