# Fuzzing

Fuzz targets for the decoders that read bytes Harpas does not control: packets and connections from the
network, and world files. They run with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) and libFuzzer.

A target fails when the code panics, overflows in arithmetic, runs out of memory or takes too long on an
input, or when a check in the target does not hold, such as a value that reads but does not write back the same.

| Target | What it reads |
|---|---|
| `nbt` | NBT as packets and as files carry it (`harpas-core`) |
| `java_packets` | The serverbound packets of every connection state (`harpas-protocol-java`) |
| `java_connection` | A whole offline-mode connection: legacy ping, framing, states and compression (`harpas-java`) |
| `region` | A region file and each of its chunks (`harpas-anvil`) |
| `nethernet_signaling` | The HTTP requests of the signaling endpoint (`harpas-nethernet`) |
| `nethernet_segments` | Segment framing and reassembly (`harpas-nethernet`) |

## Running

This folder is its own workspace, because libFuzzer needs a nightly toolchain and the server builds on stable.

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run nbt -- -max_total_time=60
cargo +nightly fuzz run region corpus/region seeds/region    # with the seeds in seeds/
```

- `corpus/<target>` keeps the inputs the fuzzer found interesting; it is not committed.
- `seeds/<target>` holds committed inputs to start from, for formats the fuzzer does not find alone.
- An input that fails is written to `artifacts/<target>`; `cargo +nightly fuzz run <target> <file>` runs it again.

When a target fails, fix the code, and add the input as a test of the crate, so the stable test suite keeps
it.

## In CI

The [Fuzz workflow](../.github/workflows/fuzz.yml) runs every target for two minutes each night on `main`, and on
pull requests that change this folder. It keeps the corpus between runs.

## Not covered yet

- Packets after an online-mode login, which need a key exchange the fuzzer cannot complete.
- Bedrock batches and packets, which have no decoder yet.
- The parsing of chunks into blocks (`Chunk::read`), `level.dat` and player data.
