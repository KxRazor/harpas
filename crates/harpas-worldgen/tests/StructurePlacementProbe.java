// Test driver only: loads the owner-provided server's real registries and writes where structures may start.
//
// Arguments: <output> <seed> <radius in chunks> <window center x,z>...
// Writes the possible structure sets, the ring positions, and each chunk around the origin and in small windows
// around the centers where a set's placement says a structure may start.
import java.io.PrintWriter;
import java.util.Comparator;
import java.util.Locale;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.Registries;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.WorldLoader;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.util.Util;
import net.minecraft.world.level.WorldDataConfiguration;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.presets.WorldPresets;
import net.minecraft.world.level.levelgen.structure.placement.ConcentricRingsStructurePlacement;

public final class StructurePlacementProbe {
    static final int WINDOW = 64;

    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        long seed = Long.parseLong(args[1]);
        int radius = Integer.parseInt(args[2]);
        RegistryAccess.Frozen registries = loadRegistries();

        var generator = WorldPresets.getNormalOverworld(registries).generator();
        var random = RandomState.create(registries, NoiseGeneratorSettings.OVERWORLD, seed);
        var state = generator.createState(registries.lookupOrThrow(Registries.STRUCTURE_SET), random, seed);
        state.ensureStructuresGenerated();
        var sets = state.possibleStructureSets().stream()
            .sorted(Comparator.comparing(set -> set.unwrapKey().orElseThrow().identifier().toString())).toList();

        try (var out = new PrintWriter(args[0])) {
            for (var set : sets) {
                var name = set.unwrapKey().orElseThrow().identifier();
                out.println("set " + name);
                var placement = set.value().placement();
                if (placement instanceof ConcentricRingsStructurePlacement rings) {
                    for (var position : state.getRingPositionsFor(rings)) {
                        out.println("ring " + name + " " + position.x() + " " + position.z());
                    }
                }
                writeStarts(out, name.toString(), state, placement, 0, 0, radius);
                for (int index = 3; index < args.length; index++) {
                    var center = args[index].split(",");
                    writeStarts(out, name.toString(), state, placement,
                        Integer.parseInt(center[0]), Integer.parseInt(center[1]), WINDOW);
                }
            }
        }
        Util.shutdownExecutors();
    }

    /** The registries a dedicated server loads from the vanilla data pack, with their tags. */
    static RegistryAccess.Frozen loadRegistries() throws Exception {
        var server = loadServer();
        server.resources().close();
        return server.registries();
    }

    /** The registries and the open resources of the vanilla data pack, which structure templates are read from. */
    record Server(RegistryAccess.Frozen registries, net.minecraft.server.packs.resources.CloseableResourceManager resources) {
    }

    static Server loadServer() throws Exception {
        var packs = new WorldLoader.PackConfig(
            ServerPacksSource.createVanillaTrustedRepository(), WorldDataConfiguration.DEFAULT, false, false);
        var init = new WorldLoader.InitConfig(packs, Commands.CommandSelection.DEDICATED,
            net.minecraft.server.permissions.LevelBasedPermissionSet.OWNER);
        return WorldLoader.<Void, Server>load(init,
            context -> new WorldLoader.DataLoadOutput<>(null, context.datapackDimensions()),
            (resources, reloadable, layers, cookie) -> new Server(layers.compositeAccess(), resources),
            Util.backgroundExecutor(), Runnable::run).get();
    }

    static void writeStarts(PrintWriter out, String name, net.minecraft.world.level.chunk.ChunkGeneratorStructureState state,
            net.minecraft.world.level.levelgen.structure.placement.StructurePlacement placement,
            int centerX, int centerZ, int radius) {
        for (int x = centerX - radius; x < centerX + radius; x++) {
            for (int z = centerZ - radius; z < centerZ + radius; z++) {
                if (placement.isStructureChunk(state, x, z)) out.println("start " + name + " " + x + " " + z);
            }
        }
    }
}
