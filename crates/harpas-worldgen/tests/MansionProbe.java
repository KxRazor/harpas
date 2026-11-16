// Test driver only: observes vanilla's woodland mansion placement on caller-supplied carved terrain.
// Arguments: <input NBT> <output NBT>. No game code or data is copied into the repository.
//
// Each case gives a chunk, the ring of blocks around it and the mansion's saved start. The probe places the
// mansion's pieces in that chunk with vanilla's own StructureStart.placeInChunk, through a WorldGenLevel backed by
// the chunk, and writes the chunk's blocks and block entities, the blocks it reshaped in the ring and any write
// beyond the ring. Mobs are created but not kept.
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

public final class MansionProbe {
    /// The level the mansion's mobs are created in, made without running a constructor: it only answers what
    /// creating a mob asks of it.
    static final class MobLevel extends net.minecraft.server.level.ServerLevel {
        static net.minecraft.core.RegistryAccess registries;

        private MobLevel() {
            super(null, null, null, null, null, null, false, 0L, null, false);
        }

        @Override
        public net.minecraft.world.flag.FeatureFlagSet enabledFeatures() {
            return net.minecraft.world.flag.FeatureFlags.REGISTRY.allFlags();
        }

        @Override
        public net.minecraft.core.RegistryAccess registryAccess() {
            return registries;
        }

        static net.minecraft.server.MinecraftServer server;

        @Override
        public net.minecraft.server.MinecraftServer getServer() {
            return server;
        }

        static MobLevel create(net.minecraft.core.RegistryAccess registries) throws Exception {
            MobLevel.registries = registries;
            var field = sun.misc.Unsafe.class.getDeclaredField("theUnsafe");
            field.setAccessible(true);
            var unsafe = (sun.misc.Unsafe) field.get(null);
            // A server without a constructor either, with no debug subscribers.
            server = (net.minecraft.server.MinecraftServer)
                unsafe.allocateInstance(net.minecraft.server.dedicated.DedicatedServer.class);
            var subscribers = net.minecraft.server.MinecraftServer.class.getDeclaredField("debugSubscribers");
            unsafe.putObject(server, unsafe.objectFieldOffset(subscribers),
                new net.minecraft.util.debug.ServerDebugSubscribers(server));
            return (MobLevel) unsafe.allocateInstance(MobLevel.class);
        }
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
        var mansion = structures.getValue(net.minecraft.resources.Identifier.parse("mansion"));
        var step = mansion.step();
        int index = structures.stream().filter(s -> s.step() == step).toList().indexOf(mansion);

