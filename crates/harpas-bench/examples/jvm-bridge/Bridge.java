// Synthetic M1 fixture, MIT OR Apache-2.0. No Bukkit/Paper classes or game code.
import java.lang.foreign.*;
import java.lang.invoke.*;
import java.lang.management.*;
import static java.lang.foreign.ValueLayout.*;

// Addresses, layouts and lifetimes are supplied by the single-threaded Rust host, not plugin input.
@SuppressWarnings("restricted")
public final class Bridge {
    private static final int PLAYERS = 200;
    private static final int STRIDE = 24;
    private static final long EVENT = PLAYERS * STRIDE;
    private static final MemorySegment MEMORY = MemorySegment.ofAddress(
        Long.parseLong(System.getProperty("harpas.bench.memory"))).reinterpret(EVENT + 32);
    private static final Linker LINKER = Linker.nativeLinker();
    private static final MethodHandle READ = readHandle(false);
    private static final MethodHandle CRITICAL_READ = readHandle(true);
    private static final MethodHandle TELEPORT = LINKER.downcallHandle(address("teleport"),
        FunctionDescriptor.ofVoid(ADDRESS, JAVA_DOUBLE, JAVA_DOUBLE, JAVA_DOUBLE));
    private static final Player[] VIEWS = new Player[PLAYERS];
    private static final Thread LOGIC_THREAD = Thread.currentThread();
    private static Throwable failure;
    private static boolean throwInListener;
    private static volatile double sink;

    private static native double readX(long address);
    private static native void teleport(long address, double x, double y, double z);

    private static MemorySegment address(String name) {
        return MemorySegment.ofAddress(Long.parseLong(System.getProperty("harpas.bench." + name)));
    }

    private static MethodHandle readHandle(boolean critical) {
        return LINKER.downcallHandle(address("read"), FunctionDescriptor.of(JAVA_DOUBLE, ADDRESS),
            critical ? new Linker.Option[] { Linker.Option.critical(false) } : new Linker.Option[0]);
    }

    private record Location(double x, double y, double z) {}

    private static final class Player {
        final int id;
        final MemorySegment memory;
        Player(int id) { this.id = id; this.memory = MEMORY.asSlice((long) id * STRIDE, STRIDE); }
        Location getLocation() {
            return new Location(memory.get(JAVA_DOUBLE, 0), memory.get(JAVA_DOUBLE, 8),
                memory.get(JAVA_DOUBLE, 16));
        }
        void teleport(Location to) throws Throwable {
            // Deliberately plain: a real teleport can fire a reentrant event, so it cannot be critical.
            TELEPORT.invokeExact(memory, to.x, to.y, to.z);
        }
    }

    private static final class PlayerMoveEvent {
        final Player player;
        Location to;
        boolean cancelled;
        PlayerMoveEvent(Player player, Location to) { this.player = player; this.to = to; }
    }

    @FunctionalInterface
    private interface Listener { void handle(PlayerMoveEvent event) throws Throwable; }

    // Fixed registration order stands in for three generated event executors, not reflection.
    private static final Listener[] LISTENERS = {
        event -> {
            if (throwInListener) throw new IllegalStateException("test listener failure");
            event.cancelled = event.player.id % 8 == 0;
        },
        event -> event.to = new Location(event.to.x + 0.25, event.to.y, event.to.z),
        event -> {
            Location before = event.player.getLocation();
            if (event.player.id % 16 == 1) {
                Location destination = new Location(event.to.x + 0.75, event.to.y, event.to.z);
                event.player.teleport(destination);
                if (!event.player.getLocation().equals(destination))
                    throw new AssertionError("teleport not immediately visible");
            }
            sink = before.x + before.y + before.z;
        }
    };

    public static long ping(long value) { return value + 1; }

    public static long dispatch(long unused) {
        // No Throwable may escape an FFM upcall. Report it through the result and retain the cause.
        try {
            if (Thread.currentThread() != LOGIC_THREAD) throw new AssertionError("wrong logic thread");
            PlayerMoveEvent event = new PlayerMoveEvent(VIEWS[MEMORY.get(JAVA_INT, EVENT)],
                new Location(MEMORY.get(JAVA_DOUBLE, EVENT + 8),
                    MEMORY.get(JAVA_DOUBLE, EVENT + 16), MEMORY.get(JAVA_DOUBLE, EVENT + 24)));
            for (Listener listener : LISTENERS) listener.handle(event);
            MEMORY.set(JAVA_INT, EVENT + 4, event.cancelled ? 1 : 0);
            MEMORY.set(JAVA_DOUBLE, EVENT + 8, event.to.x);
            MEMORY.set(JAVA_DOUBLE, EVENT + 16, event.to.y);
            MEMORY.set(JAVA_DOUBLE, EVENT + 24, event.to.z);
            return 0;
        } catch (Throwable caught) {
            failure = caught;
            if (!throwInListener) {
                try { caught.printStackTrace(); } catch (Throwable ignored) { /* Keep the upcall boundary closed. */ }
            }
            return -1;
        }
    }

