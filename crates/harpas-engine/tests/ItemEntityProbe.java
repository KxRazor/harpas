// Observable-codec probe: native ItemEntity.save/load and EntityStorage write real entity-region fixtures.
import java.io.*;
import java.nio.file.*;
import java.util.*;
import com.google.gson.*;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.RegistryOps;
import net.minecraft.nbt.*;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.entity.*;
import net.minecraft.world.entity.item.ItemEntity;
import net.minecraft.world.item.*;
import net.minecraft.world.level.*;
import net.minecraft.world.level.chunk.storage.*;
import net.minecraft.world.level.entity.ChunkEntities;
import net.minecraft.world.level.storage.*;
import net.minecraft.util.ProblemReporter;
import net.minecraft.util.datafix.DataFixTypes;

public final class ItemEntityProbe {
    static final class TestItem extends ItemEntity {
        TestItem(ServerLevel level) { super(EntityType.ITEM, null); setLevel(level); }
    }

    static CompoundTag save(ItemEntity item, net.minecraft.core.HolderLookup.Provider lookup) {
        var out = TagValueOutput.createWithContext(ProblemReporter.DISCARDING, lookup);
        if (!item.save(out)) throw new AssertionError("item refused saving");
        return out.buildResult();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        for (var pending : BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) pending.apply();
        var access = RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY);
        // A level shell supplies only the native codecs' registry and server-side flag; no world ticks run.
        var unsafe = Class.forName("sun.misc.Unsafe");
        var field = unsafe.getDeclaredField("theUnsafe"); field.setAccessible(true);
        var level = (ServerLevel) unsafe.getMethod("allocateInstance", Class.class).invoke(field.get(null), ServerLevel.class);
        var registry = Level.class.getDeclaredField("registryAccess"); registry.setAccessible(true); registry.set(level, access);
        var pos = new ChunkPos(-1, 2);
        var info = new RegionStorageInfo("item-parity", Level.OVERWORLD, "entities");
        if (args[0].equals("write")) {
            var items = new ArrayList<Entity>();
            var ops = RegistryOps.create(com.mojang.serialization.JsonOps.INSTANCE, lookup);
            String[] patches = {"{}", "{\"minecraft:custom_name\":\"Saved drop\"}",
                "{\"minecraft:damage\":7}", "{\"!minecraft:custom_name\":{}}"};
            for (int i = 0; i < 6; i++) {
                var item = new TestItem(level);
                item.setUUID(new UUID(0x123456789abcdef0L + i, 0xfedcba9876543210L - i));
                item.setPos(-0.5 - i, 64.25 + i, 32.5 + i);
                item.setDeltaMovement(0.125 * i, -0.25, -0.0625 * i);
                item.setItem(ItemStack.CODEC.parse(ops, JsonParser.parseString(
                    "{\"id\":\"minecraft:stone\",\"count\":" + (i + 1) + ",\"components\":" + patches[i % patches.length] + "}")).getOrThrow());
                var tag = save(item, lookup);
                tag.putBoolean("OnGround", i % 2 == 0);
                tag.putShort("Age", (short) (i == 4 ? -32768 : i == 5 ? 5999 : i * 100));
                tag.putShort("PickupDelay", (short) (i == 3 ? 32767 : i * 10));
                tag.putShort("Health", (short) (i % 2 == 0 ? 5 : 2));
                item.load(TagValueInput.create(ProblemReporter.DISCARDING, lookup, tag));
                items.add(item);
            }
            try (var storage = new SimpleRegionStorage(info, Path.of(args[1]), null, false, DataFixTypes.ENTITY_CHUNK)) {
                var entities = new EntityStorage(storage, level, Runnable::run);
                entities.storeEntities(new ChunkEntities<Entity>(pos, items));
                entities.flush(true);
            }
            var names = new JsonArray();
            for (var type : BuiltInRegistries.DATA_COMPONENT_TYPE) names.add(BuiltInRegistries.DATA_COMPONENT_TYPE.getKey(type).toString());
            Files.writeString(Path.of(args[2]), names.toString());
        } else {
            try (var storage = new SimpleRegionStorage(info, Path.of(args[1]), null, false, DataFixTypes.ENTITY_CHUNK)) {
                var root = storage.read(pos).join().orElseThrow();
                var entries = root.getListOrEmpty("Entities");
                var saved = new ListTag();
                for (var entry : entries) {
                    var item = new TestItem(level);
                    item.load(TagValueInput.create(ProblemReporter.DISCARDING, lookup, (CompoundTag) entry));
                    saved.add(save(item, lookup));
                }
                root.put("Entities", saved);
                try (var out = new DataOutputStream(Files.newOutputStream(Path.of(args[2])))) { NbtIo.writeAnyTag(root, out); }
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
