#!/usr/bin/env bash
# Build gitvis and package a release the updater can install, on Linux or
# macOS (scripts/package.ps1 is the Windows equivalent):
#
#   dist/gitvis-<version>/                     gitvis, README.md, LICENSE files, licenses/
#   dist/gitvis-<version>-<os>-<arch>.zip      the release asset
#   dist/SHA256SUMS
#
# Usage: scripts/package.sh [--debug] [--no-build] [--exe PATH] [--out DIR]
# The version comes from [workspace.package] in Cargo.toml.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
profile=release
build=1
exe=""
out="$root/dist"
while [ $# -gt 0 ]; do
  case "$1" in
    --debug) profile=debug ;;
    --no-build) build=0 ;;
    --exe) exe="$2"; shift ;;
    --out) out="$2"; shift ;;
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

version="$(awk '
  /^\[workspace\.package\]/ { in_pkg = 1; next }
  /^\[/ { in_pkg = 0 }
  in_pkg && /^version[ \t]*=/ { gsub(/.*=[ \t]*"|".*/, ""); print; exit }
' "$root/Cargo.toml")"
[ -n "$version" ] || { echo "no version in [workspace.package] of Cargo.toml" >&2; exit 1; }

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) echo "unsupported OS $(uname -s); use scripts/package.ps1 on Windows" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) arch="$(uname -m)" ;;
esac
name="gitvis-$version"
echo "gitvis $version for $os-$arch ($profile)"

if [ "$build" = 1 ]; then
  args=(build -p gitvis --locked)
  [ "$profile" = release ] && args+=(--release)
  echo "== cargo ${args[*]}"
  cargo "${args[@]}"
fi
if [ -z "$exe" ]; then
  exe="${CARGO_TARGET_DIR:-$root/target}/$profile/gitvis"
fi
[ -f "$exe" ] || { echo "no executable at $exe (build first, or pass --exe PATH)" >&2; exit 1; }

mkdir -p "$out"
out="$(cd "$out" && pwd)"
stage="$out/$name"
echo "== stage $stage"
rm -rf "$stage"
mkdir -p "$stage/licenses"
cp "$exe" "$stage/gitvis"
chmod 755 "$stage/gitvis"
cp "$root/README.md" "$stage/"
found_licence=0
for f in LICENSE LICENSE.md LICENSE.txt LICENSE-MIT LICENSE-APACHE; do
  if [ -f "$root/$f" ]; then cp "$root/$f" "$stage/"; found_licence=1; fi
done
[ "$found_licence" = 1 ] || echo "warning: the repository has no LICENSE file; only the font licences are packaged" >&2
cp "$root"/crates/gitvis/assets/fonts/*-OFL.txt "$stage/licenses/"
# Every crate compiled into gitvis, with its licence files.
notice_args="run --quiet --locked -p gitvis-core --example notices"
[ "$profile" = release ] && notice_args="$notice_args --release"
(cd "$root" && cargo $notice_args -- --out "$stage/licenses/THIRD-PARTY-LICENSES.txt")

zip_path="$out/$name-$os-$arch.zip"
echo "== zip $zip_path"
rm -f "$zip_path"
if command -v zip >/dev/null 2>&1; then
  (cd "$out" && zip -qr -X "$zip_path" "$name")
elif command -v python3 >/dev/null 2>&1; then
  (cd "$out" && python3 -m zipfile -c "$zip_path" "$name")
else
  echo "neither zip nor python3 is installed (apt install zip / brew install zip)" >&2
  exit 1
fi

echo "== checksums $out/SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$out" && sha256sum "$(basename "$zip_path")" > SHA256SUMS)
else
  (cd "$out" && shasum -a 256 "$(basename "$zip_path")" > SHA256SUMS)
fi
cat "$out/SHA256SUMS"
echo "release contract: tag v$version with $(basename "$zip_path") and SHA256SUMS"