    // One timed Java loop, with a constant handle at each call site. The returned time excludes JNI entry.
    public static long micro(long input) throws Throwable {
        int mode = (int) (input >>> 32);
        int calls = (int) input;
        double sum = 0;
        long start = System.nanoTime();
        switch (mode) {
            case 0 -> { for (int i = 0; i < calls; i++) sum += i % PLAYERS; }
            case 1 -> { for (int i = 0; i < calls; i++) sum += VIEWS[i % PLAYERS].memory.get(JAVA_DOUBLE, 0); }
            case 2 -> { for (int i = 0; i < calls; i++) sum += (double) READ.invokeExact(VIEWS[i % PLAYERS].memory); }
            case 3 -> { for (int i = 0; i < calls; i++) sum += (double) CRITICAL_READ.invokeExact(VIEWS[i % PLAYERS].memory); }
            case 4 -> { for (int i = 0; i < calls; i++) sum += readX(VIEWS[i % PLAYERS].memory.address()); }
            case 5 -> {
                for (int i = 0; i < calls; i++) {
                    Location location = VIEWS[i % PLAYERS].getLocation();
                    sum += location.x + location.y + location.z;
                }
            }
            default -> throw new IllegalArgumentException("unknown microbenchmark");
        }
        long elapsed = System.nanoTime() - start;
        sink = sum;
        if (!Double.isFinite(sum)) throw new AssertionError("non-finite result");
        return elapsed;
    }

    public static long control(long operation) throws Throwable {
        return switch ((int) operation) {
            case 0 -> {
                if (Runtime.version().feature() != 25) throw new IllegalStateException("use JDK 25");
                for (int i = 0; i < PLAYERS; i++) {
                    Player player = VIEWS[i] = new Player(i);
                    double expected = i;
                    if (player.getLocation().x != expected || readX(player.memory.address()) != expected
                        || (double) READ.invokeExact(player.memory) != expected
                        || (double) CRITICAL_READ.invokeExact(player.memory) != expected)
                        throw new AssertionError("native/shared read mismatch at player " + i);
                }
                Player player = VIEWS[0];
                teleport(player.memory.address(), 10, 20, 30);
                if (!player.getLocation().equals(new Location(10, 20, 30))) throw new AssertionError("JNI teleport");
                player.teleport(new Location(0, 64, 0));
                if (readX(player.memory.address()) != 0) throw new AssertionError("FFM teleport");
                yield 0;
            }
            case 1, 2 -> {
                String method = operation == 1 ? "ping" : "dispatch";
                yield LINKER.upcallStub(MethodHandles.lookup().findStatic(Bridge.class, method,
                    MethodType.methodType(long.class, long.class)),
                    FunctionDescriptor.of(JAVA_LONG, JAVA_LONG), Arena.global()).address();
            }
            case 3 -> {
                System.out.println(System.getProperty("java.vm.vendor") + " " + System.getProperty("java.runtime.version")
                    + "; " + System.getProperty("os.name") + " " + System.getProperty("os.arch"));
                System.out.println("VM arguments: " + ManagementFactory.getRuntimeMXBean().getInputArguments()
                    .stream().filter(arg -> !arg.startsWith("-Dharpas.bench.")).toList());
                System.out.println("Collectors: " + ManagementFactory.getGarbageCollectorMXBeans().stream()
                    .map(MemoryManagerMXBean::getName).toList());
                yield 0;
            }
            case 4 -> {
                if (failure != null) throw new AssertionError("listener failed", failure);
                for (GarbageCollectorMXBean gc : ManagementFactory.getGarbageCollectorMXBeans())
                    System.out.println("GC " + gc.getName() + ": " + gc.getCollectionCount()
                        + " collections, " + gc.getCollectionTime() + " ms collection time (whole run)");
                System.out.println("Heap used: " + ManagementFactory.getMemoryMXBean().getHeapMemoryUsage().getUsed()
                    + " bytes; compiler: " + ManagementFactory.getCompilationMXBean().getTotalCompilationTime() + " ms");
                yield 0;
            }
            case 5 -> { throwInListener = true; yield 0; }
            case 6 -> {
                if (!(failure instanceof IllegalStateException)) throw new AssertionError("exception boundary", failure);
                failure = null;
                throwInListener = false;
                yield 1;
            }
            default -> throw new IllegalArgumentException("unknown control operation");
        };
    }
}
