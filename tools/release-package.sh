#!/bin/sh
# Packs the release: signs the one binary, makes the ZIP file, the checksum file and the release notes.
# Usage: tools/release-package.sh TAG            (for example v0.1.0, run after cargo build --release)
# PROFILE=dist takes the binary of cargo build --profile dist (the release workflow does).
# Environment: TARGET_DIR (default target).
# The archive holds the two programs nxb and nxkit-host, MANIFEST.json, LICENSE and THIRD_PARTY.md.
# nxb starts nxkit-host from its own folder, so both stay together. There are no other programs and no links.
# MACOS_MIN (default 26.0) is the macOS version that the release asks for. The release workflow builds with the same
# value as MACOSX_DEPLOYMENT_TARGET, and this script checks it in both binaries.
# Output in dist/: nxb-TAG-macos-arm64.zip, SHA256SUMS, notes.md
set -eu
cd "$(dirname "$0")/.."
tag=${1:?usage: release-package.sh TAG}
target=${TARGET_DIR:-${CARGO_TARGET_DIR:-target}}/${PROFILE:-release}
version=${tag#v}
name="nxb-${tag}-macos-arm64"
macos_min=${MACOS_MIN:-26.0}

rm -rf dist; mkdir -p "dist/$name"
for p in nxb nxkit-host; do
  test -x "$target/$p" || { echo "missing binary: $target/$p" >&2; exit 1; }
  cp "$target/$p" "dist/$name/$p"
  # the deployment target of the binary must be the one that the manifest names
  minos=$(vtool -show-build "dist/$name/$p" | awk '$1 == "minos" { print $2 }')
  test "$minos" = "$macos_min" || { echo "$p has minos $minos, expected $macos_min (set MACOSX_DEPLOYMENT_TARGET for the build)" >&2; exit 1; }
  # ad-hoc signature of each program
  codesign -s - --force "dist/$name/$p"
  codesign --verify "dist/$name/$p"
done
cp LICENSE THIRD_PARTY.md "dist/$name/"
sum_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
cat > "dist/$name/MANIFEST.json" <<MANIFEST
{
  "format": 1,
  "version": "$version",
  "platform": "macos",
  "arch": "arm64",
  "macos_min": "$macos_min",
  "files": {
    "nxb": "$(sum_of "dist/$name/nxb")",
    "nxkit-host": "$(sum_of "dist/$name/nxkit-host")"
  }
}
MANIFEST
( cd dist && zip -qr "$name.zip" "$name" && shasum -a 256 "$name.zip" > SHA256SUMS \
  && ( cd "$name" && shasum -a 256 nxb nxkit-host MANIFEST.json ) | sed "s|  |  $name/|" >> SHA256SUMS && cat SHA256SUMS )

# check the zip: the two programs, the manifest, the two texts, no links and no other program
chk=$(mktemp -d "${TMPDIR:-/tmp}/nxb-zipcheck.XXXXXX")
trap 'rm -rf "$chk"' EXIT
unzip -q "dist/$name.zip" -d "$chk"
for p in nxb nxkit-host; do
  test -x "$chk/$name/$p" || { echo "zip check: $p is missing or not executable" >&2; exit 1; }
done
for f in LICENSE THIRD_PARTY.md MANIFEST.json; do
  test -f "$chk/$name/$f" || { echo "zip check: $f is missing" >&2; exit 1; }
done
test -z "$(find "$chk/$name" -type l)" || { echo "zip check: the zip holds a link" >&2; exit 1; }
extra=$(cd "$chk/$name" && find . -maxdepth 1 -type f -perm -u+x ! -name nxb ! -name nxkit-host)
test -z "$extra" || { echo "zip check: more than the two programs: $extra" >&2; exit 1; }
for p in nxb nxkit-host; do
  codesign --verify "$chk/$name/$p"
  cmp "$chk/$name/$p" "dist/$name/$p"
done
"$chk/$name/nxb" version >/dev/null
# SHA256SUMS: the zip and each file inside it
( cd dist && shasum -a 256 -c SHA256SUMS >/dev/null )

# notes: the section of CHANGELOG.md for this version
awk -v v="$version" '
  $0 ~ "^## " v "( |$)" { on = 1; next }
  on && /^## / { exit }
  on { print }
' CHANGELOG.md > dist/notes.md
test -s dist/notes.md || { echo "no section for $version in CHANGELOG.md" >&2; exit 1; }
echo "release files are in dist/"
