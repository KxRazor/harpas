// Black-box calls to vanilla's complete per-block and finalization pipeline, without placing blocks.
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
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.structure.templatesystem.*;

public final class CappedProcessorProbe {
    // CappedProcessor asks the backing ServerLevel only for its world seed.
    static final class SeedLevel extends net.minecraft.server.level.ServerLevel {
        long seed;
        SeedLevel() { super(null, null, null, null, null, null, false, 0, null, false); }
        @Override public long getSeed() { return seed; }
    }
    static SeedLevel seedLevel(long seed) throws Exception {
        var type = Class.forName("sun.misc.Unsafe");
        var field = type.getDeclaredField("theUnsafe"); field.setAccessible(true);
        var level = (SeedLevel) type.getMethod("allocateInstance", Class.class).invoke(field.get(null), SeedLevel.class);
        level.seed = seed;
        return level;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        var ops = RegistryOps.create(JsonOps.INSTANCE, StructurePlacementProbe.loadRegistries());
        var fixtures = JsonParser.parseString(Files.readString(Path.of(args[0]))).getAsJsonObject();
        var pipelines = new ArrayList<List<StructureProcessor>>();
        for (var pipeline : fixtures.getAsJsonArray("pipelines")) {
            var processors = new ArrayList<StructureProcessor>();
            for (var json : pipeline.getAsJsonArray()) {
                try {
                    processors.add(StructureProcessorType.SINGLE_CODEC.parse(ops, json).getOrThrow());
                } catch (RuntimeException error) {
                    throw new IllegalArgumentException("pipeline " + pipelines.size() + ": " + json, error);
                }
            }
            pipelines.add(processors);
        }
        var results = new ListTag();
        for (var fixture : fixtures.getAsJsonArray("cases")) {
            var c = fixture.getAsJsonObject();
            var origin = RuleProcessorProbe.position(c.get("origin"));
            var pivot = RuleProcessorProbe.position(c.get("pivot"));
            long seed = c.get("seed").getAsLong();
            var level = seedLevel(seed);
            var original = new ArrayList<StructureTemplate.StructureBlockInfo>();
            var terrain = new HashMap<BlockPos, BlockState>();
            for (var entry : c.getAsJsonArray("blocks")) {
                var b = entry.getAsJsonArray();
                var pos = RuleProcessorProbe.position(b.get(0));
                original.add(new StructureTemplate.StructureBlockInfo(pos.subtract(origin),
                    Block.stateById(b.get(1).getAsInt()), RuleProcessorProbe.inputTag(b.get(2).getAsInt())));
                terrain.put(pos, Block.stateById(b.get(3).getAsInt()));
            }
            var world = (ServerLevelAccessor) Proxy.newProxyInstance(CappedProcessorProbe.class.getClassLoader(),
                new Class<?>[]{ServerLevelAccessor.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getLevel" -> level;
                    case "getBlockState" -> Objects.requireNonNull(terrain.get(arguments[0]), "unexpected terrain position");
                    default -> throw new UnsupportedOperationException(method.toString());
                });
            var settingsRandom = new net.minecraft.world.level.levelgen.XoroshiroRandomSource(42);
            var settings = new StructurePlaceSettings().setRandom(settingsRandom);
            for (var processor : pipelines.get(c.get("pipeline").getAsInt())) settings.addProcessor(processor);
            var blocks = StructureTemplate.processBlockInfos(world, origin, pivot, settings, original);
            var snapshots = new ListTag();
            for (var block : blocks) {
                var snapshot = new CompoundTag();
                snapshot.putInt("state", Block.getId(block.state()));
                snapshot.putIntArray("pos", new int[]{block.pos().getX(), block.pos().getY(), block.pos().getZ()});
                if (block.nbt() != null) snapshot.put("nbt", block.nbt());
                snapshots.add(snapshot);
            }
            // Neither the cap nor the rule may consume the placement settings' random source.
            if (settingsRandom.nextLong() != new net.minecraft.world.level.levelgen.XoroshiroRandomSource(42).nextLong())
                throw new AssertionError("settings random consumed");
            var result = new CompoundTag(); result.put("blocks", snapshots); results.add(result);
        }
        var root = new CompoundTag(); root.put("results", results);
        try (var output = new java.io.DataOutputStream(new java.io.BufferedOutputStream(Files.newOutputStream(Path.of(args[1]))))) {
            NbtIo.write(root, output);
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
