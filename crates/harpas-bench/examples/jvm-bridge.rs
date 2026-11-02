//! M1 embedded JDK 25 bridge prototype. See `jvm-bridge/README.md` beside this file.

use std::cell::UnsafeCell;
use std::error::Error;
use std::hint::black_box;
use std::mem::{offset_of, size_of};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use harpas_bench::measure::time;
use jni::objects::{JClass, JStaticMethodID};
use jni::signature::{Primitive, ReturnType};
use jni::sys::{jdouble, jlong, jvalue};
use jni::{Env, EnvUnowned, InitArgsBuilder, JavaVM, NativeMethod, jni_sig, jni_str};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Upcall = unsafe extern "C" fn(i64) -> i64;
const PLAYERS: usize = 200;
const TICK: Duration = Duration::from_millis(50);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Location {
    x: f64,
    y: f64,
    z: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Event {
    player: i32,
    cancelled: i32,
    to: Location,
}

#[repr(C)]
struct Shared {
    players: [Location; PLAYERS],
    event: Event,
}

// These leaf functions neither block, allocate nor call Java; only read_x is linked as critical.
unsafe extern "C" fn read_x(player: *const Location) -> f64 {
    // SAFETY: Java passes a live, aligned player slot in the process-lifetime Shared allocation.
    unsafe { (*player).x }
}

unsafe extern "C" fn teleport(player: *mut Location, x: f64, y: f64, z: f64) {
    // SAFETY: same allocation as read_x, accessed exclusively on the attached logic thread.
    unsafe { player.write(Location { x, y, z }) };
}

extern "system" fn jni_read_x(_: EnvUnowned, _: JClass, address: jlong) -> jdouble {
    // SAFETY: the fixture only passes addresses of live player slots.
    unsafe { read_x(address as *const Location) }
}

extern "system" fn jni_teleport(
    _: EnvUnowned,
    _: JClass,
    address: jlong,
    x: jdouble,
    y: jdouble,
    z: jdouble,
) {
    // SAFETY: the fixture only passes addresses of live player slots, on the logic thread.
    unsafe { teleport(address as *mut Location, x, y, z) };
}

fn call(env: &mut Env, class: &JClass, method: JStaticMethodID, arg: i64) -> Result<i64> {
    // SAFETY: all cached method IDs are looked up on this class with the signature (J)J.
    Ok(unsafe {
        env.call_static_method_unchecked(
            class,
            method,
            ReturnType::Primitive(Primitive::Long),
            &[jvalue { j: arg }],
        )?
        .j()?
    })
}

fn warmup(duration: Duration, mut f: impl FnMut()) {
    let start = Instant::now();
    while start.elapsed() < duration {
        f();
    }
}

fn reset(shared: *mut Shared) {
    for i in 0..PLAYERS {
        // SAFETY: no Java call is active; no reference to this UnsafeCell contents is retained.
        unsafe {
            (*shared).players[i] = Location {
                x: i as f64,
                y: 64.0,
                z: -(i as f64),
            };
        }
    }
}

fn tick(shared: *mut Shared, mut dispatch: impl FnMut() -> i64, verify: bool) {
    for i in 0..PLAYERS {
        // SAFETY: all accesses are serial on this thread; copies end before Java can mutate a slot.
        let before = unsafe { (*shared).players[i] };
        let proposed = Location {
            x: before.x + 0.5,
            ..before
        };
        // SAFETY: no Java call is active, and the slot is within Shared.
        unsafe {
            (*shared).event = Event {
                player: i as i32,
                cancelled: 0,
                to: proposed,
            };
        }
        assert_eq!(dispatch(), 0, "listener failed; see the Java exception");
        // SAFETY: Java has returned, so the event and entity view can be read and changed again.
        unsafe {
            let event = (*shared).event;
            if verify {
                assert_eq!(event.cancelled, i32::from(i % 8 == 0));
                assert_eq!(event.to.x, proposed.x + 0.25);
                let expected = if i % 16 == 1 {
                    Location {
                        x: proposed.x + 1.0,
                        ..proposed
                    }
                } else {
                    before
                };
                assert_eq!((*shared).players[i], expected, "immediate teleport");
            }
            if event.cancelled == 0 {
                (*shared).players[i] = event.to;
            }
            black_box((*shared).players[i]);
        }
    }
}

fn paced_ticks(ticks: usize, mut run: impl FnMut()) {
    let mut samples = Vec::with_capacity(ticks);
    let start = Instant::now();
    let mut overruns = 0;
    for i in 0..ticks {
        let deadline = start + TICK.mul_f64((i + 1) as f64);
        let begin = Instant::now();
        run();
        samples.push(begin.elapsed().as_secs_f64() * 1e6);
        if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(remaining);
        } else {
            overruns += 1;
        }
    }
    let mean = samples.iter().sum::<f64>() / ticks as f64;
    samples.sort_by(f64::total_cmp);
    println!(
        "{mean:.2} | {:.2} | {overruns} |",
        samples[(ticks * 99).div_ceil(100) - 1]
    );
}

