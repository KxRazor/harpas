// Opens and removes chests using the shipped data pack and a deterministic level.
import java.nio.file.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.BlockPos;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.component.DataComponents;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.nbt.*;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.resources.Identifier;
import net.minecraft.server.*;
import net.minecraft.server.dedicated.DedicatedServer;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.util.RandomSource;
import net.minecraft.util.Util;
import net.minecraft.world.RandomSequences;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.item.ItemEntity;
import net.minecraft.world.flag.*;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.WorldDataConfiguration;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.properties.ChestType;
import net.minecraft.world.level.gamerules.GameRules;
import net.minecraft.world.level.storage.loot.BuiltInLootTables;

public final class ChestLootProbe {
    static final class ProbeServer extends DedicatedServer {
        ReloadableServerRegistries.Holder registries;
        RandomSequences sequences;
        ProbeServer() { super(null, null, null, null, null, null, null, null); }
        @Override public ReloadableServerRegistries.Holder reloadableRegistries() { return registries; }
        @Override public RandomSource getRandomSequence(Identifier name) { return sequences.get(name, 0); }
    }

    static final class ProbeLevel extends ServerLevel {
        ProbeServer server;
        RegistryAccess access;
        GameRules rules;
        List<ItemStack> drops;
        ProbeLevel() { super(null, null, null, null, null, null, false, 0, List.of(), false); }
        @Override public MinecraftServer getServer() { return server; }
        @Override public FeatureFlagSet enabledFeatures() { return FeatureFlags.VANILLA_SET; }
        @Override public RegistryAccess registryAccess() { return access; }
        @Override public boolean hasChunk(int x, int z) { return false; }
        @Override public GameRules getGameRules() { return rules; }
        @Override public void updateNeighbourForOutputSignal(BlockPos pos, Block block) {}
        @Override public boolean addFreshEntity(Entity entity) {
            drops.add(((ItemEntity) entity).getItem().copy());
            return true;
        }
    }

    public static void main(String[] args) throws Exception {
        try {
            SharedConstants.tryDetectVersion();
            Bootstrap.bootStrap();
            var pack = new WorldLoader.PackConfig(
                ServerPacksSource.createVanillaTrustedRepository(), WorldDataConfiguration.DEFAULT, false, false);
            var init = new WorldLoader.InitConfig(pack, Commands.CommandSelection.DEDICATED,
                LevelBasedPermissionSet.OWNER);
            try (var stem = WorldLoader.<Void, WorldStem>load(init,
                    context -> new WorldLoader.DataLoadOutput<>(null, context.datapackDimensions()),
                    (resources, reloadable, layers, cookie) -> new WorldStem(resources, reloadable, layers, null),
                    Util.backgroundExecutor(), Runnable::run).get()) {
                var unsafeClass = Class.forName("sun.misc.Unsafe");
                var field = unsafeClass.getDeclaredField("theUnsafe");
                field.setAccessible(true);
                var unsafe = field.get(null);
                var allocate = unsafeClass.getMethod("allocateInstance", Class.class);
                var level = (ProbeLevel) allocate.invoke(unsafe, ProbeLevel.class);
                level.server = (ProbeServer) allocate.invoke(unsafe, ProbeServer.class);
                level.server.registries = stem.dataPackResources().fullRegistries();
                level.server.sequences = new RandomSequences();
                level.access = stem.registries().compositeAccess();
                level.rules = new GameRules(FeatureFlags.VANILLA_SET);
                var randomField = Level.class.getDeclaredField("random");
                randomField.setAccessible(true);
                randomField.set(level, RandomSource.create(0));
                var lookup = level.server.registries.lookup();
                for (var pending : BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(lookup)) pending.apply();
                var componentNames = BuiltInRegistries.DATA_COMPONENT_TYPE.keySet().stream()
                    .sorted(Comparator.comparingInt(key -> BuiltInRegistries.DATA_COMPONENT_TYPE.getId(
                        BuiltInRegistries.DATA_COMPONENT_TYPE.getValue(key))))
                    .map(Identifier::toString).toList();
                Files.write(Path.of(args[1]), componentNames);
                var enchantments = level.access.lookupOrThrow(Registries.ENCHANTMENT);
                var enchantmentNames = new ArrayList<String>();
                for (var enchantment : enchantments) {
                    enchantmentNames.add(enchantments.getKey(enchantment).toString());
                }
                Files.write(Path.of(args[2]), enchantmentNames);
                var table = switch (args[3]) {
                    case "simple_dungeon" -> BuiltInLootTables.SIMPLE_DUNGEON;
                    case "desert_pyramid" -> BuiltInLootTables.DESERT_PYRAMID;
                    case "buried_treasure" -> BuiltInLootTables.BURIED_TREASURE;
                    case "shipwreck_treasure" -> BuiltInLootTables.SHIPWRECK_TREASURE;
                    case "shipwreck_supply" -> BuiltInLootTables.SHIPWRECK_SUPPLY;
                    default -> throw new IllegalArgumentException(args[3]);
                };
                Files.write(Path.of(args[4]), BuiltInRegistries.POTION.stream()
                    .map(potion -> BuiltInRegistries.POTION.getKey(potion).toString()).toList());
                Files.write(Path.of(args[5]), BuiltInRegistries.MOB_EFFECT.stream()
                    .map(effect -> BuiltInRegistries.MOB_EFFECT.getKey(effect).toString()).toList());
                var tags = new ListTag();
                var ops = lookup.createSerializationContext(NbtOps.INSTANCE);
                for (int i = 6; i < args.length; i++) {
                    var chest = ChestProbe.make(ChestProbe.SINGLE, ChestType.SINGLE, "Loot Chest", lookup);
                    chest.setLevel(level);
                    chest.setLootTable(table, Long.parseLong(args[i]));
                    var pending = chest.saveWithFullMetadata(lookup);
                    tags.add(pending);
                    level.server.sequences = new RandomSequences();
                    chest.unpackLootTable(null);
                    tags.add(chest.saveWithFullMetadata(lookup));

                    level.server.sequences = new RandomSequences();
                    var unopened = ChestProbe.load(chest.getBlockPos(), ChestType.SINGLE, pending, lookup);
                    unopened.setLevel(level);
                    level.drops = new ArrayList<>();
                    unopened.preRemoveSideEffects(unopened.getBlockPos(), unopened.getBlockState());
                    var drops = new ListTag();
                    for (var drop : level.drops) drops.add(ItemStack.CODEC.encodeStart(ops, drop).getOrThrow());
                    var removal = new CompoundTag();
                    removal.put("Drops", drops);
                    var stewBytes = new CompoundTag();
                    var type = DataComponents.SUSPICIOUS_STEW_EFFECTS;
                    for (int slot = 0; slot < chest.getContainerSize(); slot++) {
                        var effects = chest.getItem(slot).get(type);
                        if (effects == null) continue;
                        var buffer = new RegistryFriendlyByteBuf(io.netty.buffer.Unpooled.buffer(), level.access);
                        try {
                            type.streamCodec().encode(buffer, effects);
                            stewBytes.putByteArray(Integer.toString(slot), io.netty.buffer.ByteBufUtil.getBytes(buffer));
                        } finally {
                            buffer.release();
                        }
                    }
                    removal.put("StewBytes", stewBytes);
                    tags.add(removal);
                }
                try (var out = new java.io.DataOutputStream(Files.newOutputStream(Path.of(args[0])))) {
                    NbtIo.writeAnyTag(tags, out);
                }
            }
        } finally {
            Util.shutdownExecutors();
        }
    }
}
