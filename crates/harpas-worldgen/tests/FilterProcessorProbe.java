// Black-box calls to vanilla's processor pipeline, including filtered blocks and settings randomness.
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

public final class FilterProcessorProbe {
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
        var results = new ListTag();
        for (var fixture : fixtures.getAsJsonArray("cases")) {
            var c = fixture.getAsJsonObject();
            var origin = RuleProcessorProbe.position(c.get("origin"));
            var pivot = origin.offset(7, -3, 2);
            long seed = c.get("seed").getAsLong();
            var level = CappedProcessorProbe.seedLevel(seed);
            var original = new ArrayList<StructureTemplate.StructureBlockInfo>();
            var terrain = new HashMap<BlockPos, BlockState>();
            for (var entry : c.getAsJsonArray("blocks")) {
                var b = entry.getAsJsonArray();
                var pos = RuleProcessorProbe.position(b.get(0));
                original.add(new StructureTemplate.StructureBlockInfo(pos.subtract(origin),
                    Block.stateById(b.get(1).getAsInt()), RuleProcessorProbe.inputTag(b.get(2).getAsInt())));
                terrain.put(pos, Block.stateById(b.get(3).getAsInt()));
            }
            var world = (ServerLevelAccessor) Proxy.newProxyInstance(FilterProcessorProbe.class.getClassLoader(),
                new Class<?>[]{ServerLevelAccessor.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getLevel" -> level;
                    case "getBlockState" -> Objects.requireNonNull(terrain.get(arguments[0]), "unexpected terrain position");
                    default -> throw new UnsupportedOperationException(method.toString());
                });
            var random = new net.minecraft.world.level.levelgen.XoroshiroRandomSource(seed);
            var settings = new StructurePlaceSettings();
            boolean explicitRandom = c.get("random").getAsBoolean();
            if (explicitRandom) settings.setRandom(random);
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
            var result = new CompoundTag(); result.put("blocks", snapshots);
            if (explicitRandom) result.putLong("random", random.nextLong());
            results.add(result);
        }
        var root = new CompoundTag(); root.put("results", results);
        try (var output = new java.io.DataOutputStream(new java.io.BufferedOutputStream(Files.newOutputStream(Path.of(args[1]))))) {
            NbtIo.write(root, output);
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
