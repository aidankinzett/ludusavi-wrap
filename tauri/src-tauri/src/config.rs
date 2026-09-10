//! Application settings — persisted to `%LOCALAPPDATA%\Spool\config.json`.
//!
//! The on-disk format mirrors the C# `ConfigData` exactly so an existing
//! Spool installation's config loads without migration. Fields that aren't
//! yet exposed in the new UI (LAN share) are still modelled so
//! the file round-trips cleanly with the C# app — they're just inert until
//! v2.

use crate::error::{AppError, AppResult};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

/// UI density / layout mode. `Auto` resolves at runtime (frontend) to
/// `desktop` or `touch` from pointer + panel size; `Desktop`/`Touch` force
/// it. Serialized lowercase (`"auto"`/`"desktop"`/`"touch"`) to match the
/// `UiMode` union in types.ts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiMode {
    #[default]
    Auto,
    Desktop,
    Touch,
}

/// On-disk shape. Every field has a default so older config.json files
/// without newer fields parse cleanly via `#[serde(default)]`.
///
/// The cloud / LAN / Proton-launch settings are grouped into sub-structs for
/// clarity in Rust, but `#[serde(flatten)]` keeps the on-disk JSON flat — the
/// historical `cloud_*` / `lan_*` / `umu_run_path` keys are unchanged, so
/// existing config.json files and the frontend's flat `ConfigData` mirror load
/// without migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigData {
    pub steamgriddb_enabled: bool,
    pub steamgriddb_api_key: String,
    pub spool_exe: String,

    // ── Identity (assigned once at first run) ────────────────────────────
    pub device_id: String,
    pub device_name: String,

    /// Touch-optimised UI mode (handheld). Resolved to a concrete
    /// desktop/touch density at boot by `lib/uiMode.svelte.ts`.
    pub ui_mode: UiMode,

    /// Set to true after the user has been shown the "Spool is in the tray"
    /// intro toast at least once. Defaults to false on legacy configs (and
    /// new installs) so the toast appears on the first close-to-tray.
    pub tray_intro_seen: bool,

    /// Set to true once the first-run onboarding flow has been finished (or
    /// dismissed). Defaults to false so it shows on a fresh install. A
    /// pre-existing config that predates this field is migrated to `true` on
    /// load (see `migrate_onboarding_completed`) so upgrading users don't get
    /// the first-run flow thrown at them.
    pub onboarding_completed: bool,

    /// The bundled Decky plugin version the user was last nudged to update to
    /// (Linux). The library window shows a one-time "plugin update available"
    /// toast when the AppImage bundles a plugin newer than the installed copy;
    /// this records the version that toast was shown for so it fires only once
    /// per bundled version instead of on every launch. Empty until the first
    /// such toast.
    pub decky_update_notified_version: String,

    /// Number of full save revisions ludusavi retains per game (the
    /// `backup.retention.full` knob). More revisions = more rollback points,
    /// at the cost of more disk + cloud upload per game. Differentials stay at
    /// 0 (see `ludusavi_config::ensure_config`). Default 5; clamped to 3–10
    /// when applied. The floor is 3, not 1: with `full == 1` ludusavi reuses a
    /// single in-place backup and overwrites the save files directly, so a
    /// force-kill (Steam Game Mode) mid-backup can truncate the only copy. From
    /// 2+ each run writes a fresh generation, leaving the prior good backup
    /// intact as a safety net — saves are small, so the disk cost is trivial.
    pub save_retention_full: u32,

    /// User-managed install roots (typically one per drive). Each is a folder
    /// where game installs can live; the "Move install" flow lists these as
    /// destinations and LAN downloads land in the default-install one (see
    /// [`ConfigData::lan_install_root`]). Empty by default — adding one
    /// (Settings → Library folders) creates a `Spool/` subfolder on the chosen
    /// drive. A flat top-level `library_folders` array on disk.
    pub library_folders: Vec<LibraryFolder>,

    /// User-defined game collections (Library sidebar). Manual collections, a
    /// game can belong to many, each carries its own accent colour. Membership
    /// is stored on the collection (a list of game ids) rather than per-game so
    /// it mirrors the sidebar's data model directly and adding a field to a game
    /// never touches collections. Edited only by the library window; persisted
    /// here (GUI-owned, single-writer) alongside the rest of the config. Game
    /// ids that no longer exist are inert — membership filtering simply matches
    /// nothing for them.
    pub collections: Vec<Collection>,

    /// "Offline mode": all cloud/network work is paused — ludusavi cloud sync,
    /// the rclone control plane (session markers, folds, probes), the umu
    /// runtime-update check, and the metadata backfill. Toggled by the
    /// `go_offline` / `go_online` commands (Settings → Cloud sync), which also
    /// run the prepare/reconcile steps around the flip. Read across processes
    /// via [`offline_mode_enabled`] since the headless server and attached
    /// `--run` workflows each load their own config.
    pub offline_mode: bool,

    /// Cloud-save / rclone settings (flattened to the flat `cloud_*` JSON keys).
    #[serde(flatten)]
    pub cloud: CloudConfig,

    /// LAN game-sharing settings (flattened to the flat `lan_*` JSON keys).
    #[serde(flatten)]
    pub lan: LanConfig,

    /// Proton / Linux launch settings (flattened; field names match their keys).
    #[serde(flatten)]
    pub launch: LaunchConfig,
}

