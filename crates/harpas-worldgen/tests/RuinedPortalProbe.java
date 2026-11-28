// Test driver only: observes vanilla's placement on caller-supplied carved terrain, then saves and resumes it.
// Ruined portals are the subject; worker cases also place the other structures that reach the chunk.
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

public final class RuinedPortalProbe {
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
        for (var id : List.of("ruined_portal", "ruined_portal_desert", "ruined_portal_jungle", "ruined_portal_mountain", "ruined_portal_ocean", "ruined_portal_swamp")) {
            var structure = structures.getValue(net.minecraft.resources.Identifier.parse(id));
            int expected = switch(id) { case "ruined_portal" -> 10; case "ruined_portal_desert" -> 11; case "ruined_portal_jungle" -> 12; case "ruined_portal_mountain" -> 13; case "ruined_portal_ocean" -> 15; default -> 16; };
            if (structure.step().ordinal() != 4 || structures.stream().filter(s -> s.step() == structure.step()).toList().indexOf(structure) != expected)
                throw new AssertionError("26.1 ruined portal seed order");
        }

        var storage = net.minecraft.world.level.storage.LevelStorageSource.createDefault(Path.of("ruined-portal-worlds")).createAccess("test");
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
            var entities = proto.getBlockEntities();
            for (var value : input.getListOrEmpty("initial_entities")) {
                var entity = (CompoundTag) value;
                var at = new BlockPos(entity.getIntOr("x", 0), entity.getIntOr("y", 0), entity.getIntOr("z", 0));
                var loaded = BlockEntity.loadStatic(at, proto.getBlockState(at), entity, registries);
                if (loaded == null) throw new AssertionError("cannot load block entity");
                entities.put(at, loaded);
            }
            var pass = new ChunkPos[] {chunkPos};
            var columns = input.getCompoundOrEmpty("columns");
            var external = new HashMap<BlockPos, net.minecraft.world.level.block.state.BlockState>();
            // Observe real piece placement with a bounded, chunk-backed WorldGenLevel. Unexpected operations fail.
            var chunkViews = new ProbeTerrain.ChunkViews(proto, registries);
            var world = (WorldGenLevel) Proxy.newProxyInstance(RuinedPortalProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "holderLookup" -> registries.lookupOrThrow((net.minecraft.resources.ResourceKey) arguments[0]);
                    case "getHeight" -> arguments == null ? proto.getHeight() : ProbeTerrain.height((WorldGenLevel) proxy,
                        (Heightmap.Types) arguments[0], (int) arguments[1], (int) arguments[2]);
                    case "getHeightmapPos" -> {
                        var at = (BlockPos) arguments[1];
                        yield new BlockPos(at.getX(), ((WorldGenLevel) proxy).getHeight((Heightmap.Types) arguments[0], at.getX(), at.getZ()), at.getZ());
                    }
                    case "getFluidState" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).getFluidState();
                    case "getChunk" -> chunkViews.get((WorldGenLevel) proxy, pass[0], arguments);
                    case "registryAccess" -> registries;
                    case "getLevel" -> level;
                    case "getSeaLevel" -> 63;
                    case "environmentAttributes" -> net.minecraft.world.attribute.EnvironmentAttributeReader.EMPTY;
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
                        if (proto.isOutsideBuildHeight(at)) yield Blocks.VOID_AIR.defaultBlockState();
                        if (!ProbeTerrain.canRead(pass[0], at.getX() >> 4, at.getZ() >> 4))
                            throw new AssertionError("read outside generation region: " + at);
                        if ((at.getX() >> 4) == cx && (at.getZ() >> 4) == cz) yield proto.getBlockState(at);
                        if (external.containsKey(at)) yield external.get(at);
                        int[] column = columns.getIntArray(at.getX() + "," + at.getZ()).orElseThrow(() -> new AssertionError("missing column " + at));
                        yield Block.stateById(column[at.getY() + 64]);
                    }
                    case "ensureCanWrite" -> ProbeTerrain.canWrite(pass[0], (BlockPos) arguments[0]);
                    case "setBlock" -> {
                        var at = ((BlockPos) arguments[0]).immutable();
                        if (!ProbeTerrain.canWrite(pass[0], at) || proto.isOutsideBuildHeight(at)) yield false;
                        var state = (net.minecraft.world.level.block.state.BlockState) arguments[1];
                        var before = ((WorldGenLevel) proxy).getBlockState(at);
                        if ((at.getX() >> 4) == cx && (at.getZ() >> 4) == cz) proto.setBlockState(at, state, 0);
                        else external.put(at, state);
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
                if (start == null || !start.isValid()) throw new AssertionError("failed to load start " + id);
                starts.add(start);
            }
            // Worker chunks also hold every other structure that reaches them, including starts owned by
            // neighbors, which are placed but not saved here. Place them all in vanilla's step and index order.
            boolean all = input.getBooleanOr("all_structures", false);
            var placed = new ArrayList<>(starts);
            for (var value : input.getListOrEmpty("neighbor_starts")) {
                var start = StructureStart.loadStaticStart(context, (CompoundTag) value, seed);
                if (start == null || !start.isValid()) throw new AssertionError("failed to load neighbor start");
                placed.add(start);
            }
            // Spill from a portal is produced by decoration in the chunk containing its piece center.
            var portalPasses = new TreeMap<ChunkPos, List<StructureStart>>(Comparator.comparingInt(ChunkPos::x)
                .thenComparingInt(ChunkPos::z));
            var targetStarts = new ArrayList<StructureStart>();
            for (var start : placed) {
                var id = structures.getKey(start.getStructure()).getPath();
                if (id.startsWith("ruined_portal") && !id.endsWith("_nether")) {
                    var owner = ProbeTerrain.chunkPos(start.getPieces().getFirst().getBoundingBox().getCenter());
                    if (owner.equals(chunkPos)) targetStarts.add(start);
                    else portalPasses.computeIfAbsent(owner, key -> new ArrayList<>()).add(start);
                } else if (all) {
                    targetStarts.add(start);
                }
            }
            for (var entry : portalPasses.entrySet()) {
                var owner = entry.getKey();
                pass[0] = owner;
                var bounds = ProbeTerrain.writableArea(owner, proto.getHeightAccessorForGeneration());
                ProbeTerrain.decorate(structures, entry.getValue(), seed, owner,
                    (start, random) -> start.placeInChunk(world, null, null, random, bounds, owner));
            }
            pass[0] = chunkPos;
            var bounds = ProbeTerrain.writableArea(proto);
            ProbeTerrain.decorate(structures, targetStarts, seed, chunkPos,
                (start, random) -> start.placeInChunk(world, null, null, random, bounds, chunkPos));
            entities.entrySet().removeIf(e -> (e.getKey().getX() >> 4) != cx || (e.getKey().getZ() >> 4) != cz);
            var persisted = starts.stream().filter(start -> input.getBooleanOr("keep_neighbor_starts", true)
                || start.getChunkPos().equals(chunkPos)).toList();
            var result = snapshot(proto, persisted, entities, context, registries);
            result.put("PostProcessing", ProbeTerrain.postProcessing(proto));
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
        return BuriedTreasureProbe.snapshot(proto, starts, entities, context, registries);
    }

}
