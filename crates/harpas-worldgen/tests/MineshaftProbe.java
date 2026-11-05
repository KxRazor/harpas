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

public final class MineshaftProbe {
    // Entities are never ticked or saved by this block-placement probe. A minimal level lets vanilla
    // construct a chest minecart and consume its real loot-seed draw before addFreshEntity discards it.
    static final class EntityLevel extends net.minecraft.server.level.ServerLevel {
        net.minecraft.core.RegistryAccess access;
        EntityLevel() { super(null,null,null,null,null,null,false,0,List.of(),false); }
        static EntityLevel create(net.minecraft.core.RegistryAccess access) throws Exception {
            var type=Class.forName("sun.misc.Unsafe"); var field=type.getDeclaredField("theUnsafe"); field.setAccessible(true);
            var level=(EntityLevel)type.getMethod("allocateInstance",Class.class).invoke(field.get(null),EntityLevel.class);
            level.access=access; return level;
        }
        @Override public net.minecraft.world.flag.FeatureFlagSet enabledFeatures() { return net.minecraft.world.flag.FeatureFlags.DEFAULT_FLAGS; }
        @Override public net.minecraft.core.RegistryAccess registryAccess() { return access; }
    }

    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var entityLevel=EntityLevel.create(registries);
        var structures = registries.lookupOrThrow(Registries.STRUCTURE);
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
            var world = (WorldGenLevel) Proxy.newProxyInstance(MineshaftProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getBiome" -> {
                        var at = (BlockPos) arguments[0];
                        var blocked = input.getCompoundOrEmpty("blockedBiomes").getBooleanOr(at.getX() + "," + at.getY() + "," + at.getZ(), false);
                        yield registries.lookupOrThrow(Registries.BIOME).getOrThrow(net.minecraft.resources.ResourceKey.create(Registries.BIOME,
                            net.minecraft.resources.Identifier.parse(blocked ? "deep_dark" : "plains")));
                    }
                    case "getHeight" -> proto.getHeight((Heightmap.Types) arguments[0], (int) arguments[1], (int) arguments[2]) + 1;
                    case "getHeightmapPos" -> {
                        var at = (BlockPos) arguments[1];
                        yield new BlockPos(at.getX(), proto.getHeight((Heightmap.Types) arguments[0], at.getX(), at.getZ()) + 1, at.getZ());
                    }
                    case "getFluidState" -> proto.getBlockState((BlockPos) arguments[0]).getFluidState();
                    case "getChunk" -> proto;
                    case "getLevel" -> entityLevel;
                    case "scheduleTick" -> null;
                    case "getMinY" -> -64;
                    case "getMaxY" -> 319;
                    case "getSeaLevel" -> 63;
                    case "addFreshEntity" -> true;
                    case "getBlockState" -> proto.getBlockState((BlockPos) arguments[0]);
                    case "isEmptyBlock" -> proto.getBlockState((BlockPos) arguments[0]).isAir();
                    case "setBlock" -> {
                        var at = ((BlockPos) arguments[0]).immutable();
                        if ((at.getX() >> 4) != cx || (at.getZ() >> 4) != cz) throw new AssertionError("write outside target chunk");
                        if (at.getY() < -64 || at.getY() >= 320) yield false;
                        var state = (net.minecraft.world.level.block.state.BlockState) arguments[1];
                        proto.setBlockState(at, state, 0);
                        if (state.is(Blocks.CHEST)) entities.put(at, new ChestBlockEntity(at, state));
                        else if (state.is(Blocks.SPAWNER)) entities.put(at, new SpawnerBlockEntity(at, state));
                        else entities.remove(at);
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
            place(starts,world,proto,seed,registries,bounds,chunkPos);
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

            }
            place(starts,world,proto,seed,registries,bounds,chunkPos);
            var resumed = snapshot(proto, starts, entities, context, registries);
            var resumedPost = new ListTag();
            for (var section : proto.getPostProcessing()) { var entries = new ListTag(); if (section != null) for (short value : section) entries.add(ShortTag.valueOf(value)); resumedPost.add(entries); }
            resumed.put("PostProcessing", resumedPost);
            result.put("resumed", resumed);
            results.put(name, result);
        }
        NbtIo.write(results, Path.of(args[1]));
        server.resources().close();
        net.minecraft.util.Util.shutdownExecutors();
    }

    static void place(List<StructureStart> starts, WorldGenLevel world, ProtoChunk proto, long seed,
            net.minecraft.core.RegistryAccess registries, BoundingBox bounds, ChunkPos chunkPos) {
        var structures = registries.lookupOrThrow(Registries.STRUCTURE);
        starts.sort(Comparator.comparingInt((StructureStart s) -> s.getChunkPos().x()).thenComparingInt(s -> s.getChunkPos().z()));
        for (var start : starts) if (structures.getKey(start.getStructure()).toString().equals("minecraft:buried_treasure")) {
            var random=new WorldgenRandom(new XoroshiroRandomSource(0));
            long decoration=random.setDecorationSeed(seed,chunkPos.x()*16,chunkPos.z()*16);
            random.setFeatureSeed(decoration,0,3);
            start.placeInChunk(world,null,null,random,bounds,chunkPos);
        }
        for (String id : List.of("mineshaft", "mineshaft_mesa")) {
            var structure = structures.getValue(net.minecraft.resources.Identifier.parse(id));
            var refs = new ProtoChunk(chunkPos, UpgradeData.EMPTY, LevelHeightAccessor.create(-64,384),PalettedContainerFactory.create(registries),null);
            for (var start : starts) if (start.getStructure() == structure) refs.addReferenceForStructure(structure,start.getChunkPos().pack());
            var random = new WorldgenRandom(new XoroshiroRandomSource(0));
            long decoration = random.setDecorationSeed(seed,chunkPos.x()*16,chunkPos.z()*16);
            var inStep = structures.stream().filter(s -> s.step() == structure.step()).toList();
            int index = inStep.indexOf(structure);
            if (index != (id.equals("mineshaft") ? 1 : 2) || structure.step().ordinal() != 3) throw new AssertionError("mineshaft seed order");
            random.setFeatureSeed(decoration,index,structure.step().ordinal());
            for (long ref : refs.getReferencesForStructure(structure)) for (var start : starts)
                if (start.getStructure()==structure && start.getChunkPos().pack()==ref)
                    start.placeInChunk(world,null,null,random,bounds,chunkPos);
        }
    }

    static CompoundTag snapshot(ProtoChunk proto, List<StructureStart> starts, Map<BlockPos, BlockEntity> entities,
            StructurePieceSerializationContext context, net.minecraft.core.RegistryAccess registries) {
        var result = BuriedTreasureProbe.snapshot(proto, starts, entities, context, registries);
        // Compare stored states too: ProtoChunk's game-facing getter returns ordinary air for an
        // all-air section even when that section stores cave_air. Anvil must preserve those states.
        var blocks = new int[384 * 256];
        for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
            blocks[((y + 64) * 16 + z) * 16 + x] = Block.getId(proto.getSection((y + 64) >> 4).getBlockState(x, y & 15, z));
        result.putIntArray("blocks", blocks);
        var saved = new CompoundTag();
        for (var start : starts) saved.put(registries.lookupOrThrow(Registries.STRUCTURE).getKey(start.getStructure()).toString() + "@" + start.getChunkPos().x() + "," + start.getChunkPos().z(),
            start.createTag(context, start.getChunkPos()));
        result.put("starts", saved);
        return result;
    }

}
