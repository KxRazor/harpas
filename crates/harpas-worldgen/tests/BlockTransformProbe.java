// Parity probe: writes, for every block state in registry order, its id followed by the ids of the state
// turned by each rotation (none, clockwise 90, 180, counterclockwise 90) and then reflected front to back and
// left to right, as BlockState.rotate and BlockState.mirror return them.
import java.io.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Mirror;
import net.minecraft.world.level.block.Rotation;

public final class BlockTransformProbe {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (var out = new ProbeOutput(args[0])) {
            for (var block : BuiltInRegistries.BLOCK) {
                for (var state : block.getStateDefinition().getPossibleStates()) {
                    var line = new StringBuilder().append(Block.getId(state));
                    for (var rotation : Rotation.values()) line.append(' ').append(Block.getId(state.rotate(rotation)));
                    line.append(' ').append(Block.getId(state.mirror(Mirror.FRONT_BACK)));
                    line.append(' ').append(Block.getId(state.mirror(Mirror.LEFT_RIGHT)));
                    out.println(line);
                }
            }
        }
    }
}