impl Default for ConfigData {
    fn default() -> Self {
        Self {
            steamgriddb_enabled: false,
            steamgriddb_api_key: String::new(),
            spool_exe: String::new(),
            device_id: String::new(),
            device_name: String::new(),
            ui_mode: UiMode::default(),
            tray_intro_seen: false,
            onboarding_completed: false,
            decky_update_notified_version: String::new(),
            save_retention_full: 5,
            library_folders: Vec::new(),
            collections: Vec::new(),
            offline_mode: false,
            cloud: CloudConfig::default(),
            lan: LanConfig::default(),
            launch: LaunchConfig::default(),
        }
    }
}

/// One user-managed install root. `path` is the folder games are moved into
/// (the "Move install" flow appends `<game folder name>` under it); `label` is
/// an optional friendly name shown in the UI (falls back to the path / drive
/// when unset). `default_install` marks the folder new installs (LAN
/// downloads) land in — at most one folder carries it, and when none does the
/// first folder acts as the default (see [`ConfigData::lan_install_root`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryFolder {
    pub path: String,
    pub label: Option<String>,
    pub default_install: bool,
}

/// One user-defined game collection. `id` is a stable client-minted id, `name`
/// the display label, `accent` a hex colour (`#rrggbb`) used for the collection's
/// dot/highlight, and `games` the ids of its member games. A game can appear in
/// any number of collections; the same game id may therefore live in several
/// `games` lists. Stored as-is — the library window owns all validation
/// (non-empty name, palette colours), so this round-trips the frontend shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub accent: String,
    pub games: Vec<String>,
}

/// Cloud-save + rclone settings. Flattened into [`ConfigData`], so each field
/// maps to its historical flat JSON key (`cloud_provider`, …) — existing
/// config.json files and the frontend's flat mirror round-trip unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudConfig {
    #[serde(rename = "cloud_provider")]
    pub provider: String,
    #[serde(rename = "cloud_remote")]
    pub remote: String,
    /// Base folder on the remote. Ludusavi saves go to
    /// `<base_path>/ludusavi-backup`; Spool's cross-device control plane
    /// (session markers, per-device blobs) lives under `<base_path>/_spool`.
    #[serde(rename = "cloud_base_path")]
    pub base_path: String,
    /// Legacy: the exact ludusavi remote subpath. Superseded by `base_path`
    /// (the ludusavi path is now derived). Kept for JSON round-trip with older
    /// config files; no longer read.
    #[serde(rename = "cloud_path")]
    pub path: String,
    pub rclone_args: String,
    /// WebDAV connection details for the `webdav` provider. Written when the
    /// user connects a WebDAV remote (manually or via the self-hosted Spool
    /// server). The password is never stored here — ludusavi obscures it into
    /// rclone.conf; these two fields only let the settings form re-display the
    /// active connection.
    #[serde(rename = "cloud_webdav_url")]
    pub webdav_url: String,
    #[serde(rename = "cloud_webdav_username")]
    pub webdav_username: String,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            remote: String::new(),
            base_path: "Spool".to_string(),
            path: "Spool/ludusavi-backup".to_string(),
            rclone_args: "--fast-list --ignore-checksum".to_string(),
            webdav_url: String::new(),
            webdav_username: String::new(),
        }
    }
}

/// LAN game-sharing settings. Flattened into [`ConfigData`] with the historical
/// flat `lan_*` JSON keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LanConfig {
    #[serde(rename = "lan_share_enabled")]
    pub share_enabled: bool,
    #[serde(rename = "lan_share_port")]
    pub share_port: u16,
    /// Legacy LAN install dir. Superseded by library folders — kept only so
    /// `migrate_lan_install_dir` can read a pre-library-folders config and
    /// convert the value into a `LibraryFolder` (after which it's cleared).
    /// Nothing else reads it; resolution goes through
    /// [`ConfigData::lan_install_root`].
    #[serde(rename = "lan_install_dir")]
    pub install_dir: String,
    /// Max aggregate LAN download throughput in Mbps (megabits/s, decimal).
    /// `0` = unlimited. Applied across all parallel file fetches by the throttle
    /// in `download_one_file` — convergent rather than precise, so brief bursts
    /// can exceed the cap before the next sleep brings the average back.
    #[serde(rename = "lan_download_max_mbps")]
    pub download_max_mbps: f64,
}

impl Default for LanConfig {
    fn default() -> Self {
        Self {
            share_enabled: true,
            share_port: 47632,
            install_dir: String::new(),
            download_max_mbps: 0.0,
        }
    }
}

/// Proton / Linux launch settings. Flattened into [`ConfigData`]; the field
/// names already match their JSON keys, so no renames are needed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchConfig {
    /// Path to the `umu-run` launcher. `""` = autodetect (`/usr/bin/umu-run`
    /// then PATH). Linux-only; ignored on Windows.
    pub umu_run_path: String,
    /// Default Proton build directory used when a game doesn't override it.
    /// `""` = auto-pick the newest discovered Proton.
    pub default_proton_path: String,
}

