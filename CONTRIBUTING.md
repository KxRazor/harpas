# Contributing to Harpas

Thank you for helping. This file is the set of rules every contribution follows, and everyone in the project
follows the [code of conduct](CODE_OF_CONDUCT.md). The design behind the code is
in [docs/](docs/README.md), and the order of work is in [ROADMAP.md](ROADMAP.md).

## Licensing of contributions

Contributions are licensed under the license of the component they touch, with no separate sign-off or
contributor agreement ([docs/licensing.md](docs/licensing.md#contributions)):

- a contribution to the core, the plugin API, the SDKs or the mods is licensed **MIT OR Apache-2.0**;
- a contribution to the Bukkit shim (`java/harpas-bukkit`) is licensed **GPL-3.0**.

By opening a pull request you agree to license your contribution this way. Apache-2.0 states this rule
explicitly in section 5.

Never contribute code or data copied from Mojang's game files or decompiled code. Vanilla behavior is ported
from its observable behavior and the community documentation cited in the design documents; game data is
extracted on the server owner's machine, never committed ([docs/world.md](docs/world.md)).

## Pull requests

- **One change per pull request.** A pull request does one thing that can be reviewed on its own: usually one
  task of the roadmap, or part of one. Unrelated fixes and refactors go in their own pull requests.
- **Small enough to review.** Aim for under 400 changed lines, not counting generated code, schemas and test
  data. A larger pull request is fine when its description says why it is easier to review as one change, for
  example a new subsystem or prototype whose parts only make sense together. Split it into steps that each
  build and pass the tests when the parts can be reviewed on their own.
- **Design first for new subsystems.** A change that departs from a design document updates that document in
  the same pull request, or in an earlier one.
- **Describe the change:** what it does, why, which roadmap task or issue it belongs to, and how it was tested.

## Tests

- **Behavior changes need tests.** Every pull request that adds or changes behavior includes tests that fail
  without the change. A fix includes a test that reproduces the bug.
- **Vanilla behavior is checked against vanilla.** Ported behavior gets a parity test where a harness exists
  ([docs/parity.md](docs/parity.md)). `cargo test` skips the comparisons with Mojang's server unless
  `HARPAS_VANILLA_SERVER_DIR` is set; the Parity workflow runs them on pull requests that change code.
- Refactors, documentation and build changes need no new tests, but must keep the existing ones passing.

## Checks

CI runs these on Linux, Windows and macOS; run them locally before opening a pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Adding or updating a Rust dependency also needs:

```sh
cargo deny check licenses bans sources
ci/third-party-notices.sh
```

The second command regenerates the Rust dependency section of [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md);
commit the result. Only licenses allowed in [deny.toml](deny.toml) are accepted; a new license needs a change
to `deny.toml` that explains why it is compatible with [docs/licensing.md](docs/licensing.md).

## Changelog

Every pull request that changes behavior adds its [CHANGELOG.md](CHANGELOG.md) entry as a file in
[changelog.d/](changelog.d/README.md), `<name>.<section>.md`, so that pull requests open at the same time do not
conflict. A release moves the files into `Unreleased` ([docs/releases.md](docs/releases.md#changelog)).
