#!/bin/sh
# Fails when the tree holds something that must not be published: big files, game or toolchain
# artefacts by extension, private absolute paths, attribution trailers, key material patterns.
# Usage: tools/no-game-data.sh   (run from anywhere)
set -eu
cd "$(dirname "$0")/.."
fail=0
bad() { printf 'no-game-data: %s\n' "$*" >&2; fail=1; }

# NUL-separated list of the files to check: the tracked files, or every file outside target/ and .git/ without git
list() {
  if git rev-parse --git-dir >/dev/null 2>&1; then git ls-files -z; else
    find . -type f -not -path './target/*' -not -path './.git/*' | sed 's|^\./||' | tr '\n' '\0'; fi
}

# files over 1 MB (tracked files that are missing from the work tree are skipped)
for f in $(list | xargs -0 sh -c 'find -L "$@" -prune -type f -size +1048576c 2>/dev/null' sh); do
  bad "$f is larger than 1 MB"
done

# forbidden extensions
for f in $(list | tr '\0' '\n' | grep -E '\.(nca|nsp|xci|nsz|keys|spv|air|metallib|ipa)$' || true); do
  bad "$f has a forbidden extension"
done

# forbidden strings (this script is excluded)
scan() {
  pattern=$1; label=$2
  hits=$(list | tr '\0' '\n' | grep -vx 'tools/no-game-data.sh' | tr '\n' '\0' \
    | xargs -0 sh -c 'for f; do [ -f "$f" ] && printf "%s\0" "$f"; done' sh \
    | xargs -0 grep -InE -e "$pattern" /dev/null || true)
  if [ -n "$hits" ]; then printf '%s\n' "$hits" >&2; bad "found $label"; fi
}
scan '/Users/|/Volumes/' 'a private absolute path'
scan 'Co-Authored-By|Generated with' 'an attribution trailer'
scan '(title|master)_?key[^[:alnum:]]*[=:][[:space:]]*[0-9a-fA-F]{32}' 'key material'
scan '-----BEGIN [A-Z ]*PRIVATE KEY' 'a private key block'

[ "$fail" -eq 0 ] && echo "no-game-data: ok"
exit "$fail"