fn rss() {
    // Linux reports the entire embedded process, not just the Java heap. Other OSes use external tools.
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status
            .lines()
            .filter(|s| s.starts_with("VmRSS:") || s.starts_with("VmHWM:"))
        {
            println!("{line}");
        }
    }
}

fn fixture() -> Result<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let output = root.join("target/jvm-bridge");
    let jar = output.join("bridge.jar");
    let stamp = output.join("compiled-source.java");
    let source = include_str!("jvm-bridge/Bridge.java");
    // Keep the identical JAR (including its timestamp) across AOT training and production runs.
    if jar.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|old| old == source) {
        return Ok(jar);
    }
    let classes = output.join("classes");
    if classes.exists() {
        std::fs::remove_dir_all(&classes)?;
    }
    std::fs::create_dir_all(&classes)?;
    let java_source = output.join("Bridge.java");
    std::fs::write(&java_source, source)?;
    for (tool, args) in [
        (
            "javac",
            vec![
                "--release".into(),
                "25".into(),
                "-Xlint:all".into(),
                "-Werror".into(),
                "-d".into(),
                classes.clone().into_os_string(),
                java_source.into_os_string(),
            ],
        ),
        (
            "jar",
            vec![
                "--create".into(),
                "--file".into(),
                jar.clone().into_os_string(),
                "-C".into(),
                classes.into_os_string(),
                ".".into(),
            ],
        ),
    ] {
        let executable = std::env::var_os("JAVA_HOME")
            .map(|home| PathBuf::from(home).join("bin").join(tool))
            .unwrap_or_else(|| PathBuf::from(tool));
        if !Command::new(executable).args(args).status()?.success() {
            return Err(format!("{tool} failed; use JAVA_HOME pointing to a JDK 25").into());
        }
    }
    std::fs::write(stamp, source)?;
    Ok(jar)
}

