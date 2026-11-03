// Test driver only: loads the owner-provided server's structure templates and template pools with its own code and
// writes what laying out jigsaw structures uses.
//
// Arguments: <output> <file of template ids, one per line>
// Writes each template's size, and its bounding box and jigsaw blocks for every rotation at two positions, then
// each template pool's fallback, elements and a shuffled order.
import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.Locale;
import java.util.stream.Collectors;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.nbt.NbtAccounter;
import net.minecraft.nbt.NbtIo;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.JigsawBlock;
import net.minecraft.world.level.block.Rotation;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.structure.pools.EmptyPoolElement;
import net.minecraft.world.level.levelgen.structure.pools.FeaturePoolElement;
import net.minecraft.world.level.levelgen.structure.pools.ListPoolElement;
import net.minecraft.world.level.levelgen.structure.pools.SinglePoolElement;
import net.minecraft.world.level.levelgen.structure.pools.StructurePoolElement;
import net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplate;

public final class TemplateProbe {
    static final BlockPos[] POSITIONS = {new BlockPos(10, 64, -7), new BlockPos(1234, -20, 5678)};

    public static void main(String[] args) throws Exception {
        Locale.setDefault(Locale.ROOT);
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (var out = new PrintWriter(args[0])) {
            for (var id : Files.readAllLines(Path.of(args[1]))) {
                var parts = id.split(":", 2);
                var path = "/data/" + parts[0] + "/structure/" + parts[1] + ".nbt";
                StructureTemplate template = new StructureTemplate();
                try (var stream = TemplateProbe.class.getResourceAsStream(path)) {
                    template.load(BuiltInRegistries.BLOCK, NbtIo.readCompressed(stream, NbtAccounter.unlimitedHeap()));
                }
                var size = template.getSize();
                out.println("template " + id + " " + size.getX() + " " + size.getY() + " " + size.getZ());
                for (var position : POSITIONS) {
                    var palettes = (java.util.List<?>) field(template, StructureTemplate.class, "palettes");
                    if (palettes.size() > 1) {
                        var chosen = new net.minecraft.world.level.levelgen.structure.templatesystem.StructurePlaceSettings()
                            .getRandomPalette((java.util.List) palettes, position);
                        out.println("palette " + palettes.indexOf(chosen) + " of " + palettes.size());
                    }
                    for (var rotation : Rotation.values()) {
                        var box = template.getBoundingBox(position, rotation, BlockPos.ZERO, net.minecraft.world.level.block.Mirror.NONE);
                        out.println("box " + rotation.ordinal() + " " + box.minX() + " " + box.minY() + " " + box.minZ() + " "
                            + box.maxX() + " " + box.maxY() + " " + box.maxZ());
                        for (var jigsaw : template.getJigsaws(position, rotation)) {
                            var pos = jigsaw.info().pos();
                            out.println("jigsaw " + pos.getX() + " " + pos.getY() + " " + pos.getZ() + " "
                                + jigsaw.info().state().getValue(JigsawBlock.ORIENTATION).getSerializedName() + " "
                                + jigsaw.name() + " " + jigsaw.pool().identifier() + " " + jigsaw.target() + " "
                                + jigsaw.jointType().getSerializedName() + " " + jigsaw.placementPriority() + " "
                                + jigsaw.selectionPriority());
                        }
                    }
                }
            }
            var registries = StructurePlacementProbe.loadRegistries();
            var pools = registries.lookupOrThrow(Registries.TEMPLATE_POOL);
            var entries = new ArrayList<>(pools.listElements().toList());
            entries.sort(Comparator.comparing(holder -> holder.key().identifier().toString()));
            for (var holder : entries) {
                var pool = holder.value();
                out.println("pool " + holder.key().identifier() + " " + key(pool.getFallback()) + " " + pool.size());
                for (var entry : pool.getTemplates()) out.println("element " + entry.getSecond() + " " + describe(entry.getFirst()));
                out.println("shuffled " + pool.getShuffledTemplates(new LegacyRandomSource(7)).stream()
                    .map(TemplateProbe::describe).collect(Collectors.joining(" | ")));
            }
        }
    }

    static String key(Holder<?> holder) {
        return holder.unwrapKey().map(key -> key.identifier().toString()).orElse("inline");
    }

    static Object field(Object owner, Class<?> type, String name) throws ReflectiveOperationException {
        var field = type.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(owner);
    }

    static String describe(StructurePoolElement element) {
        try {
            var type = BuiltInRegistries.STRUCTURE_POOL_ELEMENT.getKey(element.getType());
            var projection = element.getProjection().getSerializedName();
            if (element instanceof SinglePoolElement single) {
                var template = (com.mojang.datafixers.util.Either<?, ?>) field(single, SinglePoolElement.class, "template");
                var processors = (Holder<?>) field(single, SinglePoolElement.class, "processors");
                return type + " " + template.left().orElseThrow() + " " + key(processors) + " " + projection;
            } else if (element instanceof FeaturePoolElement feature) {
                return type + " " + key((Holder<?>) field(feature, FeaturePoolElement.class, "feature")) + " " + projection;
            } else if (element instanceof ListPoolElement list) {
                @SuppressWarnings("unchecked")
                var elements = (java.util.List<StructurePoolElement>) field(list, ListPoolElement.class, "elements");
                return type + " " + projection + " ["
                    + elements.stream().map(TemplateProbe::describe).collect(Collectors.joining(", ")) + "]";
            } else if (element == EmptyPoolElement.INSTANCE) {
                return type.toString();
            }
            throw new IllegalStateException("unknown element " + element);
        } catch (ReflectiveOperationException error) {
            throw new IllegalStateException(error);
        }
    }
}
