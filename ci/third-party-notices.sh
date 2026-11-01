#!/usr/bin/env bash
# Regenerates the Rust dependency section of THIRD_PARTY_NOTICES.md with cargo-about.
# With --check, fails instead of writing when the file is out of date.
set -euo pipefail

cd "$(dirname "$0")/.."
file=THIRD_PARTY_NOTICES.md
begin='<!-- BEGIN cargo-about -->'
end='<!-- END cargo-about -->'

generated=$(cargo about generate --locked --fail ci/about.hbs)
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
