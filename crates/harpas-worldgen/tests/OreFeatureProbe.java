// Parity probe: runs vanilla's OreFeature on a flat stone chunk with the decoration random source and
// writes the placed blocks and the random state after the call, for the Rust ore feature to compare.
import java.io.BufferedWriter;
import java.io.FileWriter;
import java.util.EnumSet;
import java.util.Locale;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.RegistrationInfo;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.MappedRegistry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.resources.Identifier;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.feature.ConfiguredFeature;
import net.minecraft.world.level.levelgen.feature.Feature;
import net.minecraft.world.level.levelgen.feature.FeaturePlaceContext;
import net.minecraft.world.level.levelgen.feature.configurations.OreConfiguration;
import net.minecraft.world.level.levelgen.structure.templatesystem.BlockMatchTest;

public final class OreFeatureProbe {
    record Case(long seed, int originY, int size, float discard, int stoneTop) {}

    @SuppressWarnings({ "rawtypes", "unchecked" })
    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        var biomes = new MappedRegistry<Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(
            holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(java.util.List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var height = LevelHeightAccessor.create(-64, 384);
        var key = ResourceKey.create(Registries.CONFIGURED_FEATURE, Identifier.parse("minecraft:ore_iron"));
        ConfiguredFeature<?, ?> configured = lookup.lookupOrThrow(Registries.CONFIGURED_FEATURE)
            .getOrThrow(key).value();
        Case[] cases = {
            new Case(42, 32, 9, 0.0f, 63),
            new Case(0, 32, 9, 0.0f, 63),
            new Case(-1, 12, 15, 0.0f, 63),
            new Case(42, 31, 9, 0.5f, 29),
        };
        var out = new StringBuilder();
        for (int index = 0; index < cases.length; index++) {
            Case test = cases[index];
            var proto = new ProtoChunk(new ChunkPos(0, 0), UpgradeData.EMPTY, height, factory, null);
            var stone = Blocks.STONE.defaultBlockState();
            var pos = new BlockPos.MutableBlockPos();
            for (int y = -64; y <= test.stoneTop(); y++) {
                for (int z = 0; z < 16; z++) {
                    for (int x = 0; x < 16; x++) proto.setBlockState(pos.set(x, y, z), stone, 0);
                }
            }
            Heightmap.primeHeightmaps(proto, EnumSet.of(Heightmap.Types.OCEAN_FLOOR_WG));
            proto.setPersistedStatus(net.minecraft.world.level.chunk.status.ChunkStatus.CARVERS);
            WorldGenLevel level = (WorldGenLevel) java.lang.reflect.Proxy.newProxyInstance(
                OreFeatureProbe.class.getClassLoader(), new Class<?>[] { WorldGenLevel.class },
                (proxy, method, methodArgs) -> {
                    switch (method.getName()) {
                        case "getBlockState": return proto.getBlockState((BlockPos) methodArgs[0]);
                        case "setBlock": {
                            proto.setBlockState((BlockPos) methodArgs[0], (BlockState) methodArgs[1], 0);
                            return true;
                        }
                        case "ensureCanWrite": return true;
                        case "getHeight": {
                            if (methodArgs.length == 0) return 384;
                            return test.stoneTop() + 1;
                        }
                        case "getMinY": return -64;
                        case "getMaxY": return 319;
                        case "getSectionsCount": return 24;
                        case "getMinSectionY": return -4;
                        case "getMaxSectionY": return 19;
                        case "getSectionIndex": return (((Integer) methodArgs[0]) + 64) >> 4;
                        case "getSectionYFromSectionIndex": return ((Integer) methodArgs[0]) - 4;
                        case "isOutsideBuildHeight": {
                            int y = methodArgs[0] instanceof BlockPos block ? block.getY() : (Integer) methodArgs[0];
                            return y < -64 || y >= 320;
                        }
                        case "getSeed": return test.seed();
                        case "getCenter": return proto.getPos();
                        case "getChunk": return proto;
                        case "toString": return "FlatLevel";
                        case "hashCode": return System.identityHashCode(proxy);
                        case "equals": return proxy == methodArgs[0];
                        default: {
                            Class<?> type = method.getReturnType();
                            if (type == boolean.class) return false;
                            if (type == int.class) return 0;
                            if (type == long.class) return 0L;
                            if (type == float.class) return 0.0f;
                            if (type == double.class) return 0.0;
                            return null;
                        }
                    }
                });
            var config = new OreConfiguration(new BlockMatchTest(Blocks.STONE),
                Blocks.IRON_ORE.defaultBlockState(), test.size(), test.discard());
            var random = new WorldgenRandom(new XoroshiroRandomSource(0));
            long decoration = random.setDecorationSeed(test.seed(), 0, 0);
            random.setFeatureSeed(decoration, 0, 0);
            var context = new FeaturePlaceContext(java.util.Optional.empty(), level, null, random,
                new BlockPos(8, test.originY(), 8), config);
            boolean placed = ((Feature) configured.feature()).place(context);
            out.append("case ").append(index).append(' ').append(placed).append('\n');
            for (int y = -64; y < 320; y++) {
                for (int z = 0; z < 16; z++) {
                    for (int x = 0; x < 16; x++) {
                        if (proto.getBlockState(pos.set(x, y, z)).is(Blocks.IRON_ORE)) {
                            out.append("block ").append(x).append(' ').append(y).append(' ')
                               .append(z).append('\n');
                        }
                    }
                }
            }
            out.append("post ")
               .append(random.nextInt()).append(' ')
               .append(Long.toUnsignedString(random.nextLong())).append(' ')
               .append(Integer.toUnsignedString(Float.floatToRawIntBits(random.nextFloat()))).append(' ')
               .append(Long.toUnsignedString(Double.doubleToRawLongBits(random.nextDouble()))).append('\n');
        }
        try (var writer = new BufferedWriter(new FileWriter(args[0]))) { writer.write(out.toString()); }
    }
}
