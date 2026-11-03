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

**CI guardrails:** pushes to `sayplay` run **sayplay-gpui-compile** (fast `cargo check` on `cap-gpui`) before the full installer job compiles the same binary. If you change Rust UI code, run locally:

```bash
export SAYPLAY_BUILD=1
./scripts/check-sayplay-gpui-compile.sh x86_64-pc-windows-msvc
```

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