/// Wrapper around [`ConfigData`] handling persistence and one-time setup
/// (device identity, ludusavi auto-detect, current-exe stamping).
#[derive(Debug, Default)]
pub struct Config {
    pub data: ConfigData,
}

impl Config {
    /// Loads from disk, then runs first-time setup if needed (device ID,
    /// umu-run auto-detection, current exe path). Saves if any of those
    /// touched the data so the on-disk file matches in-memory state.
    pub fn load() -> AppResult<Self> {
        let path = paths::config_file();
        // Keep the raw JSON so the onboarding migration can tell a key that
        // was absent (legacy file) from one explicitly set to false.
        let raw_json = path
            .exists()
            .then(|| std::fs::read_to_string(&path).ok())
            .flatten();
        let mut data = if let Some(json) = raw_json.as_deref() {
            match serde_json::from_str::<ConfigData>(json) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, "config.json is malformed; attempting .bak recovery");
                    let bak = path.with_extension("json.bak");
                    let recovered = bak
                        .exists()
                        .then(|| std::fs::read_to_string(&bak).ok())
                        .flatten()
                        .and_then(|s| serde_json::from_str::<ConfigData>(&s).ok());
                    match recovered {
                        Some(d) => {
                            tracing::info!("recovered config from .bak");
                            d
                        }
                        None => {
                            tracing::warn!("config.bak also unreadable; using defaults");
                            ConfigData::default()
                        }
                    }
                }
            }
        } else {
            ConfigData::default()
        };

        let mut changed = false;
        changed |= ensure_device_identity(&mut data);
        changed |= auto_detect_umu_run(&mut data);
        changed |= stamp_current_exe(&mut data);
        changed |= migrate_cloud_base_path(&mut data);
        changed |= migrate_onboarding_completed(raw_json.as_deref(), &mut data);
        changed |= migrate_retention_floor(&mut data);
        changed |= migrate_lan_install_dir(&mut data);

        let cfg = Self { data };
        if changed {
            // Best-effort save — if it fails, the next mutating op retries.
            let _ = cfg.save();
        }
        Ok(cfg)
    }

    /// Atomic save: write-temp + rename, with a `.bak` of the previous file.
    pub fn save(&self) -> AppResult<()> {
        let path = paths::config_file();
        let json = serde_json::to_string_pretty(&self.data)?;
        paths::write_atomic(&path, json.as_bytes(), true)?;
        Ok(())
    }
}

/// Shared config state. Locks are short (read/clone or mutate+save) so a
/// std::sync::Mutex is fine — same rule as the library: never hold across
/// `.await`. See `library.rs` for the rationale.
pub type SharedConfig = Mutex<Config>;

impl ConfigData {
    /// The library folder new installs land in: the one flagged
    /// `default_install`, else the first configured folder. `None` when no
    /// library folders exist.
    pub fn default_install_folder(&self) -> Option<&LibraryFolder> {
        self.library_folders
            .iter()
            .find(|f| f.default_install)
            .or_else(|| self.library_folders.first())
    }

    /// Resolves where new LAN installs land: the default-install library
    /// folder, falling back to `<app_data>/lan-games` when no library folders
    /// are configured (so the zero-config path — e.g. a Decky-initiated
    /// install on a fresh device — still works). Both the GUI install path
    /// and the plugin server resolve through here so they can't disagree.
    pub fn lan_install_root(&self) -> PathBuf {
        match self.default_install_folder() {
            Some(folder) => PathBuf::from(&folder.path),
            None => paths::app_data_dir().join("lan-games"),
        }
    }
}

// ── First-run helpers ───────────────────────────────────────────────────────

/// Assigns a stable device id (uuid v4) and device name (OS hostname).
/// Returns true if anything was written.
///
/// `device_name` is re-derived not only when empty but also when it's still the
/// literal `DEFAULT_DEVICE_NAME` fallback: configs created before the hostname
/// lookup read the OS (it previously only saw shell env vars, unset for GUI
/// launches) were stuck on that fallback, so once a real hostname is available
/// we adopt it. A name the user actually set is left untouched.
fn ensure_device_identity(data: &mut ConfigData) -> bool {
    let mut changed = false;
    // Normalise surrounding whitespace so every consumer keys on the same
    // string. `device_id` feeds the play-session `session_id` and the
    // `_spool/devices/<id>.json` blob target; some paths read it raw and others
    // `.trim()` it, so a hand-edited / migrated config with whitespace would make
    // them disagree — different session_ids (dedup fails → double-count) and a
    // split device blob. Normalising once here makes the raw value canonical, so
    // the asymmetry can't arise. (#7)
    let trimmed = data.device_id.trim();
    if trimmed != data.device_id {
        data.device_id = trimmed.to_string();
        changed = true;
    }
    if data.device_id.is_empty() {
        data.device_id = uuid::Uuid::new_v4().to_string();
        changed = true;
    }
    if data.device_name.is_empty() || data.device_name == DEFAULT_DEVICE_NAME {
        let name = hostname();
        if name != data.device_name {
            data.device_name = name;
            changed = true;
        }
    }
    changed
}

