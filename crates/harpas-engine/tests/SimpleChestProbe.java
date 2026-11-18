// Exercises vanilla's block entity storage and both directions of its three-row chest shift-click.
import java.io.*;
import java.nio.file.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.nbt.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.SimpleContainer;
import net.minecraft.world.entity.EntityEquipment;
import net.minecraft.world.entity.player.Inventory;
import net.minecraft.world.inventory.ChestMenu;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.item.Items;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.entity.ChestBlockEntity;

public final class SimpleChestProbe {
    static final BlockPos POS = new BlockPos(1, 65, 0);

    static void write(Path path, CompoundTag tag) throws IOException {
        try (var out = new DataOutputStream(Files.newOutputStream(path))) {
            NbtIo.writeAnyTag(tag, out);
        }
    }

    static ChestBlockEntity load(CompoundTag tag, HolderLookup.Provider lookup) {
        return (ChestBlockEntity) BlockEntity.loadStatic(POS, Blocks.CHEST.defaultBlockState(), tag, lookup);
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        for (var pending : BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) pending.apply();
        if (args.length == 2) {
            try (var input = new DataInputStream(Files.newInputStream(Path.of(args[0])))) {
                var saved = (CompoundTag) NbtIo.readAnyTag(input, NbtAccounter.unlimitedHeap());
                write(Path.of(args[1]), load(saved, lookup).saveWithFullMetadata(lookup));
            }
        } else {
            var chest = new ChestBlockEntity(POS, Blocks.CHEST.defaultBlockState());
            var pickaxe = new ItemStack(Items.DIAMOND_PICKAXE);
            pickaxe.setDamageValue(73);
            chest.setItem(0, new ItemStack(Items.STONE, 61));
            chest.setItem(13, pickaxe);
            chest.setItem(26, new ItemStack(Items.STONE, 9));
            var original = chest.saveWithFullMetadata(lookup);
            original.putString("CustomName", "Supplies");
            chest = load(original, lookup);
            write(Path.of(args[0]), chest.saveWithFullMetadata(lookup));

            // The menu uses a standalone container so creating it needs no level to animate the lid.
            var container = new SimpleContainer(chest.getContainerSize());
            for (int i = 0; i < container.getContainerSize(); i++) container.setItem(i, chest.getItem(i));
            var menu = ChestMenu.threeRows(1, new Inventory(null, new EntityEquipment()), container);
            if (BuiltInRegistries.MENU.getId(menu.getType()) != 2 || menu.slots.size() != 63) {
                throw new AssertionError("three-row menu registry ID or slot count changed");
            }
            menu.quickMoveStack(null, 26);
            menu.quickMoveStack(null, 62);
            for (int i = 0; i < container.getContainerSize(); i++) chest.setItem(i, container.getItem(i));
            write(Path.of(args[1]), chest.saveWithFullMetadata(lookup));

            var componentTypes = BuiltInRegistries.DATA_COMPONENT_TYPE;
            var componentNames = new java.util.ArrayList<String>();
            for (var component : componentTypes) componentNames.add(componentTypes.getKey(component).toString());
            Files.write(Path.of(args[2]), componentNames);
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
