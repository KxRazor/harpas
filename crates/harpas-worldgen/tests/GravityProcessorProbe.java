// Black-box calls to vanilla's processor pipeline in a generation region and a live ServerLevel.
import com.google.gson.*;
import com.mojang.serialization.JsonOps;
import java.lang.reflect.Proxy;
import java.nio.file.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.nbt.*;
import net.minecraft.resources.RegistryOps;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ServerLevelAccessor;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.Rotation;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.structure.templatesystem.*;

public final class GravityProcessorProbe {
    static final class Terrain extends net.minecraft.server.level.ServerLevel {
        long seed;
        ListTag reads;
        Terrain() { super(null, null, null, null, null, null, false, 0, null, false); }
        @Override public long getSeed() { return seed; }
        @Override public int getHeight(Heightmap.Types kind, int x, int z) {
            int height = kind.ordinal() * 43 - 90 + Math.floorMod(x, 23) - Math.floorMod(z, 19);
            var read = new CompoundTag(); read.putString("heightmap", kind.name());
            read.putIntArray("pos", new int[]{x, z}); read.putInt("height", height); reads.add(read);
            return height;
        }
        @Override public BlockState getBlockState(BlockPos pos) {
            var state = switch (Math.floorMod(pos.getX() + pos.getY() * 3 + pos.getZ(), 7)) {
                case 0 -> Blocks.BEDROCK.defaultBlockState();
                case 1 -> Blocks.WATER.defaultBlockState();
                default -> Blocks.STONE.defaultBlockState();
            };
            var read = new CompoundTag();
            read.putIntArray("pos", new int[]{pos.getX(), pos.getY(), pos.getZ()});
            read.putInt("state", Block.getId(state)); reads.add(read);
            return state;
        }
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        var ops = RegistryOps.create(JsonOps.INSTANCE, StructurePlacementProbe.loadRegistries());
        var fixtures = JsonParser.parseString(Files.readString(Path.of(args[0]))).getAsJsonObject();
        var pipelines = new ArrayList<List<StructureProcessor>>();
        for (var pipeline : fixtures.getAsJsonArray("pipelines")) {
            var processors = new ArrayList<StructureProcessor>();
            for (var json : pipeline.getAsJsonArray())
                processors.add(StructureProcessorType.SINGLE_CODEC.parse(ops, json).getOrThrow());
            pipelines.add(processors);
        }
        var unsafeClass = Class.forName("sun.misc.Unsafe");
        var field = unsafeClass.getDeclaredField("theUnsafe"); field.setAccessible(true);
        var unsafe = field.get(null);
        var allocate = unsafeClass.getMethod("allocateInstance", Class.class);
        var results = new ListTag();
        for (var fixture : fixtures.getAsJsonArray("cases")) {
            var c = fixture.getAsJsonObject();
            var terrain = (Terrain) allocate.invoke(unsafe, Terrain.class);
            terrain.seed = c.get("seed").getAsLong(); terrain.reads = new ListTag();
            var region = (ServerLevelAccessor) Proxy.newProxyInstance(GravityProcessorProbe.class.getClassLoader(),
                new Class<?>[]{ServerLevelAccessor.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getLevel" -> terrain;
                    case "getHeight" -> terrain.getHeight((Heightmap.Types) arguments[0], (int) arguments[1], (int) arguments[2]);
                    case "getBlockState" -> terrain.getBlockState((BlockPos) arguments[0]);
                    default -> throw new UnsupportedOperationException(method.toString());
                });
            var original = new ArrayList<StructureTemplate.StructureBlockInfo>();
            for (var entry : c.getAsJsonArray("blocks")) {
                var b = entry.getAsJsonArray();
                original.add(new StructureTemplate.StructureBlockInfo(RuleProcessorProbe.position(b.get(0)),
                    Block.stateById(b.get(1).getAsInt()), RuleProcessorProbe.inputTag(b.get(2).getAsInt())));
            }
            var random = new net.minecraft.world.level.levelgen.XoroshiroRandomSource(terrain.seed);
            var settings = new StructurePlaceSettings().setRotation(Rotation.values()[c.get("rotation").getAsInt()]);
            if (c.get("random").getAsBoolean()) settings.setRandom(random);
            for (var processor : pipelines.get(c.get("pipeline").getAsInt())) settings.addProcessor(processor);
            var blocks = StructureTemplate.processBlockInfos(c.get("live").getAsBoolean() ? terrain : region,
                RuleProcessorProbe.position(c.get("origin")), RuleProcessorProbe.position(c.get("pivot")), settings, original);
            var snapshots = new ListTag();
            for (var block : blocks) {
                var snapshot = new CompoundTag(); snapshot.putInt("state", Block.getId(block.state()));
                snapshot.putIntArray("pos", new int[]{block.pos().getX(), block.pos().getY(), block.pos().getZ()});
                if (block.nbt() != null) snapshot.put("nbt", block.nbt()); snapshots.add(snapshot);
            }
            var result = new CompoundTag(); result.put("blocks", snapshots); result.put("reads", terrain.reads);
            result.putLong("random", random.nextLong()); results.add(result);
        }
        var root = new CompoundTag(); root.put("results", results);
        try (var output = new java.io.DataOutputStream(new java.io.BufferedOutputStream(Files.newOutputStream(Path.of(args[1]))))) {
            NbtIo.write(root, output);
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
