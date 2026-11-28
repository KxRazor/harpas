import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.List;
import it.unimi.dsi.fastutil.longs.LongOpenHashSet;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Registry;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.chunk.status.ChunkPyramid;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.structure.BoundingBox;
import net.minecraft.world.level.levelgen.structure.Structure;
import net.minecraft.world.level.levelgen.structure.StructureStart;

/** Coordinate and stage rules shared by the synthetic world-generation regions. */
final class ProbeTerrain {
    private static final net.minecraft.world.level.chunk.status.ChunkStep FEATURES =
        ChunkPyramid.GENERATION_PYRAMID.getStepTo(ChunkStatus.FEATURES);

    static boolean canRead(ChunkPos center, int x, int z) {
        return center.getChessboardDistance(x, z) <= FEATURES.directDependencies().getRadius();
    }

    static boolean canWrite(ChunkPos center, BlockPos pos) {
        return center.getChessboardDistance(pos.getX() >> 4, pos.getZ() >> 4) <= FEATURES.blockStateWriteRadius();
    }

    static ChunkPos requestedChunk(Object[] arguments) {
        if (arguments[0] instanceof BlockPos pos) return chunkPos(pos);
        return new ChunkPos((int) arguments[0], (int) arguments[1]);
    }

    static ChunkPos chunkPos(BlockPos pos) {
        return new ChunkPos(pos.getX() >> 4, pos.getZ() >> 4);
    }

    static BlockState targetState(ChunkAccess target, BlockPos pos) {
        if (target.isOutsideBuildHeight(pos)) return Blocks.VOID_AIR.defaultBlockState();
        return target.getPos().equals(chunkPos(pos)) ? target.getBlockState(pos) : Blocks.AIR.defaultBlockState();
    }

    static int targetHeight(ChunkAccess target, Heightmap.Types kind, int x, int z) {
        if (target.getPos().getChessboardDistance(x >> 4, z >> 4) != 0) return target.getMinY();
        return target.getHeight(kind, x & 15, z & 15) + 1;
    }

    static int height(WorldGenLevel world, Heightmap.Types kind, int x, int z) {
        var pos = new BlockPos.MutableBlockPos();
        for (int y = world.getMaxY(); y >= world.getMinY(); y--) {
            if (kind.isOpaque().test(world.getBlockState(pos.set(x, y, z)))) return y + 1;
        }
        return world.getMinY();
    }

    static net.minecraft.nbt.ListTag postProcessing(ChunkAccess chunk) {
        var snapshot = new net.minecraft.nbt.ListTag();
        for (var section : chunk.getPostProcessing()) {
            var entries = new net.minecraft.nbt.ListTag();
            if (section != null) for (short value : section) entries.add(net.minecraft.nbt.ShortTag.valueOf(value));
            snapshot.add(entries);
        }
        return snapshot;
    }

    static BlockState paletteState(ChunkAccess chunk, BlockPos pos) {
        if (chunk.isOutsideBuildHeight(pos)) return Blocks.VOID_AIR.defaultBlockState();
        return chunk.getSection(chunk.getSectionIndex(pos.getY()))
            .getBlockState(pos.getX() & 15, pos.getY() & 15, pos.getZ() & 15);
    }

    static BoundingBox writableArea(ChunkAccess chunk) {
        return writableArea(chunk.getPos(), chunk.getHeightAccessorForGeneration());
    }

    static BoundingBox writableArea(ChunkPos pos, net.minecraft.world.level.LevelHeightAccessor height) {
        return new BoundingBox(pos.getMinBlockX(), height.getMinY() + 1, pos.getMinBlockZ(),
            pos.getMaxBlockX(), height.getMaxY(), pos.getMaxBlockZ());
    }

    static boolean intersectsChunk(StructureStart start, ChunkPos pos) {
        return start.getBoundingBox().intersects(pos.getMinBlockX(), pos.getMinBlockZ(), pos.getMaxBlockX(), pos.getMaxBlockZ());
    }

