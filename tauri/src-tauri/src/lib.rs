//! Crate root for the Spool Tauri backend.
//!
//! ## Tray-resident lifecycle
//!
//! Spool runs as a single long-lived process (the cassette deck stays in
//! the dock). The library window is one *view* on that process; closing
//! it hides to the tray rather than quitting the app. Secondary `spool`
//! invocations from Steam shortcuts / Armoury Crate launchers are
//! intercepted by `tauri-plugin-single-instance` and forwarded as argv
//! to the running primary — no cold-start cost on game launch.
//!
//! Quit is **only** via the tray menu's "Quit Spool" item, which calls
//! `app.exit(0)`. Window close + `RunEvent::ExitRequested` are both
//! prevented otherwise.
//!
//! ## Modules
//!   - [`error`]       — unified error type used across the backend
//!   - [`paths`]       — single source of truth for filesystem locations
//!   - [`config`]      — app settings: persistence, identity, ludusavi auto-detect
//!   - [`library`]     — the game library: data model, persistence, commands
//!   - [`ludusavi`]    — CLI subprocess + manifest cache + search/enrich
//!   - [`steamgriddb`] — cover art fetch
//!   - [`process`]     — game process spawn
//!   - [`runner`]      — run workflow state machine
//!   - [`cli`]         — argv parsing for `--run` mode
//!   - [`steamgriddb`] — cover art fetch

mod accent_backfill;
mod cli;
mod config;
mod custom_saves;
mod decky_install;
mod diagnostics;
mod drives;
mod error;
mod gamemode;
mod gamepad;
mod guided_install;
mod headless;
mod lan;
mod launcher;
mod library;
mod logind;
mod ludusavi;
mod ludusavi_config;
mod metadata;
mod metadata_backfill;
mod move_install;
mod offline;
mod paths;
mod plugin_server;
mod proc_lock;
mod process;
mod proton;
mod rclone;
mod redirects;
mod registry;
mod runner;
mod save_template;
mod session;
mod size_backfill;
mod sleep_inhibit;
mod steam;
mod steam_cdn;
mod steam_collections;
mod steam_process;
mod steamgriddb;
mod suspend;
mod system_open;
mod tray;
mod util;
mod virtual_keyboard;

use cli::CliMode;
use config::{Config, SharedConfig};
use lan::{LanDownloadState, LanManifests, LanServerShutdown, LanState, LanUploadsState};
use library::{Library, SharedLibrary};
use ludusavi::LudusaviClient;
use rclone::SyncStatusState;
use runner::RunState;
use std::sync::{Arc, Mutex};
use steamgriddb::SteamGridDbClient;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};

/// Holds a game id queued for launch at startup. The cold-start path
/// for `spool --run "Name" "Exe"` writes here; the frontend's library
/// page reads + clears it via `take_pending_run` after its event
/// listeners are wired up.
#[derive(Default)]
pub struct PendingRun {
    inner: Mutex<Option<String>>,
}

impl PendingRun {
    fn set(&self, id: String) {
        if let Ok(mut g) = self.inner.lock() {
            *g = Some(id);
        }
    }
    fn take(&self) -> Option<String> {
        self.inner.lock().ok().and_then(|mut g| g.take())
    }
}

#[tauri::command]
fn take_pending_run(state: State<'_, PendingRun>) -> Option<String> {
    state.take()
}

/// Readiness gate for the attached Game-Mode splash. The attached `--run`
/// workflow emits `run:phase` events the moment it starts, so it must not
/// begin until the splash window has wired up its `run:phase` listener —
/// otherwise the early phases (restoring → launching → playing) fire into
/// the void and the splash stays stuck on its default "Restoring saves…"
/// label for the whole session. The splash calls `notify_splash_ready`
/// once its listener is registered; the workflow task waits on this
/// (with a timeout fallback so a webview that never loads can't hang the
/// launch). Mirrors the `PendingRun` handshake the main library window
/// uses for the desktop `--run` path.
#[derive(Default)]
pub struct SplashReady {
    notify: tokio::sync::Notify,
}

