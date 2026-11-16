// Test driver only: observes vanilla's StructurePoolElement.place for elements of the owner's template pools, in one
// chunk of layered terrain whose eight neighbors may also be written, as in a generation region. Arguments: <input NBT> <output NBT>. No game code or data is copied into the repository.
//
// Each input case names a pool and element index, the template position, the processors' pivot, a rotation, the
// target chunk, a seed for the placement random, the liquid settings and the terrain as [top Y, state] layers from
// the world bottom, with air above. The output holds the chunk's blocks that differ from the terrain as
// [index, state] pairs, the blocks written to neighboring chunks as [x, y, z, state], its block entities with full
// metadata and the next long of the placement random afterwards. The placement random is a WorldgenRandom over
// Xoroshiro, as structure pieces receive.
// With the arguments normalize <input NBT> <output NBT>, it instead loads Harpas's block entities and saves them again.
import java.lang.reflect.Proxy;
import java.nio.file.Path;
import java.util.*;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.nbt.*;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.entity.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.structure.BoundingBox;
import net.minecraft.world.level.levelgen.structure.templatesystem.LiquidSettings;

public final class PoolElementProbe {
    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        if (args[0].equals("normalize")) { normalize(registries, Path.of(args[1]), Path.of(args[2])); server.resources().close(); return; }
        var pools = registries.lookupOrThrow(Registries.TEMPLATE_POOL);
        var storage = net.minecraft.world.level.storage.LevelStorageSource.createDefault(Path.of("element-worlds")).createAccess("test");
        var manager = new net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager(
            server.resources(), storage, net.minecraft.util.datafix.DataFixers.getDataFixer(), registries.lookupOrThrow(Registries.BLOCK));
        // Placed features read the world's height range from the generator.
        var generator = new NoiseBasedChunkGenerator(
            net.minecraft.world.level.biome.MultiNoiseBiomeSource.createFromPreset(registries.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
                .getOrThrow(net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists.OVERWORLD)),
            registries.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD));
        CompoundTag cases;
        try (var stream = new java.io.DataInputStream(new java.io.BufferedInputStream(java.nio.file.Files.newInputStream(Path.of(args[0]))))) {
            cases = NbtIo.read(stream);
        }
        var results = new CompoundTag();
        for (String name : cases.keySet()) {
            var input = cases.getCompoundOrEmpty(name);
            int cx = input.getIntOr("cx", 0), cz = input.getIntOr("cz", 0);
            long seed = input.getLongOr("seed", 0);
            int[] layers = input.getIntArray("layers").orElseThrow();
            var pool = pools.getValue(net.minecraft.resources.Identifier.parse(input.getStringOr("pool", "")));
            var element = pool.getTemplates().get(input.getIntOr("index", 0)).getFirst();
            var chunkPos = new ChunkPos(cx, cz);
            var proto = new ProtoChunk(chunkPos, UpgradeData.EMPTY, LevelHeightAccessor.create(-64, 384),
                PalettedContainerFactory.create(registries), null);
            var pos = new BlockPos.MutableBlockPos();
            for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
                proto.setBlockState(pos.set(cx * 16 + x, y, cz * 16 + z), terrain(layers, y), 0);
            proto.setPersistedStatus(ChunkStatus.CARVERS);
            var entities = new HashMap<BlockPos, BlockEntity>();
            // Blocks written to the eight neighboring chunks, which a generation region also lets features write.
            var outside = new HashMap<BlockPos, BlockState>();
            var level = CappedProcessorProbe.seedLevel(seed);
            // Every column has the same layers; heights describe them before placement, as processors read them.
            var world = (WorldGenLevel) Proxy.newProxyInstance(PoolElementProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getHeight" -> arguments == null ? 384 : height(layers, (Heightmap.Types) arguments[0]);
                    case "getHeightmapPos" -> {
                        var at = (BlockPos) arguments[1];
                        yield new BlockPos(at.getX(), height(layers, (Heightmap.Types) arguments[0]), at.getZ());
                    }
                    case "getFluidState" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).getFluidState();
                    case "isStateAtPosition" -> ((java.util.function.Predicate<BlockState>) arguments[1])
                        .test(((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]));
                    case "isFluidAtPosition" -> ((java.util.function.Predicate<net.minecraft.world.level.material.FluidState>) arguments[1])
                        .test(((WorldGenLevel) proxy).getFluidState((BlockPos) arguments[0]));
                    case "isEmptyBlock" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).isAir();
                    case "getChunk" -> proto;
                    case "registryAccess" -> registries;
                    case "holderLookup" -> registries.lookupOrThrow((net.minecraft.resources.ResourceKey) arguments[0]);
                    case "getLevel" -> level;
                    case "isClientSide" -> false;
                    case "getRandom" -> net.minecraft.util.RandomSource.create(0);
                    case "blockUpdated", "updateNeighborsAt", "scheduleTick", "addParticle", "playSound", "gameEvent", "levelEvent" -> null;
                    case "getMinY" -> -64;
                    case "getMaxY" -> 319;
                    case "getSeed" -> seed;
                    case "ensureCanWrite" -> {
                        var at = (BlockPos) arguments[0];
                        yield Math.abs((at.getX() >> 4) - cx) <= 1 && Math.abs((at.getZ() >> 4) - cz) <= 1;
                    }
                    case "getBlockState" -> {
                        var at = (BlockPos) arguments[0];
                        if (at.getY() < -64 || at.getY() >= 320) yield Blocks.VOID_AIR.defaultBlockState();
                        if ((at.getX() >> 4) == cx && (at.getZ() >> 4) == cz) yield proto.getBlockState(at);
                        yield outside.getOrDefault(at, terrain(layers, at.getY()));
                    }
                    case "setBlock" -> {
                        var at = ((BlockPos) arguments[0]).immutable();
                        if (at.getY() < -64 || at.getY() >= 320) yield false;
                        var state = (BlockState) arguments[1];
                        if ((at.getX() >> 4) != cx || (at.getZ() >> 4) != cz) {
                            if (Math.abs((at.getX() >> 4) - cx) > 1 || Math.abs((at.getZ() >> 4) - cz) > 1) throw new AssertionError("write outside region " + at);
                            outside.put(at, state);
                            yield true;
                        }
                        var before = proto.getBlockState(at);
                        proto.setBlockState(at, state, 0);
                        if (before.getBlock() != state.getBlock()) entities.remove(at);
                        yield true;
                    }
                    // Like a proto chunk, a block entity exists once something asks for it, not with its block.
                    case "getBlockEntity" -> {
                        var at = ((BlockPos) arguments[0]).immutable();
                        var state = proto.getBlockState(at);
                        var entity = state.hasBlockEntity()
                            ? entities.computeIfAbsent(at, k -> ((EntityBlock) state.getBlock()).newBlockEntity(k, state)) : null;
                        yield arguments.length == 1 ? entity : Optional.ofNullable(entity);
                    }
                    default -> throw new UnsupportedOperationException(method.toString());
                });
            var random = new WorldgenRandom(new XoroshiroRandomSource(seed));
            var at = new BlockPos(input.getIntOr("x", 0), input.getIntOr("y", 0), input.getIntOr("z", 0));
            var pivot = new BlockPos(input.getIntOr("px", 0), input.getIntOr("py", 0), input.getIntOr("pz", 0));
            var rotation = Rotation.values()[input.getIntOr("rotation", 0)];
            var bounds = new BoundingBox(cx * 16, -64, cz * 16, cx * 16 + 15, 319, cz * 16 + 15);
            var liquid = input.getBooleanOr("ignore_waterlogging", false) ? LiquidSettings.IGNORE_WATERLOGGING : LiquidSettings.APPLY_WATERLOGGING;
            element.place(manager, world, null, generator, at, pivot, rotation, bounds, random, liquid, false);

            var result = new CompoundTag();
            var changes = new ArrayList<Integer>();
            for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                var state = proto.getBlockState(pos.set(cx * 16 + x, y, cz * 16 + z));
                if (state != terrain(layers, y)) { changes.add(((y + 64) * 16 + z) * 16 + x); changes.add(Block.getId(state)); }
            }
            result.putIntArray("changes", changes.stream().mapToInt(Integer::intValue).toArray());
            var saved = new ListTag();
            var positions = new ArrayList<>(entities.keySet());
            positions.sort(Comparator.comparingInt((BlockPos p) -> p.getY()).thenComparingInt(BlockPos::getZ).thenComparingInt(BlockPos::getX));
            for (var position : positions) saved.add(entities.get(position).saveWithFullMetadata(registries));
            result.put("entities", saved);
            var written = new ArrayList<Integer>();
            outside.forEach((where, state) -> {
                if (state != terrain(layers, where.getY())) { written.add(where.getX()); written.add(where.getY()); written.add(where.getZ()); written.add(Block.getId(state)); }
            });
            result.putIntArray("outside", written.stream().mapToInt(Integer::intValue).toArray());
            result.putLong("random_tail", random.nextLong());
            results.put(name, result);
        }
        try (var stream = new java.io.DataOutputStream(new java.io.BufferedOutputStream(java.nio.file.Files.newOutputStream(Path.of(args[1]))))) {
            NbtIo.write(results, stream);
        }
        storage.close();
        server.resources().close();
        net.minecraft.util.Util.shutdownExecutors();
    }

    // Loads each [state, block entity] pair the way vanilla reads saved block entities, and saves it again with
    // full metadata, so that data equal to a block entity's defaults compares equal.
    static void normalize(HolderLookup.Provider registries, Path input, Path output) throws Exception {
        CompoundTag cases;
        try (var stream = new java.io.DataInputStream(new java.io.BufferedInputStream(java.nio.file.Files.newInputStream(input)))) {
            cases = NbtIo.read(stream);
        }
        var results = new CompoundTag();
        for (String name : cases.keySet()) {
            var saved = new ListTag();
            for (var value : cases.getListOrEmpty(name)) {
                var pair = (CompoundTag) value;
                var tag = pair.getCompoundOrEmpty("entity");
                var at = new BlockPos(tag.getIntOr("x", 0), tag.getIntOr("y", 0), tag.getIntOr("z", 0));
                var loaded = BlockEntity.loadStatic(at, Block.stateById(pair.getIntOr("state", 0)), tag, registries);
                if (loaded == null) throw new AssertionError("vanilla cannot load " + tag);
                saved.add(loaded.saveWithFullMetadata(registries));
            }
            results.put(name, saved);
        }
        try (var stream = new java.io.DataOutputStream(new java.io.BufferedOutputStream(java.nio.file.Files.newOutputStream(output)))) {
            NbtIo.write(results, stream);
        }
    }

    static BlockState terrain(int[] layers, int y) {
        for (int i = 0; i < layers.length; i += 2) if (y < layers[i]) return Block.stateById(layers[i + 1]);
        return Blocks.AIR.defaultBlockState();
    }

    /** The first Y above the layers that the heightmap counts, as LevelReader.getHeight returns it. */
    static int height(int[] layers, Heightmap.Types type) {
        for (int y = 319; y >= -64; y--) if (type.isOpaque().test(terrain(layers, y))) return y + 1;
        return -64;
    }
}
