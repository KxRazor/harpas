// Test driver only: generates terrain through carvers with the references of real vanilla structures.
// Arguments: <output> <seed> <x,z>... . Structure blocks are not placed.
import java.io.*;
import java.nio.file.Path;
import java.util.*;
import net.minecraft.core.registries.*;
import net.minecraft.world.level.*;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.presets.WorldPresets;

public final class StructureTerrainProbe {
    public static void main(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        long seed = Long.parseLong(args[1]);
        var server = StructurePlacementProbe.loadServer();
        var registries = server.registries();
        var storage = net.minecraft.world.level.storage.LevelStorageSource
            .createDefault(Path.of("probe-worlds")).createAccess("probe");
        var templates = new net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager(
            server.resources(), storage, net.minecraft.util.datafix.DataFixers.getDataFixer(), BuiltInRegistries.BLOCK);
        var generator = (NoiseBasedChunkGenerator) WorldPresets.getNormalOverworld(registries).generator();
        var random = RandomState.create(registries, NoiseGeneratorSettings.OVERWORLD, seed);
        var state = generator.createState(registries.lookupOrThrow(Registries.STRUCTURE_SET), random, seed);
        state.ensureStructuresGenerated();
        var height = LevelHeightAccessor.create(-64, 384);
        var factory = PalettedContainerFactory.create(registries);
        var biomes = registries.lookupOrThrow(Registries.BIOME);
        var chunks = new HashMap<Long, ChunkAccess>();
        var options = new WorldOptions(seed, true, false);
        var saved = new net.minecraft.nbt.CompoundTag();
        var serialization = new net.minecraft.world.level.levelgen.structure.pieces.StructurePieceSerializationContext(server.resources(), registries, templates);
        try (var out = new DataOutputStream(new BufferedOutputStream(new FileOutputStream(args[0])))) {
            out.writeInt(biomes.size());
            for (var biome : biomes) out.writeUTF(biomes.getKey(biome).toString());
            for (int argument = 2; argument < args.length; argument++) {
                var coordinates = args[argument].split(",");
                int cx = Integer.parseInt(coordinates[0]), cz = Integer.parseInt(coordinates[1]);
                // Cache starts across neighboring target chunks, as a real world's STRUCTURE_STARTS stage does.
                for (int x = cx - 8; x <= cx + 8; x++) {
                    for (int z = cz - 8; z <= cz + 8; z++) {
                        long key = key(x, z);
                        if (chunks.containsKey(key)) continue;
                        var proto = new ProtoChunk(new ChunkPos(x, z), UpgradeData.EMPTY, height, factory, null);
                        var manager = new StructureManager(null, options, null);
                        ChunkParityProbe.onNewThread(() ->
                            generator.createStructures(registries, state, manager, proto, templates, Level.OVERWORLD));
                        chunks.put(key, proto);
                    }
                }
                // Terrain generation caches NoiseChunk in its target, so each target is filled once.
                var proto = (ProtoChunk) chunks.get(key(cx, cz));
                var region = WorldgenReferenceProbe.TestRegion.create(registries, height, factory, proto, seed);
                region.chunks = chunks;
                var manager = new StructureManager(region, options, null);
                generator.createReferences(region, manager, proto);
                ChunkParityProbe.writeChunk(out, seed, biomes, factory, generator, random, region, manager, proto);
                var starts = new net.minecraft.nbt.CompoundTag();
                for (var entry : proto.getAllStarts().entrySet()) {
                    starts.put(registries.lookupOrThrow(Registries.STRUCTURE).getKey(entry.getKey()).toString(), entry.getValue().createTag(serialization, proto.getPos()));
                }
                var references = new net.minecraft.nbt.CompoundTag();
                for (var entry : proto.getAllReferences().entrySet()) {
                    long[] sources = entry.getValue().toLongArray(); java.util.Arrays.sort(sources);
                    references.putLongArray(registries.lookupOrThrow(Registries.STRUCTURE).getKey(entry.getKey()).toString(), sources);
                }
                var tag = new net.minecraft.nbt.CompoundTag();
                tag.put("starts", starts); tag.put("References", references);
                saved.put(cx + "," + cz, tag);
            }
        }
        net.minecraft.nbt.NbtIo.write(saved, Path.of(args[0] + ".nbt"));
        storage.close();
        server.resources().close();
        net.minecraft.util.Util.shutdownExecutors();
    }

    static long key(int x, int z) { return ((long) x << 32) ^ (z & 0xffffffffL); }
}
