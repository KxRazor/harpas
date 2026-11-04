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

public final class DesertPyramidProbe {
    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var structures = registries.lookupOrThrow(Registries.STRUCTURE);
        var pyramid = structures.getValue(net.minecraft.resources.Identifier.parse("desert_pyramid"));
        var step = pyramid.step();
        var inStep = structures.stream().filter(s -> s.step() == step).toList();
        int index = inStep.indexOf(pyramid);
        if (index != 1 || step.ordinal() != 4) throw new AssertionError("26.1 pyramid seed order");

        var context = new StructurePieceSerializationContext(server.resources(), registries, null);
        var cases = NbtIo.read(Path.of(args[0]));
        var results = new CompoundTag();
        var randomStates = new HashMap<Long, RandomState>();
        for (String name : cases.keySet()) {
            var input = cases.getCompoundOrEmpty(name);
            int cx = input.getIntOr("x", 0), cz = input.getIntOr("z", 0);
            long seed = input.getLongOr("seed", 0);
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
            int[] surrounding = input.getIntArray("surrounding").orElseThrow();
            int[] heights = input.getIntArray("heights").orElseThrow();
            int originX = input.getIntOr("originX", 0), originZ = input.getIntOr("originZ", 0);
            var regionRandom = randomStates.computeIfAbsent(seed, k -> RandomState.create(registries, NoiseGeneratorSettings.OVERWORLD, k))
                .getOrCreateRandomFactory(net.minecraft.resources.Identifier.parse("minecraft:worldgen_region_random"))
                .at(cx * 16, 0, cz * 16);
            // Observe real piece placement with a bounded, chunk-backed WorldGenLevel. Unexpected operations fail.
            var world = (WorldGenLevel) Proxy.newProxyInstance(DesertPyramidProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getHeight" -> proto.getHeight((Heightmap.Types) arguments[0], (int) arguments[1], (int) arguments[2]) + 1;
                    case "getHeightmapPos" -> {
                        var at = (BlockPos) arguments[1];
                        yield new BlockPos(at.getX(), heights[(at.getZ() - originZ) * 21 + at.getX() - originX], at.getZ());
                    }
                    case "getFluidState" -> proto.getBlockState((BlockPos) arguments[0]).getFluidState();
                    case "getChunk" -> proto;
                    case "scheduleTick" -> null;
                    case "getMinY" -> -64;
                    case "getMaxY" -> 319;
                    case "getSeed" -> seed;
                    case "getRandom" -> regionRandom;
                    case "getBlockState" -> {
                        var at = (BlockPos) arguments[0];
                        if ((at.getX() >> 4) == cx && (at.getZ() >> 4) == cz) yield proto.getBlockState(at);
                        int x = at.getX() - cx * 16 + 1, z = at.getZ() - cz * 16 + 1;
                        if (x < 0 || x >= 18 || z < 0 || z >= 18) throw new AssertionError("read outside supplied neighborhood");
                        yield at.getY() < -64 || at.getY() >= 320 ? Blocks.AIR.defaultBlockState()
                            : Block.stateById(surrounding[((at.getY() + 64) * 18 + z) * 18 + x]);
                    }
                    case "setBlock" -> {
                        var at = ((BlockPos) arguments[0]).immutable();
                        if ((at.getX() >> 4) != cx || (at.getZ() >> 4) != cz) throw new AssertionError("write outside target chunk");
                        if (at.getY() < -64 || at.getY() >= 320) yield false;
                        var state = (net.minecraft.world.level.block.state.BlockState) arguments[1];
                        var before = proto.getBlockState(at);
                        proto.setBlockState(at, state, 0);
                        if (before.getBlock() != state.getBlock()) entities.remove(at);
                        if (state.is(Blocks.CHEST)) entities.computeIfAbsent(at, k -> new ChestBlockEntity(k, state));
                        if (state.is(Blocks.SUSPICIOUS_SAND)) entities.computeIfAbsent(at, k -> new BrushableBlockEntity(k, state));
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
                if (input.getBooleanOr("source_first", false) && start.getStructure() == pyramid) {
                    var sourceRandom = new WorldgenRandom(new XoroshiroRandomSource(0));
                    var sourcePos = start.getChunkPos();
                    long decoration = sourceRandom.setDecorationSeed(seed, sourcePos.x() * 16, sourcePos.z() * 16);
                    sourceRandom.setFeatureSeed(decoration, index, step.ordinal());
                    var align = ScatteredFeaturePiece.class.getDeclaredMethod("updateHeightPositionToLowestGroundHeight", LevelAccessor.class, int.class);
                    align.setAccessible(true);
                    for (var piece : start.getPieces()) align.invoke(piece, world, -sourceRandom.nextInt(3));
                }
            }
            var bounds = new BoundingBox(cx * 16, -64, cz * 16, cx * 16 + 15, 319, cz * 16 + 15);
            for (var start : starts) {
                if (start.getStructure() != pyramid) continue;
                var random = new WorldgenRandom(new XoroshiroRandomSource(0));
                long decoration = random.setDecorationSeed(seed, cx * 16, cz * 16);
                random.setFeatureSeed(decoration, index, step.ordinal());
                placeBlocks(start, context, world, random, bounds, chunkPos);
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
                if (input.getBooleanOr("defer_chests", false) && start.getStructure() == pyramid) {
                    var original = input.getCompoundOrEmpty("starts").getCompoundOrEmpty(id).getListOrEmpty("Children");
                    var children = saved.getCompoundOrEmpty(id).getListOrEmpty("Children");
                    for (int i = 0; i < children.size(); i++) for (int flag = 0; flag < 4; flag++) {
                        var key = "hasPlacedChest" + flag;
                        ((CompoundTag) children.get(i)).putBoolean(key, ((CompoundTag) original.get(i)).getBooleanOr(key, false));
                    }
                }
            }
            results.put(name, result);
        }
        NbtIo.write(results, Path.of(args[1]));
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

    static void placeBlocks(StructureStart start, StructurePieceSerializationContext context, WorldGenLevel world,
            WorldgenRandom random, BoundingBox bounds, ChunkPos chunkPos) {
        start.placeInChunk(world, null, null, random, bounds, chunkPos);
    }
}
