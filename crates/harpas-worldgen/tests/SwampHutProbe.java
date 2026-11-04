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

public final class SwampHutProbe {
    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var structures = registries.lookupOrThrow(Registries.STRUCTURE);
        var hut = structures.getValue(net.minecraft.resources.Identifier.parse("swamp_hut"));
        var step = hut.step();
        var inStep = structures.stream().filter(s -> s.step() == step).toList();
        int index = inStep.indexOf(hut);

        var context = new StructurePieceSerializationContext(server.resources(), registries, null);
        var cases = NbtIo.read(Path.of(args[0]));
        var results = new CompoundTag();
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
            // Observe real piece placement with a bounded, chunk-backed WorldGenLevel. Unexpected operations fail.
            var world = (WorldGenLevel) Proxy.newProxyInstance(SwampHutProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getHeight" -> proto.getHeight((Heightmap.Types) arguments[0], (int) arguments[1], (int) arguments[2]) + 1;
                    case "getHeightmapPos" -> {
                        var at = (BlockPos) arguments[1];
                        yield new BlockPos(at.getX(), proto.getHeight((Heightmap.Types) arguments[0], at.getX(), at.getZ()) + 1, at.getZ());
                    }
                    case "getFluidState" -> proto.getBlockState((BlockPos) arguments[0]).getFluidState();
                    case "getChunk" -> proto;
                    case "scheduleTick" -> null;
                    case "getMinY" -> -64;
                    case "getBlockState" -> proto.getBlockState((BlockPos) arguments[0]);
                    case "setBlock" -> {
                        var at = ((BlockPos) arguments[0]).immutable();
                        if ((at.getX() >> 4) != cx || (at.getZ() >> 4) != cz) throw new AssertionError("write outside target chunk");
                        if (at.getY() < -64 || at.getY() >= 320) yield false;
                        var state = (net.minecraft.world.level.block.state.BlockState) arguments[1];
                        proto.setBlockState(at, state, 0);
                        if (state.is(Blocks.CHEST)) entities.put(at, new ChestBlockEntity(at, state));
                        yield true;
                    }
                    case "getBlockEntity" -> entities.get(arguments[0]);
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
                if (start.getStructure() != hut) continue;
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
            // A carvers chunk will run FEATURES again in vanilla. Reload the saved records before doing so.
            var restored = result.getCompoundOrEmpty("starts");
            starts.clear();
            entities.replaceAll((at, be) -> BlockEntity.loadStatic(at, proto.getBlockState(at), be.saveWithFullMetadata(registries), registries));
            for (String id : restored.keySet()) {
                var start = StructureStart.loadStaticStart(context, restored.getCompoundOrEmpty(id), seed);
                starts.add(start);
                if (start.getStructure() != hut) continue;
                var random = new WorldgenRandom(new XoroshiroRandomSource(0));
                long decoration = random.setDecorationSeed(seed, cx * 16, cz * 16);
                random.setFeatureSeed(decoration, index, step.ordinal());
                placeBlocks(start, context, world, random, bounds, chunkPos);
            }
            var resumed = snapshot(proto, starts, entities, context, registries);
            var comparable = result.copy();
            comparable.remove("PostProcessing");
            // Fixtures also compare a second Harpas placement. HPos < 0 intentionally repeats the height
            // adjustment, as vanilla does; normal-height worker chunks must resume unchanged.
            var expectedResume = input.contains("resumed") ? input.getCompoundOrEmpty("resumed") : comparable;
            if (!expectedResume.equals(resumed)) throw new AssertionError("resumption differs for " + name);
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

    // Suppress entity spawning only while observing blocks, then restore the original flags. Harpas leaves
    // these flags untouched so a vanilla FEATURES pass can spawn the witch and cat later.
    static void placeBlocks(StructureStart start, StructurePieceSerializationContext context, WorldGenLevel world,
            WorldgenRandom random, BoundingBox bounds, ChunkPos chunkPos) throws Exception {
        var witch = net.minecraft.world.level.levelgen.structure.structures.SwampHutPiece.class.getDeclaredField("spawnedWitch");
        var cat = net.minecraft.world.level.levelgen.structure.structures.SwampHutPiece.class.getDeclaredField("spawnedCat");
        witch.setAccessible(true); cat.setAccessible(true);
        for (var piece : start.getPieces()) {
            boolean w = witch.getBoolean(piece), c = cat.getBoolean(piece);
            witch.setBoolean(piece, true); cat.setBoolean(piece, true);
            try { piece.postProcess(world, null, null, random, bounds, chunkPos, BlockPos.ZERO); }
            finally { witch.setBoolean(piece, w); cat.setBoolean(piece, c); }
        }
    }
}
