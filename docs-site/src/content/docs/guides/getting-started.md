---
title: Getting Started
description: Clone Spool, install dependencies, and run it in development.
---

Spool is a [Tauri 2](https://v2.tauri.app/) desktop app: a **Rust** backend
(`tauri/src-tauri/`) paired with a **SvelteKit 5** frontend (`tauri/src/`).
Windows and Linux are both primary targets (notably the gaming-handheld
distros — Bazzite, CachyOS, SteamOS).

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- [Bun](https://bun.sh/) for the frontend
- Tauri's platform prerequisites — see the
  [Tauri prerequisites guide](https://v2.tauri.app/start/prerequisites/)
- **Linux only:** the WebView + tray system libraries —
  `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, and `librsvg2-dev` (add
  `patchelf` and `xdg-utils` if you build an AppImage — the bundler copies
  `/usr/bin/xdg-open` into it). `umu-launcher` (`umu-run`) on the host
  is optional — only needed to test the Proton runner, and is the one Linux
  dependency that is *not* bundled.

## Bundled sidecars

Spool shells out to [`ludusavi`](https://github.com/mtkennerly/ludusavi) and
[`rclone`](https://rclone.org/), which ship as **Tauri sidecars** — end users
never install them separately. For development you fetch them once into
`tauri/src-tauri/binaries/`:

```bash
cd tauri
bun run download-sidecars
```

You need these before **anything that compiles the backend** — `tauri dev`,
`tauri build`, and the [local checks](#run-the-checks-locally) — because Tauri
verifies the sidecar binaries exist at build time.

## Install & run

All commands run from `tauri/` unless noted.

```bash
cd tauri

# Install frontend dependencies (first time, or after package.json changes)
bun install

# Fetch the bundled sidecars (first time — see "Bundled sidecars" above)
bun run download-sidecars

# Run the app in development (hot-reload frontend + auto-rebuild backend)
bun tauri dev
```

## Build a release binary

```bash
cd tauri
bun tauri build
# Output:
#   tauri/src-tauri/target/release/spool.exe
#   tauri/src-tauri/target/release/bundle/nsis/Spool_<version>_x64-setup.exe
```

On Linux the release build produces an AppImage — `Spool_*_amd64.AppImage` on
x86_64, `Spool_*_aarch64.AppImage` on ARM64. Tauri cannot cross-compile ARM
AppImages, so an aarch64 build has to run on an ARM machine.

## Run the checks locally

CI fails on any clippy warning, so run these before pushing.

:::note[Sidecars required]
`cargo check`, `cargo clippy`, and `cargo test` compile the backend, so the
bundled sidecars must be present first — run `bun run download-sidecars` (see
[Bundled sidecars](#bundled-sidecars)) if you haven't already.
:::

```bash
# Backend (from tauri/src-tauri)
cargo check
cargo clippy --all-targets -- -D warnings
cargo test

# Frontend (from tauri/)
bun run check     # svelte-check
bun run lint      # ESLint
bun run test      # Vitest unit tests
```

End-to-end tests drive a real Tauri window via `tauri-driver` + WebdriverIO:

```bash
cd tauri
bun run test:e2e  # builds the app then runs the WebDriver suite
# Headless Linux: xvfb-run -a bun run e2e
```

## Where things live

| Path | What it is |
| --- | --- |
| `tauri/src-tauri/src/` | Rust backend (persistence, subprocess orchestration, OS integration) |
| `tauri/src/` | SvelteKit frontend (routes + `lib/`) |
| `server/` | Self-hostable Hono sync server |
| `decky/` | Companion Decky Loader plugin |
| `docs-site/` | This documentation site |

See [Architecture → Overview](/architecture/overview/) for the full
module map.
