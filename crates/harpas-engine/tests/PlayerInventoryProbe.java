// Observable-codec probe only. Uses vanilla's Inventory, EntityEquipment and item component codecs to create
// a player-data fixture and describe its network values, then reads back the document written by Harpas.
import java.io.*;
import java.nio.file.*;
import java.util.*;
import com.google.gson.*;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.core.*;
import net.minecraft.core.component.*;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.RegistryOps;
import net.minecraft.nbt.*;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.world.ItemStackWithSlot;
import net.minecraft.world.entity.*;
import net.minecraft.world.entity.player.*;
import net.minecraft.world.item.*;
import net.minecraft.world.level.GameType;
import net.minecraft.world.level.storage.*;
import net.minecraft.util.ProblemReporter;

public final class PlayerInventoryProbe {
    static final class TestPlayer extends Player {
        Inventory inventory;
        Abilities abilities;
        TestPlayer() { super(null, null); }
        @Override public GameType gameMode() { return GameType.SURVIVAL; }
        @Override public Inventory getInventory() { return inventory; }
        @Override public Abilities getAbilities() { return abilities; }
    }

    static TestPlayer player(EntityEquipment equipment) throws Exception {
        var type = Class.forName("sun.misc.Unsafe");
        var field = type.getDeclaredField("theUnsafe"); field.setAccessible(true);
        var player = (TestPlayer) type.getMethod("allocateInstance", Class.class).invoke(field.get(null), TestPlayer.class);
        player.inventory = new Inventory(player, equipment); player.abilities = new Abilities();
        return player;
    }

    static void write(Path path, CompoundTag tag) throws Exception {
        try (var out = new DataOutputStream(Files.newOutputStream(path))) { NbtIo.writeAnyTag(tag, out); }
    }

    static CompoundTag save(Inventory inventory, EntityEquipment equipment, HolderLookup.Provider lookup) {
        var output = TagValueOutput.createWithContext(ProblemReporter.DISCARDING, lookup);
        inventory.save(output.list("Inventory", ItemStackWithSlot.CODEC));
        output.store("equipment", EntityEquipment.CODEC, equipment);
        output.putInt("SelectedItemSlot", inventory.getSelectedSlot());
        return output.buildResult();
    }

