# Harpas

A Minecraft server written in Rust for Java and Bedrock players. The design is in [docs/](docs/README.md), the
build order in [ROADMAP.md](ROADMAP.md), and contribution rules in [CONTRIBUTING.md](CONTRIBUTING.md).

## Crates

| Crate | Purpose |
|---|---|
| `harpas-core` | Shared types used by every crate |
| `harpas-engine` | Game engine: vanilla tick order on one logic thread |
| `harpas-protocol-java` | Java Edition packet codecs, generated from `schema/` at build time |
| `harpas-protocol-codegen` | Protocol schema loader, Rust code generator and minecraft-data check |
| `harpas-protocol-bedrock` | Bedrock Edition batching, compression, encryption and packet codecs |
| `harpas-raknet` | RakNet transport for Bedrock connections |
| `harpas-bedrock` | Translation between the engine and Bedrock packets |
| `harpas-jvm` | Embedded JVM bridge for Bukkit and Paper plugins |
| `harpas-server` | The `harpas` binary |
| `harpas-bench` | Benchmarks for the M1 risk prototypes, run with `cargo run --release -p harpas-bench --example <name>` |

## Commands

| Command | What it does |
|---|---|
| `cargo build --workspace` | Build everything |
| `cargo run --bin harpas` | Run the server |
| `cargo test --workspace` | Run the tests |
| `cargo fmt --all` | Format the code |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lint, as CI does |
| `cargo deny check licenses bans sources` | Check dependency licenses and sources |
| `ci/third-party-notices.sh` | Regenerate the Rust dependency section of THIRD_PARTY_NOTICES.md |

## Branch names

| Prefix | Use |
|---|---|
| `m<N>/<task>` | A roadmap task, for example `m1/raknet-prototype` |
| `fix/<name>` | Bug fixes |
| `docs/<name>` | Documentation only |
| `chore/<name>` | Build, CI and tooling |

`<task>` and `<name>` are short, lowercase and hyphenated, and describe the change rather than the person or
the date: `m3/anvil-reader`, `fix/keepalive-timeout`.

## Pull requests

Pull requests are squash merged, so the title becomes the commit message on `main`. Write it to read well in
`git log`:

```
<area>: <what the change does>
```

- **`<area>`** is the part of the project the change touches: a crate name without the `harpas-` prefix
  (`raknet`, `protocol-java`, `engine`), or `docs`, `ci`, `deps` or `workspace`. A change that spans many
  areas uses the main one.
- **The summary** is in the imperative, lowercase after the colon, with no final period, and the whole title
  stays under about 72 characters. It says what changes, not how hard it was.

| Good | Not as good | Why |
|---|---|---|
| `raknet: add the offline handshake` | `RakNet stuff` | Says nothing about the change |
| `engine: run random ticks in vanilla order` | `engine: fixed ticks.` | Past tense, vague, final period |
| `protocol-java: generate Status and Login codecs` | `M1 task 3` | The reader of `git log` has no roadmap open |
| `docs: record the JVM bridge benchmark` | `Update jvm.md` | Says which file, not what changed in it |

The description follows [the pull request template](.github/pull_request_template.md): what changes and why,
the issue or roadmap task it belongs to (`Closes #12`, `M1: RakNet prototype`), and how it was tested.

## Issues

Issues are opened from the forms in [.github/ISSUE_TEMPLATE](.github/ISSUE_TEMPLATE):

- **Bug report:** what happened, what vanilla does in the same situation, steps to reproduce, the Harpas
  version (`harpas --version`), the client edition and version, the platform and the relevant part of
  `logs/latest.log`. Vanilla is the reference, so "what vanilla does" is what makes a difference a bug.
- **Feature request:** the problem to solve, the proposed behavior, and the design document it affects.

Issue titles describe the problem in plain words, without an area prefix: "Bedrock players fall through
slabs", "Support Velocity modern forwarding".

## Labels

Issues and pull requests combine one label from each group that applies:

| Group | Labels |
|---|---|
| Type | `bug`, `enhancement`, `documentation`, `question`, `parity` (differs from vanilla), `performance`, `security`, `dependencies` |
| Area | `area: <crate>` (the crate name without `harpas-`), `area: ci`, `area: workspace` — the same areas as pull request titles |
| Edition | `edition: java`, `edition: bedrock`, when only one edition is affected |
| Status | `needs triage` (added by the issue forms), `blocked`, `breaking`, `good first issue`, `help wanted` |
