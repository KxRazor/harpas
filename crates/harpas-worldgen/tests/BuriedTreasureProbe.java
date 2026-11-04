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

public final class BuriedTreasureProbe {
    public static void main(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var structures = registries.lookupOrThrow(Registries.STRUCTURE);
        var treasure = structures.getValue(net.minecraft.resources.Identifier.parse("buried_treasure"));
        var step = treasure.step();
        var inStep = structures.stream().filter(s -> s.step() == step).toList();
        int index = inStep.indexOf(treasure);
        if (index != 0 || step.ordinal() != 3) throw new AssertionError("26.1 treasure seed order: " + index + ", " + step);
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
            var entities = new HashMap<BlockPos, BlockEntity>();
            // Observe real piece placement with a bounded, chunk-backed WorldGenLevel. Unexpected operations fail.
            var world = (WorldGenLevel) Proxy.newProxyInstance(BuriedTreasureProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getHeight" -> proto.getHeight((Heightmap.Types) arguments[0], (int) arguments[1], (int) arguments[2]) + 1;
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
                if (start.getStructure() != treasure) continue;
                var random = new WorldgenRandom(new XoroshiroRandomSource(0));
                long decoration = random.setDecorationSeed(seed, cx * 16, cz * 16);
                random.setFeatureSeed(decoration, index, step.ordinal());
                start.placeInChunk(world, null, null, random, bounds, chunkPos);
            }
            var result = snapshot(proto, starts, entities, context, registries);
            // A carvers chunk will run FEATURES again in vanilla. Reload the saved records before doing so.
            var restored = result.getCompoundOrEmpty("starts");
            starts.clear();
            entities.replaceAll((at, be) -> BlockEntity.loadStatic(at, proto.getBlockState(at), be.saveWithFullMetadata(registries), registries));
            for (String id : restored.keySet()) {
                var start = StructureStart.loadStaticStart(context, restored.getCompoundOrEmpty(id), seed);
                starts.add(start);
                if (start.getStructure() != treasure) continue;
                var random = new WorldgenRandom(new XoroshiroRandomSource(0));
                long decoration = random.setDecorationSeed(seed, cx * 16, cz * 16);
                random.setFeatureSeed(decoration, index, step.ordinal());
                start.placeInChunk(world, null, null, random, bounds, chunkPos);
            }
            var resumed = snapshot(proto, starts, entities, context, registries);
            if (!result.equals(resumed)) throw new AssertionError("resuming changed " + name);
            results.put(name, result);
        }
        NbtIo.write(results, Path.of(args[1]));
        server.resources().close();
        net.minecraft.util.Util.shutdownExecutors();
    }

    static CompoundTag snapshot(ProtoChunk proto, List<StructureStart> starts, Map<BlockPos, BlockEntity> entities,
            StructurePieceSerializationContext context, net.minecraft.core.RegistryAccess registries) {
        var result = new CompoundTag();
        int[] blocks = new int[384 * 256];
        var pos = new BlockPos.MutableBlockPos();
        var cp = proto.getPos();
        for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
            blocks[((y + 64) * 16 + z) * 16 + x] = Block.getId(proto.getBlockState(pos.set(cp.x() * 16 + x, y, cp.z() * 16 + z)));
        result.putIntArray("blocks", blocks);
        var saved = new CompoundTag();
        for (var start : starts) saved.put(registries.lookupOrThrow(Registries.STRUCTURE).getKey(start.getStructure()).toString(), start.createTag(context, cp));
        result.put("starts", saved);
        var tags = new ListTag();
        entities.values().stream().sorted(Comparator.comparingLong(be -> be.getBlockPos().asLong()))
            .forEach(be -> tags.add(be.saveWithFullMetadata(registries)));
        result.put("block_entities", tags);
        Heightmap.primeHeightmaps(proto, EnumSet.allOf(Heightmap.Types.class));
        var maps = new CompoundTag();
        for (var kind : Heightmap.Types.values()) maps.putLongArray(kind.name(), proto.getOrCreateHeightmapUnprimed(kind).getRawData());
        result.put("Heightmaps", maps);
        return result;
    }
}
