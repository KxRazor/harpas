// Opens and removes chests using the shipped data pack and a deterministic level.
import java.nio.file.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.BlockPos;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.Holder;
import net.minecraft.core.component.DataComponents;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.nbt.*;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.*;
import net.minecraft.server.dedicated.DedicatedServer;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.util.RandomSource;
import net.minecraft.util.Util;
import net.minecraft.tags.TagKey;
import net.minecraft.world.RandomSequences;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.item.ItemEntity;
import net.minecraft.world.flag.*;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.LevelReader;
import net.minecraft.world.level.StructureManager;
import net.minecraft.world.level.WorldDataConfiguration;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.properties.ChestType;
import net.minecraft.world.level.gamerules.GameRules;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.presets.WorldPresets;
import net.minecraft.world.level.levelgen.structure.*;
import net.minecraft.world.level.levelgen.structure.placement.*;
import net.minecraft.world.level.saveddata.maps.*;
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
        MapSearch search;
        MapId latestMap;
        MapItemSavedData mapData;
        ProbeLevel() { super(null, null, null, null, null, null, false, 0, List.of(), false); }
        @Override public MinecraftServer getServer() { return server; }
        @Override public FeatureFlagSet enabledFeatures() { return FeatureFlags.VANILLA_SET; }
        @Override public RegistryAccess registryAccess() { return access; }
        @Override public boolean hasChunk(int x, int z) { return false; }
        @Override public GameRules getGameRules() { return rules; }
        @Override public ResourceKey<Level> dimension() { return Level.OVERWORLD; }
        @Override public BiomeManager getBiomeManager() { return search.biomes; }
        @Override public BlockPos findNearestMapStructure(TagKey<Structure> tag, BlockPos pos, int radius, boolean skip) {
            if (skip || !tag.location().toString().equals("minecraft:on_treasure_maps") || radius != 50) {
                throw new AssertionError("Unexpected shipwreck map search");
            }
            return search == null ? null : search.find(this, pos, radius);
        }
        @Override public MapId getFreeMapId() { return new MapId(0); }
        @Override public MapItemSavedData getMapData(MapId id) { return mapData; }
        @Override public void setMapData(MapId id, MapItemSavedData data) {
            latestMap = id;
            mapData = data;
        }
        @Override public void updateNeighbourForOutputSignal(BlockPos pos, Block block) {}
        @Override public boolean addFreshEntity(Entity entity) {
            drops.add(((ItemEntity) entity).getItem().copy());
            return true;
        }
    }

    // Calls the vanilla random-spread search and provides vanilla-generated structure starts on demand.
    static final class MapSearch extends StructureManager {
        final ChunkGenerator generator;
        final ChunkGeneratorStructureState state;
        final RegistryAccess registries;
        final Holder<Structure> treasure;
        final RandomSpreadStructurePlacement placement;
        final PalettedContainerFactory palettes;
        final BiomeManager biomes;
        final java.lang.reflect.Method nearest;
        final Map<ChunkPos, StructureCheckResult> starts = new HashMap<>();

        MapSearch(RegistryAccess registries) throws Exception {
            super(null, new WorldOptions(0, true, false), null);
            this.registries = registries;
            palettes = PalettedContainerFactory.create(registries);
            var set = registries.lookupOrThrow(Registries.STRUCTURE_SET).getOrThrow(ResourceKey.create(
                Registries.STRUCTURE_SET, Identifier.withDefaultNamespace("buried_treasures")));
            treasure = set.value().structures().getFirst().structure();
            placement = (RandomSpreadStructurePlacement) set.value().placement();
            generator = WorldPresets.getNormalOverworld(registries).generator();
            var random = RandomState.create(registries, NoiseGeneratorSettings.OVERWORLD, 0);
            var ctor = ChunkGeneratorStructureState.class.getDeclaredConstructor(
                RandomState.class, BiomeSource.class, long.class, long.class, List.class);
            ctor.setAccessible(true);
            state = ctor.newInstance(random, generator.getBiomeSource(), 0L, 0L, List.of(set));
            biomes = new BiomeManager((x, y, z) -> generator.getBiomeSource().getNoiseBiome(x, y, z, random.sampler()),
                BiomeManager.obfuscateSeed(0));
            nearest = ChunkGenerator.class.getDeclaredMethod("getNearestGeneratedStructure", Set.class,
                LevelReader.class, StructureManager.class, int.class, int.class, int.class,
                boolean.class, long.class, RandomSpreadStructurePlacement.class);
            nearest.setAccessible(true);
        }

        @Override public StructureCheckResult checkStructurePresence(ChunkPos pos, Structure structure,
                StructurePlacement placement, boolean skip) {
            return starts.computeIfAbsent(pos, p -> {
                // The saved test chest's chunk has an empty structure starts compound.
                if (p.x() == 0 && p.z() == 0) return StructureCheckResult.START_NOT_PRESENT;
                var chunk = new ProtoChunk(p, UpgradeData.EMPTY, LevelHeightAccessor.create(-64, 384), palettes, null);
                generator.createStructures(registries, state, this, chunk, null, Level.OVERWORLD);
                var start = chunk.getStartForStructure(structure);
                return start != null && start.isValid() ? StructureCheckResult.START_PRESENT
                    : StructureCheckResult.START_NOT_PRESENT;
            });
        }

        BlockPos find(ProbeLevel level, BlockPos origin, int radius) {
            try {
                for (int ring = 0; ring <= radius; ring++) {
                    var result = (com.mojang.datafixers.util.Pair<?, ?>) nearest.invoke(null, Set.of(treasure),
                        level, this, origin.getX() >> 4, origin.getZ() >> 4, ring, false, 0L, placement);
                    if (result != null) return (BlockPos) result.getFirst();
                }
                return null;
            } catch (ReflectiveOperationException e) {
                throw new IllegalStateException(e);
            }
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
                    case "shipwreck_map", "shipwreck_map_empty" -> BuiltInLootTables.SHIPWRECK_MAP;
                    default -> throw new IllegalArgumentException(args[3]);
                };
                Files.write(Path.of(args[4]), BuiltInRegistries.POTION.stream()
                    .map(potion -> BuiltInRegistries.POTION.getKey(potion).toString()).toList());
                Files.write(Path.of(args[5]), BuiltInRegistries.MOB_EFFECT.stream()
                    .map(effect -> BuiltInRegistries.MOB_EFFECT.getKey(effect).toString()).toList());
                var tags = new ListTag();
                var ops = lookup.createSerializationContext(NbtOps.INSTANCE);
                if (args[3].equals("shipwreck_map")) level.search = new MapSearch(level.access);
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
                    if (level.mapData != null) {
                        removal.put("MapData", MapItemSavedData.type(level.latestMap).codec()
                            .encodeStart(ops, level.mapData).getOrThrow());
                    }
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
