// Test driver only: observes vanilla's StructureStart.placeInChunk for jigsaw starts saved by Harpas, on carved
// chunks Harpas supplies. Arguments: <input NBT> <output NBT>. No game code or data is copied into the repository.
//
// The input holds the world seed, the carved chunks by "x,z" as block state IDs, the saved starts by name and the
// cases, each a start and a target chunk. A case builds proto-chunks at carvers for the target and its eight
// neighbors, which a generation region lets features write; farther chunks read as air with heights at the world's
// bottom. The random source is seeded as vanilla's decoration does, from the structure's index in its step. The
// output holds, for each case, the target's block states and its block entities with full metadata.
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
import net.minecraft.world.level.levelgen.structure.*;
import net.minecraft.world.level.levelgen.structure.pieces.StructurePieceSerializationContext;

public final class JigsawChunkProbe {
    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var structures = registries.lookupOrThrow(Registries.STRUCTURE);
        var storage = net.minecraft.world.level.storage.LevelStorageSource.createDefault(Path.of("jigsaw-worlds")).createAccess("test");
        var manager = new net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager(
            server.resources(), storage, net.minecraft.util.datafix.DataFixers.getDataFixer(), registries.lookupOrThrow(Registries.BLOCK));
        var context = new StructurePieceSerializationContext(server.resources(), registries, manager);
        var generator = new NoiseBasedChunkGenerator(
            net.minecraft.world.level.biome.MultiNoiseBiomeSource.createFromPreset(registries.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
                .getOrThrow(net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists.OVERWORLD)),
            registries.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD));
        CompoundTag input;
        try (var stream = new java.io.DataInputStream(new java.io.BufferedInputStream(java.nio.file.Files.newInputStream(Path.of(args[0]))))) {
            input = NbtIo.read(stream);
        }
        long seed = input.getLongOr("seed", 0);
        var carved = input.getCompoundOrEmpty("chunks");
        var saved = input.getCompoundOrEmpty("starts");
        var results = new CompoundTag();
        var level = CappedProcessorProbe.seedLevel(seed);
        for (var value : input.getListOrEmpty("cases")) {
            var c = (CompoundTag) value;
            String name = c.getStringOr("name", "");
            int cx = c.getIntOr("x", 0), cz = c.getIntOr("z", 0);
            var start = StructureStart.loadStaticStart(context, saved.getCompoundOrEmpty(c.getStringOr("start", "")), seed);
            if (start == null) throw new AssertionError("vanilla cannot load start for " + name);
            var protos = new HashMap<Long, ProtoChunk>();
            for (int dx = -1; dx <= 1; dx++) for (int dz = -1; dz <= 1; dz++) {
                var at = new ChunkPos(cx + dx, cz + dz);
                int[] blocks = carved.getIntArray(at.x() + "," + at.z()).orElseThrow(() -> new AssertionError("missing chunk " + at));
                var proto = new ProtoChunk(at, UpgradeData.EMPTY, LevelHeightAccessor.create(-64, 384), PalettedContainerFactory.create(registries), null);
                var pos = new BlockPos.MutableBlockPos();
                for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
                    proto.setBlockState(pos.set(at.x() * 16 + x, y, at.z() * 16 + z), Block.stateById(blocks[((y + 64) * 16 + z) * 16 + x]), 0);
                proto.setPersistedStatus(ChunkStatus.CARVERS);
                Heightmap.primeHeightmaps(proto, EnumSet.allOf(Heightmap.Types.class));
                protos.put(ChunkPos.pack(at.x(), at.z()), proto);
            }
            var entities = new HashMap<BlockPos, BlockEntity>();
            var world = (WorldGenLevel) Proxy.newProxyInstance(JigsawChunkProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> {
                    java.util.function.Function<BlockPos, ProtoChunk> chunkAt = at -> protos.get(ChunkPos.pack(at.getX() >> 4, at.getZ() >> 4));
                    return switch (method.getName()) {
                        case "getHeight" -> {
                            if (arguments == null) yield 384;
                            int x = (int) arguments[1], z = (int) arguments[2];
                            var proto = protos.get(ChunkPos.pack(x >> 4, z >> 4));
                            yield proto == null ? -64 : proto.getHeight((Heightmap.Types) arguments[0], x & 15, z & 15) + 1;
                        }
                        case "getHeightmapPos" -> {
                            var at = (BlockPos) arguments[1];
                            yield new BlockPos(at.getX(), ((WorldGenLevel) proxy).getHeight((Heightmap.Types) arguments[0], at.getX(), at.getZ()), at.getZ());
                        }
                        case "getBlockState" -> {
                            var at = (BlockPos) arguments[0];
                            var proto = chunkAt.apply(at);
                            yield proto == null || at.getY() < -64 || at.getY() >= 320 ? Blocks.AIR.defaultBlockState() : proto.getBlockState(at);
                        }
                        case "getFluidState" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).getFluidState();
                        case "isStateAtPosition" -> ((java.util.function.Predicate<BlockState>) arguments[1])
                            .test(((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]));
                        case "isFluidAtPosition" -> ((java.util.function.Predicate<net.minecraft.world.level.material.FluidState>) arguments[1])
                            .test(((WorldGenLevel) proxy).getFluidState((BlockPos) arguments[0]));
                        case "isEmptyBlock" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).isAir();
                        case "ensureCanWrite" -> chunkAt.apply((BlockPos) arguments[0]) != null;
                        case "setBlock" -> {
                            var at = ((BlockPos) arguments[0]).immutable();
                            var proto = chunkAt.apply(at);
                            if (proto == null) throw new AssertionError("write outside region " + at);
                            if (at.getY() < -64 || at.getY() >= 320) yield false;
                            var state = (BlockState) arguments[1];
                            var before = proto.getBlockState(at);
                            proto.setBlockState(at, state, 0);
                            if (before.getBlock() != state.getBlock()) entities.remove(at);
                            yield true;
                        }
                        // Like a proto-chunk, a block entity exists once something asks for it, not with its block.
                        case "getBlockEntity" -> {
                            var at = ((BlockPos) arguments[0]).immutable();
                            var proto = chunkAt.apply(at);
                            var state = proto == null ? Blocks.AIR.defaultBlockState() : proto.getBlockState(at);
                            var entity = state.hasBlockEntity()
                                ? entities.computeIfAbsent(at, k -> ((EntityBlock) state.getBlock()).newBlockEntity(k, state)) : null;
                            yield arguments.length == 1 ? entity : Optional.ofNullable(entity);
                        }
                        case "getChunk" -> arguments[0] instanceof BlockPos at ? chunkAt.apply(at)
                            : chunkAt.apply(new BlockPos((int) arguments[0] * 16, 0, (int) arguments[1] * 16));
                        case "registryAccess" -> registries;
                        case "holderLookup" -> registries.lookupOrThrow((net.minecraft.resources.ResourceKey) arguments[0]);
                        case "getLevel" -> level;
                        case "isClientSide" -> false;
                        case "getRandom" -> net.minecraft.util.RandomSource.create(0);
                        case "blockUpdated", "updateNeighborsAt", "scheduleTick", "addParticle", "playSound", "gameEvent", "levelEvent" -> null;
                        case "getMinY" -> -64;
                        case "getMaxY" -> 319;
                        case "getSeed" -> seed;
                        default -> throw new UnsupportedOperationException(method.toString());
                    };
                });
            var structure = start.getStructure();
            var step = structure.step();
            int index = structures.stream().filter(s -> s.step() == step).sorted(Comparator.comparing(s -> structures.getKey(s).toString()))
                .toList().indexOf(structure);
            var random = new WorldgenRandom(new XoroshiroRandomSource(0));
            long decoration = random.setDecorationSeed(seed, cx * 16, cz * 16);
            random.setFeatureSeed(decoration, index, step.ordinal());
            var chunkPos = new ChunkPos(cx, cz);
            start.placeInChunk(world, null, generator, random, new BoundingBox(cx * 16, -64, cz * 16, cx * 16 + 15, 319, cz * 16 + 15), chunkPos);

            var result = new CompoundTag();
            var target = protos.get(ChunkPos.pack(cx, cz));
            int[] blocks = new int[16 * 384 * 16];
            var pos = new BlockPos.MutableBlockPos();
            for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
                blocks[((y + 64) * 16 + z) * 16 + x] = Block.getId(target.getBlockState(pos.set(cx * 16 + x, y, cz * 16 + z)));
            result.putIntArray("blocks", blocks);
            var list = new ListTag();
            var positions = new ArrayList<>(entities.keySet());
            positions.removeIf(at -> (at.getX() >> 4) != cx || (at.getZ() >> 4) != cz);
            positions.sort(Comparator.comparingInt((BlockPos p) -> p.getY()).thenComparingInt(BlockPos::getZ).thenComparingInt(BlockPos::getX));
            for (var at : positions) list.add(entities.get(at).saveWithFullMetadata(registries));
            result.put("entities", list);
            results.put(name, result);
        }
        try (var stream = new java.io.DataOutputStream(new java.io.BufferedOutputStream(java.nio.file.Files.newOutputStream(Path.of(args[1]))))) {
            NbtIo.write(results, stream);
        }
        storage.close();
        server.resources().close();
        net.minecraft.util.Util.shutdownExecutors();
    }
}
