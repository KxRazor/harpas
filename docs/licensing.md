# Licensing

Scope: the licenses of Harpas' own code, the one component that needs a different license, and how
contributions are accepted. Third-party material is listed in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

Status: decided for Harpas' code. Protocol and registry data come only from permissively licensed community
sources, as described in [protocol-codecs.md](protocol-codecs.md). This document is not legal
advice: it records the project's decisions, the risks it knowingly accepts, and how it mitigates them.

---

## Goals

- Anyone, including paid hosting providers and companies, can use, modify and offer Harpas as a service without
  conditions beyond keeping the copyright notices.
- Plugin authors can license their plugins however they want, including as closed source.
- Contributors and adopters are covered by an explicit patent grant.

---

## Licenses by component

| Component | License |
|---|---|
| Harpas core: engine, networking, Bedrock interoperability, WASM host, the Rust side of the JVM bridge | **MIT OR Apache-2.0** |
| Plugin API (WIT definitions) and the SDKs for each language | **MIT OR Apache-2.0** |
| Harpas companion mod and Harpas client mod | **MIT OR Apache-2.0** |
| Bukkit compatibility shim (`java/harpas-bukkit`) | **GPL-3.0** |

### MIT OR Apache-2.0

The license files are [LICENSE-MIT](../LICENSE-MIT) and [LICENSE-APACHE](../LICENSE-APACHE) at the repository
root. Users may choose either license. This is the usual licensing of the Rust ecosystem.

- **Apache-2.0** includes an explicit patent grant.
- **MIT** keeps the code compatible with the widest range of other licenses, including GPLv2.

Each crate declares `license = "MIT OR Apache-2.0"` in its `Cargo.toml`.

### GPL-3.0 for the Bukkit shim

The Bukkit and Paper APIs are licensed under GPLv3, inherited from Bukkit and CraftBukkit. The shim implements
those interfaces, so it is treated as a work based on them and licensed GPL-3.0.

To keep the rest of Harpas permissive:

- The shim lives in its own directory with its own `LICENSE` file (GPL-3.0), and declares GPL-3.0 in its build
  metadata.
- It is a separate artifact (a Java library), not compiled into the Harpas binary.
- It is provisioned on demand together with the JVM runtime, only when Bukkit or Paper plugins are present
  ([jvm.md](jvm.md#runtime-provisioning)).
- It talks to the core through Harpas' own interface (shared memory and FFM calls defined by Harpas), not by
  linking the core into it.

When Harpas distributes the shim, it complies with GPL-3.0, including offering its source code.

---

## Mixing licenses in one repository

A root license with a different license for one component is common and valid, provided:

1. The component's license is stated clearly where it lives (its own `LICENSE` file and build metadata).
2. The components are distributed as separate works. Combining GPL-3.0 code into the same binary as the core
   would make that combined binary subject to GPL-3.0; keeping the shim a separate artifact avoids that.
3. License compatibility runs the right way: GPL-3.0 code may use MIT or Apache-2.0 code, but MIT or Apache-2.0
   code must not include GPL-3.0 code.

Source files carry no license headers: the license files and the build metadata of each component state its
license.

---

## Contributions

Contributions are licensed under the license of the component they touch, with no separate sign-off or
contributor agreement: a contribution to the core is licensed MIT OR Apache-2.0, and a contribution to the
Bukkit shim is licensed GPL-3.0. Apache-2.0 states this rule for contributions explicitly (section 5).

---

## Accepted risks

| Risk | Mitigation |
|---|---|
| The shim could be considered a work based on the GPLv3 Bukkit/Paper APIs | The shim is licensed GPL-3.0 and kept a separate artifact, so the obligation is met where it applies |
| The separation between the shim and the core could be challenged | No linking between them: a separate Java artifact, provisioned on demand, talking through Harpas' own interface |
| Protocol and registry data come from the community | Only permissively licensed community data (minecraft-data) is committed; Mojang's files and code never enter the project |
| Vanilla game data (world generation, recipes, loot, generated registry IDs) is Mojang's | Extracted or generated on the owner's machine from Mojang's official jar, never redistributed; the server itself is used only as a test reference |

## Open questions

1. Exact GPL-3.0 variant for the shim: `GPL-3.0-only` or `GPL-3.0-or-later`.