/// Locates `umu-run` (`/usr/bin/umu-run` then PATH) on non-Windows. Returns
/// true if a path was set. No-op on Windows where Proton launch isn't used.
fn auto_detect_umu_run(data: &mut ConfigData) -> bool {
    if cfg!(windows) {
        return false;
    }
    if !data.launch.umu_run_path.is_empty() && PathBuf::from(&data.launch.umu_run_path).is_file() {
        return false;
    }
    if let Ok(p) = crate::proton::resolve_umu_run(None) {
        data.launch.umu_run_path = p.to_string_lossy().to_string();
        return true;
    }
    false
}

/// Stamps `spool_exe` with the process path so generated launcher stubs (the
/// Armoury Crate stub, etc.) know where to call back to. Uses the AppImage-
/// aware resolver so this is the stable `.AppImage` path, not the ephemeral
/// /tmp mount, when running as an AppImage.
fn stamp_current_exe(data: &mut ConfigData) -> bool {
    if let Some(exe) = paths::spool_executable() {
        let s = exe.to_string_lossy().to_string();
        if data.spool_exe != s {
            data.spool_exe = s;
            return true;
        }
    }
    false
}

/// Migrates pre-rclone-control-plane configs onto `cloud_base_path`.
///
/// Older versions stored the exact ludusavi remote subpath in `cloud_path`
/// (a user-editable Settings field) and had no `cloud_base_path`. With the
/// container-level `#[serde(default)]`, such a config loads with
/// `cloud_base_path` at its default `"Spool"` — so a user who'd customized
/// `cloud_path` would silently have their remote folder switched to
/// `Spool/ludusavi-backup`, hiding their existing saves. When the default
/// base no longer matches a non-default `cloud_path`, derive the base from
/// `cloud_path` (stripping the conventional `/ludusavi-backup` leaf) and
/// normalize `cloud_path` to the canonical derived value so this never fires
/// again. Idempotent and inert for new installs (base already non-default, or
/// `cloud_path` already canonical). Returns true if anything changed.
fn migrate_cloud_base_path(data: &mut ConfigData) -> bool {
    const DEFAULT_BASE: &str = "Spool";
    let old_path = data.cloud.path.trim().trim_end_matches('/').to_string();
    if old_path.is_empty() || data.cloud.base_path.trim() != DEFAULT_BASE {
        // No legacy path to migrate, or the base was already set explicitly
        // (new-scheme config) — leave it alone.
        return false;
    }
    let canonical = format!("{DEFAULT_BASE}/ludusavi-backup");
    if old_path == canonical {
        // The default path — nothing to preserve.
        return false;
    }
    let base = old_path
        .strip_suffix("/ludusavi-backup")
        .unwrap_or(&old_path)
        .trim_end_matches('/');
    if base.is_empty() || base == DEFAULT_BASE {
        return false;
    }
    data.cloud.base_path = base.to_string();
    data.cloud.path = format!("{base}/ludusavi-backup");
    tracing::info!(base, "migrated legacy cloud_path to cloud_base_path");
    true
}

/// Marks a pre-existing config as having finished onboarding.
///
/// The first-run onboarding flow shows whenever `onboarding_completed` is
/// false. A fresh install has no config file at all, so it correctly starts
/// false and the flow runs. But a config written by a Spool build that predates
/// this field would also deserialize to `false` (via the container-level
/// `#[serde(default)]`) — and we don't want to throw the first-run flow at a
/// returning user. Such legacy files are recognised by the
/// `onboarding_completed` key being *absent* from the raw JSON; when it's
/// missing we flip the flag to true. Brand-new installs write the key
/// explicitly (as false), so it's present on every subsequent load and this
/// never fires for them. Returns true if it changed anything.
fn migrate_onboarding_completed(raw_json: Option<&str>, data: &mut ConfigData) -> bool {
    if data.onboarding_completed {
        return false;
    }
    let Some(json) = raw_json else {
        // No file on disk — a genuine first run. Leave it false so the flow shows.
        return false;
    };
    let key_present = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.as_object()
                .map(|o| o.contains_key("onboarding_completed"))
        })
        .unwrap_or(false);
    if key_present {
        return false;
    }
    // Legacy config without the key — a returning user. Skip onboarding.
    data.onboarding_completed = true;
    tracing::info!("marked pre-existing config as onboarding-completed");
    true
}

/// Last-resort device name when neither the OS hostname syscall nor the env
/// vars yield anything. `ensure_device_identity` treats a stored name still
/// equal to this as re-derivable, so a later successful lookup can replace it.
const DEFAULT_DEVICE_NAME: &str = "Spool device";

/// Raise an existing config's `save_retention_full` to the current floor of 3.
/// Pre-existing configs (and the old default) could be 1 or 2; `full == 1` made
/// ludusavi overwrite the single in-place backup, so a force-kill mid-backup
/// could truncate the only copy. Bumping to 3 guarantees a prior good
/// generation survives. Also clamps a stray high value to the 10 ceiling.
fn migrate_retention_floor(data: &mut ConfigData) -> bool {
    let clamped = data.save_retention_full.clamp(3, 10);
    if clamped != data.save_retention_full {
        tracing::info!(
            from = data.save_retention_full,
            to = clamped,
            "raised save retention to the safe floor"
        );
        data.save_retention_full = clamped;
        return true;
    }
    false
}

