// Test driver only: runs vanilla's room and complete monument passes on supplied terrain.
// Reflection exposes child pieces; subclasses suppress only elder spawning (M4).
import java.io.*;
import java.lang.reflect.*;
import java.nio.file.*;
import java.util.*;
import java.util.zip.Deflater;
import java.util.zip.DeflaterOutputStream;
import net.minecraft.core.*;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.structure.*;
import net.minecraft.world.level.levelgen.structure.pieces.PiecesContainer;
import net.minecraft.world.level.levelgen.structure.structures.OceanMonumentPieces.*;
import net.minecraft.world.level.levelgen.structure.structures.OceanMonumentStructure;

public final class MonumentInteriorProbe {
    static class Wing extends OceanMonumentWingRoom {
        Wing(Direction direction, BoundingBox bounds, int design) { super(direction, bounds, design); }
        @Override protected void spawnElder(WorldGenLevel world, BoundingBox clip, int x, int y, int z) { }
    }
    static class Penthouse extends OceanMonumentPenthouse {
        Penthouse(Direction direction, BoundingBox bounds) { super(direction, bounds); }
        @Override protected void spawnElder(WorldGenLevel world, BoundingBox clip, int x, int y, int z) { }
    }
    @SuppressWarnings("unchecked")
    static List<StructurePiece> children(MonumentBuilding building) throws Exception {
        var f = MonumentBuilding.class.getDeclaredField("childPieces");
        f.setAccessible(true);
        return (List<StructurePiece>) f.get(building);
    }
    static MonumentBuilding building(long seed, int ox, int oz, int dir, boolean reload) throws Exception {
        int cx = ox >> 4, cz = oz >> 4;
        MonumentBuilding building;
        if (reload) {
            var tag = new CompoundTag();
            tag.putIntArray("BB", new int[]{ox, 39, oz, ox + 57, 61, oz + 57});
            tag.putInt("O", dir);
            tag.putInt("GD", 0);
            building = (MonumentBuilding) OceanMonumentStructure.regeneratePiecesAfterLoad(
                new ChunkPos(cx, cz), seed, new PiecesContainer(List.of(new MonumentBuilding(tag)))).pieces().getFirst();
        } else {
            var random = new WorldgenRandom(new LegacyRandomSource(0));
            random.setLargeFeatureSeed(seed, cx, cz);
            Direction.Plane.HORIZONTAL.getRandomDirection(random);
            building = new MonumentBuilding(random, ox, oz, Direction.from2DDataValue(dir));
        }
        var list = children(building);
        for (int i = 0; i < list.size(); i++) {
            var child = list.get(i);
            if (child instanceof OceanMonumentWingRoom) {
                var f = OceanMonumentWingRoom.class.getDeclaredField("mainDesign");
                f.setAccessible(true);
                list.set(i, new Wing(child.getOrientation(), child.getBoundingBox(), f.getInt(child)));
            } else if (child instanceof OceanMonumentPenthouse) {
                list.set(i, new Penthouse(child.getOrientation(), child.getBoundingBox()));
            }
        }
        return building;
    }
    public static void main(String[] args) throws Exception {
        try { run(args); } catch (Throwable error) { error.printStackTrace(); System.exit(1); }
    }
    static void run(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var lines = Files.readAllLines(Path.of(args[0]));
        int[] palette = Arrays.stream(lines.getFirst().split(" ")).mapToInt(Integer::parseInt).toArray();
        // Mixed terrain has few adjacent equal states. Compress the RLE stream to keep the full
        // comparison's temporary output small without weakening the block-by-block checks.
        try (var out = new DataOutputStream(new BufferedOutputStream(new DeflaterOutputStream(
                Files.newOutputStream(Path.of(args[1])), new Deflater(Deflater.BEST_SPEED), 65536)))) {
            for (String line : lines.subList(1, lines.size())) {
                String[] v = line.split(" ");
                long seed = Long.parseLong(v[0]), decoration = Long.parseLong(v[12]);
                int ox = Integer.parseInt(v[1]), oy = Integer.parseInt(v[2]), oz = Integer.parseInt(v[3]);
                int dir = Integer.parseInt(v[4]), mode = Integer.parseInt(v[5]), sea = Integer.parseInt(v[6]);
                int minY = Integer.parseInt(v[7]), height = Integer.parseInt(v[8]);
                int cx = Integer.parseInt(v[9]), cz = Integer.parseInt(v[10]), selection = Integer.parseInt(v[11]);
                int[] blocks = new int[height * 256];
                for (int y = minY; y < minY + height; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++)
                    blocks[((y - minY) * 16 + z) * 16 + x] = MonumentExteriorProbe.terrain(cx * 16 + x, y, cz * 16 + z, oy, mode, sea, palette);
                var world = (WorldGenLevel) Proxy.newProxyInstance(MonumentInteriorProbe.class.getClassLoader(),
                    new Class<?>[]{WorldGenLevel.class}, (proxy, method, a) -> switch (method.getName()) {
                        case "getMinY" -> minY;
                        case "getMaxY" -> minY + height - 1;
                        case "getSeaLevel" -> sea;
                        case "scheduleTick" -> null;
                        case "getBlockState", "getFluidState" -> {
                            var p = (BlockPos) a[0];
                            if ((p.getX() >> 4) != cx || (p.getZ() >> 4) != cz) throw new AssertionError("read outside chunk");
                            var state = p.getY() < minY || p.getY() >= minY + height ? Blocks.AIR.defaultBlockState()
                                : Block.stateById(blocks[((p.getY() - minY) * 16 + (p.getZ() & 15)) * 16 + (p.getX() & 15)]);
                            yield method.getName().equals("getFluidState") ? state.getFluidState() : state;
                        }
                        case "setBlock" -> {
                            var p = (BlockPos) a[0];
                            if ((p.getX() >> 4) != cx || (p.getZ() >> 4) != cz) throw new AssertionError("write outside chunk");
                            if (p.getY() < minY || p.getY() >= minY + height) yield false;
                            blocks[((p.getY() - minY) * 16 + (p.getZ() & 15)) * 16 + (p.getX() & 15)] = Block.getId((BlockState) a[1]);
                            yield true;
                        }
                        default -> throw new UnsupportedOperationException(method.toString());
                    });
                for (int pass = 0; pass < 2; pass++) {
                    var building = building(seed, ox, oz, dir, pass == 1);
                    building.getBoundingBox().move(0, oy - 39, 0);
                    for (var child : children(building)) child.getBoundingBox().move(0, oy - 39, 0);
                    var random = RandomSource.create(decoration + pass);
                    var clip = new BoundingBox(cx * 16, minY, cz * 16, cx * 16 + 15, minY + height - 1, cz * 16 + 15);
                    if (selection == -1) {
                        building.postProcess(world, null, null, random, clip, new ChunkPos(cx, cz), BlockPos.ZERO);
                    } else {
                        var list = children(building);
                        for (int i = 0; i < list.size(); i++) {
                            var child = list.get(i);
                            if ((selection == -2 || selection == i) && child.getBoundingBox().intersects(clip))
                                child.postProcess(world, null, null, random, clip, new ChunkPos(cx, cz), BlockPos.ZERO);
                        }
                    }
                    for (int begin = 0; begin < blocks.length;) {
                        int end = begin + 1;
                        while (end < blocks.length && blocks[begin] == blocks[end]) end++;
                        out.writeInt(end - begin); out.writeInt(blocks[begin]); begin = end;
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
