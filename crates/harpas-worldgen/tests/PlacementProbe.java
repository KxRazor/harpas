// Parity probe: generates 3x3 chunks through carvers and runs every placed feature's placement modifiers on the
// center chunk with vanilla's own classes, seeded as applyBiomeDecoration seeds each feature. The configured
// features are not placed, so the terrain stays as carved; each modifier stream is consumed as
// PlacedFeature.placeWithContext consumes it. Writes "feature <step> <index>" and then every position that
// comes out, for the Rust modifiers to compare.
import java.io.*;
import java.util.*;
import java.util.stream.Stream;
import java.util.zip.ZipFile;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.server.Bootstrap;
import net.minecraft.tags.TagKey;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.level.*;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.carver.CarverConfiguration;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import net.minecraft.world.level.levelgen.placement.PlacementContext;
import net.minecraft.world.level.levelgen.structure.Structure;

public final class PlacementProbe {
    /** The built-in registries leave block tags unbound; binds every block tag in the jar. */
    static void bindBlockTags(String jar) throws Exception {
        var tags = new HashMap<TagKey<Block>, List<Holder<Block>>>();
        try (var zip = new ZipFile(jar)) {
            var entries = zip.entries();
            while (entries.hasMoreElements()) {
                var name = entries.nextElement().getName();
                var parts = name.split("/", 5);
                if (parts.length != 5 || !parts[0].equals("data") || !parts[2].equals("tags")
                        || !parts[3].equals("block") || !name.endsWith(".json")) continue;
                var id = parts[1] + ":" + parts[4].substring(0, parts[4].length() - ".json".length());
                var holders = new LinkedHashSet<Holder<Block>>();
                WorldgenReferenceProbe.addBlockTag(id, holders, new HashSet<>());
                tags.put(TagKey.create(Registries.BLOCK, Identifier.parse(id)), new ArrayList<>(holders));
            }
        }
        ((MappedRegistry<Block>) BuiltInRegistries.BLOCK)
            .prepareTagReload(new TagLoader.LoadResult<>(Registries.BLOCK, tags)).apply();
    }

    static long key(int x, int z) { return ((long) x << 32) ^ (z & 0xffffffffL); }

    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        bindBlockTags(args[1]);
        long seed = Long.parseLong(args[2]);
        var lookup = VanillaRegistries.createLookup();
        var random = RandomState.create(lookup, NoiseGeneratorSettings.OVERWORLD, seed);
        var biomes = new MappedRegistry<Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(
            holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var structures = new MappedRegistry<Structure>(Registries.STRUCTURE, com.mojang.serialization.Lifecycle.stable());
        structures.freeze();
        var placed = new MappedRegistry<PlacedFeature>(Registries.PLACED_FEATURE, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.PLACED_FEATURE).listElements().forEach(
            holder -> placed.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        placed.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes, structures, placed));
        var factory = PalettedContainerFactory.create(access);
        var biomeSource = MultiNoiseBiomeSource.createFromPreset(
            lookup.getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        var height = LevelHeightAccessor.create(-64, 384);
        var generator = new NoiseBasedChunkGenerator(biomeSource, lookup.getOrThrow(NoiseGeneratorSettings.OVERWORLD));

        var carverBlocks = new LinkedHashSet<Holder<Block>>();
        WorldgenReferenceProbe.addBlockTag("minecraft:overworld_carver_replaceables", carverBlocks, new HashSet<>());
        var replaceable = HolderSet.direct(new ArrayList<>(carverBlocks));
        var replaceableField = CarverConfiguration.class.getDeclaredField("replaceable");
        replaceableField.setAccessible(true);
        for (var holder : lookup.lookupOrThrow(Registries.CONFIGURED_CARVER).listElements().toList()) {
            replaceableField.set(holder.value().config(), replaceable);
        }

        var field = ChunkGenerator.class.getDeclaredField("featuresPerStep");
        field.setAccessible(true);
        @SuppressWarnings("unchecked")
        var steps = ((java.util.function.Supplier<List<FeatureSorter.StepFeatureData>>) field.get(generator)).get();

        var discard = new DataOutputStream(OutputStream.nullOutputStream());
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (int argument = 3; argument < args.length; argument++) {
                var coordinates = args[argument].split(",");
                int cx = Integer.parseInt(coordinates[0]), cz = Integer.parseInt(coordinates[1]);
                var chunks = new HashMap<Long, ProtoChunk>();
                for (int dx = -1; dx <= 1; dx++) {
                    for (int dz = -1; dz <= 1; dz++) {
                        var at = new ChunkPos(cx + dx, cz + dz);
                        var proto = new ProtoChunk(at, UpgradeData.EMPTY, height, factory, null);
                        var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                        var manager = new StructureManager(region, new WorldOptions(seed, false, false), null);
                        ChunkParityProbe.writeChunk(discard, seed, biomes, factory, generator, random, region, manager, proto);
                        chunks.put(key(at.x(), at.z()), proto);
                    }
                }
                var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, chunks.get(key(cx, cz)), seed);
                region.chunks.putAll(chunks);
                region.biomes = new BiomeManager(
                    (qx, qy, qz) -> region.getChunk(qx >> 2, qz >> 2).getNoiseBiome(qx, qy, qz),
                    BiomeManager.obfuscateSeed(seed));

                out.println("chunk " + cx + " " + cz);
                var featureRandom = new WorldgenRandom(new XoroshiroRandomSource(0));
                var origin = new BlockPos(cx * 16, height.getMinY(), cz * 16);
                long decoration = featureRandom.setDecorationSeed(seed, origin.getX(), origin.getZ());
                for (int step = 0; step < steps.size(); step++) {
                    var features = steps.get(step).features();
                    for (int index = 0; index < features.size(); index++) {
                        var feature = features.get(index);
                        featureRandom.setFeatureSeed(decoration, index, step);
                        out.println("feature " + step + " " + index);
                        var context = new PlacementContext(region, generator, Optional.of(feature));
                        Stream<BlockPos> positions = Stream.of(origin);
                        for (var modifier : feature.placement()) {
                            positions = positions.flatMap(position -> modifier.getPositions(context, featureRandom, position));
                        }
                        positions.forEach(position ->
                            out.println(position.getX() + " " + position.getY() + " " + position.getZ()));
                    }
                }
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
