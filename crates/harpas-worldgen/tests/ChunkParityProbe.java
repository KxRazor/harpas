// Test driver only: generates chunks with the owner-provided server's own pipeline and writes every block.
//
// Arguments: <output> <seed> <x,z>...
// The chunks have no structure references, as with generate-structures=false, and stop after carvers.
// Ties between climate parameter points resolve to the thread's last result, which on a real server depends on
// what its worker did before. Each stage that searches them runs on a new thread, so that it starts without one.
import java.io.BufferedOutputStream;
import java.io.DataOutputStream;
import java.io.FileOutputStream;
import java.util.EnumSet;
import java.util.HashMap;
import java.util.Locale;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.HolderSet;
import net.minecraft.core.MappedRegistry;
import net.minecraft.core.RegistrationInfo;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.StructureManager;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldOptions;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.carver.CarverConfiguration;

public final class ChunkParityProbe {
    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        long seed = Long.parseLong(args[1]);
        var lookup = VanillaRegistries.createLookup();
        var random = RandomState.create(lookup, NoiseGeneratorSettings.OVERWORLD, seed);
        var biomes = new MappedRegistry<Biome>(Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(
            holder -> biomes.register(holder.key(), holder.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        var access = new RegistryAccess.ImmutableRegistryAccess(java.util.List.of(BuiltInRegistries.BLOCK, biomes));
        var factory = PalettedContainerFactory.create(access);
        var biomeSource = MultiNoiseBiomeSource.createFromPreset(
            lookup.getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        var height = LevelHeightAccessor.create(-64, 384);
        var generator = new NoiseBasedChunkGenerator(biomeSource, lookup.getOrThrow(NoiseGeneratorSettings.OVERWORLD));

        // The built-in registries leave block tags unbound, so carvers get theirs from the jar's data.
        var blocks = new java.util.LinkedHashSet<net.minecraft.core.Holder<Block>>();
        WorldgenReferenceProbe.addBlockTag("minecraft:overworld_carver_replaceables", blocks, new java.util.HashSet<>());
        var replaceable = HolderSet.direct(new java.util.ArrayList<>(blocks));
        var replaceableField = CarverConfiguration.class.getDeclaredField("replaceable");
        replaceableField.setAccessible(true);
        lookup.lookupOrThrow(Registries.CONFIGURED_CARVER).listElements().forEach(holder -> {
            try { replaceableField.set(holder.value().config(), replaceable); }
            catch (IllegalAccessException error) { throw new IllegalStateException(error); }
        });


        try (var out = new DataOutputStream(new BufferedOutputStream(new FileOutputStream(args[0])))) {
            out.writeInt(biomes.size());
            for (var biome : biomes) out.writeUTF(biomes.getKey(biome).toString());
            for (int argument = 2; argument < args.length; argument++) {
                var coordinates = args[argument].split(",");
                int cx = Integer.parseInt(coordinates[0]), cz = Integer.parseInt(coordinates[1]);
                var chunkPos = new ChunkPos(cx, cz);
                var proto = new ProtoChunk(chunkPos, UpgradeData.EMPTY, height, factory, null);
                var region = WorldgenReferenceProbe.TestRegion.create(access, height, factory, proto, seed);
                var structures = new StructureManager(region, new WorldOptions(seed, false, false), null);
                writeChunk(out, seed, biomes, factory, generator, random, region, structures, proto);
            }
        }
        net.minecraft.util.Util.shutdownExecutors();
    }

    // Shared by the terrain-adaptation probe: the stages and binary comparison format stay identical.
    static void writeChunk(DataOutputStream out, long seed, net.minecraft.core.Registry<Biome> biomes,
            PalettedContainerFactory factory, NoiseBasedChunkGenerator generator, RandomState random,
            WorldgenReferenceProbe.TestRegion region, StructureManager structures, ProtoChunk proto) throws Exception {
        var chunkPos = proto.getPos();
        int cx = chunkPos.x(), cz = chunkPos.z();
        var height = LevelHeightAccessor.create(-64, 384);
        var pos = new BlockPos.MutableBlockPos();
        var createBiomes = NoiseBasedChunkGenerator.class.getDeclaredMethod("doCreateBiomes", Blender.class,
            RandomState.class, StructureManager.class, ChunkAccess.class);
        createBiomes.setAccessible(true);
        Stage fillBiomes = () -> createBiomes.invoke(generator, Blender.empty(), random, structures, proto);
        onNewThread(fillBiomes);
        proto.setPersistedStatus(ChunkStatus.BIOMES);
        // As in a world, biomes are read from the chunks, not computed again.
        var neighbors = new HashMap<ChunkPos, ProtoChunk>();
        var biomeManager = new BiomeManager((qx, qy, qz) -> {
            var at = new ChunkPos(qx >> 2, qz >> 2);
            var chunk = at.equals(chunkPos) ? proto : neighbors.computeIfAbsent(at, ignored -> {
                var neighbor = new ProtoChunk(at, UpgradeData.EMPTY, height, factory, null);
                Stage fill = () -> createBiomes.invoke(generator, Blender.empty(), random, structures, neighbor);
                onNewThread(fill);
                neighbor.setPersistedStatus(ChunkStatus.BIOMES);
                return neighbor;
            });
            return chunk.getNoiseBiome(qx, qy, qz);
        }, BiomeManager.obfuscateSeed(seed));
        region.biomes = biomeManager;
        generator.fillFromNoise(Blender.empty(), random, structures, proto).join();
        generator.buildSurface(region, structures, random, proto);
        Stage carve = () -> generator.applyCarvers(region, seed, random, biomeManager, structures, proto);
        onNewThread(carve);

        for (int y = -64; y < 320; y++) {
            for (int z = 0; z < 16; z++) {
                for (int x = 0; x < 16; x++) {
                    int id = Block.getId(proto.getBlockState(pos.set(cx * 16 + x, y, cz * 16 + z)));
                    if (id > 0xffff) throw new IllegalStateException("state " + id);
                    out.writeShort(id);
                }
            }
        }
        for (int qy = -16; qy < 80; qy++) {
            for (int qz = 0; qz < 4; qz++) {
                for (int qx = 0; qx < 4; qx++) {
                    out.writeShort(biomes.getId(proto.getNoiseBiome(cx * 4 + qx, qy, cz * 4 + qz).value()));
                }
            }
        }
        writeLongs(out, proto.getOrCreateCarvingMask().toArray());
        var post = proto.getPostProcessing();
        int count = 0;
        for (var list : post) if (list != null) count += list.size();
        out.writeInt(count);
        for (int section = 0; section < post.length; section++) {
            if (post[section] == null) continue;
            for (short value : post[section]) {
                out.writeByte(section);
                out.writeShort(value);
            }
        }
        Heightmap.primeHeightmaps(proto, EnumSet.allOf(Heightmap.Types.class));
        for (var kind : Heightmap.Types.values()) {
            writeLongs(out, proto.getOrCreateHeightmapUnprimed(kind).getRawData());
        }
    }

    interface Stage {
        void run() throws Exception;
    }

    static void onNewThread(Stage stage) {
        var failure = new java.util.concurrent.atomic.AtomicReference<Exception>();
        var thread = new Thread(() -> {
            try { stage.run(); } catch (Exception error) { failure.set(error); }
        });
        thread.start();
        try { thread.join(); } catch (InterruptedException error) { throw new IllegalStateException(error); }
        if (failure.get() != null) throw new IllegalStateException(failure.get());
    }

    static void writeLongs(DataOutputStream out, long[] values) throws java.io.IOException {
        out.writeInt(values.length);
        for (long value : values) out.writeLong(value);
    }
}