#[tauri::command]
fn notify_splash_ready(state: State<'_, SplashReady>) {
    state.notify.notify_one();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ── Linux WebKitGTK rendering workaround ──────────────────────────────
    // WebKitGTK's GPU-accelerated compositing + DMA-BUF renderer fail to
    // initialise on many Linux GPU/compositor combos (AMD/radeonsi + Mesa on
    // Wayland here; also common on NVIDIA), leaving a black window with no
    // error. The ecosystem-standard fix is to disable both paths *before*
    // the webview initialises. Set in-process (not via the launch env) so it
    // works from the desktop entry, a terminal, and the AppImage alike.
    //
    // Only set when the user hasn't already chosen a value, so power users can
    // still opt back into the GPU path. These are WebKit-specific and harmless
    // to the umu-run/Proton children (which also strip GDK_* in process.rs).
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    // Initialize tracing first — everything below logs through it. The
    // worker guard is bound to the function frame so background log
    // writes flush before the process exits.
    let _log_guard = init_tracing();
    install_panic_hook();
    tracing::info!("spool starting up");

    // Parse argv once — used for early-exit checks and threaded into setup.
    let initial_args: Vec<String> = std::env::args().collect();
    let cli_mode = cli::parse_args(&initial_args);

    // Headless subcommands — no GUI, no tray, no single-instance.
    if let CliMode::HeadlessServer = cli_mode {
        std::process::exit(headless::run_headless_server());
    }

    // One-shot migration: pull `%LOCALAPPDATA%\ludusavi-wrap\` data
    // into the new Spool dir on first run. No-op if already migrated,
    // if there's no legacy dir, or if Spool already has a library.
    paths::migrate_from_ludusavi_wrap();

    // When running as an AppImage, refresh the stable launcher wrapper so
    // Steam shortcuts / Armoury stubs (which point at the wrapper, not the
    // version-stamped .AppImage) exec the current AppImage. Self-heals after
    // an update relocates the file. No-op on native installs.
    let _ = paths::refresh_appimage_launcher();

    // Open the library database (config is a small single-writer JSON file).
    // block_on: this runs on the main thread before Tauri's runtime spins up.
    let library = tauri::async_runtime::block_on(Library::open()).unwrap_or_else(|err| {
        tracing::error!(error = %err, "failed to open library database; starting with an empty in-memory library");
        tauri::async_runtime::block_on(Library::open_in_memory())
            .expect("in-memory library must open")
    });
    let config = Config::load().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "failed to load config, starting with defaults");
        Config::default()
    });

    // Decide whether this is an attached launch: `spool --run` inside a SteamOS
    // gamescope session, OR a `--run … --attached` shortcut (Apollo/Sunshine
    // streaming host). If so, we skip the tray, single-instance plugin, and
    // background pollers, run the game, then exit — so the host sees the game
    // stop when Spool does.
    let attached = matches!(cli_mode, CliMode::Run { attached: true, .. })
        || (matches!(cli_mode, CliMode::Run { .. }) && gamemode::is_steam_game_mode());
    if attached {
        tracing::info!("attached launch mode — no tray, exit on game close");
    }

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // Native OS toast notifications. Used by the run workflow to
        // tell the user "Saves backed up" / "Save restore failed"
        // while Spool itself is hidden in the tray during gameplay.
        .plugin(tauri_plugin_notification::init())
        // Auto-update via Tauri's updater. Polls a signed JSON
        // manifest (URL configured in tauri.conf.json), verifies the
        // ed25519 signature, then runs the NSIS installer silently.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Process control — used by the updater UI to relaunch Spool
        // after an update installs. On Windows the NSIS installer
        // relaunches us itself, but on the Linux AppImage the updater
        // only swaps the file in place, so we must restart explicitly.
        .plugin(tauri_plugin_process::init())
        // OS info (platform / version / arch) — read by the frontend's
        // "Report issue" toast action to stamp environment details into a
        // prefilled GitHub issue.
        .plugin(tauri_plugin_os::init())
        // Persist + restore each window's size/position so Spool reopens
        // where the user last left it. Two deliberate tweaks:
        //   * `VISIBLE` is excluded from the saved flags — `main` is a
        //     tray-resident window whose visibility we manage by hand
        //     (close hides to tray; we `show()` it explicitly in setup).
        //     Letting the plugin restore visibility would fight that and
        //     could pop the window open on launch / reintroduce the
        //     white-flash the hidden-then-show dance exists to avoid.
        //   * the `splash` window is denylisted — it's the fullscreen
        //     Game-Mode launch splash, never something to restore a
        //     prior geometry for.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .with_denylist(&["splash"])
                .build(),
        );
    if !attached {
        // Single-instance: secondary `spool` invocations land here. We
        // dispatch on argv to either focus the library or kick off a
        // game launch. Must come early — adds the IPC channel.
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            handle_forwarded_launch(app, &argv);
        }));
    }
    let app = builder
        .manage::<SharedLibrary>(Arc::new(library))
        .manage::<SharedConfig>(Mutex::new(config))
        .manage::<LudusaviClient>(LudusaviClient::new())
        .manage::<SteamGridDbClient>(SteamGridDbClient::new())
        .manage::<metadata::MetadataClient>(metadata::MetadataClient::new())
        .manage::<RunState>(RunState::default())
        .manage::<PendingRun>(PendingRun::default())
        .manage::<SplashReady>(SplashReady::default())
        .manage::<LanState>(LanState::new())
        // Single reqwest::Client shared across LAN code — reqwest is
        // designed for reuse (connection pooling, DNS cache). Per
        // `domain-web` best practice + `m07-concurrency`'s "avoid
        // per-call allocations in hot paths", every LAN call gets
        // this client via `app.state::<reqwest::Client>()` and uses
        // RequestBuilder::timeout for per-request limits.
        .manage::<reqwest::Client>(
            reqwest::Client::builder()
                .build()
                .expect("reqwest client build"),
        )
        .manage::<Arc<LanDownloadState>>(Arc::new(LanDownloadState::default()))
        .manage::<Arc<move_install::MoveState>>(Arc::new(move_install::MoveState::default()))
        .manage::<LanUploadsState>(LanUploadsState::default())
        // Seeded from the persisted hash cache so a restart doesn't re-hash
        // every shared game before its manifest can be served. Managed before
        // the LAN server spawns, so the first request sees the warm cache.
        .manage::<LanManifests>(LanManifests::load())
        .manage::<LanServerShutdown>(LanServerShutdown::default())
        .manage::<SyncStatusState>(SyncStatusState::default())
        .manage::<rclone::OAuthState>(rclone::OAuthState::default())
        .manage::<gamepad::GamepadPresence>(gamepad::GamepadPresence::default())
        .invoke_handler(tauri::generate_handler![
            take_pending_run,
            notify_splash_ready,
            virtual_keyboard::show_virtual_keyboard,
            virtual_keyboard::hide_virtual_keyboard,
            // library
            library::list_games,
            library::list_play_sessions,
            library::add_game,
            library::update_game,
            library::remove_game,
            library::delete_game_from_disk,
            library::uninstall_game,
            // config
            config::get_config,
            config::update_config,
            config::detect_umu_run,
            config::app_platform,
            config::list_collections,
            config::set_collections,
            // offline mode
            offline::go_offline,
            offline::go_online,
            // drives / library folders + move install
            drives::list_drives,
            drives::folder_free_space,
            drives::folder_capacity,
            drives::prepare_library_folder,
            drives::default_lan_install_dir,
            move_install::move_game_install,
            move_install::cancel_move,
            move_install::current_move,
            diagnostics::check_dependencies,
            // proton / linux launch
            proton::list_proton_versions,
            proton::install_proton_deps,
            proton::run_exe_in_prefix,
            guided_install::run_guided_installer,
            // ludusavi
            ludusavi::search_games,
            ludusavi::manifest_save_locations,
            ludusavi::search_by_exe,
            ludusavi::open_ludusavi_gui,
            ludusavi::set_cloud_webdav,
            // steamgriddb
            steamgriddb::fetch_cover,
            steamgriddb::fetch_hero,
            metadata::fetch_metadata,
            // steam shortcut
            steam::add_spool_to_steam,
            steam::add_to_steam,
            steam::remove_from_steam,
            steam_process::steam_game_running,
            steam_collections::sync_spool_steam_collection,
            // armoury crate launcher
            launcher::generate_armoury_launcher,
            // registry compat-flag probe
            registry::get_run_as_admin_in_registry,
            // cloud control-plane status (rclone reachability)
            rclone::current_sync_status,
            rclone::refresh_sync_status,
            rclone::check_cloud_remote_exists,
            rclone::connect_cloud_oauth,
            rclone::cancel_cloud_oauth,
            // lan discovery
            lan::discovery::list_lan_peers,
            lan::install::fetch_peer_games,
            lan::install::start_peer_install,
            lan::install::current_peer_download,
            lan::install::cancel_peer_install,
            lan::server::list_active_uploads,
            lan::server::cancel_upload,
            lan::server::get_manifest_status,
            lan::server::prepare_manifest,
            // runner
            runner::launch_game,
            runner::manual_backup,
            runner::refresh_save_metadata,
            runner::manual_restore,
            runner::pull_cloud_saves,
            runner::list_save_revisions,
            runner::restore_save_revision,
            runner::resolve_cloud_conflict,
            runner::get_cloud_conflict_details,
            // custom save locations (non-manifest games)
            custom_saves::set_custom_save,
            custom_saves::clear_custom_save,
            custom_saves::set_manifest_override,
            custom_saves::clear_manifest_override,
            custom_saves::derive_save_template,
            custom_saves::save_picker_start_dir,
            custom_saves::prefix_ready,
            // decky plugin installer (Linux / SteamOS)
            decky_install::decky_plugin_status,
            decky_install::install_decky_plugin,
            // open a path / external URL with the OS default handler (AppImage-safe)
            system_open::open_path,
            system_open::open_url,
            // gamepad presence (drives the "switch to Gamepad layout?" prompt)
            gamepad::any_gamepad_connected,
        ])
        .setup(move |app| {
            if attached {
                run_attached_launch(app, cli_mode)
            } else {
                run_normal_setup(app, cli_mode)
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    // Run with an exit-event interceptor so the app stays alive when
    // the last window closes (it's a tray app — only "Quit" exits).
    app.run(|app_handle, event| {
        match &event {
            // code is `Some(_)` when we explicitly called `app.exit()` — let
            // that through. Otherwise (last-window-closed), block the exit.
            RunEvent::ExitRequested { api, code, .. } if code.is_none() => {
                api.prevent_exit();
            }
            // Gate the gamepad bridge on window focus: a Spool window gaining
            // focus turns input on, losing it (alt-tab away, hide to tray,
            // game launch) turns it off. Fires for every window, so switching
            // between the library and a child window keeps it on.
            RunEvent::WindowEvent {
                event: WindowEvent::Focused(focused),
                ..
            } => {
                app_handle
                    .state::<gamepad::GamepadPresence>()
                    .set_active(*focused);
            }
            _ => {}
        }
    });
}

/// Looks up a game by exact `game_name` match. Returns the entry id.
/// Sync wrapper (block_on) for the cold-start / single-instance callback paths,
/// which run on the main thread rather than inside the async runtime.
fn find_game_id_by_name(
    library: &SharedLibrary,
    name: &str,
    exe_path: Option<&str>,
) -> Option<String> {
    tauri::async_runtime::block_on(library.find_id_by_name(name, exe_path))
        .ok()
        .flatten()
}

/// Re-stamp the rclone binary path **and** its timeout arguments into Spool's
/// owned ludusavi config on every boot.
///
/// Two reasons this must run each launch:
///   * Spool ships rclone as an AppImage sidecar, so on Linux the resolved
///     path lives inside the AppImage's FUSE mount (`/tmp/.mount_Spool_XXXX`)
///     whose name is randomised per launch — a path persisted last run is dead
///     today.
///   * The fast-fail timeout flags (see [`ludusavi_config::ensure_rclone_timeouts`])
///     must be present so `--cloud-sync` can't block a launch for minutes when
///     the save-sync remote is unreachable (the classic SteamOS Game-Mode boot,
///     before Wi-Fi is up).
///
/// Critically this is called from the attached Game-Mode launch path too — the
/// one place a wedged cloud sync is most painful (no window, just a splash) and
/// the one that historically skipped this step.
fn restamp_rclone(app: &AppHandle) {
    let rclone_args = app
        .state::<SharedConfig>()
        .lock()
        .ok()
        .map(|g| g.data.cloud.rclone_args.clone())
        .unwrap_or_default();
    // Only sync the user's rclone_args into ludusavi's config — not the binary
    // path. The path is always "rclone" (set by ensure_config) and resolved via
    // PATH injection in run_api, so each process uses its own bundled binary.
    if let Err(e) = ludusavi_config::set_cloud(None, None, None, None, Some(&rclone_args)) {
        tracing::warn!(error = %e, "failed to re-stamp rclone args");
    }
}

/// Size past which `debug.log` is rotated at startup. One previous file is
/// kept, so the pair stays under roughly twice this.
const LOG_ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// Copy an oversized `debug.log` to `debug.log.1` and empty the original, so a
/// new run starts clean with one previous run still readable.
///
/// Rotation is done here rather than with `tracing-appender`'s rolling
/// appenders because those name the live file after its date. The fixed name is
/// load-bearing: `paths::log_file()` is shown to the user in a launch-crash
/// message, and support asks for `debug.log` by name.
///
/// It copies and truncates rather than renaming, because every Spool process
/// runs this — it happens before the single-instance plugin, so a secondary
/// `spool --run` forwarding argv does it too — and several may hold the file
/// open at once (tray GUI, attached `--run`, headless Decky server). A rename
/// would leave the long-lived GUI writing to the renamed inode: `debug.log.1`
/// would then grow unbounded for the rest of its session while `debug.log` held
/// a couple of startup lines, and the next start would see a small file and
/// never reclaim it. Truncating keeps the inode, and the appender opens with
/// `O_APPEND`, so every writer seeks to the end on its next write — which is
/// now zero. They all carry on into the same file.
///
/// Because several processes can hit this at once, the check / copy / truncate
/// runs under a machine-wide lock ([`proc_lock::try_acquire_log_rotate`]).
/// Without it two processes both pass the size check, one truncates the log
/// between the other's check and its copy, and `debug.log.1` ends up a copy of
/// the just-emptied file — the previous run's log lost. A process that can't
/// take the lock skips rotation: whoever holds it is already doing the work.
///
/// Best-effort: if the copy fails the original is left alone rather than
/// emptied, and a large log beats no logging.
fn rotate_log_if_oversized(log: &std::path::Path) {
    if !log_is_oversized(log) {
        return;
    }
    let Ok(Some(_guard)) = proc_lock::try_acquire_log_rotate() else {
        return; // another Spool process is already rotating
    };
    rotate_oversized_log_locked(log);
}

/// `true` when `log` exists and is past [`LOG_ROTATE_BYTES`].
fn log_is_oversized(log: &std::path::Path) -> bool {
    std::fs::metadata(log).is_ok_and(|m| m.len() > LOG_ROTATE_BYTES)
}

/// The check / copy / truncate itself, run by [`rotate_log_if_oversized`] while
/// it holds the cross-process rotation lock. The size is re-checked here: another
/// process may have rotated between the unlocked check and the lock, and rotating
/// again would replace `debug.log.1` with the freshly emptied log.
fn rotate_oversized_log_locked(log: &std::path::Path) {
    if !log_is_oversized(log) {
        return; // no log, under the cap, or already rotated by another process
    }
    if let Err(e) = std::fs::copy(log, log.with_extension("log.1")) {
        eprintln!("spool: could not copy {} aside: {e}", log.display());
        return;
    }
    if let Err(e) = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(log)
    {
        eprintln!("spool: could not truncate {}: {e}", log.display());
    }
}

/// Initialises the global `tracing` subscriber. Two layers:
///
///   * **stderr** — what `cargo run` / `tauri dev` show in the terminal
///   * **file**   — appended to `%LOCALAPPDATA%\Spool\debug.log`, the same
///     path the C# app used, so existing support workflows
///     ("send me your debug.log") still work. Rotated at startup once it
///     passes [`LOG_ROTATE_BYTES`] — see [`rotate_log_if_oversized`].
///
/// Default verbosity: `info`, with the noisy crates (tauri / hyper /
/// reqwest / h2) clamped to `warn`. Override with `SPOOL_LOG=debug` (or
/// any standard `EnvFilter` spec) to widen.
///
/// Returns the non-blocking worker guard; binding it to the call frame
/// keeps the writer alive and flushes buffered lines at shutdown.
fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let log_dir = paths::app_data_dir();
    let _ = std::fs::create_dir_all(&log_dir);

    rotate_log_if_oversized(&paths::log_file());
    let file_appender = tracing_appender::rolling::never(&log_dir, "debug.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_env("SPOOL_LOG").unwrap_or_else(|_| {
        // gilrs/gilrs_core clamped to error: their force-feedback thread spams a
        // benign "force feedback loop took more than 50ms" warning, and we don't
        // use rumble. Our own "gamepad bridge" logs are under the `spool` target,
        // so they're unaffected.
        EnvFilter::new(
            "info,tauri=warn,h2=warn,hyper=warn,hyper_util=warn,reqwest=warn,rustls=warn,gilrs=error,gilrs_core=error",
        )
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_target(false).with_writer(std::io::stderr))
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_writer(file_writer),
        )
        .init();

    guard
}

