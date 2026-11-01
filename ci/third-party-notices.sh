#!/usr/bin/env bash
# Regenerates the Rust dependency section of THIRD_PARTY_NOTICES.md with cargo-about and jq.
# With --check, fails instead of writing when the file is out of date.
#
# Only the crates linked into the `harpas` binary are listed, since that is what Harpas distributes: build
# scripts, code generators, benchmarks and tests are left out. ci/notices.jq prints each license text once.
set -euo pipefail

cd "$(dirname "$0")/.."
file=THIRD_PARTY_NOTICES.md
begin='<!-- BEGIN cargo-about -->'
end='<!-- END cargo-about -->'

generated=$(cargo about generate --locked --fail --format json \
    --manifest-path crates/harpas-server/Cargo.toml --config about.toml | jq -r -f ci/notices.jq)
updated=$(awk -v begin="$begin" -v end="$end" -v body="$generated" '
    $0 == begin { print; print body; skip = 1; next }
    $0 == end   { skip = 0 }
    !skip       { print }
' "$file")

if [[ "${1:-}" == "--check" ]]; then
    if ! diff -u "$file" <(printf '%s\n' "$updated"); then
        echo "$file is out of date: run ci/third-party-notices.sh and commit the result." >&2
        exit 1
    fi
else
    printf '%s\n' "$updated" > "$file"
fi
