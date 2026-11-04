// Test driver only: samples the owner-provided server's Beardifier from synthetic structure pieces.
// Arguments: <input> <output>. The input describes starts, pieces, junctions and sample coordinates.
import java.io.*;
import java.nio.file.*;
import java.util.*;
import net.minecraft.core.*;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.Rotation;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.structure.*;
import net.minecraft.world.level.levelgen.structure.pieces.PiecesContainer;
import net.minecraft.world.level.levelgen.structure.pools.*;
import net.minecraft.world.level.levelgen.structure.templatesystem.LiquidSettings;

public final class BeardifierProbe {
    static final class PlainPiece extends StructurePiece {
        PlainPiece(BoundingBox box) { super(null, 0, box); }
        @Override protected void addAdditionalSaveData(
                net.minecraft.world.level.levelgen.structure.pieces.StructurePieceSerializationContext context,
                net.minecraft.nbt.CompoundTag tag) { }
        @Override public void postProcess(WorldGenLevel level, StructureManager manager,
                net.minecraft.world.level.chunk.ChunkGenerator generator, net.minecraft.util.RandomSource random,
                BoundingBox box, ChunkPos chunk, BlockPos position) { }
    }

    static StructureStart start(TerrainAdjustment adjustment, List<StructurePiece> pieces) {
        var structure = new net.minecraft.world.level.levelgen.structure.structures.DesertPyramidStructure(
            new Structure.StructureSettings(HolderSet.direct(List.of()), Map.of(),
                GenerationStep.Decoration.SURFACE_STRUCTURES, adjustment));
        return new StructureStart(structure, new ChunkPos(0, 0), 0, new PiecesContainer(pieces));
    }

    static StructureManager manager(List<StructureStart> starts) {
        return new StructureManager(null, new WorldOptions(0, true, false), null) {
            @Override public List<StructureStart> startsForStructure(ChunkPos chunk,
                    java.util.function.Predicate<Structure> filter) {
                return starts.stream().filter(start -> filter.test(start.getStructure())).toList();
            }
        };
    }

    public static void main(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var starts = new ArrayList<StructureStart>();
        var pieces = new ArrayList<StructurePiece>();
        TerrainAdjustment adjustment = null;
        Beardifier beard = Beardifier.EMPTY;
        try (var input = Files.newBufferedReader(Path.of(args[0]));
             var out = new DataOutputStream(new BufferedOutputStream(Files.newOutputStream(Path.of(args[1]))))) {
            String line;
            while ((line = input.readLine()) != null) {
                var words = line.split(" ");
                if (words[0].equals("sample")) {
                    out.writeDouble(beard.compute(new DensityFunction.SinglePointContext(
                        Integer.parseInt(words[1]), Integer.parseInt(words[2]), Integer.parseInt(words[3]))));
                    continue;
                }
                int[] values = Arrays.stream(words).skip(1).filter(word -> word.matches("-?\\d+"))
                    .mapToInt(Integer::parseInt).toArray();
                switch (words[0]) {
                    case "reset" -> { starts.clear(); pieces = new ArrayList<>(); adjustment = null; }
                    case "start" -> {
                        if (adjustment != null) starts.add(start(adjustment, pieces));
                        adjustment = TerrainAdjustment.valueOf(words[1].toUpperCase(Locale.ROOT));
                        pieces = new ArrayList<>();
                    }
                    case "piece" -> {
                        var box = new BoundingBox(values[0], values[1], values[2], values[3], values[4], values[5]);
                        if (words[1].equals("plain")) pieces.add(new PlainPiece(box));
                        else pieces.add(new PoolElementStructurePiece(null,
                            StructurePoolElement.single("test:piece").apply(words[1].equals("rigid")
                                ? StructureTemplatePool.Projection.RIGID : StructureTemplatePool.Projection.TERRAIN_MATCHING),
                            new BlockPos(values[0], values[1], values[2]), values[6], Rotation.NONE, box,
                            LiquidSettings.APPLY_WATERLOGGING));
                    }
                    case "junction" -> ((PoolElementStructurePiece) pieces.getLast()).addJunction(
                        new JigsawJunction(values[0], values[1], values[2], values[3], values[4] == 0
                            ? StructureTemplatePool.Projection.TERRAIN_MATCHING : StructureTemplatePool.Projection.RIGID));
                    case "chunk" -> {
                        if (adjustment != null) { starts.add(start(adjustment, pieces)); adjustment = null; }
                        beard = Beardifier.forStructuresInChunk(manager(starts), new ChunkPos(values[0], values[1]));
                    }
                    default -> throw new IllegalArgumentException(line);
                }
            }
        }
    }
}
