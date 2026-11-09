#!/usr/bin/env bash
# Moves the entries in changelog.d/ into the Unreleased section of CHANGELOG.md (docs/releases.md#changelog).
#
# Each pull request adds its entry as its own file, so pull requests open at the same time never change the same
# lines. A release runs this script once, before it moves Unreleased under its version.
#
# Usage: ci/changelog.sh            move the entries and delete their files
#        ci/changelog.sh --check    only check that every file in changelog.d/ has a valid name
set -euo pipefail

cd "$(dirname "$0")/.."
sections=(Added Changed Fixed Removed Security)
pattern='^changelog\.d/[a-z0-9]+(-[a-z0-9]+)*\.(added|changed|fixed|removed|security)\.md$'

shopt -s nullglob
entries=()
for file in changelog.d/*; do
    [[ $file == changelog.d/README.md ]] && continue
    if [[ ! $file =~ $pattern ]]; then
        echo "$file: name it changelog.d/<name>.<section>.md, with <section> one of: ${sections[*],,}" >&2
        exit 1
    fi
    if [[ $(head -c 2 "$file") != "- " ]]; then
        echo "$file: an entry starts with '- '" >&2
        exit 1
    fi
    entries+=("$file")
done
[[ ${1:-} == --check ]] && exit 0
if [[ ${#entries[@]} -eq 0 ]]; then
    echo "changelog.d/ has no entries"
    exit 0
fi

# One file per section with its entries, newest first, as CHANGELOG.md lists them. A file's date is that of the
# commit that added it.
blocks=$(mktemp -d)
trap 'rm -rf "$blocks"' EXIT
for file in "${entries[@]}"; do
    added=$(git log -1 --diff-filter=A --format=%ct -- "$file")
    printf '%s %s\n' "${added:-9999999999}" "$file"
done | sort -rn -k1,1 -k2,2 | while read -r _ file; do
    section=${file%.md}
    section=${section##*.}
    { cat "$file"; echo; } >> "$blocks/$section"
done

awk -v blocks="$blocks" -v order="${sections[*]}" '
function block(name,    file, line) {
    file = blocks "/" tolower(name)
    while ((getline line < file) > 0) print line
    close(file)
}
function has(name,    file, line, found) {
    file = blocks "/" tolower(name)
    found = (getline line < file) > 0
    close(file)
    return found
}
# Writes the sections before `upto` (all of them when empty) that have entries but no heading in Unreleased yet.
function missing(upto,    i) {
    for (i = 1; i <= count; i++) {
        if (names[i] == upto) return
        if (!(names[i] in done) && has(names[i])) {
            print "### " names[i]; print ""; block(names[i]); done[names[i]] = 1
        }
    }
}
BEGIN { count = split(order, names, " ") }
state == 1 && /^## \[/ { missing(""); state = 2 }
state == 1 && /^### / {
    name = substr($0, 5)
    missing(name)
    print
    if (has(name) && !(name in done)) {
        # The entries go first under the heading, before those already there.
        more = (getline line) > 0
        print ""
        block(name); done[name] = 1
        if (more && line != "") print line
    }
    next
}
{ print }
/^## \[Unreleased\]$/ && state == 0 { state = 1 }
END { if (state == 1) missing("") }
' CHANGELOG.md > "$blocks/CHANGELOG.md"
mv "$blocks/CHANGELOG.md" CHANGELOG.md
git rm -q -- "${entries[@]}" 2>/dev/null || rm -f -- "${entries[@]}"
echo "Moved ${#entries[@]} entries into CHANGELOG.md"
