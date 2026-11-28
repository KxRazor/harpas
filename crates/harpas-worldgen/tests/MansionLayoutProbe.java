// Test driver only: observes the layout the owner-provided server makes for a woodland mansion.
// Reflection reads the in-memory grids, which vanilla does not serialize.
//
// Arguments: <output> <seed>...
// Writes, for each seed, the cells of the base grid, the third floor's grid and each floor's rooms, column by
// column, then the next value of the random the layout drew from.
import java.lang.reflect.Field;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;

public final class MansionLayoutProbe {
    static Object field(Object object, String name) throws Exception {
        Field field = object.getClass().getDeclaredField(name);
        field.setAccessible(true);
        return field.get(object);
    }

    static String cells(Object grid) throws Exception {
        var line = new StringBuilder();
        for (int[] column : (int[][]) field(grid, "grid")) for (int cell : column) line.append(' ').append(cell);
        return line.toString();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var type = Class.forName(
            "net.minecraft.world.level.levelgen.structure.structures.WoodlandMansionPieces$MansionGrid");
        var constructor = type.getDeclaredConstructor(RandomSource.class);
        constructor.setAccessible(true);
        try (var out = new ProbeOutput(args[0])) {
            for (int index = 1; index < args.length; index++) {
                var random = RandomSource.create(Long.parseLong(args[index]));
                var layout = constructor.newInstance(random);
                out.println("seed " + args[index]);
                out.println("base" + cells(field(layout, "baseGrid")));
                out.println("third" + cells(field(layout, "thirdFloorGrid")));
                for (Object rooms : (Object[]) field(layout, "floorRooms")) out.println("rooms" + cells(rooms));
                out.println("tail " + random.nextLong());
            }
        }
    }
}
