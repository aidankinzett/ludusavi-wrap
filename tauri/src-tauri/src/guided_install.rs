//! Guided Windows-installer flow (Linux).
//!
//! Windows game installers (`setup.exe`) are Windows programs, so on Linux they
//! have to run through Proton. This module wraps that: it creates a clean host
//! folder for the game, builds a dedicated Wine prefix, mounts the host folder
//! into that prefix as a drive letter (so the installer can write there and the
//! game still lands on the host filesystem), then runs the installer through
//! umu-run and waits for it to finish.
//!
//! The prefix is kept and handed back to the caller as `prefix_path`. The
//! frontend passes it to `add_game` as `wine_prefix_path` so the installed game
//! launches in the same prefix it was installed into — keeping any vcredist /
//! dotnet / registry state the installer set up.
//!
//! Linux-only: on other platforms the command returns an error (Windows users
//! run `setup.exe` natively and use Add Game).

use crate::error::AppResult;
use serde::Serialize;
use tauri::AppHandle;
#[cfg(target_os = "linux")]
use tauri::Emitter;

/// Result of a guided install, returned to the frontend. `prefix_path` is
/// forwarded to `add_game` as `wine_prefix_path`; `drive_letter` is shown to
/// the user as the install target ("install into the D: drive").
#[derive(Debug, Clone, Serialize)]
pub struct GuidedInstallResult {
    /// Host folder the game was installed into (the mounted drive's target).
    pub install_dir: String,
    /// Wine prefix ROOT the installer ran in — reused at launch time.
    pub prefix_path: String,
    /// Wine drive letter the install folder was mounted as, e.g. `"D:"`.
    pub drive_letter: String,
    /// Proton build dir used for this install (absolute path). Saved onto the
    /// game entry so launches use the same Proton version the prefix was built
    /// with. `None` when umu-run used its own bundled default.
    pub proton_path: Option<String>,
}

/// Runs a Windows `setup.exe` through Proton/umu with a clean host folder
/// mounted as a Wine drive, and waits for the installer to exit.
///
/// `game_name` only seeds the default install folder name; the caller confirms
/// the real game name later when adding to the library. `install_dir_override`
/// is the library folder the game's install folder gets created under — a
/// safe-filename subfolder of `game_name` is appended the same way the
/// no-override default (`installed_games_dir()`) is, so callers just pass a
/// library folder root, not the final directory. `proton_version_override`
/// pins a specific Proton build for the installer (path to a Proton dir);
/// when absent the backend prefers any installed GE-Proton, then the global
/// config default, then umu-run's own bundled default.
#[tauri::command]
pub async fn run_guided_installer(
    app: AppHandle,
    setup_exe: String,
    game_name: String,
    install_dir_override: Option<String>,
    proton_version_override: Option<String>,
) -> AppResult<GuidedInstallResult> {
    run_impl(
        app,
        setup_exe,
        game_name,
        install_dir_override,
        proton_version_override,
    )
    .await
}

