// Test driver only: observes vanilla's placement on caller-supplied carved terrain, then saves and resumes it.
// Arguments: <input NBT> <output NBT>. No game code or data is copied into the repository.
import java.lang.reflect.Proxy;
import java.nio.file.Path;
import java.util.*;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.nbt.*;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.entity.*;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.structure.*;
import net.minecraft.world.level.levelgen.structure.pieces.StructurePieceSerializationContext;

public final class OceanRuinProbe {
    // Only the seed and feature flags are used. Empty flags suppress gameplay entity creation.
    static final class SeedLevel extends net.minecraft.server.level.ServerLevel {
        long seed;
        SeedLevel() { super(null, null, null, null, null, null, false, 0, null, false); }
        @Override public long getSeed() { return seed; }
        @Override public net.minecraft.world.flag.FeatureFlagSet enabledFeatures() { return net.minecraft.world.flag.FeatureFlagSet.of(); }
    }
    static SeedLevel seedLevel(long seed) throws Exception {
        var type = Class.forName("sun.misc.Unsafe");
        var field = type.getDeclaredField("theUnsafe"); field.setAccessible(true);
        var level = (SeedLevel) type.getMethod("allocateInstance", Class.class).invoke(field.get(null), SeedLevel.class);
        level.seed = seed;
        return level;
    }

    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var structures = registries.lookupOrThrow(Registries.STRUCTURE);
        var ruin = structures.getValue(net.minecraft.resources.Identifier.parse("ocean_ruin_cold"));
        var step = ruin.step();
        var inStep = structures.stream().filter(s -> s.step() == step).toList();
        int index = inStep.indexOf(ruin);
        if (index != 7 || step.ordinal() != 4 || inStep.indexOf(structures.getValue(net.minecraft.resources.Identifier.parse("ocean_ruin_warm"))) != 8)
            throw new AssertionError("26.1 ocean ruin seed order");

