// Test driver only: observes the owner-provided server's monument constructor and reload API.
// Reflection reads the in-memory room graph and child pieces, which vanilla does not serialize.
import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.TreeMap;
import net.minecraft.SharedConstants;
import net.minecraft.core.Direction;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.levelgen.structure.StructurePiece;
import net.minecraft.world.level.levelgen.structure.pieces.PiecesContainer;
import net.minecraft.world.level.levelgen.structure.structures.OceanMonumentPieces.MonumentBuilding;
import net.minecraft.world.level.levelgen.structure.structures.OceanMonumentStructure;

public final class MonumentLayoutProbe {
    static Object field(Object object, String name) throws Exception {
        for (Class<?> type = object.getClass(); type != null; type = type.getSuperclass()) {
            try {
                Field field = type.getDeclaredField(name);
                field.setAccessible(true);
                return field.get(object);
            } catch (NoSuchFieldException ignored) { }
        }
        throw new NoSuchFieldException(name);
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (var out = new PrintWriter(args[1])) {
            for (String line : Files.readAllLines(Path.of(args[0]))) {
                String[] words = line.split(" ");
                int mode = Integer.parseInt(words[0]);
                long seed = Long.parseLong(words[1]);
                int x = Integer.parseInt(words[2]), z = Integer.parseInt(words[3]);
                int direction = Integer.parseInt(words[4]);
                RandomSource random = RandomSource.create(seed);
                MonumentBuilding building;
                if (mode == 0) {
                    building = new MonumentBuilding(random, x, z, Direction.from2DDataValue(direction));
                } else {
                    var tag = new CompoundTag();
                    tag.putIntArray("BB", new int[] {x, 39, z, x + 57, 61, z + 57});
                    tag.putInt("O", direction);
                    tag.putInt("GD", 0);
                    var loaded = new MonumentBuilding(tag);
                    var chunk = new ChunkPos(Integer.parseInt(words[5]), Integer.parseInt(words[6]));
                    building = (MonumentBuilding) OceanMonumentStructure.regeneratePiecesAfterLoad(
                        chunk, seed, new PiecesContainer(List.of(loaded))).pieces().getFirst();
                }
                out.println("case " + line);
                write(out, building);
                if (mode == 0) out.println("tail " + random.nextLong());
            }
        }
    }

    static void write(PrintWriter out, MonumentBuilding building) throws Exception {
        out.println("layout " + building.getOrientation().get2DDataValue() + " "
            + field(field(building, "sourceRoom"), "index") + " "
            + field(field(building, "coreRoom"), "index"));
        var pending = new ArrayList<Object>();
        var rooms = new TreeMap<Integer, Object>();
        pending.add(field(building, "sourceRoom"));
        for (int i = 0; i < pending.size(); i++) {
            Object room = pending.get(i);
            if (rooms.putIfAbsent((int) field(room, "index"), room) != null) continue;
            for (Object neighbor : (Object[]) field(room, "connections")) {
                if (neighbor != null) pending.add(neighbor);
            }
        }
        for (var entry : rooms.entrySet()) {
            out.print("room " + entry.getKey());
            Object[] neighbors = (Object[]) field(entry.getValue(), "connections");
            boolean[] openings = (boolean[]) field(entry.getValue(), "hasOpening");
            for (int d = 0; d < 6; d++) {
                out.print(" " + (neighbors[d] == null ? -1 : field(neighbors[d], "index"))
                    + ":" + (openings[d] ? 1 : 0));
            }
            out.println();
        }
        for (Object child : (List<?>) field(building, "childPieces")) {
            StructurePiece piece = (StructurePiece) child;
            String kind = child.getClass().getSimpleName().replace("OceanMonument", "");
            Object room = field(child, "roomDefinition");
            int design = kind.equals("SimpleRoom") || kind.equals("WingRoom")
                ? (int) field(child, "mainDesign") : -1;
            var b = piece.getBoundingBox();
            out.println("piece " + kind + " " + (room == null ? -1 : field(room, "index")) + " "
                + b.minX() + " " + b.minY() + " " + b.minZ() + " "
                + b.maxX() + " " + b.maxY() + " " + b.maxZ() + " " + design);
        }
    }
}