#[cfg(target_os = "linux")]
async fn run_impl(
    app: AppHandle,
    setup_exe: String,
    game_name: String,
    install_dir_override: Option<String>,
    proton_version_override: Option<String>,
) -> AppResult<GuidedInstallResult> {
    use crate::config::SharedConfig;
    use crate::error::AppError;
    use crate::library::make_safe_filename;
    use crate::process::{run_game, LaunchSpec};
    use crate::{paths, proton};
    use std::path::PathBuf;
    use tauri::Manager;

    let setup_path = PathBuf::from(&setup_exe);
    if !setup_path.is_file() {
        return Err(AppError::Other(format!("installer not found: {setup_exe}")));
    }

    // Resolve umu-run + default Proton from config, mirroring proton.rs.
    let (umu_run_path, default_proton_path) = {
        let config = app.state::<SharedConfig>();
        let cfg = config.lock().map_err(|_| AppError::LockPoisoned)?;
        (
            cfg.data.launch.umu_run_path.clone(),
            cfg.data.launch.default_proton_path.clone(),
        )
    };
    let umu_run = proton::resolve_umu_run(Some(&umu_run_path))?;

    // Explicit override → global config default → umu-run's own bundled default.
    // We don't auto-select GE-Proton here: in practice UMU-Proton is more
    // reliable for running these installers (better decompressor compatibility),
    // while GE-Proton is preferable for the installed game at launch time.
    let proton_path = proton_version_override
        .as_deref()
        .and_then(|p| proton::resolve_proton_path(Some(p), None))
        .or_else(|| proton::resolve_proton_path(None, Some(&default_proton_path)));

    // Clean host folder for the game, under the chosen library folder (or
    // Spool's app-data folder when the user didn't pick one).
    let install_root = match install_dir_override {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => paths::installed_games_dir(),
    };
    let install_dir = install_root.join(make_safe_filename(&game_name));
    let install_dir_existed = install_dir.exists();
    std::fs::create_dir_all(&install_dir)
        .map_err(|e| AppError::Other(format!("failed to create install folder: {e}")))?;

    // Dedicated prefix for this install. The id is throwaway — its only job is
    // to give the prefix a stable path we hand back as wine_prefix_path.
    let install_id = uuid::Uuid::new_v4().to_string();
    let prefix_root = proton::game_prefix_path(&install_id);

    // Run the remaining setup and installer execution in an async block
    // to catch any error and perform cleanup of prefix_root and install_dir.
    let run_steps = async {
        // Keep the machine awake for the whole flow, not just the installer:
        // init_prefix() below can download the Steam Linux Runtime + Proton on
        // first run (multi-minute), and suspending mid-install can leave a
        // half-written game. Held until this async block returns. Desktop power
        // UIs (e.g. KDE's battery applet) show this as Spool blocking sleep.
        let _sleep_guard =
            crate::sleep_inhibit::SleepInhibitor::acquire(&format!("Installing {game_name}")).await;

        std::fs::create_dir_all(&prefix_root)
            .map_err(|e| AppError::Other(format!("failed to create prefix dir: {e}")))?;

        // Build the prefix so dosdevices/ exists before we mount a drive into it.
        init_prefix(&umu_run, &prefix_root, proton_path.as_deref(), &install_id).await?;

        // Mount the install folder as a free drive letter.
        let dosdevices = prefix_root.join("dosdevices");
        std::fs::create_dir_all(&dosdevices)
            .map_err(|e| AppError::Other(format!("failed to create dosdevices dir: {e}")))?;
        let letter = pick_free_drive_letter(&dosdevices).ok_or_else(|| {
            AppError::Other("no free Wine drive letter to mount the install folder".into())
        })?;
        let link = dosdevices.join(format!("{letter}:"));
        let _ = std::fs::remove_file(&link); // fresh prefix, but be tolerant
        std::os::unix::fs::symlink(&install_dir, &link)
            .map_err(|e| AppError::Other(format!("failed to mount install drive: {e}")))?;

        // Tell the frontend the drive letter now, before the installer blocks.
        let _ = app.emit(
            "install:drive-ready",
            format!("{}:", letter.to_ascii_uppercase()),
        );

        // Run the installer and wait for it to exit. run_game handles strip-appimage
        // env + cwd; the setup.exe's window staying open blocks here intentionally.
        //
        // WINE_LARGE_ADDRESS_AWARE=1: some installers decompress large archives
        // in-process. Wine's default virtual address space is too small, causing
        // ISDone.dll / unarc.dll error codes -5/-11/-12 ("not enough memory") even
        // when the machine has plenty of RAM. This flag widens the 32-bit address
        // space so the decompressor can allocate the buffers it needs.
        run_game(
            &setup_path,
            LaunchSpec::Proton {
                umu_run: &umu_run,
                prefix_root: &prefix_root,
                proton_path: proton_path.as_deref(),
                game_id: &install_id,
                extra_args: &[],
                extra_env: &[("WINE_LARGE_ADDRESS_AWARE", "1")],
            },
        )
        .await
        .map(|_| ())?;

        Ok::<_, AppError>(letter)
    };

    match run_steps.await {
        Ok(letter) => Ok(GuidedInstallResult {
            install_dir: install_dir.to_string_lossy().to_string(),
            prefix_path: prefix_root.to_string_lossy().to_string(),
            drive_letter: format!("{}:", letter.to_ascii_uppercase()),
            proton_path: proton_path.map(|p| p.to_string_lossy().to_string()),
        }),
        Err(e) => {
            // Clean up the prefix root and the host install folder (best effort).
            let _ = std::fs::remove_dir_all(&prefix_root);
            if !install_dir_existed {
                let _ = std::fs::remove_dir_all(&install_dir);
            }
            Err(e)
        }
    }
}

/// Initialise the Wine prefix so its `dosdevices/` directory exists before we
/// mount a drive into it. umu-run builds a prefix without launching anything
/// when handed an empty program argument (`umu-run ""`). Best-effort: umu
/// emitting a non-zero status here (e.g. a protonfixes warning) shouldn't abort
/// the install, since the installer launch would build the prefix anyway.
#[cfg(target_os = "linux")]
async fn init_prefix(
    umu_run: &std::path::Path,
    prefix_root: &std::path::Path,
    proton_path: Option<&std::path::Path>,
    install_id: &str,
) -> AppResult<()> {
    use crate::process::{run_game, LaunchSpec};

    // Hand umu-run an empty program argument so it builds the prefix without
    // launching anything. Goes through run_game (same spawn/strip-appimage-env
    // path as the installer launch) for consistency.
    let res = run_game(
        std::path::Path::new(""),
        LaunchSpec::Proton {
            umu_run,
            prefix_root,
            proton_path,
            game_id: install_id,
            extra_args: &[],
            extra_env: &[],
        },
    )
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "umu createprefix failed; continuing (installer will build the prefix)");
    }
    Ok(())
}

/// Pick a free Wine drive letter to mount the install folder as. Skips letters
/// already present in `dosdevices/` (typically `c:` and `z:`), and avoids
/// `a`/`b` (floppies) and `c`/`z` (system + root).
#[cfg(target_os = "linux")]
fn pick_free_drive_letter(dosdevices: &std::path::Path) -> Option<char> {
    let mut used = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(dosdevices) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            // Entries look like "c:" (drive) or "c::" (device). Take the letter.
            if name.contains(':') {
                if let Some(first) = name.chars().next() {
                    used.insert(first);
                }
            }
        }
    }
    ('d'..='y').find(|c| !used.contains(c))
}

#[cfg(not(target_os = "linux"))]
async fn run_impl(
    _app: AppHandle,
    _setup_exe: String,
    _game_name: String,
    _install_dir_override: Option<String>,
    _proton_version_override: Option<String>,
) -> AppResult<GuidedInstallResult> {
    Err(crate::error::AppError::Other(
        "The guided installer runs Windows installers through Proton and is only available on Linux. On Windows, run the installer directly and use Add Game.".into(),
    ))
}
