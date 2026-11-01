# Contributing to Harpas

Thank you for helping. This file is the set of rules every contribution follows. The design behind the code is
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
  data. Larger changes are split into steps that each build and pass the tests; if that is not possible,
  explain why in the description.
- **Design first for new subsystems.** A change that departs from a design document updates that document in
  the same pull request, or in an earlier one.
- **Describe the change:** what it does, why, which roadmap task or issue it belongs to, and how it was tested.

## Tests

- **Behavior changes need tests.** Every pull request that adds or changes behavior includes tests that fail
  without the change. A fix includes a test that reproduces the bug.
- **Vanilla behavior is checked against vanilla.** Ported behavior gets a parity test where a harness exists
  ([docs/world.md](docs/world.md#parity-strategy)).
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

Every pull request that changes behavior adds an entry under `Unreleased` in [CHANGELOG.md](CHANGELOG.md)
([docs/releases.md](docs/releases.md#changelog)).
