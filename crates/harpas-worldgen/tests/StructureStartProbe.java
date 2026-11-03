// Test driver only: runs the owner-provided server's own createStructures and createReferences and writes their
// starts and references.
//
// Arguments: <output> <seed> <radius in chunks> <file of reference chunks, "x z" per line> <structure set>...
// Writes the base heights of a grid of columns, then the starts of each chunk around the origin, then the
// references of the listed chunks.
// Only the given structure sets are possible, so that structures Harpas cannot make yet are left out.
import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.Locale;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.StructureManager;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.ChunkGeneratorStructureState;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldOptions;
import net.minecraft.world.level.levelgen.presets.WorldPresets;
import net.minecraft.world.level.levelgen.structure.StructureStart;

public final class StructureStartProbe {
    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        long seed = Long.parseLong(args[1]);
        int radius = Integer.parseInt(args[2]);
        var registries = StructurePlacementProbe.loadRegistries();
        var structures = registries.lookupOrThrow(Registries.STRUCTURE);
        var sets = registries.lookupOrThrow(Registries.STRUCTURE_SET);
        var generator = WorldPresets.getNormalOverworld(registries).generator();
        var random = RandomState.create(registries, NoiseGeneratorSettings.OVERWORLD, seed);
        var chosen = new ArrayList<net.minecraft.core.Holder<net.minecraft.world.level.levelgen.structure.StructureSet>>();
        for (int index = 4; index < args.length; index++) {
            chosen.add(sets.getOrThrow(ResourceKey.create(Registries.STRUCTURE_SET, Identifier.parse(args[index]))));
        }
        // A normal world's state, but only with the given sets. Their placements are not concentric rings, whose
        // seed is the only difference from a normal world's.
        var state = ChunkGeneratorStructureState.createForFlat(random, seed, generator.getBiomeSource(), chosen.stream());
        var height = LevelHeightAccessor.create(-64, 384);
        var factory = PalettedContainerFactory.create(registries);
        var manager = new StructureManager(null, new WorldOptions(seed, true, false), null);
        var chunks = new HashMap<Long, ChunkAccess>();

        try (var out = new PrintWriter(args[0])) {
            for (int x : heightCoordinates()) {
                for (int z : heightCoordinates()) {
                    out.println("height " + x + " " + z + " "
                        + generator.getBaseHeight(x, z, Heightmap.Types.WORLD_SURFACE_WG, height, random) + " "
                        + generator.getBaseHeight(x, z, Heightmap.Types.OCEAN_FLOOR_WG, height, random));
                }
            }
            for (int x = -radius; x < radius; x++) {
                for (int z = -radius; z < radius; z++) {
                    var proto = new ProtoChunk(new ChunkPos(x, z), UpgradeData.EMPTY, height, factory, null);
                    generator.createStructures(registries, state, manager, proto, null, Level.OVERWORLD);
                    chunks.put(key(x, z), proto);
                    var starts = new ArrayList<>(proto.getAllStarts().entrySet());
                    starts.sort(Comparator.comparing(entry -> structures.getKey(entry.getKey()).toString()));
                    for (var entry : starts) write(out, structures.getKey(entry.getKey()).toString(), entry.getValue());
                }
            }
            for (var line : Files.readAllLines(Path.of(args[3]))) {
                var coordinates = line.split(" ");
                int x = Integer.parseInt(coordinates[0]), z = Integer.parseInt(coordinates[1]);
                var target = (ProtoChunk) chunks.get(key(x, z));
                var region = WorldgenReferenceProbe.TestRegion.create(registries, height, factory, target, seed);
                region.chunks = chunks;
                generator.createReferences(region, manager, target);
                var references = new ArrayList<String>();
                for (var entry : target.getAllReferences().entrySet()) {
                    for (long source : entry.getValue()) {
                        references.add("ref " + x + " " + z + " " + structures.getKey(entry.getKey()) + " "
                            + ChunkPos.getX(source) + " " + ChunkPos.getZ(source));
                    }
                }
                references.sort(null);
                references.forEach(out::println);
            }
        }
    }

    /** Block coordinates whose base heights are compared, as in `structure_start_parity.rs`. */
    static int[] heightCoordinates() {
        var coordinates = new ArrayList<Integer>();
        for (int value = -3000; value <= 3000; value += 151) coordinates.add(value);
        coordinates.add(29_999_000);
        coordinates.add(-29_999_001);
        return coordinates.stream().mapToInt(Integer::intValue).toArray();
    }

    static long key(int x, int z) {
        return ((long) x << 32) ^ (z & 0xffffffffL);
    }

    static void write(PrintWriter out, String structure, StructureStart start) {
        var chunk = start.getChunkPos();
        var box = start.getBoundingBox();
        out.println("start " + structure + " " + chunk.x() + " " + chunk.z() + " " + box.minX() + " " + box.minY() + " "
            + box.minZ() + " " + box.maxX() + " " + box.maxY() + " " + box.maxZ());
        for (var piece : start.getPieces()) {
            var bb = piece.getBoundingBox();
            var orientation = piece.getOrientation();
            out.println("piece " + BuiltInRegistries.STRUCTURE_PIECE.getKey(piece.getType()) + " " + bb.minX() + " "
                + bb.minY() + " " + bb.minZ() + " " + bb.maxX() + " " + bb.maxY() + " " + bb.maxZ() + " "
                + (orientation == null ? -1 : orientation.get2DDataValue()) + " " + piece.getGenDepth());
        }
    }
}
