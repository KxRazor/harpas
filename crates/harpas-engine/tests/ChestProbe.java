// Exercises vanilla's chest combination, menu transfers and block entity storage.
import java.io.*;
import java.lang.reflect.Proxy;
import java.nio.file.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.nbt.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.CompoundContainer;
import net.minecraft.world.Container;
import net.minecraft.world.SimpleContainer;
import net.minecraft.world.entity.EntityEquipment;
import net.minecraft.world.entity.player.Inventory;
import net.minecraft.world.inventory.ChestMenu;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.item.Items;
import net.minecraft.world.level.LevelAccessor;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.ChestBlock;
import net.minecraft.world.level.block.DoubleBlockCombiner;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.entity.BlockEntityType;
import net.minecraft.world.level.block.entity.ChestBlockEntity;
import net.minecraft.world.level.block.state.properties.ChestType;

public final class ChestProbe {
    static final BlockPos SINGLE = new BlockPos(1, 65, 0);
    static final BlockPos RIGHT = new BlockPos(0, 65, 0);
    static final BlockPos LEFT = new BlockPos(-1, 65, 0);

    static void write(Path path, List<ChestBlockEntity> chests, HolderLookup.Provider lookup) throws IOException {
        var tags = new ListTag();
        for (var chest : chests) tags.add(chest.saveWithFullMetadata(lookup));
        try (var out = new DataOutputStream(Files.newOutputStream(path))) {
            NbtIo.writeAnyTag(tags, out);
        }
    }

    static ChestBlockEntity load(BlockPos pos, ChestType type, CompoundTag tag, HolderLookup.Provider lookup) {
        var state = Blocks.CHEST.defaultBlockState().setValue(ChestBlock.TYPE, type);
        return (ChestBlockEntity) BlockEntity.loadStatic(pos, state, tag, lookup);
    }

    static ChestBlockEntity make(BlockPos pos, ChestType type, String name, HolderLookup.Provider lookup) {
        var state = Blocks.CHEST.defaultBlockState().setValue(ChestBlock.TYPE, type);
        var tag = new ChestBlockEntity(pos, state).saveWithFullMetadata(lookup);
        tag.putString("CustomName", name);
        return load(pos, type, tag, lookup);
    }

    static List<ChestBlockEntity> combine(ChestBlockEntity clicked, ChestBlockEntity other) {
        var blocks = Map.of(clicked.getBlockPos(), clicked, other.getBlockPos(), other);
        var level = (LevelAccessor) Proxy.newProxyInstance(
            LevelAccessor.class.getClassLoader(), new Class<?>[]{LevelAccessor.class},
            (proxy, method, args) -> switch (method.getName()) {
                case "getBlockEntity" -> blocks.get(args[0]);
                case "getBlockState" -> blocks.get(args[0]).getBlockState();
                default -> throw new AssertionError(method);
            });
        return DoubleBlockCombiner.combineWithNeigbour(
            BlockEntityType.CHEST, ChestBlock::getBlockType, ChestBlock::getConnectedDirection,
            ChestBlock.FACING, clicked.getBlockState(), level, clicked.getBlockPos(), (l, p) -> false
        ).apply(new DoubleBlockCombiner.Combiner<ChestBlockEntity, List<ChestBlockEntity>>() {
            public List<ChestBlockEntity> acceptDouble(ChestBlockEntity first, ChestBlockEntity second) {
                return List.of(first, second);
            }

            public List<ChestBlockEntity> acceptSingle(ChestBlockEntity chest) {
                throw new AssertionError("vanilla did not combine the chest");
            }

            public List<ChestBlockEntity> acceptNone() {
                throw new AssertionError("vanilla could not open the chest");
            }
        });
    }

    static SimpleContainer copy(ChestBlockEntity chest) {
        // The temporary container has no level and does not animate a lid when opened.
        var container = new SimpleContainer(chest.getContainerSize());
        for (int i = 0; i < container.getContainerSize(); i++) container.setItem(i, chest.getItem(i));
        return container;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        for (var pending : BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) pending.apply();
        if (args.length == 2) {
            try (var input = new DataInputStream(Files.newInputStream(Path.of(args[0])))) {
                var tags = (ListTag) NbtIo.readAnyTag(input, NbtAccounter.unlimitedHeap());
                var chests = tags.size() == 1 ? List.of(load(SINGLE, ChestType.SINGLE, tags.getCompoundOrEmpty(0), lookup))
                    : List.of(load(RIGHT, ChestType.RIGHT, tags.getCompoundOrEmpty(0), lookup),
                              load(LEFT, ChestType.LEFT, tags.getCompoundOrEmpty(1), lookup));
                write(Path.of(args[1]), chests, lookup);
            }
        } else {
            boolean doubleChest = Boolean.parseBoolean(args[3]);
            var first = doubleChest ? make(RIGHT, ChestType.RIGHT, "Supplies", lookup)
                                    : make(SINGLE, ChestType.SINGLE, "Supplies", lookup);
            var pickaxe = new ItemStack(Items.DIAMOND_PICKAXE);
            pickaxe.setDamageValue(73);
            first.setItem(0, new ItemStack(Items.STONE, 61));
            first.setItem(13, pickaxe);
            List<ChestBlockEntity> chests;
            if (doubleChest) {
                var second = make(LEFT, ChestType.LEFT, "Other supplies", lookup);
                second.setItem(0, new ItemStack(Items.STONE, 60));
                second.setItem(26, new ItemStack(Items.STONE, 9));
                chests = combine(second, first);
                if (!chests.equals(combine(first, second))) throw new AssertionError("slot order depends on clicked half");
            } else {
                first.setItem(26, new ItemStack(Items.STONE, 9));
                chests = List.of(first);
            }
            write(Path.of(args[0]), chests, lookup);

            var player = new Inventory(null, new EntityEquipment());
            Container container = doubleChest ? new CompoundContainer(copy(chests.get(0)), copy(chests.get(1)))
                                              : copy(first);
            var menu = doubleChest ? ChestMenu.sixRows(1, player, container) : ChestMenu.threeRows(1, player, container);
            int lastChestSlot = container.getContainerSize() - 1;
            int lastPlayerSlot = container.getContainerSize() + 35;
            if (BuiltInRegistries.MENU.getId(menu.getType()) != (doubleChest ? 5 : 2)
                    || menu.slots.size() != lastPlayerSlot + 1) {
                throw new AssertionError("chest menu registry ID or slot count changed");
            }
            menu.quickMoveStack(null, lastChestSlot);
            menu.quickMoveStack(null, lastPlayerSlot);
            for (int i = 0; i <= lastChestSlot; i++) chests.get(i / 27).setItem(i % 27, container.getItem(i));
            write(Path.of(args[1]), chests, lookup);

            var componentTypes = BuiltInRegistries.DATA_COMPONENT_TYPE;
            var componentNames = new ArrayList<String>();
            for (var component : componentTypes) componentNames.add(componentTypes.getKey(component).toString());
            Files.write(Path.of(args[2]), componentNames);
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
