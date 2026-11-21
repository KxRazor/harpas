// Observe Vanilla's pickaxe speeds, drop eligibility, and wear with a grounded Survival player.
import java.nio.file.*;
import java.util.*;
import com.google.gson.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.BlockPos;
import net.minecraft.core.component.DataComponents;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.*;
import net.minecraft.server.level.*;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.stats.Stat;
import net.minecraft.util.Util;
import net.minecraft.world.entity.EquipmentSlot;
import net.minecraft.world.item.*;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.state.BlockState;

public final class MiningProbe {
    static final class TestLevel extends ServerLevel {
        TestLevel() { super(null, null, null, null, null, null, false, 0, List.of(), false); }
    }

    static final class Miner extends ServerPlayer {
        ServerLevel level;
        ItemStack tool;
        Miner() { super(null, null, null, null); }
        @Override public ServerLevel level() { return level; }
        @Override public boolean isCreative() { return false; }
        @Override public boolean hasInfiniteMaterials() { return false; }
        @Override public void awardStat(Stat<?> stat, int count) {}
        @Override public void onEquippedItemBroken(Item item, EquipmentSlot slot) {}
        @Override public float getDestroySpeed(BlockState state) { return tool.getDestroySpeed(state); }
        @Override public boolean hasCorrectToolForDrops(BlockState state) {
            return !state.requiresCorrectToolForDrops() || tool.isCorrectToolForDrops(state);
        }
    }

    public static void main(String[] args) throws Exception {
        try {
            SharedConstants.tryDetectVersion();
            Bootstrap.bootStrap();
            var packs = new WorldLoader.PackConfig(ServerPacksSource.createVanillaTrustedRepository(),
                WorldDataConfiguration.DEFAULT, false, false);
            var config = new WorldLoader.InitConfig(packs, Commands.CommandSelection.DEDICATED,
                LevelBasedPermissionSet.OWNER);
            try (var stem = WorldLoader.<Void, WorldStem>load(config,
                    context -> new WorldLoader.DataLoadOutput<>(null, context.datapackDimensions()),
                    (resources, registries, layers, cookie) -> new WorldStem(resources, registries, layers, null),
                    Util.backgroundExecutor(), Runnable::run).get()) {
                var lookup = stem.dataPackResources().fullRegistries().lookup();
                for (var pending : BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) pending.apply();
                var unsafe = Class.forName("sun.misc.Unsafe");
                var field = unsafe.getDeclaredField("theUnsafe");
                field.setAccessible(true);
                var allocate = unsafe.getMethod("allocateInstance", Class.class);
                var miner = (Miner) allocate.invoke(field.get(null), Miner.class);
                miner.level = (TestLevel) allocate.invoke(field.get(null), TestLevel.class);
                var report = new JsonObject();
                var ids = new JsonObject();
                var types = BuiltInRegistries.DATA_COMPONENT_TYPE;
                for (var type : new net.minecraft.core.component.DataComponentType<?>[] {
                        DataComponents.MAX_STACK_SIZE, DataComponents.MAX_DAMAGE,
                        DataComponents.DAMAGE, DataComponents.UNBREAKABLE, DataComponents.TOOL}) {
                    ids.addProperty(types.getKey(type).toString(), types.getId(type));
                }
                report.add("components", ids);
                var cases = new JsonArray();
                for (var item : List.of(Items.WOODEN_PICKAXE, Items.STONE_PICKAXE, Items.IRON_PICKAXE,
                        Items.DIAMOND_PICKAXE)) {
                    for (var name : List.of("stone", "cobblestone", "coal_ore", "iron_ore", "deepslate_iron_ore",
                            "gold_ore", "redstone_ore", "diamond_ore", "deepslate_diamond_ore", "obsidian",
                            "crying_obsidian", "ancient_debris",
                            "dirt", "short_grass")) {
                        var block = BuiltInRegistries.BLOCK.getValue(net.minecraft.resources.Identifier.withDefaultNamespace(name));
                        var state = block.defaultBlockState();
                        var tool = new ItemStack(item);
                        miner.tool = tool;
                        var example = new JsonObject();
                        example.addProperty("block", "minecraft:" + name);
                        example.addProperty("tool", BuiltInRegistries.ITEM.getKey(item).toString());
                        example.addProperty("speed", miner.getDestroySpeed(state));
                        example.addProperty("harvest", miner.hasCorrectToolForDrops(state));
                        example.addProperty("progress_bits", Float.floatToIntBits(state.getDestroyProgress(miner, EmptyBlockGetter.INSTANCE, BlockPos.ZERO)));
                        example.addProperty("maximum", tool.getMaxDamage());
                        tool.mineBlock(miner.level, state, BlockPos.ZERO, miner);
                        example.addProperty("damage", tool.getDamageValue());
                        if (tool.getDamageValue() > 0) {
                            tool.setDamageValue(tool.getMaxDamage() - 1);
                            tool.mineBlock(miner.level, state, BlockPos.ZERO, miner);
                        }
                        example.addProperty("broken", tool.isEmpty());
                        cases.add(example);
                    }
                }
                report.add("cases", cases);
                Files.writeString(Path.of(args[0]), report.toString());
            }
        } finally {
            Util.shutdownExecutors();
        }
    }
}