/// Routes panics through `tracing` so they land in `debug.log` with a
/// backtrace, then chains to the previous hook. Rust's default panic
/// handler writes to stderr, which is invisible in a windowed/tray app with
/// no console — and a panic in a spawned task whose `JoinHandle` is dropped
/// is swallowed entirely. That combination once hid an axum router-build
/// panic that silently killed LAN discovery. The backtrace is force-captured
/// so it's present regardless of `RUST_BACKTRACE`, paid only on an actual
/// panic.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".into());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".into());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(%location, %message, "panic\n{backtrace}");
        default(info);
    }));
}

/// Dispatches a forwarded secondary-launch's argv. Either focuses the
/// library (no args) or queues a game launch (`--run "Name" "Exe"`).
fn handle_forwarded_launch(app: &AppHandle, argv: &[String]) {
    match cli::parse_args(argv) {
        CliMode::Run {
            game_name,
            exe_path,
            ..
        } => {
            tray::show_library(app); // bring the window up so the user sees the workflow run
            let Some(id) =
                find_game_id_by_name(&app.state::<SharedLibrary>(), &game_name, Some(&exe_path))
            else {
                tracing::warn!(name = %game_name, "forwarded --run: no library entry matches");
                return;
            };
            let app_clone = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = runner::launch_game_inner(&app_clone, &id).await {
                    tracing::error!(error = %e, "forwarded --run workflow failed");
                }
            });
        }
        CliMode::Normal => tray::show_library(app),
        CliMode::HeadlessServer => {
            // --headless-server doesn't register with single-instance, so
            // this branch is unreachable in practice.
        }
    }
}

