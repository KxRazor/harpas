// Black-box calls to vanilla's configured block piles. Fixtures are generated locally by the Rust harness.
import com.google.gson.*;
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
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.*;

public final class BlockPileProbe {
    static int[] blocks(WorldgenReferenceProbe.TestRegion world, BlockPos p) {
        int[] values = new int[4 * 9 * 9];
        int i = 0;
        for (int dy = -1; dy <= 2; dy++) for (int dz = -4; dz <= 4; dz++) for (int dx = -4; dx <= 4; dx++)
            values[i++] = Block.getId(world.getBlockState(p.offset(dx, dy, dz)));
        return values;
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
        var fixtures = JsonParser.parseString(Files.readString(Path.of(args[2]))).getAsJsonObject();
        var ops = RegistryOps.create(com.mojang.serialization.JsonOps.INSTANCE, lookup);
        var features = new ArrayList<ConfiguredFeature<?, ?>>();
        for (var json : fixtures.getAsJsonArray("features"))
            features.add(ConfiguredFeature.DIRECT_CODEC.parse(ops, json).getOrThrow());
        var cases = fixtures.getAsJsonArray("cases");
        try (var out = new ProbeOutput(args[0])) {
            for (long seed : new long[]{0, 42, -4172144997902289642L}) for (boolean legacy : new boolean[]{false, true})
                for (int config = 0; config < features.size(); config++) for (int test = 0; test < cases.size(); test++) {
                    var fixture = cases.get(test).getAsJsonObject();
                    var at = fixture.getAsJsonArray("origin");
                    var p = new BlockPos(at.get(0).getAsInt(), at.get(1).getAsInt(), at.get(2).getAsInt());
                    var world = WorldgenReferenceProbe.TestRegion.create(access, height, factory,
                        new ProtoChunk(new ChunkPos(p.getX() >> 4, p.getZ() >> 4), UpgradeData.EMPTY, height, factory, null), seed);
                    for (int cx = (p.getX() - 4) >> 4; cx <= (p.getX() + 4) >> 4; cx++)
                        for (int cz = (p.getZ() - 4) >> 4; cz <= (p.getZ() + 4) >> 4; cz++)
                            ((ProtoChunk)world.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                    for (var cell : fixture.getAsJsonArray("cells")) {
                        var c = cell.getAsJsonArray();
                        var pos = p.offset(c.get(0).getAsInt(), c.get(1).getAsInt(), c.get(2).getAsInt());
                        if (pos.getY() >= -64 && pos.getY() < 320) world.setBlock(pos, Block.stateById(c.get(3).getAsInt()), 2);
                    }
                    int[] before = blocks(world, p);
                    var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                    for (int pass = 0; pass < 2; pass++) {
                        boolean placed = features.get(config).place(world, null, random, p);
                        out.println("case " + seed + " " + legacy + " " + config + " " + test + " " + pass + " " + placed + " " + random.nextInt());
                        int[] after = blocks(world, p);
                        for (int i = 0; i < after.length; i++) if (before[i] != after[i]) out.println("block " + i + " " + after[i]);
                        for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG")) {
                            long hash = 0xcbf29ce484222325L;
                            for (int dz = -4; dz <= 4; dz++) for (int dx = -4; dx <= 4; dx++)
                                hash = (hash ^ world.getHeight(kind, p.getX() + dx, p.getZ() + dz)) * 0x100000001b3L;
                            out.printf("height %016x%n", hash);
                        }
                    }
                }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