    @SuppressWarnings("unchecked")
    static JsonObject describe(ItemStack stack, RegistryAccess access) {
        var result = new JsonObject(); result.addProperty("item", stack.isEmpty() ? 0 : Item.getId(stack.getItem()));
        result.addProperty("count", stack.isEmpty() ? 0 : stack.getCount());
        var added = new JsonArray(); var removed = new JsonArray();
        var entries = new ArrayList<>(stack.getComponentsPatch().entrySet());
        entries.sort(Comparator.comparingInt(e -> BuiltInRegistries.DATA_COMPONENT_TYPE.getId(e.getKey())));
        for (var entry : entries) {
            int id = BuiltInRegistries.DATA_COMPONENT_TYPE.getId(entry.getKey());
            if (entry.getValue().isEmpty()) { removed.add(id); continue; }
            var type = (DataComponentType<Object>) entry.getKey();
            var buffer = new RegistryFriendlyByteBuf(io.netty.buffer.Unpooled.buffer(), access);
            type.streamCodec().encode(buffer, entry.getValue().get());
            var bytes = new byte[buffer.readableBytes()]; buffer.readBytes(bytes); buffer.release();
            var component = new JsonArray(); component.add(id); component.add(HexFormat.of().formatHex(bytes)); added.add(component);
        }
        result.add("added", added); result.add("removed", removed); return result;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        for (var pending : BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) pending.apply();
        var enchantments = new MappedRegistry<net.minecraft.world.item.enchantment.Enchantment>(
            net.minecraft.core.registries.Registries.ENCHANTMENT, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(net.minecraft.core.registries.Registries.ENCHANTMENT).listElements()
            .sorted(Comparator.comparing(h -> h.key().identifier().toString()))
            .forEach(h -> enchantments.register(h.key(), h.value(), RegistrationInfo.BUILT_IN));
        enchantments.freeze();
        var ops = RegistryOps.create(NbtOps.INSTANCE, lookup);
        var equipment = new EntityEquipment();
        var player = player(equipment);
        if (args[0].equals("read")) {
            CompoundTag root;
            try (var in = new DataInputStream(Files.newInputStream(Path.of(args[1])))) {
                root = (CompoundTag) NbtIo.readAnyTag(in, NbtAccounter.unlimitedHeap());
            }
            var input = TagValueInput.create(ProblemReporter.DISCARDING, lookup, root);
            equipment.setAll(input.read("equipment", EntityEquipment.CODEC).orElseGet(EntityEquipment::new));
            player.inventory.load(input.listOrEmpty("Inventory", ItemStackWithSlot.CODEC));
            player.inventory.setSelectedSlot(input.getIntOr("SelectedItemSlot", 0));
            write(Path.of(args[2]), save(player.inventory, equipment, lookup));
        } else {
            var jsonOps = RegistryOps.create(com.mojang.serialization.JsonOps.INSTANCE, lookup);
            String[] components = {
                "{\"minecraft:custom_data\":{\"audit\":7}}",
                "{\"minecraft:max_stack_size\":32}", "{\"minecraft:max_damage\":999}", "{\"minecraft:damage\":73}",
                "{\"minecraft:unbreakable\":{}}", "{\"minecraft:custom_name\":\"Named tool\"}",
                "{\"minecraft:minimum_attack_charge\":0.5}", "{\"minecraft:item_name\":{\"text\":\"Gold\",\"color\":\"gold\"}}",
                "{\"minecraft:item_model\":\"minecraft:test_model\"}", "{\"minecraft:lore\":[\"First\",\"Second\"]}",
                "{\"minecraft:rarity\":\"epic\"}", "{\"minecraft:repair_cost\":5}",
                "{\"minecraft:enchantment_glint_override\":true}", "{\"minecraft:intangible_projectile\":{}}",
                "{\"minecraft:glider\":{}}", "{\"minecraft:tooltip_style\":\"minecraft:test_style\"}",
                "{\"minecraft:dyed_color\":1193046}", "{\"minecraft:map_color\":6636321}", "{\"minecraft:map_id\":14}",
                "{\"minecraft:potion_duration_scale\":2.5}", "{\"minecraft:bucket_entity_data\":{\"NoAI\":1}}",
                "{\"minecraft:ominous_bottle_amplifier\":3}", "{\"minecraft:note_block_sound\":\"minecraft:block.note_block.bell\"}",
                "{\"minecraft:enchantable\":{\"value\":12}}", "{\"!minecraft:max_stack_size\":{}}",
                "{\"minecraft:enchantments\":{\"minecraft:efficiency\":3}}",
                "{\"minecraft:stored_enchantments\":{\"minecraft:fortune\":2}}",
                "{\"minecraft:lore\":[\"Plain\",{\"text\":\"Styled\",\"italic\":false}]}",
                "{\"minecraft:lore\":[]}",
                "{\"minecraft:potion_contents\":{\"potion\":\"minecraft:water_breathing\"}}",
                "{\"minecraft:potion_contents\":{\"custom_color\":1193046,\"custom_name\":\"test_potion\"}}",
                "{\"minecraft:potion_contents\":{}}",
            };
            for (int i = 0; i < components.length; i++) {
                var json = JsonParser.parseString("{\"id\":\"minecraft:stone\",\"count\":1,\"components\":" + components[i] + "}");
                player.inventory.setItem(i, ItemStack.CODEC.parse(jsonOps, json).getOrThrow());
            }
            equipment.set(EquipmentSlot.HEAD, new ItemStack(Items.IRON_HELMET));
            equipment.set(EquipmentSlot.CHEST, new ItemStack(Items.IRON_CHESTPLATE));
            equipment.set(EquipmentSlot.LEGS, new ItemStack(Items.IRON_LEGGINGS));
            equipment.set(EquipmentSlot.FEET, new ItemStack(Items.IRON_BOOTS));
            equipment.set(EquipmentSlot.OFFHAND, new ItemStack(Items.SHIELD));
            player.inventory.setSelectedSlot(8);
            var root = save(player.inventory, equipment, lookup);
            root.putInt("DataVersion", SharedConstants.getCurrentVersion().dataVersion().version());
            root.putString("future_player_field", "must survive");
            write(Path.of(args[1]), root);
            var registries = new ArrayList<Registry<?>>();
            RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY).registries().forEach(e -> registries.add(e.value()));
            registries.add(enchantments);
            var access = new RegistryAccess.ImmutableRegistryAccess(registries);
            var result = new JsonObject(); var names = new JsonArray();
            for (var type : BuiltInRegistries.DATA_COMPONENT_TYPE) names.add(BuiltInRegistries.DATA_COMPONENT_TYPE.getKey(type).toString());
            result.add("components", names);
            var enchantmentNames = new JsonArray();
            for (var value : enchantments) enchantmentNames.add(enchantments.getKey(value).toString());
            result.add("enchantments", enchantmentNames);
            var potionNames = new JsonArray();
            for (var value : BuiltInRegistries.POTION) potionNames.add(BuiltInRegistries.POTION.getKey(value).toString());
            result.add("potions", potionNames);
            var slots = new JsonArray();
            for (int i = 0; i < 41; i++) slots.add(describe(player.inventory.getItem(i), access));
            result.add("slots", slots);
            Files.writeString(Path.of(args[2]), result.toString());
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
