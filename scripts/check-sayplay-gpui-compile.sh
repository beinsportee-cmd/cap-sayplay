#!/usr/bin/env bash
# Fail fast on SayPlay desktop-gpui compile errors (same flags as CI).
#
#   ./scripts/check-sayplay-gpui-compile.sh [--syntax-only] [target]
#
# --syntax-only parses every cap-gpui source file with rustfmt. It needs no
# native deps and takes seconds, so run it before every push. Without it the
# script also runs a full `cargo check` of the SayPlay build.
set -euo pipefail

syntax_only=0
if [[ "${1:-}" == "--syntax-only" ]]; then
	syntax_only=1
	shift
fi

target="${1:-${RUST_TARGET_TRIPLE:-x86_64-pc-windows-msvc}}"
toolchain="${CAP_GPUI_RUST_TOOLCHAIN:-1.95.0}"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
gpui_dir="$repo_root/apps/desktop-gpui"
log_file="$gpui_dir/target/sayplay-check.log"

export SAYPLAY_BUILD="${SAYPLAY_BUILD:-1}"
export RUST_TARGET_TRIPLE="$target"

# With VCINSTALLDIR set, rustc takes `link.exe` from PATH, which in Git Bash is
# coreutils' /usr/bin/link. Unset it so rustc locates MSVC's linker itself.
case "$(uname -s)" in
MINGW* | MSYS*) unset VCINSTALLDIR ;;
esac

if ! rustup run "$toolchain" rustc --version >/dev/null 2>&1; then
	rustup toolchain install "$toolchain" --profile minimal
fi

# rustfmt follows every `mod` from the crate root, so one call parses the crate.
rustup component add rustfmt --toolchain "$toolchain" >/dev/null 2>&1 || true
if ! rustfmt +"$toolchain" --edition 2024 --emit stdout "$gpui_dir/src/main.rs" >/dev/null; then
	echo "::error::cap-gpui has Rust syntax errors (see the rustfmt output above)"
	exit 1
fi
echo "SayPlay gpui syntax OK"
if [[ "$syntax_only" == 1 ]]; then
	exit 0
fi

rustup target add "$target" --toolchain "$toolchain"
"$repo_root/scripts/prepare-gpui-dependency.sh"
node "$repo_root/scripts/sync-desktop-versions.mjs"

# Prints each rustc error block (from `error` to the blank line ending it).
print_errors() {
	awk '/^error/ { printing = 1 } printing { print } printing && /^[[:space:]]*$/ { printing = 0 }' "$log_file"
}

# Turns each rustc error into a GitHub annotation on the run summary page.
annotate_errors() {
	awk '
		function emit(message, location,    file, parts) {
			gsub(/%/, "%25", message)
			gsub(/\\/, "/", location)
			if (!match(location, /:[0-9]+:[0-9]+$/)) {
				printf "::error::%s\n", message
				return
			}
			file = substr(location, 1, RSTART - 1)
			split(substr(location, RSTART + 1), parts, ":")
			if (file ~ /^(src|build\.rs)/) file = "apps/desktop-gpui/" file
			sub(/^\.\.\/\.\.\//, "", file)
			printf "::error file=%s,line=%s,col=%s::%s\n", file, parts[1], parts[2], message
		}
		pending != "" {
			if (match($0, /^[[:space:]]*--> /)) emit(pending, substr($0, RSTART + RLENGTH))
			else emit(pending, "")
			pending = ""
		}
		/^error/ { pending = $0 }
		END { if (pending != "") emit(pending, "") }
	' "$log_file"
}

mkdir -p "$(dirname "$log_file")"
set +e
(
	cd "$gpui_dir"
	CARGO_TERM_COLOR=never cargo +"$toolchain" check --release --target "$target" -p cap-desktop-gpui --bin cap-gpui
) 2>&1 | tee "$log_file"
status="${PIPESTATUS[0]}"
set -e

if [[ "$status" != 0 ]]; then
	echo
	echo "==================== SayPlay compile errors ===================="
	print_errors
	if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
		annotate_errors
	fi
	if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
		{
			echo "### cap-gpui compile errors"
			echo '```'
			print_errors | head -n 400
			echo '```'
		} >> "$GITHUB_STEP_SUMMARY"
	fi
	exit "$status"
fi

echo "SayPlay gpui check OK for $target"
