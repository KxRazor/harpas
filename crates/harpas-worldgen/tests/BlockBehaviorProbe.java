// Parity probe: writes, for every block, the property flags its BlockBehaviour.Properties sets, and for every
// block state in registry order, the flags block predicates and survival rules read: isSolid, canBeReplaced,
// liquid, blocksMotion and isFaceSturdy for each direction, with full support and then center support, then
// isSolidRender, hasBlockEntity and the full faces of the occlusion shape when the state can occlude.
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

public final class BlockBehaviorProbe {
    static boolean flag(BlockBehaviour.Properties properties, String name) throws Exception {
        var field = BlockBehaviour.Properties.class.getDeclaredField(name);
        field.setAccessible(true);
        return field.getBoolean(properties);
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
        }
    }
}