fn main() -> Result<()> {
    let mut ticks = 200usize;
    let mut warmup_ms = 2000u64;
    let mut self_test = false;
    let mut options = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ticks" => ticks = args.next().ok_or("--ticks needs a value")?.parse()?,
            "--warmup-ms" => warmup_ms = args.next().ok_or("--warmup-ms needs a value")?.parse()?,
            "--self-test" => self_test = true,
            "--" => {
                options.extend(args);
                break;
            }
            _ => {
                return Err(
                    format!("unknown argument: {arg}; see examples/jvm-bridge/README.md").into(),
                );
            }
        }
    }
    if ticks == 0 || warmup_ms == 0 {
        return Err("ticks and warmup-ms must be positive".into());
    }
    assert_eq!(size_of::<Location>(), 24);
    assert_eq!(offset_of!(Shared, event), 4800);
    assert_eq!(size_of::<Shared>(), 4832);
    let jar = fixture()?;
    // The JVM and its fixture live until process exit. UnsafeCell permits serial Java writes through
    // raw pointers without retaining Rust references across calls. Never share this pointer with workers.
    let shared = Box::leak(Box::new(UnsafeCell::new(Shared {
        players: [Location {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        }; PLAYERS],
        event: Event {
            player: 0,
            cancelled: 0,
            to: Location {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
        },
    })))
    .get();
    reset(shared);
    let mut builder = InitArgsBuilder::new()
        .option(format!("-Djava.class.path={}", jar.display()))
        .option("--enable-native-access=ALL-UNNAMED")
        .option("-Xrs")
        .option("-Xms64m")
        .option("-Xmx256m")
        .option(format!("-Dharpas.bench.memory={}", shared as usize))
        .option(format!(
            "-Dharpas.bench.read={}",
            read_x as *const () as usize
        ))
        .option(format!(
            "-Dharpas.bench.teleport={}",
            teleport as *const () as usize
        ));
    for option in &options {
        builder = builder.option(option.clone());
    }
    let start = Instant::now();
    let vm = JavaVM::new(builder.build()?)?;
    println!(
        "JVM creation: {:.1} ms; extra options: {options:?}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    vm.attach_current_thread(|env| benchmark(env, shared, self_test, ticks, warmup_ms))?;
    vm.detach_current_thread()?;
    // SAFETY: the attachment closure and all its JNI references are gone; this fixture never spawns
    // native workers or attaches daemon threads. No native code will call the VM after destruction.
    // Orderly shutdown also flushes unified GC logs and permits AOT training to write its cache.
    unsafe { vm.destroy()? };
    Ok(())
}

fn benchmark(
    env: &mut Env,
    shared: *mut Shared,
    self_test: bool,
    ticks: usize,
    warmup_ms: u64,
) -> Result<()> {
    let class = env.load_class(jni_str!("Bridge"))?;
    // SAFETY: the two functions match the fixture's static native declarations exactly.
    unsafe {
        env.register_native_methods(
            &class,
            &[
                NativeMethod::from_raw_parts(
                    jni_str!("readX"),
                    jni_str!("(J)D"),
                    jni_read_x as *mut _,
                ),
                NativeMethod::from_raw_parts(
                    jni_str!("teleport"),
                    jni_str!("(JDDD)V"),
                    jni_teleport as *mut _,
                ),
            ],
        )?;
    }
    let control = env.get_static_method_id(&class, jni_str!("control"), jni_sig!("(J)J"))?;
    let ping = env.get_static_method_id(&class, jni_str!("ping"), jni_sig!("(J)J"))?;
    let dispatch = env.get_static_method_id(&class, jni_str!("dispatch"), jni_sig!("(J)J"))?;
    let micro = env.get_static_method_id(&class, jni_str!("micro"), jni_sig!("(J)J"))?;
    call(env, &class, control, 0)?;
    // SAFETY: control returns live global-arena stubs with C ABI int64_t(int64_t).
    let (ffm_ping, ffm_dispatch): (Upcall, Upcall) = unsafe {
        (
            std::mem::transmute::<usize, Upcall>(call(env, &class, control, 1)? as usize),
            std::mem::transmute::<usize, Upcall>(call(env, &class, control, 2)? as usize),
        )
    };
    // SAFETY: the stub is live and we are on the JVM-attached thread.
    assert_eq!(unsafe { ffm_ping(41) }, 42);
    assert_eq!(call(env, &class, ping, 41)?, 42);
    for ffm in [false, true] {
        reset(shared);
        tick(
            shared,
            || {
                if ffm {
                    // SAFETY: same live stub and attached thread as above.
                    unsafe { ffm_dispatch(0) }
                } else {
                    call(env, &class, dispatch, 0).expect("JNI dispatch")
                }
            },
            true,
        );
        call(env, &class, control, 5)?; // Throw inside a listener, caught at either boundary.
        assert_eq!(
            if ffm {
                unsafe { ffm_dispatch(0) }
            } else {
                call(env, &class, dispatch, 0)?
            },
            -1
        );
        assert_eq!(call(env, &class, control, 6)?, 1);
    }
    println!(
        "Self-test passed: native reads/writes, both dispatch paths, cancellation, mutation, immediate writes, exceptions."
    );
    if self_test {
        return Ok(());
    }
    call(env, &class, control, 3)?;
    println!(
        "\nWarmup: {warmup_ms} ms per case; {ticks} measured ticks per dispatch path, 200 players at 20 TPS."
    );
    let warm = Duration::from_millis(warmup_ms);
    println!("\n| Java loop (100,000 operations/batch; median of 7 batches) | ns/op |\n|---|---:|");
    for (mode, name) in [
        "loop baseline",
        "shared x read",
        "FFM plain x read",
        "FFM critical x read",
        "JNI x read",
        "shared getLocation (xyz)",
    ]
    .iter()
    .enumerate()
    {
        let arg = ((mode as i64) << 32) | 100_000;
        warmup(warm, || {
            black_box(call(env, &class, micro, arg).expect("Java microbenchmark"));
        });
        let mut samples = (0..7)
            .map(|_| call(env, &class, micro, arg).map(|ns| ns as f64 / 100_000.0))
            .collect::<Result<Vec<_>>>()?;
        samples.sort_by(f64::total_cmp);
        println!("| {name} | {:.2} |", samples[3]);
    }
    println!("\n| Rust → Java ping | ns/call |\n|---|---:|");
    for ffm in [false, true] {
        let mut ping_call = || {
            // SAFETY: live global stub, same attached thread; no Java exception can escape ping.
            black_box(if ffm {
                unsafe { ffm_ping(black_box(41)) }
            } else {
                call(env, &class, ping, black_box(41)).expect("JNI ping")
            });
        };
        warmup(warm, &mut ping_call);
        println!(
            "| {} | {:.2} |",
            if ffm {
                "FFM upcall"
            } else {
                "cached JNI (including exception check)"
            },
            time(ping_call)
        );
    }
    println!(
        "\n| 200 synchronous events/tick, 3 listeners | mean µs/tick | p99 µs/tick | deadline overruns |\n|---|---:|---:|---:|"
    );
    for ffm in [false, true] {
        reset(shared);
        let mut run = || {
            tick(
                shared,
                || {
                    if ffm {
                        // SAFETY: live stub, same attached thread; dispatch catches all listener exceptions.
                        unsafe { ffm_dispatch(0) }
                    } else {
                        call(env, &class, dispatch, 0).expect("JNI dispatch")
                    }
                },
                false,
            )
        };
        warmup(warm, &mut run);
        print!("| {} | ", if ffm { "FFM upcall" } else { "cached JNI" });
        paced_ticks(ticks, run);
    }
    call(env, &class, control, 4)?;
    rss();
    Ok(())
}
