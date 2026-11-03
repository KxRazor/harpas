// Test driver only: calls the owner-provided server's worldgen classes and writes their results.
import java.io.PrintWriter;
import java.util.Locale;
import net.minecraft.SharedConstants;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.*;
import net.minecraft.world.level.levelgen.blending.Blender;

public final class WorldgenReferenceProbe {
    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var lookup = VanillaRegistries.createLookup();
        long seed = Long.parseLong(args[1]);
        var random = RandomState.create(lookup, NoiseGeneratorSettings.OVERWORLD, seed);
        var settings = lookup.getOrThrow(NoiseGeneratorSettings.OVERWORLD).value();
        var surface = random.surfaceSystem();
        var depthMethod = SurfaceSystem.class.getDeclaredMethod("getSurfaceDepth", int.class, int.class);
        var bandMethod = SurfaceSystem.class.getDeclaredMethod("getBand", int.class, int.class, int.class);
        var oreMethod = OreVeinifier.class.getDeclaredMethod("create", DensityFunction.class,
            DensityFunction.class, DensityFunction.class, PositionalRandomFactory.class);
        var stateMethod = NoiseChunk.class.getDeclaredMethod("getInterpolatedState");
        for (var method : new java.lang.reflect.Method[] {depthMethod, bandMethod, oreMethod, stateMethod}) {
            method.setAccessible(true);
        }
        Aquifer.FluidPicker picker = (x, y, z) -> y < -54
            ? new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState())
            : new Aquifer.FluidStatus(63, Blocks.WATER.defaultBlockState());
        var biomeRegistry = new net.minecraft.core.MappedRegistry<net.minecraft.world.level.biome.Biome>(
            net.minecraft.core.registries.Registries.BIOME, com.mojang.serialization.Lifecycle.stable());
        lookup.lookupOrThrow(net.minecraft.core.registries.Registries.BIOME).listElements().forEach(
            holder -> biomeRegistry.register(holder.key(), holder.value(), net.minecraft.core.RegistrationInfo.BUILT_IN));
        biomeRegistry.freeze();
        var registryAccess = new net.minecraft.core.RegistryAccess.ImmutableRegistryAccess(java.util.List.of(
            net.minecraft.core.registries.BuiltInRegistries.BLOCK, biomeRegistry));
        var factory = net.minecraft.world.level.chunk.PalettedContainerFactory.create(registryAccess);
        var biomeSource = net.minecraft.world.level.biome.MultiNoiseBiomeSource.createFromPreset(
            lookup.getOrThrow(net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        var biomeManager = new net.minecraft.world.level.biome.BiomeManager(
            (qx, qy, qz) -> biomeSource.getNoiseBiome(qx, qy, qz, random.sampler()),
            net.minecraft.world.level.biome.BiomeManager.obfuscateSeed(seed));
        var heightAccessor = net.minecraft.world.level.LevelHeightAccessor.create(-64, 384);
        var generator = new NoiseBasedChunkGenerator(biomeSource, lookup.getOrThrow(NoiseGeneratorSettings.OVERWORLD));
        var generationContext = new WorldGenerationContext(generator, heightAccessor);

        try (PrintWriter out = new PrintWriter(args[0])) {
            for (int x = -32; x <= 32; x += 7) {
                for (int z = -32; z <= 32; z += 9) {
                    int depth = (int) depthMethod.invoke(surface, x, z);
                    for (int y = -64; y < 320; y += 13) {
                        int band = Block.getId((BlockState) bandMethod.invoke(surface, x, y, z));
                        out.printf("s %d %d %d %d %d%n", x, y, z, depth, band);
                    }
                }
            }
            for (double toggle : new double[] {-0.8, -0.6, -0.4, 0.4, 0.6, 0.8}) {
                for (double ridged : new double[] {-0.1, 0.1}) {
                    for (double gap : new double[] {-0.5, 0.0}) {
                        var ore = (NoiseChunk.BlockStateFiller) oreMethod.invoke(null,
                            DensityFunctions.constant(toggle), DensityFunctions.constant(ridged),
                            DensityFunctions.constant(gap), random.oreRandom());
                        for (int x = -8; x <= 8; x += 4) {
                            for (int z = -8; z <= 8; z += 4) {
                                for (int y = -60; y <= 50; y += 5) {
                                    var block = ore.calculate(new DensityFunction.SinglePointContext(x, y, z));
                                    out.printf("o %.1f %.1f %.1f %d %d %d %d%n", toggle, ridged, gap,
                                        x, y, z, block == null ? -1 : Block.getId(block));
                                }
                            }
                        }
                    }
                }
            }
            var chunks = new java.util.ArrayList<int[]>();
            for (int cx : new int[] {-1, 0, 1}) {
                for (int cz : new int[] {-1, 0, 1}) chunks.add(new int[] {cx, cz});
            }
            if (seed == 42) {
                for (int cx : new int[] {-120, -119}) {
                    for (int cz : new int[] {-120, -119}) chunks.add(new int[] {cx, cz}); // eroded badlands
                }
                for (int cx : new int[] {360, 361}) {
                    for (int cz : new int[] {-32, -31}) chunks.add(new int[] {cx, cz}); // frozen ocean
                }
            }
            for (var coordinates : chunks) {
                    int cx = coordinates[0], cz = coordinates[1];
                    var chunk = new NoiseChunk(4, random, cx * 16, cz * 16, settings.noiseSettings(),
                        Beardifier.EMPTY, settings, picker, Blender.empty());
                    for (int x = cx * 16 - 20; x <= cx * 16 + 20; x += 8) {
                        for (int z = cz * 16 - 20; z <= cz * 16 + 20; z += 8) {
                            out.printf("p %d %d %d %d %d%n", cx, cz, x, z, chunk.preliminarySurfaceLevel(x, z));
                        }
                    }
                    var aquifer = chunk.aquifer();
                    for (int x = cx * 16; x < cx * 16 + 16; x += 5) {
                        for (int z = cz * 16; z < cz * 16 + 16; z += 5) {
                            for (int y = -64; y < 320; y += 7) {
                                for (double density : new double[] {-1.0, -0.1, 0.0, 0.1}) {
                                    var block = aquifer.computeSubstance(new DensityFunction.SinglePointContext(x, y, z), density);
                                    out.printf("a %d %d %d %.1f %d %s%n", x, y, z, density,
                                        block == null ? -1 : Block.getId(block), aquifer.shouldScheduleFluidUpdate());
                                }
                            }
                        }
                    }
                    chunk = new NoiseChunk(4, random, cx * 16, cz * 16, settings.noiseSettings(),
                        Beardifier.EMPTY, settings, picker, Blender.empty());
                    var proto = new net.minecraft.world.level.chunk.ProtoChunk(new net.minecraft.world.level.ChunkPos(cx, cz),
                        net.minecraft.world.level.chunk.UpgradeData.EMPTY, heightAccessor, factory, null);
                    proto.fillBiomesFromNoise(biomeSource, random.sampler());
                    var blockPos = new net.minecraft.core.BlockPos.MutableBlockPos();
                    long hash = 0xcbf29ce484222325L;
                    long fluidHash = 0xcbf29ce484222325L;
                    int fluidCount = 0;
                    chunk.initializeForFirstCellX();
                    for (int cellX = 0; cellX < 4; cellX++) {
                        chunk.advanceCellX(cellX);
                        for (int cellZ = 0; cellZ < 4; cellZ++) {
                            for (int cellY = 47; cellY >= 0; cellY--) {
                                chunk.selectCellYZ(cellY, cellZ);
                                for (int iy = 7; iy >= 0; iy--) {
                                    int y = -64 + cellY * 8 + iy;
                                    chunk.updateForY(y, iy / 8.0);
                                    for (int ix = 0; ix < 4; ix++) {
                                        int x = cx * 16 + cellX * 4 + ix;
                                        chunk.updateForX(x, ix / 4.0);
                                        for (int iz = 0; iz < 4; iz++) {
                                            int z = cz * 16 + cellZ * 4 + iz;
                                            chunk.updateForZ(z, iz / 4.0);
                                            var block = (BlockState) stateMethod.invoke(chunk);
                                            var placed = block == null ? settings.defaultBlock() : block;
                                            if (!placed.isAir()) proto.setBlockState(blockPos.set(x, y, z), placed, 0);
                                            int id = Block.getId(block == null ? settings.defaultBlock() : block);
                                            hash = (hash ^ Integer.toUnsignedLong(id)) * 0x100000001b3L;
                                            if (block != null && !block.getFluidState().isEmpty()
                                                    && chunk.aquifer().shouldScheduleFluidUpdate()) {
                                                fluidHash = (fluidHash ^ (x & 15)) * 0x100000001b3L;
                                                fluidHash = (fluidHash ^ Integer.toUnsignedLong(y)) * 0x100000001b3L;
                                                fluidHash = (fluidHash ^ (z & 15)) * 0x100000001b3L;
                                                fluidCount++;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        chunk.swapSlices();
                    }
                    chunk.stopInterpolation();
                    out.printf("t %d %d %016x %d %016x%n", cx, cz, hash, fluidCount, fluidHash);
                    Heightmap.primeHeightmaps(proto, java.util.Set.of(
                        Heightmap.Types.WORLD_SURFACE_WG, Heightmap.Types.OCEAN_FLOOR_WG));
                    surface.buildSurface(random, biomeManager, biomeRegistry, false, generationContext,
                        proto, chunk, settings.surfaceRule());
                    long surfaceHash = 0xcbf29ce484222325L;
                    for (int x = 0; x < 16; x++) {
                        for (int z = 0; z < 16; z++) {
                            for (int y = -64; y < 320; y++) {
                                int id = Block.getId(proto.getBlockState(blockPos.set(cx * 16 + x, y, cz * 16 + z)));
                                surfaceHash = (surfaceHash ^ Integer.toUnsignedLong(id)) * 0x100000001b3L;
                            }
                        }
                    }
                    out.printf("u %d %d %016x%n", cx, cz, surfaceHash);
            }
        }
    }
}
