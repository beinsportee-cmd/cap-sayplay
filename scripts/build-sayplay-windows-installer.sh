#!/usr/bin/env bash
# One universal 64-bit NSIS installer for all teacher PCs (Windows 10 18362+, Windows 11).
set -euo pipefail

TARGET="${1:-x86_64-pc-windows-msvc}"
export SAYPLAY_BUILD=1
export RUST_TARGET_TRIPLE="$TARGET"

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if [[ ! -f .env ]]; then
  cp .env.example .env
fi

echo "==> cap-setup ($TARGET)"
bun run cap-setup

echo "==> sidecars + muxer"
CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=symbols \
  ./scripts/build-desktop-binaries.sh "$TARGET"

echo "==> GPUI app + NSIS (this takes a long time)"
node scripts/bundle-gpui.mjs "$TARGET" --bundles nsis

nsis_dir="apps/desktop-gpui/target/$TARGET/release/bundle/nsis"
installer="$(ls "$nsis_dir"/*.exe | head -1)"
version="$(grep '^version = ' apps/desktop-gpui/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')"
out="$repo_root/dist/SayPlay-Recorder-${version}-win64-universal-setup.exe"
mkdir -p dist
cp "$installer" "$out"
echo ""
echo "Universal installer ready:"
echo "  $out"
echo "Share this single .exe with every teacher (64-bit Windows only)."