/// Converts a legacy `lan_install_dir` into a library folder.
///
/// LAN installs used to land in their own configurable directory; they now go
/// to the default-install library folder (`ConfigData::lan_install_root`). A
/// config with a custom `lan_install_dir` keeps its behaviour: the directory
/// becomes a library folder flagged `default_install` (or, if the same path is
/// already a library folder, that folder gets the flag), and the legacy field
/// is cleared so this never fires again. An empty `lan_install_dir` (the old
/// implicit `<app_data>/lan-games` default) migrates to nothing — the new
/// resolution falls back to the same path when no library folders exist, and
/// registering an app-data folder nobody chose would clutter every upgrader's
/// folder list. Returns true if anything changed.
fn migrate_lan_install_dir(data: &mut ConfigData) -> bool {
    let dir = data.lan.install_dir.trim().to_string();
    if dir.is_empty() {
        let changed = !data.lan.install_dir.is_empty();
        data.lan.install_dir = String::new();
        return changed;
    }
    let no_default_yet = !data.library_folders.iter().any(|f| f.default_install);
    if let Some(existing) = data.library_folders.iter_mut().find(|f| f.path == dir) {
        if no_default_yet {
            existing.default_install = true;
        }
    } else {
        data.library_folders.push(LibraryFolder {
            path: dir.clone(),
            label: Some("LAN downloads".to_string()),
            default_install: no_default_yet,
        });
    }
    data.lan.install_dir = String::new();
    tracing::info!(dir, "migrated legacy lan_install_dir to a library folder");
    true
}

fn hostname() -> String {
    // The OS hostname via gethostname(2) (GetComputerNameExW on Windows) is the
    // authoritative source on both platforms. The env vars are only a fallback:
    // $HOSTNAME / $HOST are shell variables that interactive shells set but don't
    // export, so a GUI / Game-Mode launch sees neither — which is why Linux used
    // to land on the fallback every time. %COMPUTERNAME% is a real process env
    // var on Windows, kept as a secondary path. Final fallback is a literal so
    // device identity is never empty.
    let from_os = gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string();
    if !from_os.is_empty() {
        return from_os;
    }
    env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .or_else(|_| env::var("HOST"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_DEVICE_NAME.to_string())
}

// ── Tauri commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_config(state: State<'_, SharedConfig>) -> AppResult<ConfigData> {
    let cfg = state.lock().map_err(|_| AppError::LockPoisoned)?;
    Ok(cfg.data.clone())
}

/// Whether offline mode is currently on, read fresh from `config.json`.
///
/// A disk read rather than Tauri state so every Spool process (tray GUI,
/// attached `--run`, headless Decky server) sees the same answer — the same
/// pattern as `ludusavi_config::cloud_remote_is_configured()`, which the
/// callers of this typically pair it with. Every flip of the flag saves the
/// config immediately, so the file is never stale. `false` on any read/parse
/// failure (no config yet ⇒ not offline).
pub fn offline_mode_enabled() -> bool {
    let Ok(raw) = std::fs::read_to_string(paths::config_file()) else {
        return false;
    };
    serde_json::from_str::<ConfigData>(&raw)
        .map(|d| d.offline_mode)
        .unwrap_or(false)
}

/// Replaces the in-memory config with `data` and persists to disk. The
/// frontend sends back the full ConfigData; partial patches happen client-
/// side. Simpler than a per-field patch surface, matches a "live save" UX.
#[tauri::command]
pub fn update_config(
    state: State<'_, SharedConfig>,
    mut data: ConfigData,
) -> AppResult<ConfigData> {
    let mut cfg = state.lock().map_err(|_| AppError::LockPoisoned)?;

    // Collections are owned by the dedicated collections API (set_collections).
    // The Settings UI sends back a full ConfigData snapshot it loaded earlier, so
    // a collection edit made in between would be clobbered by this wholesale
    // replace — keep the current collections regardless of what the payload says.
    data.collections = cfg.data.collections.clone();

    // Normalize the retention knob up front (same range as the clamp in
    // ludusavi_config::apply_retention) so config.json, the change detection
    // below, and the set_retention call all agree on one value instead of
    // trusting the raw frontend number.
    data.save_retention_full = data.save_retention_full.clamp(3, 10);

    // Project the cloud/rclone and retention settings into the Spool-owned
    // ludusavi config.yaml — but only when those specific inputs actually
    // change, so a plain ui_mode / tray-intro toggle doesn't take the config
    // lock and fsync the YAML (set_custom_games guards the same way).
    //
    // Run the projection BEFORE committing config.json so the two files can't
    // diverge: a projection failure leaves BOTH on the old values and is
    // surfaced to the caller (the Settings UI toasts it) rather than being
    // silently swallowed, and because config.json still holds the old values
    // the change-detection re-attempts the projection on the next save. The
    // ludusavi remote subpath is derived from the base folder via the same
    // normalizer the control plane uses, so it always sits beside Spool's
    // `_spool` dir and a blank base folder can't split saves under
    // `/ludusavi-backup`.
    let cloud_changed = cfg.data.cloud.provider != data.cloud.provider
        || cfg.data.cloud.remote != data.cloud.remote
        || cfg.data.cloud.rclone_args != data.cloud.rclone_args
        || crate::rclone::base_path(&cfg.data) != crate::rclone::base_path(&data);
    let retention_changed = cfg.data.save_retention_full != data.save_retention_full;

    if cloud_changed {
        let ludusavi_remote_path = format!("{}/ludusavi-backup", crate::rclone::base_path(&data));
        crate::ludusavi_config::set_cloud(
            Some(&data.cloud.provider),
            Some(&data.cloud.remote),
            Some(&ludusavi_remote_path),
            None, // rclone path stays as "rclone"; resolved via PATH at run_api spawn time
            Some(&data.cloud.rclone_args),
        )
        .map_err(|e| {
            tracing::error!(error = %e, "update_config: failed to write cloud settings into ludusavi config.yaml");
            e
        })?;
    }

    if retention_changed {
        crate::ludusavi_config::set_retention(data.save_retention_full).map_err(|e| {
            tracing::error!(error = %e, "update_config: failed to write save retention into ludusavi config.yaml");
            e
        })?;
    }

    // Commit config.json last. If the disk write fails, roll the in-memory
    // config back to the previous value so memory and disk stay consistent — the
    // command reports the failure (Settings toasts it), and config.json keeping
    // the old values lets the next save's change-detection re-attempt the
    // projection above.
    let prev = std::mem::replace(&mut cfg.data, data);
    if let Err(e) = cfg.save() {
        cfg.data = prev;
        return Err(e);
    }
    Ok(cfg.data.clone())
}