    @FunctionalInterface
    interface Placer {
        void place(StructureStart start, WorldgenRandom random) throws Exception;
    }

    static void decorate(Registry<Structure> registry, List<StructureStart> starts, long seed, ChunkPos pos,
            Placer placer) throws Exception {
        var byStructure = new HashMap<Structure, List<StructureStart>>();
        for (var start : starts) {
            if (!start.isValid()) throw new AssertionError("invalid structure start");
            if (intersectsChunk(start, pos)) byStructure.computeIfAbsent(start.getStructure(), k -> new ArrayList<>()).add(start);
        }
        // Registry order is stable within each step; the index includes structures that have no starts here.
        var types = registry.stream().sorted(Comparator.comparing(Structure::step)).toList();
        var random = new WorldgenRandom(new XoroshiroRandomSource(0));
        long decoration = random.setDecorationSeed(seed, pos.getMinBlockX(), pos.getMinBlockZ());
        int step = -1, index = 0;
        for (var structure : types) {
            if (structure.step().ordinal() != step) {
                step = structure.step().ordinal();
                index = 0;
            }
            var batch = byStructure.get(structure);
            if (batch != null) {
                random.setFeatureSeed(decoration, index, step);
                // createReferences visits owner chunks by X then Z, and the manager iterates the resulting set.
                batch.sort(Comparator.comparingInt((StructureStart start) -> start.getChunkPos().x())
                    .thenComparingInt(start -> start.getChunkPos().z()));
                var references = new LongOpenHashSet();
                var byChunk = new HashMap<Long, StructureStart>();
                for (var start : batch) {
                    long owner = start.getChunkPos().pack();
                    references.add(owner);
                    if (byChunk.put(owner, start) != null) throw new AssertionError("duplicate start at " + start.getChunkPos());
                }
                for (long owner : references) placer.place(byChunk.get(owner), random);
            }
            index++;
        }
    }

    /** The chunk API of a region whose fixture stores only the neighbor columns it supplies. */
    static final class ChunkViews {
        private final java.util.Map<ChunkPos, ChunkAccess> views = new java.util.HashMap<>();
        private final ChunkAccess target;
        private final PalettedContainerFactory factory;

        ChunkViews(ChunkAccess target, net.minecraft.core.RegistryAccess registries) {
            this.target = target;
            this.factory = PalettedContainerFactory.create(registries);
            views.put(target.getPos(), target);
        }

        ChunkAccess get(WorldGenLevel world, Object[] arguments) {
            return get(world, target.getPos(), arguments);
        }

        ChunkAccess get(WorldGenLevel world, ChunkPos center, Object[] arguments) {
            var pos = requestedChunk(arguments);
            if (!canRead(center, pos.x(), pos.z())) throw new AssertionError("read outside generation region: " + pos);
            return views.computeIfAbsent(pos, at -> new ProtoChunk(at, UpgradeData.EMPTY,
                    target.getHeightAccessorForGeneration(), factory, null) {
                @Override public BlockState getBlockState(BlockPos where) {
                    return world.getBlockState(where);
                }
                @Override public net.minecraft.world.level.material.FluidState getFluidState(BlockPos where) {
                    return world.getFluidState(where);
                }
                @Override public net.minecraft.world.level.block.entity.BlockEntity getBlockEntity(BlockPos where) {
                    return world.getBlockEntity(where);
                }
                @Override public int getHeight(Heightmap.Types kind, int x, int z) {
                    return world.getHeight(kind, at.getMinBlockX() + (x & 15), at.getMinBlockZ() + (z & 15)) - 1;
                }
                @Override public BlockState setBlockState(BlockPos where, BlockState state, int flags) {
                    var old = world.getBlockState(where);
                    return world.setBlock(where, state, flags) ? old : null;
                }
            });
        }
    }
}
