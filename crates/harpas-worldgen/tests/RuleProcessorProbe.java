// Black-box calls to vanilla's rule processor. JSON fixtures are generated locally by the Rust harness.
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
import net.minecraft.world.level.LevelReader;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.levelgen.structure.templatesystem.*;

public final class RuleProcessorProbe {
    static BlockPos position(JsonElement value) {
        var a = value.getAsJsonArray();
        return new BlockPos(a.get(0).getAsInt(), a.get(1).getAsInt(), a.get(2).getAsInt());
    }

    static CompoundTag inputTag(int kind) {
        if (kind == 0) return null;
        var tag = new CompoundTag();
        if (kind == 2) {
            var nested = new CompoundTag(); nested.putInt("nested", 7);
            tag.put("keep", nested);
            tag.putString("LootTable", "test:old");
            tag.putLong("LootTableSeed", 123);
        }
        return tag;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        var registries = StructurePlacementProbe.loadRegistries();
        var ops = RegistryOps.create(JsonOps.INSTANCE, registries);
        var fixtures = JsonParser.parseString(Files.readString(Path.of(args[0]))).getAsJsonObject();
        var processors = new ArrayList<StructureProcessor>();
        for (var json : fixtures.getAsJsonArray("processors"))
            processors.add(StructureProcessorType.SINGLE_CODEC.parse(ops, json).getOrThrow());
        var results = new ListTag();
        for (var fixture : fixtures.getAsJsonArray("cases")) {
            var c = fixture.getAsJsonArray();
            var current = new StructureTemplate.StructureBlockInfo(position(c.get(3)), Block.stateById(c.get(1).getAsInt()), inputTag(c.get(5).getAsInt()));
            var original = new StructureTemplate.StructureBlockInfo(new BlockPos(3, 7, -4), Block.stateById(0), null);
            var location = Block.stateById(c.get(2).getAsInt());
            var world = (LevelReader) Proxy.newProxyInstance(RuleProcessorProbe.class.getClassLoader(),
                new Class<?>[]{LevelReader.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getBlockState" -> {
                        if (!arguments[0].equals(current.pos())) throw new AssertionError("unexpected terrain position");
                        yield location;
                    }
                    default -> throw new UnsupportedOperationException(method.toString());
                });
            // A custom settings random and a different origin expose accidental dependence on either.
            var settings = new StructurePlaceSettings().setRandom(new net.minecraft.world.level.levelgen.XoroshiroRandomSource(42));
            var output = processors.get(c.get(0).getAsInt()).processBlock(world, new BlockPos(1000, -20, -1000),
                position(c.get(4)), original, current, settings);
            var result = new CompoundTag();
            result.putInt("state", Block.getId(output.state()));
            result.putIntArray("pos", new int[]{output.pos().getX(), output.pos().getY(), output.pos().getZ()});
            if (output.nbt() != null) result.put("nbt", output.nbt());
            results.add(result);
        }
        var root = new CompoundTag(); root.put("results", results);
        try (var output = new java.io.DataOutputStream(new java.io.BufferedOutputStream(Files.newOutputStream(Path.of(args[1]))))) {
            NbtIo.write(root, output);
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