/// The current set of library collections. Read on the library window's mount
/// to seed the sidebar; a thin wrapper over the config so the window doesn't
/// have to pull the whole `ConfigData` just for collections.
#[tauri::command]
pub fn list_collections(state: State<'_, SharedConfig>) -> AppResult<Vec<Collection>> {
    let cfg = state.lock().map_err(|_| AppError::LockPoisoned)?;
    Ok(cfg.data.collections.clone())
}

/// Replaces the stored collections with `collections` and persists. The library
/// window computes the new array (toggle membership, create/rename/recolor,
/// delete, reorder) and sends the whole thing back — same "frontend owns the
/// shape, send it all" approach as `update_config`, which keeps the backend a
/// single setter instead of a per-operation surface. Broadcasts
/// `collections:changed` (with the saved list) so any other open window stays in
/// sync; the invoking window also updates optimistically from the return value.
#[tauri::command]
pub fn set_collections(
    app: AppHandle,
    state: State<'_, SharedConfig>,
    collections: Vec<Collection>,
) -> AppResult<Vec<Collection>> {
    let saved = {
        let mut cfg = state.lock().map_err(|_| AppError::LockPoisoned)?;
        let prev = std::mem::replace(&mut cfg.data.collections, collections);
        if let Err(e) = cfg.save() {
            // Roll memory back so it can't drift from disk on a write failure.
            cfg.data.collections = prev;
            return Err(e);
        }
        cfg.data.collections.clone()
    };
    let _ = app.emit("collections:changed", &saved);
    Ok(saved)
}

/// The host OS, so the frontend can gate Linux-only UI (Proton settings).
/// Returns `"windows"`, `"linux"`, or `"macos"` (Rust's `std::env::consts::OS`).
#[tauri::command]
pub fn app_platform() -> String {
    std::env::consts::OS.to_string()
}

