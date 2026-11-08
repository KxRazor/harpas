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
    static final class TestRegion extends net.minecraft.server.level.WorldGenRegion {
        net.minecraft.core.RegistryAccess access;
        net.minecraft.world.level.LevelHeightAccessor height;
        net.minecraft.world.level.chunk.PalettedContainerFactory factory;
        java.util.Map<Long, net.minecraft.world.level.chunk.ChunkAccess> chunks;
        net.minecraft.world.level.biome.BiomeManager biomes;
        net.minecraft.world.level.ChunkPos center;
        long seed;
        TestRegion() { super(null, null, null, null); }
        static TestRegion create(net.minecraft.core.RegistryAccess access,
                                 net.minecraft.world.level.LevelHeightAccessor height,
                                 net.minecraft.world.level.chunk.PalettedContainerFactory factory,
                                 net.minecraft.world.level.chunk.ChunkAccess target, long seed) throws Exception {
            var unsafeClass = Class.forName("sun.misc.Unsafe");
            var field = unsafeClass.getDeclaredField("theUnsafe"); field.setAccessible(true);
            var unsafe = field.get(null);
            var region = (TestRegion)unsafeClass.getMethod("allocateInstance", Class.class).invoke(unsafe, TestRegion.class);
            region.access = access; region.height = height; region.factory = factory; region.seed = seed;
            region.chunks = new java.util.HashMap<>();
            var pos = target.getPos(); region.center = pos; region.chunks.put(((long)pos.x() << 32) ^ (pos.z() & 0xffffffffL), target);
            return region;
        }
        @Override public net.minecraft.world.level.biome.BiomeManager getBiomeManager() { return biomes; }
        @Override public net.minecraft.world.level.ChunkPos getCenter() { return center; }
        @Override public boolean isOldChunkAround(net.minecraft.world.level.ChunkPos pos, int radius) { return false; }
        @Override public boolean ensureCanWrite(net.minecraft.core.BlockPos pos) { return true; }
        // WorldGenRegion.setBlock without the level's points of interest, which a test region has no level for.
        @Override public boolean setBlock(net.minecraft.core.BlockPos pos, BlockState state, int flags, int recursion) {
            var chunk = getChunk(pos.getX() >> 4, pos.getZ() >> 4);
            var old = chunk.setBlockState(pos, state, flags);
            if (state.hasBlockEntity()) {
                var tag = new net.minecraft.nbt.CompoundTag();
                tag.putInt("x", pos.getX()); tag.putInt("y", pos.getY()); tag.putInt("z", pos.getZ());
                tag.putString("id", "DUMMY");
                chunk.setBlockEntityNbt(tag);
            } else if (old != null && old.hasBlockEntity()) {
                chunk.removeBlockEntity(pos);
            }
            if ((flags & 16) == 0) {
                var post = state.getPostProcessPos(this, pos);
                if (post != null) chunk.markPosForPostprocessing(post);
            }
            return true;
        }
        // Ticks scheduled during generation, such as by leaves next to a new tree, go to each chunk's own ticks,
        // as in WorldGenRegion, at game time 0. The counter is static: regions are allocated without initializers.
        static final java.util.concurrent.atomic.AtomicLong subTicks = new java.util.concurrent.atomic.AtomicLong();
        @Override public net.minecraft.world.ticks.LevelTickAccess<Block> getBlockTicks() {
            return new net.minecraft.world.ticks.WorldGenTickAccess<>(pos -> getChunk(pos).getBlockTicks());
        }
        @Override public net.minecraft.world.ticks.LevelTickAccess<net.minecraft.world.level.material.Fluid> getFluidTicks() {
            return new net.minecraft.world.ticks.WorldGenTickAccess<>(pos -> getChunk(pos).getFluidTicks());
        }
        @Override public long getGameTime() { return 0; }
        @Override public long nextSubTickCount() { return subTicks.getAndIncrement(); }
        @Override public int getMinY() { return height.getMinY(); }
        @Override public int getHeight() { return height.getHeight(); }
        @Override public long getSeed() { return seed; }
        @Override public net.minecraft.core.RegistryAccess registryAccess() { return access; }
        @Override public net.minecraft.world.level.chunk.ChunkAccess getChunk(int x, int z) { return getChunk(x,z,null,false); }
        @Override public net.minecraft.world.level.chunk.ChunkAccess getChunk(int x, int z,
                net.minecraft.world.level.chunk.status.ChunkStatus status, boolean required) {
            long key = ((long)x << 32) ^ (z & 0xffffffffL);
            return chunks.computeIfAbsent(key, ignored -> new net.minecraft.world.level.chunk.ProtoChunk(
                new net.minecraft.world.level.ChunkPos(x,z), net.minecraft.world.level.chunk.UpgradeData.EMPTY,
                height, factory, null));
        }
    }

    static void addBlockTag(String name, java.util.Set<net.minecraft.core.Holder<Block>> blocks,
                            java.util.Set<String> visited) throws Exception {
        if (!visited.add(name)) return;
        var id = net.minecraft.resources.Identifier.parse(name);
        String path = "/data/" + id.getNamespace() + "/tags/block/" + id.getPath() + ".json";
        try (var stream = WorldgenReferenceProbe.class.getResourceAsStream(path)) {
            if (stream == null) throw new IllegalArgumentException("missing tag " + name);
            var json = com.google.gson.JsonParser.parseReader(new java.io.InputStreamReader(stream, java.nio.charset.StandardCharsets.UTF_8)).getAsJsonObject();
            for (var value : json.getAsJsonArray("values")) {
                boolean required = true;
                String entry;
                if (value.isJsonPrimitive()) entry = value.getAsString();
                else {
                    var object = value.getAsJsonObject();
                    entry = object.get("id").getAsString();
                    if (object.has("required")) required = object.get("required").getAsBoolean();
                }
                if (entry.startsWith("#")) {
                    String child = entry.substring(1);
                    try { addBlockTag(child, blocks, visited); }
                    catch (IllegalArgumentException error) { if (required) throw error; }
                } else {
                    var holder = net.minecraft.core.registries.BuiltInRegistries.BLOCK.get(net.minecraft.resources.Identifier.parse(entry));
                    if (holder.isPresent()) blocks.add(holder.get());
                    else if (required) throw new IllegalArgumentException("missing block " + entry);
                }
            }
        }
    }

    static net.minecraft.world.level.levelgen.carver.ConfiguredWorldCarver<?> resolvedCarver(
            net.minecraft.world.level.levelgen.carver.ConfiguredWorldCarver<?> original,
            net.minecraft.core.HolderSet<Block> replaceable) throws Exception {
        var config = original.config();
        var base = new net.minecraft.world.level.levelgen.carver.CarverConfiguration(
            config.probability, config.y, config.yScale, config.lavaLevel, config.debugSettings, replaceable);
        if (config instanceof net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration cave) {
            var field = net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration.class.getDeclaredField("floorLevel");
            field.setAccessible(true);
            var rebuilt = new net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration(base,
                cave.horizontalRadiusMultiplier, cave.verticalRadiusMultiplier,
                (net.minecraft.util.valueproviders.FloatProvider)field.get(cave));
            return net.minecraft.world.level.levelgen.carver.WorldCarver.CAVE.configured(rebuilt);
        }
        var canyon = (net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration)config;
        return net.minecraft.world.level.levelgen.carver.WorldCarver.CANYON.configured(
            new net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration(base, canyon.verticalRotation, canyon.shape));
    }

    static final class FixedFloatRandom extends LegacyRandomSource {
        final float value;
        FixedFloatRandom(float value) { super(0); this.value = value; }
        @Override public float nextFloat() { return value; }
    }

    static final class CaveTrace extends net.minecraft.world.level.levelgen.carver.CaveWorldCarver {
        long count, hash = 0xcbf29ce484222325L;
        CaveTrace() { super(net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration.CODEC); }
        @Override protected boolean carveEllipsoid(
            net.minecraft.world.level.levelgen.carver.CarvingContext context,
            net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration config,
            net.minecraft.world.level.chunk.ChunkAccess chunk,
            java.util.function.Function<net.minecraft.core.BlockPos, net.minecraft.core.Holder<net.minecraft.world.level.biome.Biome>> biome,
            Aquifer aquifer, double x, double y, double z, double horizontal, double vertical,
            net.minecraft.world.level.chunk.CarvingMask mask, CarveSkipChecker skip) {
            count++;
            for (double value : new double[] {x,y,z,horizontal,vertical})
                hash = (hash ^ Double.doubleToLongBits(value)) * 0x100000001b3L;
            return true;
        }
        void trace(net.minecraft.world.level.chunk.ChunkAccess chunk, long seed, double x, double y, double z,
                   double hs, double vs, float thickness, float yaw, float pitch, int end, double ys) {
            super.createTunnel(null, null, chunk, null, seed, null, x, y, z, hs, vs,
                thickness, yaw, pitch, 0, end, ys, null, null);
        }
        void traceRoom(net.minecraft.world.level.chunk.ChunkAccess chunk, double x, double y, double z,
                       float width, double scale) {
            super.createRoom(null, null, chunk, null, null, x, y, z, width, scale, null, null);
        }
    }

    static final class CavePlacement extends net.minecraft.world.level.levelgen.carver.CaveWorldCarver {
        long count, hash = 0xcbf29ce484222325L;
        CavePlacement() { super(net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration.CODEC); }
        void add(long value) { hash = (hash ^ value) * 0x100000001b3L; }
        @Override protected void createRoom(net.minecraft.world.level.levelgen.carver.CarvingContext context,
            net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration config,
            net.minecraft.world.level.chunk.ChunkAccess chunk,
            java.util.function.Function<net.minecraft.core.BlockPos, net.minecraft.core.Holder<net.minecraft.world.level.biome.Biome>> biome,
            Aquifer aquifer, double x, double y, double z, float width, double scale,
            net.minecraft.world.level.chunk.CarvingMask mask, CarveSkipChecker skip) {
            count++; add(1); add(Double.doubleToLongBits(x)); add(Double.doubleToLongBits(y));
            add(Double.doubleToLongBits(z)); add(Integer.toUnsignedLong(Float.floatToIntBits(width)));
            add(Double.doubleToLongBits(scale));
        }
        @Override protected void createTunnel(net.minecraft.world.level.levelgen.carver.CarvingContext context,
            net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration config,
            net.minecraft.world.level.chunk.ChunkAccess chunk,
            java.util.function.Function<net.minecraft.core.BlockPos, net.minecraft.core.Holder<net.minecraft.world.level.biome.Biome>> biome,
            long tunnelSeed, Aquifer aquifer, double x, double y, double z, double hs, double vs,
            float thickness, float yaw, float pitch, int first, int end, double scale,
            net.minecraft.world.level.chunk.CarvingMask mask, CarveSkipChecker skip) {
            count++; add(2); add(tunnelSeed); add(Double.doubleToLongBits(x)); add(Double.doubleToLongBits(y));
            add(Double.doubleToLongBits(z)); add(Double.doubleToLongBits(hs)); add(Double.doubleToLongBits(vs));
            add(Integer.toUnsignedLong(Float.floatToIntBits(thickness))); add(Integer.toUnsignedLong(Float.floatToIntBits(yaw)));
            add(Integer.toUnsignedLong(Float.floatToIntBits(pitch))); add(first); add(end); add(Double.doubleToLongBits(scale));
        }
    }

    static final class CanyonTrace extends net.minecraft.world.level.levelgen.carver.CanyonWorldCarver {
        long count, hash = 0xcbf29ce484222325L;
        CanyonTrace() { super(net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration.CODEC); }
        @Override protected boolean carveEllipsoid(
            net.minecraft.world.level.levelgen.carver.CarvingContext context,
            net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration config,
            net.minecraft.world.level.chunk.ChunkAccess chunk,
            java.util.function.Function<net.minecraft.core.BlockPos, net.minecraft.core.Holder<net.minecraft.world.level.biome.Biome>> biome,
            Aquifer aquifer, double x, double y, double z, double horizontal, double vertical,
            net.minecraft.world.level.chunk.CarvingMask mask, CarveSkipChecker skip) {
            count++;
            for (double value : new double[] {x,y,z,horizontal,vertical})
                hash = (hash ^ Double.doubleToLongBits(value)) * 0x100000001b3L;
            for (int layer : new int[] {-63, -32, 0, 64, 150, 318})
                hash = (hash ^ (skip.shouldSkip(context, 0.75, 0.5, 0.25, layer) ? 1 : 0)) * 0x100000001b3L;
            return true;
        }
    }

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
        var blocks = new java.util.LinkedHashSet<net.minecraft.core.Holder<Block>>();
        addBlockTag("minecraft:overworld_carver_replaceables", blocks, new java.util.HashSet<>());
        var replaceableBlocks = net.minecraft.core.HolderSet.direct(new java.util.ArrayList<>(blocks));
        var resolvedCarvers = new java.util.ArrayList<net.minecraft.world.level.levelgen.carver.ConfiguredWorldCarver<?>>();
        for (String name : new String[] {"minecraft:cave", "minecraft:cave_extra_underground", "minecraft:canyon"}) {
            var key = net.minecraft.resources.ResourceKey.create(
                net.minecraft.core.registries.Registries.CONFIGURED_CARVER, net.minecraft.resources.Identifier.parse(name));
            var original = lookup.getOrThrow(key).value();
            var replaceableField = net.minecraft.world.level.levelgen.carver.CarverConfiguration.class.getDeclaredField("replaceable");
            replaceableField.setAccessible(true);
            replaceableField.set(original.config(), replaceableBlocks);
            resolvedCarvers.add(resolvedCarver(original, replaceableBlocks));
        }
        var caveSkip = net.minecraft.world.level.levelgen.carver.CaveWorldCarver.class.getDeclaredMethod(
            "shouldSkip", double.class, double.class, double.class, double.class);
        caveSkip.setAccessible(true);
        var canyonSkip = net.minecraft.world.level.levelgen.carver.CanyonWorldCarver.class.getDeclaredMethod(
            "shouldSkip", net.minecraft.world.level.levelgen.carver.CarvingContext.class, float[].class,
            double.class, double.class, double.class, int.class);
        canyonSkip.setAccessible(true);
        var ellipsoidMethod = net.minecraft.world.level.levelgen.carver.WorldCarver.class.getDeclaredMethod(
            "carveEllipsoid", net.minecraft.world.level.levelgen.carver.CarvingContext.class,
            net.minecraft.world.level.levelgen.carver.CarverConfiguration.class,
            net.minecraft.world.level.chunk.ChunkAccess.class, java.util.function.Function.class,
            Aquifer.class, double.class, double.class, double.class, double.class, double.class,
            net.minecraft.world.level.chunk.CarvingMask.class,
            net.minecraft.world.level.levelgen.carver.WorldCarver.CarveSkipChecker.class);
        ellipsoidMethod.setAccessible(true);

        try (PrintWriter out = new PrintWriter(args[0])) {
            var carvers = lookup.lookupOrThrow(net.minecraft.core.registries.Registries.CONFIGURED_CARVER)
                .listElements().sorted(java.util.Comparator.comparing(h -> h.key().identifier().toString())).toList();
            for (var holder : carvers) {
                var carver = holder.value();
                var rng = new LegacyRandomSource(seed);
                long hash = 0xcbf29ce484222325L;
                for (int i = 0; i < 1000; i++)
                    hash = (hash ^ (carver.isStartChunk(rng) ? 1 : 0)) * 0x100000001b3L;
                float p = carver.config().probability;
                boolean below = carver.isStartChunk(new FixedFloatRandom(Math.nextDown(p)));
                boolean equal = carver.isStartChunk(new FixedFloatRandom(p));
                boolean above = carver.isStartChunk(new FixedFloatRandom(Math.nextUp(p)));
                out.printf("q %s %016x %d %s %s %s%n", holder.key().identifier(), hash, rng.nextInt(),
                    below, equal, above);
            }
            net.minecraft.util.valueproviders.FloatProvider[] floatProviders = {
                net.minecraft.util.valueproviders.ConstantFloat.of(3.5F),
                net.minecraft.util.valueproviders.UniformFloat.of(-1.0F, 1.4F),
                net.minecraft.util.valueproviders.TrapezoidFloat.of(0.0F, 6.0F, 2.0F),
                net.minecraft.util.valueproviders.TrapezoidFloat.of(-2.0F, 4.0F, 0.0F),
                net.minecraft.util.valueproviders.TrapezoidFloat.of(1.0F, 3.0F, 2.0F),
            };
            for (int index = 0; index < floatProviders.length; index++) {
                var rng = new LegacyRandomSource(seed);
                long hash = 0xcbf29ce484222325L;
                for (int i = 0; i < 100; i++)
                    hash = (hash ^ Integer.toUnsignedLong(Float.floatToIntBits(floatProviders[index].sample(rng))))
                        * 0x100000001b3L;
                out.printf("f %d %016x %d%n", index, hash, rng.nextInt());
            }
            net.minecraft.world.level.levelgen.heightproviders.HeightProvider[] heightProviders = {
                net.minecraft.world.level.levelgen.heightproviders.UniformHeight.of(
                    VerticalAnchor.aboveBottom(8), VerticalAnchor.absolute(47)),
                net.minecraft.world.level.levelgen.heightproviders.UniformHeight.of(
                    VerticalAnchor.bottom(), VerticalAnchor.top()),
                net.minecraft.world.level.levelgen.heightproviders.UniformHeight.of(
                    VerticalAnchor.absolute(12), VerticalAnchor.absolute(12)),
            };
            for (int index = 0; index < heightProviders.length; index++) {
                var rng = new LegacyRandomSource(seed);
                long hash = 0xcbf29ce484222325L;
                for (int i = 0; i < 100; i++)
                    hash = (hash ^ Integer.toUnsignedLong(heightProviders[index].sample(rng, generationContext)))
                        * 0x100000001b3L;
                out.printf("v %d %016x %d%n", index, hash, rng.nextInt());
            }
            var probeNoise = new NoiseChunk(4, random, 0, 0, settings.noiseSettings(),
                Beardifier.EMPTY, settings, picker, Blender.empty());
            var probeContext = new net.minecraft.world.level.levelgen.carver.CarvingContext(
                generator, registryAccess, heightAccessor, probeNoise, random, settings.surfaceRule());
            long caveHash = 0xcbf29ce484222325L, canyonHash = 0xcbf29ce484222325L;
            for (int parameter = 0; parameter < 3; parameter++) {
                double floor = new double[] {-1.0, -0.4, 0.0}[parameter];
                float[] width = new float[384];
                java.util.Arrays.fill(width, new float[] {0.5F, 1.0F, 1.5F}[parameter]);
                for (int xi = -5; xi <= 5; xi++) {
                    for (int yi = -5; yi <= 5; yi++) {
                        for (int zi = -5; zi <= 5; zi++) {
                            double x = xi / 5.0, y = yi / 5.0, z = zi / 5.0;
                            boolean caveResult = (boolean)caveSkip.invoke(null, x, y, z, floor);
                            boolean canyonResult = (boolean)canyonSkip.invoke(
                                net.minecraft.world.level.levelgen.carver.WorldCarver.CANYON,
                                probeContext, width, x, y, z, 0);
                            caveHash = (caveHash ^ (caveResult ? 1 : 0)) * 0x100000001b3L;
                            canyonHash = (canyonHash ^ (canyonResult ? 1 : 0)) * 0x100000001b3L;
                        }
                    }
                }
            }
            out.printf("k %016x %016x%n", caveHash, canyonHash);
            long trigHash = 0xcbf29ce484222325L;
            double trigScale = 65536.0 / (2.0 * Math.PI);
            for (int i = 0; i < 65536; i++) {
                double angle = (i + 0.25) / trigScale;
                trigHash = (trigHash ^ Integer.toUnsignedLong(Float.floatToIntBits(net.minecraft.util.Mth.sin(angle)))) * 0x100000001b3L;
                trigHash = (trigHash ^ Integer.toUnsignedLong(Float.floatToIntBits(net.minecraft.util.Mth.cos(angle)))) * 0x100000001b3L;
            }
            out.printf("j %016x%n", trigHash);
            var probeProto = new net.minecraft.world.level.chunk.ProtoChunk(
                new net.minecraft.world.level.ChunkPos(0, 0),
                net.minecraft.world.level.chunk.UpgradeData.EMPTY, heightAccessor, factory, null);
            for (int index = 0; index < 5; index++) {
                var trace = new CaveTrace();
                float width = new float[] {0.0F, 0.5F, 2.3F, 6.0F, 10.0F}[index];
                double scale = new double[] {0.5, 1.0, 1.25, 2.0, 0.1}[index];
                trace.traceRoom(probeProto, -10, 32, 20, width, scale);
                out.printf("m %d %d %016x%n", index, trace.count, trace.hash);
            }
            for (int index = 0; index < 4; index++) {
                var trace = new CaveTrace();
                if (index == 0) trace.trace(probeProto, 42, 8, 64, 8, 1, 1, 2, 0, 0, 64, 1);
                if (index == 1) trace.trace(probeProto, seed, 8, 32, 8, 0.7, 1.3, 0.8F, 1.2F, 0.15F, 72, 0.75);
                if (index == 2) trace.trace(probeProto, -1, -10, -40, 16, 1.2, 0.8, 3, -0.8F, -0.2F, 112, 1);
                if (index == 3) trace.trace(probeProto, 0, 50, 20, 50, 1, 1, 0.7F, 2.2F, 0.03F, 48, 1);
                out.printf("w %d %d %016x%n", index, trace.count, trace.hash);
            }
            for (int index = 0; index < 64; index++) {
                var trace = new CaveTrace();
                double hs = new double[] {0.6, 1.0, 1.4}[index % 3];
                double vs = new double[] {0.7, 1.0, 1.3}[index % 3];
                double ys = new double[] {0.5, 1.0, 1.25}[index % 3];
                float thickness = new float[] {0.5F, 0.99F, 1.0F, 1.5F, 2.0F, 3.0F}[index % 6];
                int end = new int[] {16, 32, 64, 112}[index % 4];
                trace.trace(probeProto, seed + index * 104729L, (index % 5) * 8 - 8,
                    (index % 7) * 16 - 48, (index % 4) * 8 - 8,
                    hs, vs, thickness, index * 0.1F, (index % 9 - 4) * 0.05F, end, ys);
                out.printf("w %d %d %016x%n", index + 4, trace.count, trace.hash);
            }
            for (String name : new String[] {"minecraft:cave", "minecraft:cave_extra_underground"}) {
                var key = net.minecraft.resources.ResourceKey.create(
                    net.minecraft.core.registries.Registries.CONFIGURED_CARVER, net.minecraft.resources.Identifier.parse(name));
                var config = (net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration) lookup.getOrThrow(key).value().config();
                for (int index = 0; index < 64; index++) {
                    var trace = new CavePlacement();
                    var rng = new LegacyRandomSource(seed + index * 104729L);
                    var source = new net.minecraft.world.level.ChunkPos(index % 9 - 4, index % 7 - 3);
                    trace.carve(probeContext, config, probeProto, null, rng, null, source, null);
                    out.printf("r %s %d %d %016x %d%n", name, index, trace.count, trace.hash, rng.nextInt());
                }
            }
            var canyonKey = net.minecraft.resources.ResourceKey.create(
                net.minecraft.core.registries.Registries.CONFIGURED_CARVER,
                net.minecraft.resources.Identifier.parse("minecraft:canyon"));
            var canyonConfig = (net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration)
                lookup.getOrThrow(canyonKey).value().config();
            var widthMethod = net.minecraft.world.level.levelgen.carver.CanyonWorldCarver.class.getDeclaredMethod(
                "initWidthFactors", net.minecraft.world.level.levelgen.carver.CarvingContext.class,
                net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration.class, net.minecraft.util.RandomSource.class);
            widthMethod.setAccessible(true);
            for (int smoothness : new int[] {1, 2, 3, 10}) {
                var shape = canyonConfig.shape;
                var customShape = new net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration.CanyonShapeConfiguration(
                    shape.distanceFactor, shape.thickness, smoothness, shape.horizontalRadiusFactor,
                    shape.verticalRadiusDefaultFactor, shape.verticalRadiusCenterFactor);
                var custom = new net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration(
                    canyonConfig, canyonConfig.verticalRotation, customShape);
                var rng = new LegacyRandomSource(seed);
                float[] widths = (float[])widthMethod.invoke(
                    net.minecraft.world.level.levelgen.carver.WorldCarver.CANYON, probeContext, custom, rng);
                long hash = 0xcbf29ce484222325L;
                for (float width : widths) hash = (hash ^ Integer.toUnsignedLong(Float.floatToIntBits(width))) * 0x100000001b3L;
                out.printf("b %d %d %016x %d%n", smoothness, widths.length, hash, rng.nextInt());
            }
            var canyonPathMethod = net.minecraft.world.level.levelgen.carver.CanyonWorldCarver.class.getDeclaredMethod(
                "doCarve", net.minecraft.world.level.levelgen.carver.CarvingContext.class,
                net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration.class,
                net.minecraft.world.level.chunk.ChunkAccess.class, java.util.function.Function.class,
                long.class, Aquifer.class, double.class, double.class, double.class,
                float.class, float.class, float.class, int.class, int.class, double.class,
                net.minecraft.world.level.chunk.CarvingMask.class);
            canyonPathMethod.setAccessible(true);
            for (int index = 0; index < 32; index++) {
                var shape = canyonConfig.shape;
                var customShape = new net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration.CanyonShapeConfiguration(
                    shape.distanceFactor, shape.thickness, new int[] {1,2,3,10}[index % 4], shape.horizontalRadiusFactor,
                    new float[] {1.0F,1.2F}[index % 2], new float[] {0.0F,0.8F}[index % 2]);
                var custom = new net.minecraft.world.level.levelgen.carver.CanyonCarverConfiguration(
                    canyonConfig, canyonConfig.verticalRotation, customShape);
                var trace = new CanyonTrace();
                float thickness = new float[] {0.5F,1.0F,2.0F,3.0F}[index % 4];
                int end = new int[] {16,32,64,112}[index % 4];
                canyonPathMethod.invoke(trace, probeContext, custom, probeProto, null, seed + index * 104729L,
                    null, (double)((index % 5) * 8 - 8), (double)((index % 7) * 16 - 48), (double)((index % 4) * 8 - 8),
                    thickness, index * 0.1F, (index % 9 - 4) * 0.05F, 0, end,
                    new double[] {0.5,1.0,1.25}[index % 3], null);
                out.printf("n %d %d %016x%n", index, trace.count, trace.hash);
            }
            for (int index = 0; index < 64; index++) {
                var trace = new CanyonTrace();
                var rng = new LegacyRandomSource(seed + index * 104729L);
                var source = new net.minecraft.world.level.ChunkPos(index % 9 - 4, index % 7 - 3);
                trace.carve(probeContext, canyonConfig, probeProto, null, rng, null, source, null);
                out.printf("g %d %d %016x %d%n", index, trace.count, trace.hash, rng.nextInt());
            }
            {
                var carved = new net.minecraft.world.level.chunk.ProtoChunk(new net.minecraft.world.level.ChunkPos(0, 0),
                    net.minecraft.world.level.chunk.UpgradeData.EMPTY, heightAccessor, factory, null);
                var pos = new net.minecraft.core.BlockPos.MutableBlockPos();
                for (int x=0; x<16; x++) for (int z=0; z<16; z++) for (int y=5; y<=15; y++)
                    carved.setBlockState(pos.set(x,y,z), ((x*3+z+y)%7==0 ? Blocks.BEDROCK : Blocks.STONE).defaultBlockState(), 0);
                var one = net.minecraft.util.valueproviders.ConstantFloat.of(1);
                var anchor = net.minecraft.world.level.levelgen.heightproviders.ConstantHeight.of(VerticalAnchor.absolute(0));
                var replaceable = net.minecraft.core.HolderSet.direct(Blocks.STONE.builtInRegistryHolder());
                var config = new net.minecraft.world.level.levelgen.carver.CaveCarverConfiguration(
                    1, anchor, one, VerticalAnchor.absolute(10), replaceable, one, one, one);
                var mask = new net.minecraft.world.level.chunk.CarvingMask(384,-64);
                mask.set(8,11,8);
                class ProbeAquifer implements Aquifer {
                    boolean scheduled;
                    public BlockState computeSubstance(DensityFunction.FunctionContext context, double density) {
                        if (density != 0.0) throw new IllegalStateException();
                        scheduled = ((context.blockX() + context.blockZ()) & 1) == 0;
                        return switch (context.blockX() & 3) {
                            case 0 -> null;
                            case 1 -> Blocks.WATER.defaultBlockState();
                            default -> Blocks.AIR.defaultBlockState();
                        };
                    }
                    public boolean shouldScheduleFluidUpdate() { return scheduled; }
                }
                var aq = new ProbeAquifer();
                net.minecraft.world.level.levelgen.carver.WorldCarver.CarveSkipChecker skip = (context,x,y,z,by) -> {
                    try { return (boolean)caveSkip.invoke(null,x,y,z,-0.4); }
                    catch (Exception error) { throw new RuntimeException(error); }
                };
                for (double cx : new double[] {8.5, 9.5})
                    ellipsoidMethod.invoke(net.minecraft.world.level.levelgen.carver.WorldCarver.CAVE,
                        probeContext, config, carved, null, aq, cx, 10.5, 8.5, 3.2, 2.7, mask, skip);
                long blockHash=0xcbf29ce484222325L, maskHash=0xcbf29ce484222325L, postHash=0xcbf29ce484222325L;
                for (int x=0; x<16; x++) for (int z=0; z<16; z++) for (int y=5; y<=15; y++)
                    blockHash=(blockHash ^ Integer.toUnsignedLong(Block.getId(carved.getBlockState(pos.set(x,y,z))))) * 0x100000001b3L;
                for (long word : mask.toArray()) maskHash=(maskHash ^ word) * 0x100000001b3L;
                int posts=0;
                var lists=carved.getPostProcessing();
                for (int section=0;section<lists.length;section++) if (lists[section] != null)
                    for (short value : lists[section]) { posts++; postHash=(postHash ^ section)*0x100000001b3L; postHash=(postHash ^ (value & 65535))*0x100000001b3L; }
                out.printf("d %016x %016x %d %016x%n",blockHash,maskHash,posts,postHash);
            }
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
                                                proto.markPosForPostprocessing(blockPos.set(x, y, z));
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
                    Heightmap.primeHeightmaps(proto, java.util.EnumSet.allOf(Heightmap.Types.class));
                    for (var kind : Heightmap.Types.values()) {
                        long heightmapHash = 0xcbf29ce484222325L;
                        for (long word : proto.getOrCreateHeightmapUnprimed(kind).getRawData()) {
                            heightmapHash = (heightmapHash ^ word) * 0x100000001b3L;
                        }
                        out.printf("h %d %d %s %016x%n", cx, cz, kind.name(), heightmapHash);
                    }
                    var carvingContext = new net.minecraft.world.level.levelgen.carver.CarvingContext(
                        generator, registryAccess, heightAccessor, chunk, random, settings.surfaceRule());
                    for (int x : new int[] {0, 7, 15}) for (int z : new int[] {0, 7, 15})
                        for (int y : new int[] {-60, -1, 30, 63, 128, 300}) for (boolean fluid : new boolean[] {false, true}) {
                            var material = carvingContext.topMaterial(p -> biomeManager.getBiome(p), proto,
                                blockPos.set(cx * 16 + x, y, cz * 16 + z), fluid);
                            out.printf("q2 %d %d %d %d %d %s %d%n", cx, cz, x, y, z, fluid,
                                material.map(Block::getId).orElse(-1));
                        }
                    double[][] ellipsoids = {
                        {8.1, 10.1, 8.1, 2.6, 1.5}, {-1.3, 30.5, 8.5, 2.4, 4.0},
                        {16.9, 42.7, 15.9, 3.5, 2.3}, {8.5, -64.0, 8.5, 2.0, 2.0},
                        {8.5, 312.0, 8.5, 1.0, 2.0}, {30.0, 10.0, 8.0, 2.0, 2.0}
                    };
                    for (int index = 0; index < ellipsoids.length; index++) {
                        var e = ellipsoids[index];
                        long[] observed = {0, 0xcbf29ce484222325L};
                        net.minecraft.world.level.levelgen.carver.WorldCarver.CarveSkipChecker skip =
                            (context, x, y, z, blockY) -> {
                                observed[0]++;
                                for (long value : new long[] {Double.doubleToLongBits(x),
                                    Double.doubleToLongBits(y), Double.doubleToLongBits(z), blockY})
                                    observed[1] = (observed[1] ^ value) * 0x100000001b3L;
                                return true;
                            };
                        ellipsoidMethod.invoke(net.minecraft.world.level.levelgen.carver.WorldCarver.CAVE,
                            carvingContext, null, proto, null, null, cx * 16 + e[0], e[1], cz * 16 + e[2],
                            e[3], e[4], new net.minecraft.world.level.chunk.CarvingMask(384, -64), skip);
                        out.printf("e %d %d %d %d %016x%n", cx, cz, index, observed[0], observed[1]);
                    }
                    var mask = new net.minecraft.world.level.chunk.CarvingMask(384, -64);
                    for (int type = 0; type < resolvedCarvers.size(); type++) {
                        for (int offset = 0; offset < 3; offset++) {
                            int sx = cx + new int[] {0, 1, -1}[offset];
                            int sz = cz + new int[] {0, 0, 1}[offset];
                            var rng = new LegacyRandomSource(seed + type * 104729L + offset * 8191L);
                            resolvedCarvers.get(type).carve(carvingContext, proto, p -> biomeManager.getBiome(p),
                                rng, chunk.aquifer(), new net.minecraft.world.level.ChunkPos(sx, sz), mask);
                        }
                    }
                    long carvedHash = 0xcbf29ce484222325L, carvedMaskHash = 0xcbf29ce484222325L;
                    for (int x=0;x<16;x++) for(int z=0;z<16;z++) for(int y=-64;y<320;y++)
                        carvedHash=(carvedHash ^ Integer.toUnsignedLong(Block.getId(proto.getBlockState(blockPos.set(cx*16+x,y,cz*16+z))))) * 0x100000001b3L;
                    for(long word:mask.toArray()) carvedMaskHash=(carvedMaskHash ^ word)*0x100000001b3L;
                    long carvedPostHash=0xcbf29ce484222325L; int carvedPosts=0;
                    var post=proto.getPostProcessing();
                    for(int section=0;section<post.length;section++) if(post[section]!=null)
                        for(short value:post[section]) { carvedPosts++; carvedPostHash=(carvedPostHash ^ section)*0x100000001b3L; carvedPostHash=(carvedPostHash ^ (value & 65535))*0x100000001b3L; }
                    out.printf("a2 %d %d %016x %016x %d %016x%n",cx,cz,carvedHash,carvedMaskHash,carvedPosts,carvedPostHash);
                    var retainedNoise = chunk;
                    proto.getOrCreateNoiseChunk(ignored -> retainedNoise);
                    var region = TestRegion.create(registryAccess, heightAccessor, factory, proto, seed);
                    generator.applyCarvers(region, seed, random, biomeManager, null, proto);
                    long autoHash=0xcbf29ce484222325L, autoMaskHash=0xcbf29ce484222325L, autoPostHash=0xcbf29ce484222325L;
                    for (int x=0;x<16;x++) for(int z=0;z<16;z++) for(int y=-64;y<320;y++)
                        autoHash=(autoHash ^ Integer.toUnsignedLong(Block.getId(proto.getBlockState(blockPos.set(cx*16+x,y,cz*16+z))))) * 0x100000001b3L;
                    for(long word:proto.getOrCreateCarvingMask().toArray()) autoMaskHash=(autoMaskHash ^ word)*0x100000001b3L;
                    int autoPosts=0; post=proto.getPostProcessing();
                    for(int section=0;section<post.length;section++) if(post[section]!=null)
                        for(short value:post[section]) { autoPosts++; autoPostHash=(autoPostHash ^ section)*0x100000001b3L; autoPostHash=(autoPostHash ^ (value & 65535))*0x100000001b3L; }
                    out.printf("a3 %d %d %016x %016x %d %016x%n",cx,cz,autoHash,autoMaskHash,autoPosts,autoPostHash);
                    Heightmap.primeHeightmaps(proto, java.util.EnumSet.allOf(Heightmap.Types.class));
                    for (var kind : Heightmap.Types.values()) {
                        long hashAfterCarving=0xcbf29ce484222325L;
                        for (long word : proto.getOrCreateHeightmapUnprimed(kind).getRawData())
                            hashAfterCarving=(hashAfterCarving ^ word)*0x100000001b3L;
                        out.printf("h2 %d %d %s %016x%n",cx,cz,kind.name(),hashAfterCarving);
                    }
            }
        }
    }
}