        var storage = net.minecraft.world.level.storage.LevelStorageSource.createDefault(Path.of("ocean-ruin-worlds")).createAccess("test");
        var manager = new net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager(
            server.resources(), storage, net.minecraft.util.datafix.DataFixers.getDataFixer(), registries.lookupOrThrow(Registries.BLOCK));
        var context = new StructurePieceSerializationContext(server.resources(), registries, manager);
        CompoundTag cases;
        try (var stream = new java.io.DataInputStream(new java.io.BufferedInputStream(java.nio.file.Files.newInputStream(Path.of(args[0]))))) {
            cases = NbtIo.read(stream);
        }
        var results = new CompoundTag();
        for (String name : cases.keySet()) {
            var input = cases.getCompoundOrEmpty(name);
            int cx = input.getIntOr("x", 0), cz = input.getIntOr("z", 0);
            long seed = input.getLongOr("seed", 0);
            var level = seedLevel(seed);
            var chunkPos = new ChunkPos(cx, cz);
            var proto = new ProtoChunk(chunkPos, UpgradeData.EMPTY, LevelHeightAccessor.create(-64, 384),
                PalettedContainerFactory.create(registries), null);
            int[] blocks = input.getIntArray("blocks").orElseThrow();
            var pos = new BlockPos.MutableBlockPos();
            for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
                proto.setBlockState(pos.set(cx * 16 + x, y, cz * 16 + z), Block.stateById(blocks[((y + 64) * 16 + z) * 16 + x]), 0);
            proto.setPersistedStatus(ChunkStatus.CARVERS);
            Heightmap.primeHeightmaps(proto, EnumSet.allOf(Heightmap.Types.class));
            var initialPost = input.getListOrEmpty("PostProcessing");
            for (int section = 0; section < initialPost.size(); section++) {
                var list = (ListTag) initialPost.get(section);
                for (var value : list) proto.addPackedPostProcess(new it.unimi.dsi.fastutil.shorts.ShortArrayList(new short[]{((ShortTag) value).shortValue()}), section);
            }
            var entities = new HashMap<BlockPos, BlockEntity>();
            for (var value : input.getListOrEmpty("initial_entities")) {
                var entity = (CompoundTag) value;
                var at = new BlockPos(entity.getIntOr("x", 0), entity.getIntOr("y", 0), entity.getIntOr("z", 0));
                var loaded = BlockEntity.loadStatic(at, proto.getBlockState(at), entity, registries);
                if (loaded == null) throw new AssertionError("cannot load block entity");
                entities.put(at, loaded);
            }
            // Observe real piece placement with a bounded, chunk-backed WorldGenLevel. Unexpected operations fail.
            var world = (WorldGenLevel) Proxy.newProxyInstance(OceanRuinProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getHeight" -> {
                        int x = (int) arguments[1], z = (int) arguments[2];
                        if ((x >> 4) != cx || (z >> 4) != cz) yield input.getCompoundOrEmpty("heights").getIntArray(x + "," + z).orElseThrow(() -> new AssertionError("missing height " + name + " " + x + "," + z + " starts=" + input.get("starts")))[ arguments[0] == Heightmap.Types.WORLD_SURFACE_WG ? 0 : 1];
                        yield proto.getHeight((Heightmap.Types) arguments[0], x, z) + 1;
                    }
                    case "getHeightmapPos" -> {
                        var at = (BlockPos) arguments[1];
                        yield new BlockPos(at.getX(), ((WorldGenLevel) proxy).getHeight((Heightmap.Types) arguments[0], at.getX(), at.getZ()), at.getZ());
                    }
                    case "getFluidState" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).getFluidState();
                    case "getChunk" -> proto;
                    case "registryAccess" -> registries;
                    case "getLevel" -> level;
                    case "getSeaLevel" -> 63;
                    case "isClientSide" -> false;
                    case "getRandom" -> net.minecraft.util.RandomSource.create(0);
                    case "isEmptyBlock" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).isAir();
                    case "blockUpdated", "updateNeighborsAt" -> null;
                    case "scheduleTick" -> null;
                    case "getMinY" -> -64;
                    case "getMaxY" -> 319;
                    case "getSeed" -> seed;
                    case "getBlockState" -> {
                        var at = (BlockPos) arguments[0];
                        if ((at.getX() >> 4) == cx && (at.getZ() >> 4) == cz) yield proto.getBlockState(at);
                        int[] column = input.getCompoundOrEmpty("columns").getIntArray(at.getX() + "," + at.getZ()).orElseThrow(() -> new AssertionError("missing column " + at));
                        yield at.getY() < -64 || at.getY() >= 320 ? Blocks.AIR.defaultBlockState() : Block.stateById(column[at.getY()+64]);
                    }
                    case "setBlock" -> {
                        var at = ((BlockPos) arguments[0]).immutable();
                        if ((at.getX() >> 4) != cx || (at.getZ() >> 4) != cz) throw new AssertionError("write outside target chunk");
                        if (at.getY() < -64 || at.getY() >= 320) yield false;
                        var state = (net.minecraft.world.level.block.state.BlockState) arguments[1];
                        var before = proto.getBlockState(at);
                        proto.setBlockState(at, state, 0);
                        if (before.getBlock() != state.getBlock()) entities.remove(at);
                        if (state.hasBlockEntity()) entities.computeIfAbsent(at, k -> ((EntityBlock) state.getBlock()).newBlockEntity(k, state));
                        yield true;
                    }
                    case "getBlockEntity" -> arguments.length == 1 ? entities.get(arguments[0]) : Optional.ofNullable(entities.get(arguments[0]));
                    default -> throw new UnsupportedOperationException(method.toString());
                });
            var starts = new ArrayList<StructureStart>();
            for (String id : input.getCompoundOrEmpty("starts").keySet()) {
                var start = StructureStart.loadStaticStart(context, input.getCompoundOrEmpty("starts").getCompoundOrEmpty(id), seed);
                if (start == null) throw new AssertionError("failed to load start " + id);
                starts.add(start);
            }
            var bounds = new BoundingBox(cx * 16, -64, cz * 16, cx * 16 + 15, 319, cz * 16 + 15);
            for (var start : starts) {
                var id = structures.getKey(start.getStructure()).toString();
                if (!id.equals("minecraft:ocean_ruin_cold") && !id.equals("minecraft:ocean_ruin_warm")) continue;
                var random = new WorldgenRandom(new XoroshiroRandomSource(0));
                long decoration = random.setDecorationSeed(seed, cx * 16, cz * 16);
                random.setFeatureSeed(decoration, id.endsWith("_warm") ? 8 : index, step.ordinal());
                start.placeInChunk(world, null, null, random, bounds, chunkPos);
            }
            var result = snapshot(proto, starts, entities, context, registries).copy();
            var post = new ListTag();
            for (var section : proto.getPostProcessing()) {
                var entries = new ListTag();
                if (section != null) for (short value : section) entries.add(ShortTag.valueOf(value));
                post.add(entries);
            }
            result.put("PostProcessing", post);
            var saved = result.getCompoundOrEmpty("starts");
            for (var start : starts) {
                var id = structures.getKey(start.getStructure()).toString();
                if (!input.getBooleanOr("keep_neighbor_starts", true) && !start.getChunkPos().equals(chunkPos)) {
                    saved.remove(id); continue;
                }
            }
            results.put(name, result);
        }
        try (var stream = new java.io.DataOutputStream(new java.io.BufferedOutputStream(java.nio.file.Files.newOutputStream(Path.of(args[1]))))) {
            NbtIo.write(results, stream);
        }
        storage.close();
        server.resources().close();
        net.minecraft.util.Util.shutdownExecutors();
    }

    static CompoundTag snapshot(ProtoChunk proto, List<StructureStart> starts, Map<BlockPos, BlockEntity> entities,
            StructurePieceSerializationContext context, net.minecraft.core.RegistryAccess registries) {
        var result = BuriedTreasureProbe.snapshot(proto, starts, entities, context, registries);
        var saved = new CompoundTag();
        for (var start : starts) saved.put(registries.lookupOrThrow(Registries.STRUCTURE).getKey(start.getStructure()).toString(),
            start.createTag(context, start.getChunkPos()));
        result.put("starts", saved);
        return result;
    }

}