/// Attached Game-Mode launch: no tray, no pollers, no library window. Show a
/// splash, launch the game from Rust, then exit when the workflow ends so the
/// host (SteamOS gamescope / Apollo / Sunshine) sees the game stop with Spool.
fn run_attached_launch(
    app: &tauri::App,
    cli_mode: CliMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let CliMode::Run {
        game_name,
        exe_path,
        ..
    } = cli_mode
    else {
        app.handle().exit(1);
        return Ok(());
    };
    let Some(id) = find_game_id_by_name(&app.state::<SharedLibrary>(), &game_name, Some(&exe_path))
    else {
        tracing::error!(name = %game_name, "attached --run: no library entry matches");
        app.handle().exit(1);
        return Ok(());
    };

    // Write the session record (appid matches the Steam shortcut).
    if let Some(exe) = paths::spool_executable() {
        let appid = session::compute_steam_appid(&exe.to_string_lossy(), &game_name);
        if let Err(e) = session::write_start(&game_name, appid) {
            tracing::warn!(error = %e, "failed to write active-session record");
        }
    }

    // Make sure ludusavi config exists before the workflow runs.
    if let Err(e) = ludusavi_config::ensure_config() {
        tracing::warn!(error = %e, "failed to initialise ludusavi config dir");
    }

    // Re-stamp the rclone path + fast-fail cloud-sync timeouts. The normal
    // startup branch does this; the attached launch skipped it, leaving a stale
    // AppImage-mount rclone path and (worse) unbounded rclone retries that
    // wedged the restore phase forever when the cloud remote was unreachable at
    // Game-Mode boot. Game Mode is exactly where that hang hurts most — there's
    // no window to recover from, just a splash.
    restamp_rclone(app.handle());

    // Controller input for the splash (conflict-resolution modal etc.). The
    // attached path runs none of the normal startup tasks, so start the bridge
    // here explicitly — this is the surface where controller nav matters most.
    gamepad::spawn_bridge(app.handle().clone());

    // Splash window (the `main` window stays hidden / unused).
    if let Err(e) =
        tauri::WebviewWindowBuilder::new(app, "splash", tauri::WebviewUrl::App("splash".into()))
            .title("Spool")
            .decorations(false)
            .fullscreen(true)
            .resizable(false)
            .build()
    {
        tracing::warn!(error = %e, "failed to create splash window");
    }

    // Launch + exit when done. app.exit(0) lets Steam see the game stop
    // (RunEvent::ExitRequested only blocks code.is_none()).
    //
    // Wait for the splash to wire its `run:phase` listener before starting the
    // workflow — otherwise the restoring/launching/playing phases fire before
    // the webview is listening and the splash sits on its default "Restoring
    // saves…" label for the whole session. A timeout fallback keeps a webview
    // that never loads from blocking the launch entirely.
    let app_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        {
            let ready = app_handle.state::<SplashReady>();
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(5), ready.notify.notified())
                    .await;
        }
        if let Err(e) = runner::launch_game_inner(&app_handle, &id).await {
            tracing::error!(error = %e, "attached --run workflow failed");
            // A cloud-sync conflict is interactive: the workflow emitted an
            // `error` phase and the splash is now showing CloudConflictModal.
            // Return WITHOUT exiting so the app stays alive for the user to
            // resolve/retry/cancel — the splash's modal handlers call
            // `exit(0)` themselves once the user is done (see splash/+page.svelte).
            // Exiting here would tear the modal down before they can act.
            if e.to_string().contains("Cloud sync conflict") {
                return;
            }
            // Any other error is terminal and non-interactive: hold it on the
            // splash briefly so the user can read the reason (e.g. a restore
            // timeout) before we exit and hand control back to the host.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        app_handle.exit(0);
    });

    Ok(())
}

