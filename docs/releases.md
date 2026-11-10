# Versioning and releases

Scope: how Harpas versions are numbered, what each release declares, which release channels exist, and the
steps to publish a release. The milestones that releases deliver are in [ROADMAP.md](../ROADMAP.md).

Status: decided for the project; applies from the first release.

---

## Version numbers

Harpas uses **Semantic Versioning**: `MAJOR.MINOR.PATCH`.

Harpas stays on `0.x` for now:

| Part | Meaning in `0.x` |
|---|---|
| **MAJOR** | Stays `0` |
| **MINOR** | A milestone, new features, support for a new Minecraft server version, or breaking changes |
| **PATCH** | Fixes, and Bedrock window updates that change nothing else |

What changes at 1.0 is not defined yet (see [1.0](#10)).

### Minecraft versions in the build metadata

Each version carries the main Minecraft versions it supports as SemVer **build metadata**, after a `+`, in the
a form other Minecraft server projects already use:

```
MAJOR.MINOR.PATCH+<java>-<bedrock>
0.4.0+26.1-26.60
0.4.0-beta.1+26.1-26.60
```

- **`<java>`** is the **server version** in normal mode ([versions.md](versions.md#normal-mode-rust-engine)):
  the game rules and worlds the release runs, not the newest translated client.
- **`<bedrock>`** is the **newest release in the Bedrock window** ([versions.md](versions.md#bedrock-window)).
- Versions use Mojang's current names ([versions.md](versions.md#version-names)).
- Build metadata does not affect version ordering in SemVer, so tools still compare `0.4.0` and `0.4.1` normally.
- The full ranges (translated Java clients, every Bedrock protocol in the window, modded mode lines) are in the
  release notes and in `harpas --version --verbose`.

### Not in the version number

- `MAJOR.MINOR.PATCH` itself never encodes Minecraft versions: one Harpas version supports several client
  versions ([versions.md](versions.md)), and they change with Bedrock's frequent releases.
- `harpas --version` prints the Harpas version and the supported Minecraft versions.

### What each release declares

Every release lists, in its release notes and in `harpas --version --verbose`:

| Item | Example |
|---|---|
| Server version (normal mode) | Java 26.1 |
| Java client versions | 26.1 (native), 26.2 and 26.3 (translated) |
| Bedrock window | Three protocol numbers |
| Modded mode versions | 26.x lines with companion mod builds |
| Native plugin API versions | `harpas:plugin@0.1` |
| Pinned JVM build | Temurin JDK 25 build per platform |
| Data revisions | minecraft-data and mapping revisions the release was built from |

### Other version numbers

- **Native plugin API:** WIT packages are versioned on their own (`harpas:plugin@MAJOR.MINOR`). A Harpas release
  hosts several API versions at once ([plugin-api.md](plugin-api.md)); when an API version may be retired is
  defined together with 1.0.
- **Bukkit shim, companion mod and Harpas client mod** share Harpas' version number and are released with it.

---

## Release channels

| Channel | What it is | Version form |
|---|---|---|
| **Nightly** | Automatic build of the main branch | `0.4.0-nightly.20261101+26.1-26.60` |
| **Beta** | Release candidate of a milestone, for testing | `0.4.0-beta.1+26.1-26.60` |
| **Stable** | Recommended for servers | `0.4.0+26.1-26.60` |

---

## Changelog

`CHANGELOG.md` at the repository root, in the "Keep a Changelog" format: an `Unreleased` section with `Added`,
`Changed`, `Fixed`, `Removed` and `Security` subsections, which a release moves under its version and date.

Every pull request that changes behavior adds its entry as a file in [changelog.d/](../changelog.d/README.md),
`<name>.<section>.md`, rather than editing `CHANGELOG.md`. Entries added to the same place in one file would
conflict between any two pull requests open at the same time; separate files never do. `ci/changelog.sh` moves
the files into `Unreleased`, newest first, and CI checks their names with `ci/changelog.sh --check`. The changes
since the last release are those in `Unreleased` and those still in `changelog.d/`.

---

## Release process

### Regular release (MINOR or MAJOR)

1. **Freeze:** create the release branch `release/<version>`; only fixes land on it.
2. **Beta:** publish `-beta.N` builds from the branch until no blocking issue remains.
3. **Checks, all required:**
   - build and tests on Linux, Windows and macOS (macOS runs nightly on `main`, so start the macOS workflow by hand
     on the release branch);
   - fuzzing smoke runs of every decoder;
   - parity harnesses against vanilla ([parity.md](parity.md));
   - plugin compatibility suite against Paper ([jvm.md](jvm.md#validation));
   - benchmarks, with no unexplained regression against the previous release.
4. **Documentation:** move the entries of `changelog.d/` into `Unreleased` with `ci/changelog.sh`, then
   `Unreleased` under the version; publish the parity report, regenerate
   `THIRD_PARTY_NOTICES.md`, and update the supported versions in the design documents.
5. **Tag:** a signed tag `v<version>`, including the build metadata (`v0.4.0+26.1-26.60`), on the release commit.
6. **Artifacts:** binaries per platform, the Bukkit shim (GPL-3.0, with its source), companion and client mod
   builds, the container image, SHA-256 checksums and their signature.
7. **Publish:** release notes with what the release declares (above), migration notes for breaking changes,
   and the changelog section.

### Patch release

Same checks, without the beta phase: a fix branch from the release tag, checks, tag, artifacts, notes.

### New Bedrock release (fast path)

Bedrock players who update first cannot join until their release is supported ([versions.md](versions.md#bedrock-window)),
so a new Bedrock protocol ships as a **PATCH** release as soon as possible:

1. Update the Bedrock schema from minecraft-data, contributing the update if it lags.
2. Regenerate the codecs; move the window (add the new protocol, drop the oldest) and update the Bedrock part of
   the build metadata.
3. Run the Bedrock tests and the gameplay parity scenarios with a client of the new release.
4. Tag and publish as a patch release.

### Security release

A fix for a vulnerability ships as a PATCH on the latest release, with a `Security` changelog entry
and a release note describing the impact.

---

## Support policy

- While Harpas is on `0.x`, only the latest stable release receives fixes, including security fixes.
- A longer support policy is defined together with 1.0.

---

## 1.0

Not defined yet. What 1.0 requires, what it changes in versioning and support, and when it comes will be decided
later, once the `0.x` milestones show where the project stands.

---

## Open questions

1. What 1.0 requires, and the versioning and support rules from then on.
2. Where releases are published beyond the repository's releases, which carry the binaries the Release
   workflow builds for Linux and Windows (macOS once its runners are reliable): container registry, website.
