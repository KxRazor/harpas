// Parity probe: generates 3x3 chunks through carvers, plants the grass blocks on top (plantGround) and places
// vanilla's own tree configured features in the center chunk, one after another, so that later trees grow through, are cut short by or change the leaves of
// earlier ones. Each tree gets a fresh decoration random source seeded from the chunk and its index. Writes
// each tree's origin, its result and the next value of its random source, then the blocks that changed in the
// 3x3 chunks, then the block entities of the 3x3 chunks as they would be saved, in network NBT and hex.
//
// Arguments: the output file, the server jar, the seed, the number of trees per chunk, a comma-separated list
// of configured features cycled through, then the chunks as x,z.
import java.io.*;
import java.util.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.*;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.*;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.carver.CarverConfiguration;
import net.minecraft.world.level.levelgen.structure.Structure;

public final class TreeFeatureProbe {
    static long key(int x, int z) { return ((long) x << 32) ^ (z & 0xffffffffL); }

    // Gives the trees neighbors whose shapes they update: on the top grass block of each column, by a pattern of
    // its coordinates, tall grass, short grass, a poppy, or snow on the grass block itself.
    static void plantGround(Map<Long, ProtoChunk> chunks, int cx, int cz) {
        var pos = new BlockPos.MutableBlockPos();
        var tall = net.minecraft.world.level.block.Blocks.TALL_GRASS.defaultBlockState();
        var half = net.minecraft.world.level.block.DoublePlantBlock.HALF;
        for (int z = (cz - 1) * 16; z < (cz + 2) * 16; z++) {
            for (int x = (cx - 1) * 16; x < (cx + 2) * 16; x++) {
                var chunk = chunks.get(key(x >> 4, z >> 4));
                int top = 319;
                while (top > -64 && chunk.getBlockState(pos.set(x, top, z)).isAir()) top--;
                var ground = chunk.getBlockState(pos.set(x, top, z));
                if (!ground.is(net.minecraft.world.level.block.Blocks.GRASS_BLOCK)) continue;
                switch (Math.floorMod(x * 3 + z * 7, 6)) {
                    case 0 -> {
                        chunk.setBlockState(pos.set(x, top + 1, z), tall.setValue(half, net.minecraft.world.level.block.state.properties.DoubleBlockHalf.LOWER), 0);
                        chunk.setBlockState(pos.set(x, top + 2, z), tall.setValue(half, net.minecraft.world.level.block.state.properties.DoubleBlockHalf.UPPER), 0);
                    }
                    case 1 -> chunk.setBlockState(pos.set(x, top + 1, z), net.minecraft.world.level.block.Blocks.SHORT_GRASS.defaultBlockState(), 0);
                    case 2 -> chunk.setBlockState(pos.set(x, top, z), ground.setValue(net.minecraft.world.level.block.SnowyBlock.SNOWY, true), 0);
                    case 3 -> chunk.setBlockState(pos.set(x, top + 1, z), net.minecraft.world.level.block.Blocks.POPPY.defaultBlockState(), 0);
                    default -> {}
                }
            }
        }
    }

    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        PlacementProbe.bindTags(args[1], BuiltInRegistries.BLOCK, Registries.BLOCK, "block");
        PlacementProbe.bindTags(args[1], BuiltInRegistries.FLUID, Registries.FLUID, "fluid");
        long seed = Long.parseLong(args[2]);
        int trees = Integer.parseInt(args[3]);
        String[] features = args[4].split(",");
        var lookup = VanillaRegistries.createLookup();
        var random = RandomState.create(lookup, NoiseGeneratorSettings.OVERWORLD, seed);
        var biomes = new MappedRegistry<Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(
            holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var structures = new MappedRegistry<Structure>(Registries.STRUCTURE, com.mojang.serialization.Lifecycle.stable());
        structures.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(List.of(BuiltInRegistries.BLOCK, biomes, structures));
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
        var configured = lookup.lookupOrThrow(Registries.CONFIGURED_FEATURE);

        var discard = new DataOutputStream(OutputStream.nullOutputStream());
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (int argument = 5; argument < args.length; argument++) {
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
                        proto.setPersistedStatus(ChunkStatus.CARVERS);
                        chunks.put(key(at.x(), at.z()), proto);
                    }
                }
                var target = chunks.get(key(cx, cz));
                var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, target, seed);
                region.chunks.putAll(chunks);
                region.biomes = new BiomeManager((qx, qy, qz) -> region.getChunk(qx >> 2, qz >> 2).getNoiseBiome(qx, qy, qz),
                    BiomeManager.obfuscateSeed(seed));

                var pos = new BlockPos.MutableBlockPos();
                plantGround(chunks, cx, cz);
                int[] before = new int[48 * 48 * 384];
                int index = 0;
                for (int y = -64; y < 320; y++)
                    for (int z = (cz - 1) * 16; z < (cz + 2) * 16; z++)
                        for (int x = (cx - 1) * 16; x < (cx + 2) * 16; x++)
                            before[index++] = Block.getId(chunks.get(key(x >> 4, z >> 4)).getBlockState(pos.set(x, y, z)));

                out.println("chunk " + cx + " " + cz);
                for (int tree = 0; tree < trees; tree++) {
                    String name = features[tree % features.length];
                    var feature = configured.getOrThrow(ResourceKey.create(Registries.CONFIGURED_FEATURE, Identifier.parse(name)));
                    int x = cx * 16 + (tree * 7 + 3) % 16, z = cz * 16 + (tree * 5 + 11) % 16;
                    int y = region.getHeight(Heightmap.Types.WORLD_SURFACE_WG, x, z);
                    var worldgenRandom = new WorldgenRandom(new net.minecraft.world.level.levelgen.XoroshiroRandomSource(0L));
                    long decoration = worldgenRandom.setDecorationSeed(seed, cx * 16, cz * 16);
                    worldgenRandom.setFeatureSeed(decoration, tree, 9);
                    boolean placed = feature.value().place(region, generator, worldgenRandom, new BlockPos(x, y, z));
                    out.println("tree " + tree + " " + name + " " + x + " " + y + " " + z + " " + placed + " "
                        + worldgenRandom.nextInt());
                }
                index = 0;
                for (int y = -64; y < 320; y++) {
                    for (int z = (cz - 1) * 16; z < (cz + 2) * 16; z++) {
                        for (int x = (cx - 1) * 16; x < (cx + 2) * 16; x++, index++) {
                            int id = Block.getId(chunks.get(key(x >> 4, z >> 4)).getBlockState(pos.set(x, y, z)));
                            if (before[index] != id) out.println(x + " " + y + " " + z + " " + id);
                        }
                    }
                }
                for (var chunk : chunks.values()) {
                    for (var at : chunk.getBlockEntitiesPos()) {
                        var bytes = new ByteArrayOutputStream();
                        net.minecraft.nbt.NbtIo.writeAnyTag(chunk.getBlockEntityNbtForSaving(at, access), new DataOutputStream(bytes));
                        out.println("entity " + HexFormat.of().formatHex(bytes.toByteArray()));
                    }
                }
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }
}