/// Normal tray-resident startup: mount the tray, wire the window-close→hide
/// behaviour, ensure the ludusavi config, kick off background pollers, and
/// queue any startup `--run` for the frontend to pick up.
fn run_normal_setup(app: &tauri::App, cli_mode: CliMode) -> Result<(), Box<dyn std::error::Error>> {
    // Mount tray icon + menu.
    tray::mount_tray(app.handle())?;

    // Intercept the main window's close button → hide instead. First time the
    // user does this, emit `tray:first-hide` so the library page can surface a
    // sticky info toast explaining where Spool went. The toast survives the
    // hide (it's in component state) so the user sees it next time they re-open
    // the window.
    if let Some(main) = app.get_webview_window("main") {
        let win = main.clone();
        let app_handle = app.handle().clone();
        // In Steam Game Mode, gamescope composites a single window onto the
        // whole output. The window-state plugin restores a *windowed* size from
        // a prior desktop session, which gamescope then pillarboxes (black bars
        // on the sides). Force fullscreen so it fills the screen. Fullscreen
        // isn't one of the persisted window-state flags, so desktop sessions
        // don't inherit it. Done while still hidden to avoid a windowed flash.
        if gamemode::is_steam_game_mode() {
            if let Err(e) = main.set_fullscreen(true) {
                tracing::warn!(error = %e, "failed to set main window fullscreen in Game Mode");
            }
            // gamescope advertises a device scale factor of 1 and doesn't carry
            // over the desktop session's display scaling (e.g. KDE's 150%), so
            // the webview renders at "100%" and the UI looks tiny on a handheld.
            // Scale the webview up to compensate. The factor is overridable via
            // $SPOOL_GAMEMODE_ZOOM for screens where 1.5 is too much/little.
            let zoom = gamemode::game_mode_zoom();
            if let Err(e) = main.set_zoom(zoom) {
                tracing::warn!(error = %e, zoom, "failed to set main window zoom in Game Mode");
            }
        }
        // `main` is now created hidden — show it explicitly (also removes the
        // startup white-flash).
        let _ = main.show();
        main.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = win.hide();
                tray::emit_tray_intro_once(&app_handle);
            }
        });
    }

    // Ensure Spool's owned ludusavi config dir + config.yaml exist and meet the
    // required invariants (backup path, manifest enabled, simple format).
    // Idempotent — fast no-op on subsequent launches.
    if let Err(e) = ludusavi_config::ensure_config() {
        tracing::warn!(error = %e, "failed to initialise ludusavi config dir");
    }

    // Re-stamp the rclone binary path + timeout args on every startup (see
    // `restamp_rclone`). Keeps `apps.rclone.path` pointing at the sidecar that
    // exists this session and guarantees the fast-fail cloud-sync flags are
    // present.
    restamp_rclone(app.handle());

    // Background pollers + one-shot startup backfills.
    spawn_startup_tasks(app.handle());

    // Startup --run dispatch: queue the game id so the frontend can pick it up
    // once its listeners are ready.
    if let CliMode::Run {
        ref game_name,
        ref exe_path,
        ..
    } = cli_mode
    {
        let library = app.state::<SharedLibrary>();
        let pending = app.state::<PendingRun>();
        if let Some(id) = find_game_id_by_name(&library, game_name, Some(exe_path)) {
            pending.set(id);
        } else {
            tracing::warn!(name = %game_name, "startup --run: no library entry matches");
        }
    }

    Ok(())
}

