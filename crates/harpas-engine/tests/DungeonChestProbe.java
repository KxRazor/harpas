// Calls vanilla's chest loot unpacking with the shipped data pack and a deterministic level.
import java.nio.file.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.BlockPos;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.nbt.*;
import net.minecraft.resources.Identifier;
import net.minecraft.server.*;
import net.minecraft.server.dedicated.DedicatedServer;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.util.RandomSource;
import net.minecraft.util.Util;
import net.minecraft.world.RandomSequences;
import net.minecraft.world.flag.*;
import net.minecraft.world.level.WorldDataConfiguration;
import net.minecraft.world.level.block.entity.ChestBlockEntity;
import net.minecraft.world.level.storage.loot.BuiltInLootTables;

public final class DungeonChestProbe {
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
        ProbeLevel() { super(null, null, null, null, null, null, false, 0, List.of(), false); }
        @Override public MinecraftServer getServer() { return server; }
        @Override public FeatureFlagSet enabledFeatures() { return FeatureFlags.VANILLA_SET; }
        @Override public RegistryAccess registryAccess() { return access; }
        @Override public boolean hasChunk(int x, int z) { return false; }
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
                var tags = new ListTag();
                for (int i = 3; i < args.length; i++) {
                    var chest = ChestProbe.make(ChestProbe.SINGLE,
                        net.minecraft.world.level.block.state.properties.ChestType.SINGLE, "Dungeon", lookup);
                    chest.setLevel(level);
                    chest.setLootTable(BuiltInLootTables.SIMPLE_DUNGEON, Long.parseLong(args[i]));
                    tags.add(chest.saveWithFullMetadata(lookup));
                    chest.unpackLootTable(null);
                    tags.add(chest.saveWithFullMetadata(lookup));
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
