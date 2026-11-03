#!/usr/bin/env bash
# Fail fast on SayPlay desktop-gpui compile errors (same flags as CI).
set -euo pipefail

target="${1:-${RUST_TARGET_TRIPLE:-x86_64-pc-windows-msvc}}"
toolchain="${CAP_GPUI_RUST_TOOLCHAIN:-1.95.0}"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
gpui_dir="$repo_root/apps/desktop-gpui"

export SAYPLAY_BUILD="${SAYPLAY_BUILD:-1}"
export RUST_TARGET_TRIPLE="$target"

if ! rustup run "$toolchain" rustc --version >/dev/null 2>&1; then
	rustup toolchain install "$toolchain" --profile minimal
fi
rustup target add "$target" --toolchain "$toolchain"

"$repo_root/scripts/prepare-gpui-dependency.sh"
node "$repo_root/scripts/sync-desktop-versions.mjs"

(
	cd "$gpui_dir"
	cargo +"$toolchain" check --release --target "$target" -p cap-desktop-gpui --bin cap-gpui
)

echo "SayPlay gpui check OK for $target"
