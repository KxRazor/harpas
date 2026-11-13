# Shortcuts for the commands of CONTRIBUTING.md, run with just (https://just.systems): `just check` before a
# pull request. `just` alone lists them. CI does not use this file: its workflows spell the commands out.

# List the recipes.
default:
    @just --list --unsorted

# Everything CI checks on a pull request, except the comparisons with vanilla.
check: fmt-check changelog lint test doc

# Format the code.
fmt:
    cargo fmt --all

# Check the formatting, as CI does.
fmt-check:
    cargo fmt --all --check

# Check the entries of changelog.d/.
changelog:
    ci/changelog.sh --check

# Lint, as CI does.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run the tests and the documentation tests of the workspace, or of what the arguments select: `just test -p harpas-anvil`.
test *args="--workspace":
    cargo nextest run {{ args }}
    cargo test --doc {{ args }}

# Build the documentation and check its links, as CI does.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Run the server in the current folder. Arguments go to it: `just run --version`.
run *args:
    cargo run --release --bin harpas -- {{ args }}

# Compare with Mojang's server in `dir` (docs/parity.md#locally), all the tests or those whose names contain a word: `just parity ~/harpas-vanilla full monument`.
parity dir sample="reduced" *tests:
    HARPAS_VANILLA_SERVER_DIR={{ quote(dir) }} HARPAS_PARITY_SAMPLE={{ sample }} \
        cargo nextest run --profile parity --release --workspace --locked \
        --test '*_parity' --test world_loading --run-ignored all {{ tests }}

# Check the licenses of the dependencies and regenerate THIRD_PARTY_NOTICES.md, after adding or updating one.
deps:
    cargo deny check licenses bans sources
    ci/third-party-notices.sh
