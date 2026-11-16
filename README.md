# Harpas

[![CI](https://github.com/KxRazor/harpas/actions/workflows/ci.yml/badge.svg)](https://github.com/KxRazor/harpas/actions/workflows/ci.yml)

A Minecraft server written in Rust, for Java and Bedrock players on the same world.

- **Vanilla first:** vanilla is the specification. Behavior is ported one to one and checked against Mojang's
  server in CI.
- **Both editions, natively:** Bedrock players connect directly, without a proxy or anything installed.
- **Plugins:** Bukkit and Paper plugins run unmodified on an embedded JVM, and native plugins run on a WASM API
  in any language.
- **Drop-in:** server folders from vanilla and Paper start without editing configuration.

## Status

**Harpas is in early development.** Release [0.2.0](https://github.com/KxRazor/harpas/releases) opens vanilla
26.1 worlds and generates new ones as vanilla does for the same seed, and Java Edition 26.1 clients explore them,
see each other and chat. There is no gameplay yet, such as breaking blocks, mobs or items, and no Bedrock support.
Progress is tracked in [ROADMAP.md](ROADMAP.md).

Release binaries for Linux, Windows and macOS are attached to each
[release](https://github.com/KxRazor/harpas/releases).
Run `harpas` from the server folder; it reads `server.properties` like vanilla.

## Building

Harpas builds with the Rust version in [rust-toolchain.toml](rust-toolchain.toml), which
[rustup](https://rustup.rs) installs on the first build.

```sh
cargo build --release
cargo run --release --bin harpas
```

## Documentation

- [docs/](docs/README.md): one design document per subsystem, with its goals, trade-offs and sources.
- [ROADMAP.md](ROADMAP.md): milestones in the order they are built.
- [CONTRIBUTING.md](CONTRIBUTING.md): how to contribute.
- [SECURITY.md](SECURITY.md): how to report a vulnerability.

Questions and ideas are welcome in [Discussions](https://github.com/KxRazor/harpas/discussions).

## License

Harpas is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. The
Bukkit compatibility shim is a separate artifact licensed under GPL-3.0; see
[docs/licensing.md](docs/licensing.md). Third-party material is listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Harpas is not an official Minecraft product and is not approved by or associated with Mojang or Microsoft.

## Special thanks

Harpas' design was inspired by these projects:

- [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin)
- [GeyserMC](https://github.com/GeyserMC)
- [Polymer](https://github.com/Patbox/polymer)
- [PaperMC](https://github.com/PaperMC)
- [PolyMc](https://github.com/TheEpicBlock/PolyMc)
- [PrismarineJS/minecraft-data](https://github.com/PrismarineJS/minecraft-data)
- [gophertunnel](https://github.com/Sandertv/gophertunnel) and [go-nethernet](https://github.com/df-mc/go-nethernet)
- [CloudburstMC/Protocol](https://github.com/CloudburstMC/Protocol)
- [GeckoLib](https://github.com/bernie-g/geckolib)