/// Runs umu-run auto-detection on demand (Settings → Compatibility). Returns
/// the resulting path, or an empty string when there's nothing usable.
/// Persists if a new path was found.
///
/// `auto_detect_umu_run` leaves a stale `umu_run_path` in place when it can't
/// find anything, so returning the field unconditionally would report a path
/// that no longer exists as a successful detection. Only a path that is
/// actually a file is reported back.
#[tauri::command]
pub fn detect_umu_run(state: State<'_, SharedConfig>) -> AppResult<String> {
    let mut cfg = state.lock().map_err(|_| AppError::LockPoisoned)?;
    if auto_detect_umu_run(&mut cfg.data) {
        cfg.save()?;
    }
    let path = cfg.data.launch.umu_run_path.clone();
    if !path.is_empty() && PathBuf::from(&path).is_file() {
        Ok(path)
    } else {
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pre-grouping config.json (a subset of the flat keys) must still load:
    /// present keys map into their sub-structs, missing keys fall back to each
    /// sub-struct's defaults. This is the `#[serde(flatten)]` + `default`
    /// contract the on-disk + frontend compatibility relies on.
    #[test]
    fn old_flat_config_loads_into_subgroups() {
        let json = r#"{
            "cloud_provider": "dropbox",
            "cloud_webdav_url": "https://dav.example.com",
            "lan_share_port": 12345,
            "umu_run_path": "/usr/bin/umu-run"
        }"#;
        let data: ConfigData = serde_json::from_str(json).unwrap();
        assert_eq!(data.cloud.provider, "dropbox");
        assert_eq!(data.cloud.webdav_url, "https://dav.example.com");
        assert_eq!(data.lan.share_port, 12345);
        assert_eq!(data.launch.umu_run_path, "/usr/bin/umu-run");
        // Everything absent from the JSON gets its sub-struct default.
        assert_eq!(data.cloud.base_path, "Spool");
        assert_eq!(data.cloud.rclone_args, "--fast-list --ignore-checksum");
        assert!(data.lan.share_enabled);
        assert_eq!(data.lan.download_max_mbps, 0.0);
        assert_eq!(data.save_retention_full, 5);
    }

    /// The grouping is internal: the serialized JSON stays flat (the historical
    /// `cloud_*` / `lan_*` / `umu_run_path` keys), and the Rust sub-struct names
    /// (`cloud`, `lan`, `launch`) never appear on the wire.
    #[test]
    fn serializes_to_flat_keys() {
        let json = serde_json::to_value(ConfigData::default()).unwrap();
        let obj = json.as_object().unwrap();
        for key in [
            "cloud_provider",
            "cloud_remote",
            "cloud_base_path",
            "cloud_path",
            "rclone_args",
            "cloud_webdav_url",
            "cloud_webdav_username",
            "lan_share_enabled",
            "lan_share_port",
            "lan_install_dir",
            "lan_download_max_mbps",
            "umu_run_path",
            "default_proton_path",
        ] {
            assert!(obj.contains_key(key), "missing flat key: {key}");
        }
        assert!(!obj.contains_key("cloud"));
        assert!(!obj.contains_key("lan"));
        assert!(!obj.contains_key("launch"));
    }

    #[test]
    fn legacy_config_without_onboarding_key_is_marked_completed() {
        // A config that predates the onboarding flag (key absent) is a
        // returning user — migrate to completed so the flow doesn't reappear.
        let json = r#"{ "device_id": "abc", "tray_intro_seen": true }"#;
        let mut data: ConfigData = serde_json::from_str(json).unwrap();
        assert!(!data.onboarding_completed); // serde default before migration
        assert!(migrate_onboarding_completed(Some(json), &mut data));
        assert!(data.onboarding_completed);
    }

    #[test]
    fn fresh_install_keeps_onboarding_pending() {
        // No file on disk (None) → genuine first run, flag stays false.
        let mut data = ConfigData::default();
        assert!(!migrate_onboarding_completed(None, &mut data));
        assert!(!data.onboarding_completed);
    }

    #[test]
    fn explicit_onboarding_false_is_left_pending() {
        // A new-scheme config that wrote the key as false (e.g. relaunched
        // mid-onboarding) keeps showing the flow — the key is present.
        let json = r#"{ "device_id": "abc", "onboarding_completed": false }"#;
        let mut data: ConfigData = serde_json::from_str(json).unwrap();
        assert!(!migrate_onboarding_completed(Some(json), &mut data));
        assert!(!data.onboarding_completed);
    }

    #[test]
    fn retention_floor_migrates_unsafe_low_values() {
        // Pre-existing configs at 1 or 2 (incl. the old in-place `full == 1`
        // mode) get raised to the safe floor of 3 on load.
        for low in [0u32, 1, 2] {
            let mut data = ConfigData {
                save_retention_full: low,
                ..Default::default()
            };
            assert!(migrate_retention_floor(&mut data), "{low} should migrate");
            assert_eq!(data.save_retention_full, 3);
        }
        // A stray high value is pulled down to the ceiling.
        let mut high = ConfigData {
            save_retention_full: 50,
            ..Default::default()
        };
        assert!(migrate_retention_floor(&mut high));
        assert_eq!(high.save_retention_full, 10);
        // Values already in range (including the default 5) are left alone.
        for ok in [3u32, 5, 10] {
            let mut data = ConfigData {
                save_retention_full: ok,
                ..Default::default()
            };
            assert!(
                !migrate_retention_floor(&mut data),
                "{ok} should not migrate"
            );
            assert_eq!(data.save_retention_full, ok);
        }
    }

    #[test]
    fn lan_install_dir_migrates_to_default_library_folder() {
        // A custom lan_install_dir becomes a default-install library folder
        // and the legacy field is cleared so the migration never re-fires.
        let mut data = ConfigData {
            lan: LanConfig {
                install_dir: "/mnt/sd/lan".to_string(),
                ..LanConfig::default()
            },
            ..Default::default()
        };
        assert!(migrate_lan_install_dir(&mut data));
        assert_eq!(data.library_folders.len(), 1);
        assert_eq!(data.library_folders[0].path, "/mnt/sd/lan");
        assert!(data.library_folders[0].default_install);
        assert!(data.lan.install_dir.is_empty());
        assert!(!migrate_lan_install_dir(&mut data));
    }

    #[test]
    fn lan_install_dir_migration_flags_existing_folder() {
        // When the dir is already a library folder, no duplicate is added —
        // the existing folder just becomes the install default.
        let mut data = ConfigData {
            library_folders: vec![LibraryFolder {
                path: "/mnt/sd/lan".to_string(),
                ..Default::default()
            }],
            lan: LanConfig {
                install_dir: "/mnt/sd/lan".to_string(),
                ..LanConfig::default()
            },
            ..Default::default()
        };
        assert!(migrate_lan_install_dir(&mut data));
        assert_eq!(data.library_folders.len(), 1);
        assert!(data.library_folders[0].default_install);
        assert!(data.lan.install_dir.is_empty());
    }

    #[test]
    fn empty_lan_install_dir_migrates_to_nothing() {
        // The old implicit <app_data>/lan-games default isn't registered as a
        // folder — the new resolution falls back to the same path anyway.
        let mut data = ConfigData::default();
        assert!(!migrate_lan_install_dir(&mut data));
        assert!(data.library_folders.is_empty());
    }

    #[test]
    fn install_root_resolution_prefers_flagged_then_first_folder() {
        let mut data = ConfigData {
            library_folders: vec![
                LibraryFolder {
                    path: "/a".to_string(),
                    ..Default::default()
                },
                LibraryFolder {
                    path: "/b".to_string(),
                    default_install: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(data.lan_install_root(), PathBuf::from("/b"));
        // Without a flagged folder the first one acts as the default.
        data.library_folders[1].default_install = false;
        assert_eq!(data.lan_install_root(), PathBuf::from("/a"));
        // No folders at all → the app-data fallback.
        data.library_folders.clear();
        assert_eq!(
            data.lan_install_root(),
            paths::app_data_dir().join("lan-games")
        );
    }

    #[test]
    fn collections_default_empty_and_round_trip() {
        // Absent from a legacy config → empty vec (container-level serde default).
        let data: ConfigData = serde_json::from_str(r#"{ "device_id": "x" }"#).unwrap();
        assert!(data.collections.is_empty());

        // A populated list (multi-membership, per-collection accent) round-trips.
        let original = ConfigData {
            collections: vec![
                Collection {
                    id: "a".to_string(),
                    name: "Currently playing".to_string(),
                    accent: "#4cc2ff".to_string(),
                    games: vec!["g1".to_string(), "g2".to_string()],
                },
                Collection {
                    id: "b".to_string(),
                    name: "Cozy".to_string(),
                    accent: "#ff8a3d".to_string(),
                    games: vec!["g2".to_string()],
                },
            ],
            ..ConfigData::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ConfigData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.collections.len(), 2);
        assert_eq!(parsed.collections[0].name, "Currently playing");
        assert_eq!(parsed.collections[0].games, vec!["g1", "g2"]);
        assert_eq!(parsed.collections[1].accent, "#ff8a3d");
    }

    #[test]
    fn round_trips_through_json() {
        let original = ConfigData {
            cloud: CloudConfig {
                provider: "gdrive".to_string(),
                base_path: "Games/Spool".to_string(),
                ..CloudConfig::default()
            },
            lan: LanConfig {
                share_port: 50000,
                download_max_mbps: 12.5,
                ..LanConfig::default()
            },
            launch: LaunchConfig {
                default_proton_path: "/opt/proton".to_string(),
                ..LaunchConfig::default()
            },
            ..ConfigData::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ConfigData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.cloud.provider, "gdrive");
        assert_eq!(parsed.cloud.base_path, "Games/Spool");
        assert_eq!(parsed.lan.share_port, 50000);
        assert_eq!(parsed.lan.download_max_mbps, 12.5);
        assert_eq!(parsed.launch.default_proton_path, "/opt/proton");
    }

    #[test]
    fn device_name_rederived_from_stuck_fallback() {
        // A config left on the literal fallback by the old env-only lookup is
        // re-derived once a real hostname is available, and the write is flagged.
        let mut data = ConfigData {
            device_id: "fixed".to_string(),
            device_name: DEFAULT_DEVICE_NAME.to_string(),
            ..ConfigData::default()
        };
        let changed = ensure_device_identity(&mut data);
        if hostname() == DEFAULT_DEVICE_NAME {
            // No real hostname on this box either — nothing to adopt.
            assert!(!changed);
            assert_eq!(data.device_name, DEFAULT_DEVICE_NAME);
        } else {
            assert!(changed);
            assert_ne!(data.device_name, DEFAULT_DEVICE_NAME);
        }
        assert_eq!(data.device_id, "fixed"); // never regenerated when present
    }

    #[test]
    fn device_id_whitespace_is_normalised() {
        // A hand-edited/migrated config with surrounding whitespace gets trimmed
        // so every consumer keys on the same canonical string. (#7)
        let mut data = ConfigData {
            device_id: "  abc-123  ".to_string(),
            device_name: "Deck".to_string(),
            ..ConfigData::default()
        };
        assert!(ensure_device_identity(&mut data));
        assert_eq!(data.device_id, "abc-123");

        // A whitespace-only id is treated as empty and regenerated (non-empty,
        // and itself trimmed).
        let mut blank = ConfigData {
            device_id: "   ".to_string(),
            device_name: "Deck".to_string(),
            ..ConfigData::default()
        };
        assert!(ensure_device_identity(&mut blank));
        assert!(!blank.device_id.is_empty());
        assert_eq!(blank.device_id.trim(), blank.device_id);
    }

    #[test]
    fn user_chosen_device_name_is_preserved() {
        let mut data = ConfigData {
            device_id: "fixed".to_string(),
            device_name: "Living Room Deck".to_string(),
            ..ConfigData::default()
        };
        assert!(!ensure_device_identity(&mut data));
        assert_eq!(data.device_name, "Living Room Deck");
    }
}