/// Spawns the long-lived background tasks and one-shot startup backfills: LAN
/// discovery, accent/size/metadata backfills, the cloud reachability poller,
/// and the cross-device device-blob fold. Each logs and degrades gracefully on
/// its own; none block startup.
fn spawn_startup_tasks(app: &AppHandle) {
    // Kick off LAN peer discovery in the background. Logs and skips if the
    // socket can't bind (port in use, firewall, etc.) — peer count stays at 0
    // in that case but everything else keeps working.
    lan::spawn_discovery(app.clone());

    // Warm the LAN manifest hash cache: prepare every shared game's manifest
    // (one at a time, after a short delay) so a peer's first request is served
    // instantly instead of waiting out a full folder hash (#435). With the
    // persisted cache this is usually just mtime stats per file.
    lan::spawn_manifest_warm(app.clone());

    // Read the controller in Rust and forward events to the webview as
    // `gamepad:input` (the webview can't see pads itself on Linux WebKitGTK).
    gamepad::spawn_bridge(app.clone());

    // Backfill accent colours for any legacy entries that have a cover but no
    // extracted accent yet. Cheap no-op when every entry is already filled.
    accent_backfill::spawn_backfill(app.clone());

    // Backfill install sizes for entries that have a folder on disk but no
    // recorded size — legacy C# library entries land here with
    // `install_size_mb: 0`. Walks the folder, sums file sizes, saves once.
    size_backfill::spawn_backfill(app.clone());

    // Backfill Steam Store metadata (description, developer, publisher, genres,
    // release date) for entries that have a steam_id but empty metadata fields.
    // Throttled to respect the store endpoint's rate limit. Skipped in offline
    // mode — every fetch would just wait out its timeout; the backfill re-runs
    // on the next online boot.
    if !config::offline_mode_enabled() {
        metadata_backfill::spawn_backfill(app.clone());
    }

    // Register the health sink so real control-plane ops can report cloud
    // reachability passively, then run a single startup probe. After this there's
    // no periodic poll — the chrome cloud icon is kept current from the
    // success/failure of claim/heartbeat/backup/fold ops (and the Settings
    // refresh button), so an idle tray-resident Spool doesn't draw on the
    // shared, quota-limited rclone remote once a minute. No-op (Unconfigured)
    // when cloud saves aren't set up.
    rclone::init_health_sink(app.clone());
    rclone::spawn_initial_sync_probe(app.clone());

    // One-shot cross-device fold: list per-device blobs in the remote, sum
    // playtime / take max last-played / derive the badge, merge into the
    // library. Runs ~4 s after launch, after the startup reachability probe.
    rclone::spawn_startup_fold(app.clone());

    // Adopt any cross-device custom-save definitions (so a save folder picked on
    // another device applies here without re-picking), then write the
    // `customGames` block so non-manifest games are recognised by ludusavi.
    custom_saves::spawn_startup_adopt(app.clone());

    // Notice library writes made by *other* Spool processes (the attached
    // `--run` launch, the headless Decky server) by polling the DB's version
    // counter — Tauri's `library:changed` event only reaches this process.
    spawn_library_change_poll(app.clone());
}

