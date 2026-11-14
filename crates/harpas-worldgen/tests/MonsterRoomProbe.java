// Calls vanilla's monster room feature on controlled caves, including overlaps and world limits.
// Arguments: output file, comma-separated seeds. No game code or generated fixtures are redistributed.
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
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.feature.*;
import net.minecraft.world.level.levelgen.feature.configurations.FeatureConfiguration;

public final class MonsterRoomProbe {
    static int y(int test) { return switch (test) { case 33 -> -63; case 34 -> -64; case 35 -> 315; case 36 -> 316; default -> 64; }; }

    static BlockState initial(int test, int dx, int dy, int dz) {
        Block[] materials = {Blocks.GLASS, Blocks.OAK_LEAVES, Blocks.ICE, Blocks.OAK_SLAB,
            Blocks.COBWEB, Blocks.SOUL_SAND, Blocks.END_PORTAL_FRAME, Blocks.DIRT};
        Block block = test >= 16 && test < 24 ? materials[test - 16] : Blocks.STONE;
        if (dy == -2 && (test == 5 || test == 31 && Math.floorMod(dx + dz, 2) == 0)) return Blocks.AIR.defaultBlockState();
        if (dy == 4 && (test == 3 && dx == 0 && dz == 0 || test == 13)) return (test == 13 ? Blocks.WATER : Blocks.AIR).defaultBlockState();
        if (dy == -1 && (test == 4 && dx == 0 && dz == 0 || test == 14)) return (test == 14 ? Blocks.WATER : Blocks.AIR).defaultBlockState();
        if (dy >= 0 && dy <= 3) {
            if (test == 24 && dx == 0 && dy == 0 && dz == 0) return Blocks.CHEST.defaultBlockState();
            if (test == 25 && dx == 0 && dy == 0 && dz == 0) return Blocks.SPAWNER.defaultBlockState();
            if (test == 26 && dx == 0 && dy == 0 && dz == 0) return Blocks.BARREL.defaultBlockState();
            if ((test == 27 && Math.abs(dx) <= 1 || test == 28 && Math.abs(dx) >= 3) && dy == 0 && Math.abs(dz) == 2)
                return Blocks.CHEST.defaultBlockState();
            if (test == 29 && dy == 1 && dz == 0) return Blocks.SPAWNER.defaultBlockState();
            if (test == 30 && dy == 2) return Blocks.SHORT_GRASS.defaultBlockState();
            if (test == 32 && dy == 1) return Blocks.WATER.defaultBlockState();
            if (test >= 40 && dx == 1 && dy == 1 && dz == 1) {
                Block[] protectedBlocks = {Blocks.BEDROCK, Blocks.END_PORTAL_FRAME, Blocks.REINFORCED_DEEPSLATE,
                    Blocks.TRIAL_SPAWNER, Blocks.VAULT, Blocks.TRAPPED_CHEST, Blocks.ENDER_CHEST, Blocks.STRUCTURE_BLOCK};
                return protectedBlocks[test - 40].defaultBlockState();
            }
        }
        if (test == 37 && dy == 2 && dx == -4) return Blocks.AIR.defaultBlockState();
        boolean corridor = test != 1 && (test != 2 || dx <= 0) &&
            (dz == 0 || (test == 6 || test == 7 || test == 38 || test == 39) && dz == 1 || test == 7 && dz == -1
                || (test == 38 || test == 39) && dx == 0 && (test == 39 || dz < 0));
        if (corridor && (dy == 0 || dy == 1 && test != 8)) {
            return (switch (test) { case 9 -> Blocks.WATER; case 10 -> Blocks.LAVA;
                case 11 -> Blocks.CAVE_AIR; case 12 -> Blocks.VOID_AIR; default -> Blocks.AIR; }).defaultBlockState();
        }
        return block.defaultBlockState();
    }

    static int[] blocks(WorldgenReferenceProbe.TestRegion region, int y) {
        var values = new ArrayList<Integer>();
        for (int dy = -2; dy <= 4; dy++) for (int dz = -6; dz <= 6; dz++) for (int dx = -6; dx <= 6; dx++)
            values.add(Block.getId(region.getBlockState(new BlockPos(15 + dx, y + dy, -1 + dz))));
        return values.stream().mapToInt(Integer::intValue).toArray();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        var biomes = new MappedRegistry<net.minecraft.world.level.biome.Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(h -> biomes.register(h.key(), h.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (String s : args[1].split(",")) for (boolean legacy : new boolean[]{false, true}) for (int test = 0; test < 48; test++) {
                long seed = Long.parseLong(s);
                int y = y(test);
                var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory,
                    new ProtoChunk(new ChunkPos(0, -1), UpgradeData.EMPTY, height, factory, null), seed);
                // Populate live FEATURES heightmaps from empty chunks. Re-priming maps already populated
                // at EMPTY can retain partial heights when the top layer is non-motion-blocking.
                for (int cx = 0; cx <= 1; cx++) for (int cz = -1; cz <= 0; cz++)
                    ((ProtoChunk)region.getChunk(cx, cz)).setPersistedStatus(ChunkStatus.CARVERS);
                for (int dy = -2; dy <= 4; dy++) for (int dz = -6; dz <= 6; dz++) for (int dx = -6; dx <= 6; dx++) {
                    var at = new BlockPos(15 + dx, y + dy, -1 + dz);
                    if (!height.isOutsideBuildHeight(at)) region.setBlock(at, initial(test, dx, dy, dz), 2);
                }
                int[] before = blocks(region, y);
                var random = new WorldgenRandom(legacy ? new LegacyRandomSource(seed) : new XoroshiroRandomSource(seed));
                for (int pass = 0; pass < 2; pass++) {
                    boolean placed = Feature.MONSTER_ROOM.place(FeatureConfiguration.NONE, region, null, random, new BlockPos(15, y, -1));
                    out.println("case " + seed + " " + legacy + " " + test + " " + pass + " " + placed + " " + random.nextInt());
                    int[] after = blocks(region, y);
                    for (int i = 0; i < after.length; i++) if (before[i] != after[i]) out.println("block " + i + " " + after[i]);
                    for (var kind : Heightmap.Types.values()) if (!kind.name().endsWith("_WG")) {
                        long hash = 0xcbf29ce484222325L;
                        for (int z = -16; z < 16; z++) for (int x = 0; x < 32; x++) hash = (hash ^ region.getHeight(kind, x, z)) * 0x100000001b3L;
                        out.printf("height %016x%n", hash);
                    }
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