        var storage = net.minecraft.world.level.storage.LevelStorageSource.createDefault(Path.of("mansion-worlds")).createAccess("test");
        var manager = new net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager(
            server.resources(), storage, net.minecraft.util.datafix.DataFixers.getDataFixer(), registries.lookupOrThrow(Registries.BLOCK));
        var context = new StructurePieceSerializationContext(server.resources(), registries, manager);
        CompoundTag cases;
        try (var stream = new java.io.DataInputStream(new java.io.BufferedInputStream(java.nio.file.Files.newInputStream(Path.of(args[0]))))) {
            cases = NbtIo.read(stream);
        }
        var mobLevel = MobLevel.create(registries);
        var results = new CompoundTag();
        results.putInt("index", index);
        results.putInt("step", step.ordinal());
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
            int[] surrounding = input.getIntArray("surrounding").orElseThrow();
            var ring = new HashMap<BlockPos, net.minecraft.world.level.block.state.BlockState>();
            var ringWrites = new CompoundTag();
            var farWrites = new CompoundTag();
            var world = (WorldGenLevel) Proxy.newProxyInstance(MansionProbe.class.getClassLoader(),
                new Class<?>[] {WorldGenLevel.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getFluidState" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).getFluidState();
                    case "getChunk" -> proto;
                    case "registryAccess" -> registries;
                    case "getLevel" -> mobLevel;
                    case "environmentAttributes" -> net.minecraft.world.attribute.EnvironmentAttributeReader.EMPTY;
                    case "isClientSide" -> false;
                    case "getRandom" -> net.minecraft.util.RandomSource.create(0);
                    case "isEmptyBlock" -> ((WorldGenLevel) proxy).getBlockState((BlockPos) arguments[0]).isAir();
                    case "blockUpdated", "updateNeighborsAt", "scheduleTick", "addFreshEntityWithPassengers", "addFreshEntity" -> null;
                    case "getCurrentDifficultyAt" -> new net.minecraft.world.DifficultyInstance(net.minecraft.world.Difficulty.NORMAL, 0, 0, 0f);
                    // No chunk has initialized its light during FEATURES, as WorldgenReferenceProbe explains.
                    case "getRawBrightness" -> Math.max(0, 15 - (int) arguments[1]);
                    case "getBrightness" -> arguments[0] == net.minecraft.world.level.LightLayer.SKY ? 15 : 0;
                    case "getMinY" -> -64;
                    case "getMaxY" -> 319;
                    case "getHeight" -> arguments == null ? 384 : null;
                    case "getSeed" -> seed;
                    case "getBlockState" -> {
                        var at = (BlockPos) arguments[0];
                        if (at.getY() < -64 || at.getY() >= 320) yield Blocks.VOID_AIR.defaultBlockState();
                        if ((at.getX() >> 4) == cx && (at.getZ() >> 4) == cz) yield proto.getBlockState(at);
                        if (ring.containsKey(at)) yield ring.get(at);
                        int x = at.getX() - cx * 16 + 1, z = at.getZ() - cz * 16 + 1;
                        if (x < 0 || x >= 18 || z < 0 || z >= 18) throw new AssertionError("read outside provided terrain in " + name + ": " + at);
                        yield Block.stateById(surrounding[((at.getY() + 64) * 18 + z) * 18 + x]);
                    }
                    case "setBlock" -> {
                        var at = ((BlockPos) arguments[0]).immutable();
                        var state = (net.minecraft.world.level.block.state.BlockState) arguments[1];
                        if (at.getY() < -64 || at.getY() >= 320) yield false;
                        if ((at.getX() >> 4) != cx || (at.getZ() >> 4) != cz) {
                            int x = at.getX() - cx * 16, z = at.getZ() - cz * 16;
                            String key = at.getX() + "," + at.getY() + "," + at.getZ();
                            if (x >= -1 && x <= 16 && z >= -1 && z <= 16) {
                                ring.put(at, state);
                                ringWrites.putInt(key, Block.getId(state));
                                yield true;
                            }
                            farWrites.putInt(key, Block.getId(state));
                            yield false;
                        }
                        var before = proto.getBlockState(at);
                        proto.setBlockState(at, state, 0);
                        if (before.getBlock() != state.getBlock()) entities.remove(at);
                        if (state.hasBlockEntity()) entities.computeIfAbsent(at, k -> ((EntityBlock) state.getBlock()).newBlockEntity(k, state));
                        yield true;
                    }
                    case "getBlockEntity" -> arguments.length == 1 ? entities.get(arguments[0]) : Optional.ofNullable(entities.get(arguments[0]));
                    default -> throw new UnsupportedOperationException(method.toString());
                });
            var start = StructureStart.loadStaticStart(context, input.getCompoundOrEmpty("start"), seed);
            if (start == null) throw new AssertionError("failed to load the start of " + name);
            var random = new WorldgenRandom(new XoroshiroRandomSource(0));
            long decoration = random.setDecorationSeed(seed, cx * 16, cz * 16);
            random.setFeatureSeed(decoration, index, step.ordinal());
            var bounds = new BoundingBox(cx * 16, -64, cz * 16, cx * 16 + 15, 319, cz * 16 + 15);
            start.placeInChunk(world, null, null, random, bounds, chunkPos);
            var result = new CompoundTag();
            int[] after = new int[384 * 256];
            for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
                after[((y + 64) * 16 + z) * 16 + x] = Block.getId(proto.getBlockState(pos.set(cx * 16 + x, y, cz * 16 + z)));
            result.putIntArray("blocks", after);
            var tags = new ListTag();
            entities.values().stream().sorted(Comparator.comparingLong(be -> be.getBlockPos().asLong()))
                .forEach(be -> tags.add(be.saveWithFullMetadata(registries)));
            result.put("block_entities", tags);
            result.put("ring_writes", ringWrites);
            result.put("far_writes", farWrites);
            result.putLong("tail", random.nextLong());
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
