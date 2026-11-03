# SayPlay Recorder

School build of [Cap](https://github.com/CapSoftware/Cap) (AGPLv3) for long Studio Mode lessons with separate mic and system-audio tracks.

## What is different from official Cap

- **No 5-minute export limit** (`SAYPLAY_BUILD=1` → unlimited local export).
- **No Instant Mode** (Cap cloud / sign-in); **Studio** + **Screenshot** only.
- **No auto-update** to official Cap (updater removed; update checks disabled).
- **No analytics** env vars in CI (telemetry off by default for new installs).
- **Rebrand**: product name `SayPlay Recorder`, bundle id `com.sayandplayschool.recorder`.
- **No Cap cloud**: no sign-in prompts, no link/share export, no upload queue, no cap.so pricing links.
- **No Cap branding in window titles** (SayPlay names for exclusion from recordings).

Icons still use Cap assets until custom artwork is added. Replace files under `apps/desktop-gpui/icons/` when ready.

## Build (GitHub Actions)

1. Push this repo to a **public** GitHub fork (AGPL requirement).
2. Create branch `sayplay` and push.
3. Run workflow **build-windows-sayplay** (or push to `sayplay`).
4. Download the NSIS `.exe` from Actions artifacts or the draft release.

### Before pushing Rust changes

Every failed installer build so far came from Rust edits pushed without a compiler run. Before pushing:

```bash
# Seconds, no native deps: catches unbalanced brackets and other parse errors.
./scripts/check-sayplay-gpui-compile.sh --syntax-only
```

For type and borrow errors, push to a work branch (for example `sayplay-dev`). **sayplay-gpui-compile** runs the full SayPlay `cargo check` there in about 10 minutes. Merge into `sayplay` only once it's green.

The same full check can run locally (`./scripts/check-sayplay-gpui-compile.sh` with no flag). It needs a one-time `bun run cap-setup`, working FFmpeg bindings (on the original dev PC, bindgen currently emits an empty `AVFormatContext`, so the check stops in `ffmpeg-next`), and the patched GPUI checkout at `../zed-cap`. The script creates that checkout via `scripts/prepare-gpui-dependency.sh`. If Windows refuses the final folder rename (`mv: Permission denied`, because the editor or antivirus is watching the folder), clone it in place instead:

```bash
git init ../zed-cap && cd ../zed-cap
git remote add origin https://github.com/wingleeio/zed.git
git fetch --depth 1 origin 5d1f83d9f27a19bec1fb241dc33b42238af9cf8d && git checkout --detach FETCH_HEAD
for p in gpui windows linux idle-frames streaming-image wayland-visibility wayland-activation closed-window-events glass; do
  git apply --unidiff-zero ../cap-sayplay/apps/desktop-gpui/patches/zed-$p.patch
done
```

Errors are summarized at the end of the output and written to `apps/desktop-gpui/target/sayplay-check.log`.

**Code rules for SayPlay changes**

- Gate behaviour at runtime with `cfg!(sayplay_build)` or `crate::sayplay::*_enabled()`. Then both Cap and SayPlay code paths are type-checked in every build. Use `#[cfg(sayplay_build)]` only where the code would not compile otherwise.
- Don't guess GPUI APIs. Copy an existing call site in this crate that already compiles.
- Don't bulk-edit code with search-and-replace. If you must, run `--syntax-only` straight after.

### CI guardrails

- **build-windows-sayplay** (push to `sayplay`, or manual): syntax check (under 1 min), then pagefile, setup, SayPlay `cargo check`, release compile, NSIS, and draft release. A newer push cancels the older run.
- **sayplay-gpui-compile** (any other branch, or a PR into `sayplay`): syntax check plus `cargo check` only. Push work-in-progress to a branch such as `sayplay-dev` to get a compile result without starting an installer build.
- Compile errors appear as annotations on the run summary page, with file and line.
- Upstream Cap's scheduled workflows (publish-nightly, A/V Sync, Engine verify, Verify recording, Instant reliability) are disabled on this fork. They need Cap's infrastructure and only add failed or stuck runs.

Windows builds are **unsigned**. SmartScreen: **More info → Run anyway**.

## Universal installer (one `.exe` for every PC)

- **Architecture:** 64-bit only (`x86_64`). Works on Windows 10 (build 18362+) and Windows 11 — Intel/AMD PCs, not ARM-only laptops.
- **Scope:** NSIS `installMode: both` — choose **install for all users** (recommended on shared school PCs) or current user only.
- **Output name after local build:** `dist/SayPlay-Recorder-<version>-win64-universal-setup.exe` — copy this single file to USB, network share, or email (size is large).

Teachers without admin rights need IT to run **all users** once per machine.

## Build locally on Windows

Requires Git Bash, [Bun](https://bun.sh), Rust **1.95.0**, and ~30+ GB free disk/RAM for linking.

```bash
cd cap-sayplay
export SAYPLAY_BUILD=1
export RUST_TARGET_TRIPLE=x86_64-pc-windows-msvc
# create .env (see .github/workflows/build-windows.yml)
bun run cap-setup
./scripts/build-desktop-binaries.sh x86_64-pc-windows-msvc
node scripts/bundle-gpui.mjs x86_64-pc-windows-msvc --bundles nsis
```

Installer output:

`apps/desktop-gpui/target/x86_64-pc-windows-msvc/release/bundle/nsis/`

## Teachers (acceptance)

See `CAP_WINDOWS_BUILD_HANDOFF.md` in the parent folder for the full checklist (45-min record, separate `audio-input.*` / `system_audio.*`, etc.).

## Russian UI

Upstream locales do not include Russian yet (`apps/desktop-gpui/locales/`). UI stays English until a `ru.toml` is contributed.

## Phase 2

Offline DeepFilterNet mic cleaning — see handoff doc; not included in this build.
