// Observe the normal furnace and its saved state without starting a reference server.
import java.nio.file.*;
import java.util.*;
import com.google.gson.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.*;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.*;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.util.Util;
import net.minecraft.world.flag.*;
import net.minecraft.world.inventory.MenuType;
import net.minecraft.world.item.*;
import net.minecraft.world.item.crafting.RecipeManager;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.entity.*;
import net.minecraft.world.level.block.state.BlockState;

public final class FurnaceProbe {
    static final class TestLevel extends ServerLevel {
        RecipeManager recipes;
        FuelValues fuels;
        RegistryAccess registries;
        BlockState state;
        TestLevel() { super(null, null, null, null, null, null, false, 0, List.of(), false); }
        @Override public RecipeManager recipeAccess() { return recipes; }
        @Override public FuelValues fuelValues() { return fuels; }
        @Override public RegistryAccess registryAccess() { return registries; }
        @Override public FeatureFlagSet enabledFeatures() { return FeatureFlags.VANILLA_SET; }
        @Override public BlockState getBlockState(BlockPos pos) { return state; }
        @Override public boolean setBlock(BlockPos pos, BlockState state, int flags, int depth) {
            this.state = state;
            return true;
        }
        @Override public void blockEntityChanged(BlockPos pos) {}
        @Override public void updateNeighbourForOutputSignal(BlockPos pos, Block block) {}
    }

    static JsonObject frame(FurnaceBlockEntity furnace, HolderLookup.Provider lookup) {
        var tag = furnace.saveWithFullMetadata(lookup);
        var properties = new JsonArray();
        for (var key : List.of("lit_time_remaining", "lit_total_time", "cooking_time_spent", "cooking_total_time")) {
            properties.add(tag.getShortOr(key, (short) 0));
        }
        var slots = new JsonArray();
        for (int i = 0; i < furnace.getContainerSize(); i++) {
            var stack = furnace.getItem(i);
            var slot = new JsonArray();
            slot.add(stack.isEmpty() ? 0 : Item.getId(stack.getItem()));
            slot.add(stack.isEmpty() ? 0 : stack.getCount());
            slots.add(slot);
        }
        var result = new JsonObject();
        result.add("properties", properties);
        result.add("slots", slots);
        return result;
    }

    public static void main(String[] args) throws Exception {
        try {
            SharedConstants.tryDetectVersion();
            Bootstrap.bootStrap();
            var packs = new WorldLoader.PackConfig(
                ServerPacksSource.createVanillaTrustedRepository(), WorldDataConfiguration.DEFAULT, false, false);
            var init = new WorldLoader.InitConfig(packs, Commands.CommandSelection.DEDICATED,
                LevelBasedPermissionSet.OWNER);
            try (var stem = WorldLoader.<Void, WorldStem>load(init,
                    context -> new WorldLoader.DataLoadOutput<>(null, context.datapackDimensions()),
                    (resources, registries, layers, cookie) -> new WorldStem(resources, registries, layers, null),
                    Util.backgroundExecutor(), Runnable::run).get()) {
                var unsafe = Class.forName("sun.misc.Unsafe");
                var field = unsafe.getDeclaredField("theUnsafe");
                field.setAccessible(true);
                var level = (TestLevel) unsafe.getMethod("allocateInstance", Class.class).invoke(field.get(null), TestLevel.class);
                level.registries = stem.registries().compositeAccess();
                level.recipes = stem.dataPackResources().getRecipeManager();
                level.recipes.finalizeRecipeLoading(FeatureFlags.VANILLA_SET);
                var lookup = stem.dataPackResources().fullRegistries().lookup();
                for (var pending : BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) pending.apply();
                level.fuels = FuelValues.vanillaBurnTimes(lookup, FeatureFlags.VANILLA_SET);
                level.state = Blocks.FURNACE.defaultBlockState();
                var furnace = new FurnaceBlockEntity(BlockPos.ZERO, level.state);
                furnace.setLevel(level);
                furnace.setItem(0, new ItemStack(Items.RAW_IRON, 2));
                furnace.setItem(1, new ItemStack(Items.COAL));
                furnace.setItem(2, new ItemStack(Items.IRON_INGOT, 64));
                var report = new JsonObject();
                report.addProperty("menu", BuiltInRegistries.MENU.getId(MenuType.FURNACE));
                var fuels = new JsonObject();
                for (var item : level.fuels.fuelItems()) {
                    fuels.addProperty(BuiltInRegistries.ITEM.getKey(item).toString(),
                        level.fuels.burnDuration(new ItemStack(item)));
                }
                report.add("fuels", fuels);
                for (int i = 0; i < 3; i++) {
                    AbstractFurnaceBlockEntity.serverTick(level, BlockPos.ZERO, level.state, furnace);
                }
                report.add("blocked", frame(furnace, lookup));
                furnace.setItem(2, ItemStack.EMPTY);
                var cooking = new JsonArray();
                cooking.add(frame(furnace, lookup));
                for (int tick = 1; tick <= 200; tick++) {
                    AbstractFurnaceBlockEntity.serverTick(level, BlockPos.ZERO, level.state, furnace);
                    cooking.add(frame(furnace, lookup));
                    if (tick == 80) {
                        var saved = furnace.saveWithFullMetadata(lookup);
                        furnace = (FurnaceBlockEntity) BlockEntity.loadStatic(BlockPos.ZERO, level.state, saved, lookup);
                        furnace.setLevel(level);
                    }
                }
                report.add("cooking", cooking);
                Files.writeString(Path.of(args[0]), report.toString());
            }
        } finally {
            Util.shutdownExecutors();
        }
    }
}
