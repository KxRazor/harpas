// Test driver only: observes vanilla's exterior on supplied terrain, excluding internal child rooms.
// Reflection removes the children to isolate this delivery; no game implementation or assets are copied.
import java.io.*;
import java.lang.reflect.Proxy;
import java.nio.file.*;
import java.util.*;
import net.minecraft.core.*;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.structure.BoundingBox;
import net.minecraft.world.level.levelgen.structure.structures.OceanMonumentPieces.MonumentBuilding;

public final class MonumentExteriorProbe {
    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }

    static int terrain(int x, int y, int z, int baseY, int mode, int sea, int[] palette) {
        if (mode == 1) return palette[1];
        if (mode == 2) return palette[0];
        if (mode == 3 && y >= baseY - 16 && y <= baseY + 30)
            return palette[Math.floorMod(x + 3 * z + 7 * y, palette.length)];
        int floor = baseY - 9;
        if (mode == 4) floor += Math.floorMod(x + z, 19) - 9;
        return palette[y < floor ? 1 : y < sea ? 2 : 0];
    }

    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var lines = Files.readAllLines(Path.of(args[0]));
        int[] palette = Arrays.stream(lines.getFirst().split(" ")).mapToInt(Integer::parseInt).toArray();
        var children = MonumentBuilding.class.getDeclaredField("childPieces");
        children.setAccessible(true);
        try (var out = new DataOutputStream(new BufferedOutputStream(Files.newOutputStream(Path.of(args[1]))))) {
            for (String line : lines.subList(1, lines.size())) {
                int[] v = Arrays.stream(line.split(" ")).mapToInt(Integer::parseInt).toArray();
                int ox = v[0], oy = v[1], oz = v[2], direction = v[3], mode = v[4], sea = v[5];
                int minY = v[6], height = v[7], cx = v[8], cz = v[9];
                var building = new MonumentBuilding(net.minecraft.util.RandomSource.create(42), ox, oz,
                    Direction.from2DDataValue(direction));
                building.getBoundingBox().move(0, oy - 39, 0);
                ((List<?>) children.get(building)).clear();
                int[] blocks = new int[height * 256];
                for (int y = minY; y < minY + height; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
                    blocks[((y - minY) * 16 + z) * 16 + x] = terrain(cx * 16 + x, y, cz * 16 + z, oy, mode, sea, palette);
                var world = (WorldGenLevel) Proxy.newProxyInstance(MonumentExteriorProbe.class.getClassLoader(),
                    new Class<?>[]{WorldGenLevel.class}, (proxy, method, a) -> switch (method.getName()) {
                        case "getMinY" -> minY;
                        case "getMaxY" -> minY + height - 1;
                        case "getSeaLevel" -> sea;
                        case "scheduleTick" -> null;
                        case "getFluidState" -> {
                            var p = (BlockPos) a[0];
                            yield (p.getY() < minY || p.getY() >= minY + height ? Blocks.AIR.defaultBlockState()
                                : Block.stateById(blocks[((p.getY() - minY) * 16 + (p.getZ() & 15)) * 16 + (p.getX() & 15)])).getFluidState();
                        }
                        case "getBlockState" -> {
                            var p = (BlockPos) a[0];
                            if ((p.getX() >> 4) != cx || (p.getZ() >> 4) != cz)
                                throw new AssertionError("read outside target chunk");
                            yield p.getY() < minY || p.getY() >= minY + height ? Blocks.AIR.defaultBlockState()
                                : Block.stateById(blocks[((p.getY() - minY) * 16 + (p.getZ() & 15)) * 16 + (p.getX() & 15)]);
                        }
                        case "setBlock" -> {
                            var p = (BlockPos) a[0];
                            if ((p.getX() >> 4) != cx || (p.getZ() >> 4) != cz)
                                throw new AssertionError("write outside target chunk");
                            if (p.getY() < minY || p.getY() >= minY + height) yield false;
                            blocks[((p.getY() - minY) * 16 + (p.getZ() & 15)) * 16 + (p.getX() & 15)] = Block.getId((BlockState) a[1]);
                            yield true;
                        }
                        default -> throw new UnsupportedOperationException(method.toString());
                    });
                for (int pass = 0; pass < 2; pass++) {
                    if (pass == 1) {
                        // Reload the same building record Harpas persists; its internal rooms are
                        // deliberately excluded from this exterior-only comparison.
                        var tag = new CompoundTag();
                        tag.putIntArray("BB", new int[]{ox, oy, oz, ox + 57, oy + 22, oz + 57});
                        tag.putInt("O", direction);
                        tag.putInt("GD", 0);
                        building = new MonumentBuilding(tag);
                        ((List<?>) children.get(building)).clear();
                    }
                    var random = net.minecraft.util.RandomSource.create(123);
                    building.postProcess(world, null, null, random,
                        new BoundingBox(cx * 16, minY, cz * 16, cx * 16 + 15, minY + height - 1, cz * 16 + 15),
                        new ChunkPos(cx, cz), BlockPos.ZERO);
                    int begin = 0;
                    while (begin < blocks.length) {
                        int end = begin + 1;
                        while (end < blocks.length && blocks[begin] == blocks[end]) end++;
                        out.writeInt(end - begin);
                        out.writeInt(blocks[begin]);
                        begin = end;
                    }
                    out.writeInt(0);
                    for (var kind : Heightmap.Types.values()) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                        int top = minY + height;
                        while (top > minY && !kind.isOpaque().test(Block.stateById(blocks[((top - 1 - minY) * 16 + z) * 16 + x]))) top--;
                        out.writeInt(top);
                    }
                    out.writeLong(random.nextLong());
                }
            }
        }
    }
}
