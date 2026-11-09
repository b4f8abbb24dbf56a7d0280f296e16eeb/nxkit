#!/bin/sh
# Packs the release: signs the one binary, makes the ZIP file, the checksum file and the release notes.
# Usage: tools/release-package.sh TAG            (for example v0.1.0, run after cargo build --release)
# Environment: TARGET_DIR (default target).
# The archive holds the one binary nxb, LICENSE and THIRD_PARTY.md.
# There are no other program names and no links: every stage is a command of nxb.
# Output in dist/: nxb-TAG-macos-arm64.zip, SHA256SUMS, notes.md
set -eu
cd "$(dirname "$0")/.."
tag=${1:?usage: release-package.sh TAG}
target=${TARGET_DIR:-${CARGO_TARGET_DIR:-target}}/release
version=${tag#v}
name="nxb-${tag}-macos-arm64"

rm -rf dist; mkdir -p "dist/$name"
test -x "$target/nxb" || { echo "missing binary: $target/nxb" >&2; exit 1; }
cp "$target/nxb" "dist/$name/nxb"
# ad-hoc signature of the one real binary
codesign -s - --force "dist/$name/nxb"
codesign --verify "dist/$name/nxb"
cp LICENSE THIRD_PARTY.md "dist/$name/"
( cd dist && zip -qr "$name.zip" "$name" && shasum -a 256 "$name.zip" > SHA256SUMS \
  && ( cd "$name" && shasum -a 256 nxb ) | sed "s|  |  $name/|" >> SHA256SUMS && cat SHA256SUMS )

# check the zip: the one binary, the two texts, no links and no other program
chk=$(mktemp -d "${TMPDIR:-/tmp}/nxb-zipcheck.XXXXXX")
trap 'rm -rf "$chk"' EXIT
unzip -q "dist/$name.zip" -d "$chk"
test -x "$chk/$name/nxb" || { echo "zip check: nxb is missing or not executable" >&2; exit 1; }
for f in LICENSE THIRD_PARTY.md; do
  test -f "$chk/$name/$f" || { echo "zip check: $f is missing" >&2; exit 1; }
done
test -z "$(find "$chk/$name" -type l)" || { echo "zip check: the zip holds a link" >&2; exit 1; }
extra=$(cd "$chk/$name" && find . -maxdepth 1 -type f -perm -u+x ! -name nxb)
test -z "$extra" || { echo "zip check: more than one program: $extra" >&2; exit 1; }
codesign --verify "$chk/$name/nxb"
"$chk/$name/nxb" version >/dev/null
cmp "$chk/$name/nxb" "dist/$name/nxb"
( cd dist && grep " $name/nxb\$" SHA256SUMS | shasum -a 256 -c - >/dev/null )

# notes: the section of CHANGELOG.md for this version
awk -v v="$version" '
  $0 ~ "^## " v "( |$)" { on = 1; next }
  on && /^## / { exit }
  on { print }
' CHANGELOG.md > dist/notes.md
test -s dist/notes.md || { echo "no section for $version in CHANGELOG.md" >&2; exit 1; }
echo "release files are in dist/"
