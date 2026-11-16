// Parity probe: writes, for every block, the property flags its BlockBehaviour.Properties sets, and for every
// block state in registry order, the flags block predicates and survival rules read: isSolid, canBeReplaced,
// liquid, blocksMotion and isFaceSturdy for each direction, with full support and then center support, then
// isSolidRender, hasBlockEntity and the full faces of the occlusion shape when the state can occlude. Then, for
// light, each state's faces of LightEngine.getOcclusionShape as 32 rows of 32 bits, and LightEngine.shapeOccludes
// for every pair of distinct faces that meet: a state with one on a face, beside a state with the other on the
// opposite face.
import java.io.*;
import java.util.Locale;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.SupportType;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.lighting.LightEngine;

public final class BlockBehaviorProbe {
    static boolean flag(BlockBehaviour.Properties properties, String name) throws Exception {
        var field = BlockBehaviour.Properties.class.getDeclaredField(name);
        field.setAccessible(true);
        return field.getBoolean(properties);
    }

    /** A face as 32 rows of 32 bits: rows along Y for the X faces and along X otherwise, bits along Z for the X
     *  and Y faces and along Y for the Z faces. */
    static String face(BlockState state, Direction direction) {
        var rows = new int[32];
        var axis = direction.getAxis();
        LightEngine.getOcclusionShape(state, direction).forAllBoxes((x1, y1, z1, x2, y2, z2) -> {
            double[] min = {x1, y1, z1}, max = {x2, y2, z2};
            int u = axis == Direction.Axis.X ? 1 : 0;
            int v = axis == Direction.Axis.Z ? 1 : 2;
            for (int row = clip(min[u]); row < clip(max[u]); row++)
                for (int bit = clip(min[v]); bit < clip(max[v]); bit++) rows[row] |= 1 << bit;
        });
        var text = new StringBuilder();
        for (int row : rows) text.append(' ').append(Integer.toUnsignedString(row, 16));
        return text.toString();
    }

    static int clip(double value) {
        return (int) Math.round(Math.max(0, Math.min(1, value)) * 32);
    }

    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var propertiesField = BlockBehaviour.class.getDeclaredField("properties");
        propertiesField.setAccessible(true);
        try (var out = new PrintWriter(new BufferedWriter(new FileWriter(args[0])))) {
            for (var block : BuiltInRegistries.BLOCK) {
                var properties = (BlockBehaviour.Properties) propertiesField.get(block);
                var line = new StringBuilder("block ").append(BuiltInRegistries.BLOCK.getKey(block));
                for (var name : new String[] {"forceSolidOn", "forceSolidOff", "replaceable", "liquid", "dynamicShape", "hasCollision"}) {
                    if (flag(properties, name)) line.append(' ').append(name);
                }
                out.println(line);
                for (var state : block.getStateDefinition().getPossibleStates()) {
                    int sturdy = 0, occlusion = 0;
                    for (var direction : Direction.values()) {
                        if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, direction)) sturdy |= 1 << direction.ordinal();
                        if (state.isFaceSturdy(EmptyBlockGetter.INSTANCE, BlockPos.ZERO, direction, SupportType.CENTER)) sturdy |= 1 << (8 + direction.ordinal());
                        if (state.canOcclude() && Block.isFaceFull(state.getOcclusionShape(), direction)) occlusion |= 1 << direction.ordinal();
                    }
                    out.println(Block.getId(state) + " " + (state.isSolid() ? 1 : 0) + " " + (state.canBeReplaced() ? 1 : 0)
                        + " " + (state.liquid() ? 1 : 0) + " " + (state.blocksMotion() ? 1 : 0) + " " + sturdy
                        + " " + (state.isSolidRender() ? 1 : 0) + " " + (state.hasBlockEntity() ? 1 : 0) + " " + occlusion);
                }
            }
            // One state for each distinct face on each side, the first in registry order.
            var faces = new java.util.ArrayList<java.util.LinkedHashMap<String, BlockState>>();
            for (var direction : Direction.values()) faces.add(new java.util.LinkedHashMap<>());
            for (var block : BuiltInRegistries.BLOCK) {
                for (var state : block.getStateDefinition().getPossibleStates()) {
                    var line = new StringBuilder("light ").append(Block.getId(state));
                    for (var direction : Direction.values()) {
                        var face = face(state, direction);
                        faces.get(direction.ordinal()).putIfAbsent(face, state);
                        line.append(' ').append(face.replace(" 0", "").isEmpty() ? "-" : face.substring(1).replace(' ', ','));
                    }
                    out.println(line);
                }
            }
            for (var direction : Direction.values()) {
                for (var from : faces.get(direction.ordinal()).values()) {
                    for (var to : faces.get(direction.getOpposite().ordinal()).values()) {
                        out.println("pair " + Block.getId(from) + " " + Block.getId(to) + " " + direction.ordinal()
                            + " " + (net.minecraft.world.phys.shapes.Shapes.faceShapeOccludes(
                                LightEngine.getOcclusionShape(from, direction),
                                LightEngine.getOcclusionShape(to, direction.getOpposite())) ? 1 : 0));
                    }
                }
            }
        }
    }
}
