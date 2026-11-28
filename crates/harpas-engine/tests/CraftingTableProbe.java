// Test driver: decodes shipped recipes with vanilla's codecs, matches and assembles them on a 3x3 input,
// then dispatches a real quick-move click. The isolated player records drops and has no server,
// network, statistics or recipe book. Results are installed explicitly because ContainerLevelAccess.NULL
// does not run the live level's recipe lookup; matching, assembly, slot movement and consumption are vanilla's.
import java.io.*;
import java.nio.file.*;
import java.util.*;
import com.google.gson.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.RegistryOps;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.entity.EntityEquipment;
import net.minecraft.world.entity.player.*;
import net.minecraft.world.inventory.*;
import net.minecraft.world.item.*;
import net.minecraft.world.item.crafting.*;
import net.minecraft.world.level.GameType;

public final class CraftingTableProbe {
    static final class TestPlayer extends Player {
        Inventory inventory;
        Abilities abilities;
        List<ItemStack> drops;
        TestPlayer() { super(null, null); }
        @Override public GameType gameMode() { return GameType.SURVIVAL; }
        @Override public Inventory getInventory() { return inventory; }
        @Override public Abilities getAbilities() { return abilities; }
        @Override public void awardStat(net.minecraft.stats.Stat<?> stat, int count) {}
        @Override public void awardStat(net.minecraft.resources.Identifier stat, int count) {}
        @Override public net.minecraft.world.entity.item.ItemEntity drop(ItemStack stack, boolean thrown) {
            if (!stack.isEmpty()) drops.add(stack.copy());
            return null;
        }
    }

    // The live menu asks the server's RecipeManager on every grid change. This isolated menu runs the same
    // recipe's native matcher and assembler instead, without constructing a server or replacing click rules.
    static final class TestMenu extends CraftingMenu {
        CraftingRecipe recipe;
        TestMenu(Inventory inventory) { super(1, inventory); }
        @Override public void slotsChanged(net.minecraft.world.Container container) {
            if (recipe == null) return;
            var input = CraftingInput.of(3, 3, getInputGridSlots().stream().map(Slot::getItem).toList());
            getSlot(0).set(recipe.matches(input, null) ? recipe.assemble(input) : ItemStack.EMPTY);
        }
    }

    static ItemStack stack(JsonElement json) {
        if (json.isJsonNull()) return ItemStack.EMPTY;
        var fields = json.getAsJsonArray();
        var item = BuiltInRegistries.ITEM.getValue(net.minecraft.resources.Identifier.parse(fields.get(0).getAsString()));
        return new ItemStack(item, fields.get(1).getAsInt());
    }

    static JsonArray describe(ItemStack stack) {
        var result = new JsonArray();
        result.add(stack.isEmpty() ? 0 : Item.getId(stack.getItem()));
        result.add(stack.isEmpty() ? 0 : stack.getCount());
        return result;
    }

    static JsonArray snapshot(CraftingMenu menu) {
        var result = new JsonArray();
        for (var slot : menu.slots) result.add(describe(slot.getItem()));
        result.add(describe(menu.getCarried()));
        return result;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.ITEM,
            net.minecraft.core.registries.Registries.ITEM, "item");
        var lookup = VanillaRegistries.createLookup();
        for (var pending : BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) pending.apply();
        var ops = RegistryOps.create(com.mojang.serialization.JsonOps.INSTANCE, lookup);
        var unsafeClass = Class.forName("sun.misc.Unsafe");
        var field = unsafeClass.getDeclaredField("theUnsafe"); field.setAccessible(true);
        var allocate = unsafeClass.getMethod("allocateInstance", Class.class);
        var results = new JsonArray();
        var cases = JsonParser.parseString(Files.readString(Path.of(args[2]))).getAsJsonArray();
        for (var element : cases) {
            var test = element.getAsJsonObject();
            var player = (TestPlayer) allocate.invoke(field.get(null), TestPlayer.class);
            player.inventory = new Inventory(player, new EntityEquipment());
            player.abilities = new Abilities();
            player.drops = new ArrayList<>();
            var menu = new TestMenu(player.inventory);
            var grid = test.getAsJsonArray("grid");
            for (int i = 0; i < 9; i++) {
                var stack = stack(grid.get(i));
                menu.getSlot(i + 1).set(stack);
            }
            try (var stream = CraftingTableProbe.class.getResourceAsStream(
                    "/data/minecraft/recipe/" + test.get("recipe").getAsString() + ".json")) {
                var json = JsonParser.parseReader(new InputStreamReader(Objects.requireNonNull(stream)));
                var recipe = (CraftingRecipe) Recipe.CODEC.parse(ops, json).getOrThrow();
                menu.recipe = recipe;
                menu.slotsChanged(null);
            }
            if (test.has("full")) {
                for (int slot = 10; slot < 46; slot++) menu.getSlot(slot).set(new ItemStack(Items.STONE, 64));
                if (!test.get("full").isJsonNull()) menu.getSlot(45).set(stack(test.get("full")));
            }
            var result = new JsonObject();
            result.add("before", snapshot(menu));
            menu.clicked(test.get("move").getAsInt(), 0, ContainerInput.QUICK_MOVE, player);
            result.add("after", snapshot(menu));
            var drops = new JsonArray();
            for (var dropped : player.drops) drops.add(describe(dropped));
            result.add("drops", drops);
            results.add(result);
        }
        var output = new JsonObject();
        output.addProperty("menu_type", BuiltInRegistries.MENU.getId(MenuType.CRAFTING));
        output.add("cases", results);
        Files.writeString(Path.of(args[0]), output.toString());
        net.minecraft.util.Util.shutdownExecutors();
    }
}