/// Polls the library DB's `meta.version` counter and re-emits `library:changed`
/// whenever it advances, so the GUI refreshes after another Spool process
/// writes the shared database. In-process mutations emit `library:changed`
/// directly for instant feedback; this only covers external writers (it may
/// fire one extra refresh shortly after a local write, which is harmless).
fn spawn_library_change_poll(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let library = app.state::<SharedLibrary>().inner().clone();
        let mut last = library.version().await.unwrap_or(0);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            match library.version().await {
                Ok(v) if v != last => {
                    last = v;
                    let _ = app.emit("library:changed", &());
                }
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // The behaviour tests drive `rotate_oversized_log_locked` directly: the
    // public `rotate_log_if_oversized` takes a single machine-wide lock, so
    // exercising it from several tests running in parallel would have them all
    // contend on one real lock file. `rotate_wrapper_rotates_when_uncontended`
    // covers the lock path once.

    #[test]
    fn rotate_empties_the_log_in_place_and_keeps_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("debug.log");
        std::fs::write(&log, vec![b'x'; (LOG_ROTATE_BYTES + 1) as usize]).unwrap();

        rotate_oversized_log_locked(&log);

        // The live file keeps its name and stays present — support asks for
        // debug.log, and the launch-crash message points at it.
        assert!(log.exists());
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
        assert_eq!(
            std::fs::metadata(dir.path().join("debug.log.1")).unwrap().len(),
            LOG_ROTATE_BYTES + 1
        );
    }

    #[test]
    fn rotate_wrapper_rotates_when_uncontended() {
        // The lock path end to end: nothing else holds the rotation lock, so
        // the wrapper should acquire it and do the rotation.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("debug.log");
        std::fs::write(&log, vec![b'x'; (LOG_ROTATE_BYTES + 1) as usize]).unwrap();

        rotate_log_if_oversized(&log);

        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
        assert!(dir.path().join("debug.log.1").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rotate_keeps_the_same_inode() {
        // The property the whole approach rests on: every Spool process runs
        // this, and others may already hold the file open. Truncating in place
        // means their O_APPEND writes continue into this same file, where a
        // rename would strand them on the renamed inode.
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("debug.log");
        std::fs::write(&log, vec![b'x'; (LOG_ROTATE_BYTES + 1) as usize]).unwrap();
        let before = std::fs::metadata(&log).unwrap().ino();

        rotate_oversized_log_locked(&log);

        assert_eq!(std::fs::metadata(&log).unwrap().ino(), before);
    }

    #[test]
    fn rotate_leaves_a_log_under_the_cap_alone() {
        // Rotating on every start would throw away the history support asks for.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("debug.log");
        std::fs::write(&log, b"a few lines").unwrap();

        rotate_oversized_log_locked(&log);

        assert_eq!(std::fs::read(&log).unwrap(), b"a few lines");
        assert!(!dir.path().join("debug.log.1").exists());
    }

    #[test]
    fn rotate_replaces_the_previous_rotation() {
        // Two files, not an ever-growing pile of them.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("debug.log");
        let previous = dir.path().join("debug.log.1");
        std::fs::write(&previous, b"older run").unwrap();
        std::fs::write(&log, vec![b'x'; (LOG_ROTATE_BYTES + 1) as usize]).unwrap();

        rotate_oversized_log_locked(&log);

        assert_eq!(
            std::fs::metadata(&previous).unwrap().len(),
            LOG_ROTATE_BYTES + 1,
            "the previous rotation should be replaced, not kept alongside"
        );
        assert!(!dir.path().join("debug.log.2").exists());
    }

    #[test]
    fn rotate_is_a_no_op_when_there_is_no_log_yet() {
        let dir = tempfile::tempdir().unwrap();
        rotate_oversized_log_locked(&dir.path().join("debug.log"));
        assert!(!dir.path().join("debug.log.1").exists());
    }
}
