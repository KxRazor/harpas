#!/usr/bin/env bash
# Downloads Mojang's server jar for parity tests only (docs/parity.md).
# The jar is never committed, cached, uploaded as an artifact or shipped in a release.
#
# Usage: ci/fetch-vanilla-server.sh <minecraft-version> <output-dir>
# Requires MOJANG_EULA_ACCEPTED=true: the maintainers' acceptance of the Minecraft EULA
# (https://aka.ms/MinecraftEULA) for running the reference server.
set -euo pipefail

version=$1
out=$2
manifest=https://piston-meta.mojang.com/mc/game/version_manifest_v2.json

if [[ "${MOJANG_EULA_ACCEPTED:-}" != "true" ]]; then
    echo "MOJANG_EULA_ACCEPTED is not 'true': the Minecraft EULA has not been accepted." >&2
    exit 1
fi

version_url=$(curl -fsSL "$manifest" | jq -er --arg v "$version" '.versions[] | select(.id == $v) | .url')
read -r url sha1 < <(curl -fsSL "$version_url" | jq -er '.downloads.server | "\(.url) \(.sha1)"')

mkdir -p "$out"
curl -fsSL -o "$out/server.jar" "$url"
echo "$sha1  $out/server.jar" | sha1sum --check --quiet
echo "eula=true" > "$out/eula.txt"
echo "Mojang server $version ready in $out"
