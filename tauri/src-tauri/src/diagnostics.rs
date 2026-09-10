//! Dependency health checks — surfaced in Settings → Compatibility.
//!
//! Reports whether umu-run, ludusavi, and rclone are reachable and where
//! each was found (user config, bundled sidecar, or system PATH). Also
//! provides a per-distro install hint for anything that's missing, so the
//! UI can show a copy-paste command rather than a generic error.

use crate::paths;
use serde::Serialize;
use tauri::State;

/// Resolution source for a dependency.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DepSource {
    /// Bundled sidecar shipped with Spool (next to the executable).
    Bundled,
    /// Found on the system PATH or a well-known path.
    System,
    /// Not found anywhere.
    Missing,
}

/// Status of a single dependency. Mirrored to `types.ts` as `DepStatus`.
#[derive(Debug, Clone, Serialize)]
pub struct DepStatus {
    /// Display name shown in the UI.
    pub name: String,
    /// True if the binary was found and is reachable.
    pub found: bool,
    /// Absolute path where the binary was found, or `""` if missing.
    pub path: String,
    /// How it was found.
    pub source: DepSource,
    /// Suggested install command for the detected distro, when missing.
    /// Empty string if the dep is present.
    pub install_hint: String,
    /// Link to a docs page with full install instructions, when one exists.
    /// Empty string if there's nothing more to link to.
    pub install_docs_url: String,
}

/// Docs page with per-distro umu-launcher install instructions (including the
/// SteamOS read-only-root path that doesn't fit a one-line command).
const UMU_DOCS_URL: &str = "https://spool.kinzett.io/guides/installing-umu/";

/// Check all three runtime dependencies and return their status.
/// Called from Settings → Compatibility on mount and after autodetect.
#[tauri::command]
pub fn check_dependencies(config: State<'_, crate::config::SharedConfig>) -> Vec<DepStatus> {
    let umu_run_cfg = {
        let cfg = config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.data.launch.umu_run_path.clone()
    };

    let distro = detect_distro();

    vec![
        check_umu_run(&umu_run_cfg, &distro),
        check_ludusavi(&distro),
        check_rclone(&distro),
    ]
}

fn check_umu_run(configured: &str, distro: &Distro) -> DepStatus {
    // umu-run is NOT bundled — it's a Python app with system lib32/vulkan deps.
    // Resolution: config override → /usr/bin/umu-run → PATH.
    if !configured.is_empty() {
        let p = std::path::PathBuf::from(configured);
        if p.is_file() {
            return found("umu-run", configured, DepSource::System);
        }
    }
    let well_known = std::path::Path::new("/usr/bin/umu-run");
    if well_known.is_file() {
        return found(
            "umu-run",
            well_known.to_str().unwrap_or(""),
            DepSource::System,
        );
    }
    if let Some(p) = paths::find_system_binary("umu-run") {
        return found("umu-run", &p.to_string_lossy(), DepSource::System);
    }
    let mut status = missing("umu-run", install_hint_umu(distro));
    status.install_docs_url = UMU_DOCS_URL.to_string();
    status
}

fn check_ludusavi(_distro: &Distro) -> DepStatus {
    if let Some(p) = paths::resolve_sidecar_path("ludusavi") {
        return found("ludusavi", &p.to_string_lossy(), DepSource::Bundled);
    }
    missing("ludusavi", String::new())
}

fn check_rclone(_distro: &Distro) -> DepStatus {
    if let Some(p) = paths::resolve_sidecar_path("rclone") {
        return found("rclone", &p.to_string_lossy(), DepSource::Bundled);
    }
    missing("rclone", String::new())
}

fn found(name: &str, path: &str, source: DepSource) -> DepStatus {
    DepStatus {
        name: name.to_string(),
        found: true,
        path: path.to_string(),
        source,
        install_hint: String::new(),
        install_docs_url: String::new(),
    }
}

fn missing(name: &str, install_hint: String) -> DepStatus {
    DepStatus {
        name: name.to_string(),
        found: false,
        path: String::new(),
        source: DepSource::Missing,
        install_hint,
        install_docs_url: String::new(),
    }
}

// ── Distro detection ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Distro {
    SteamOS, // Valve's SteamOS 3 (Steam Deck) — Arch-based but read-only root
    Arch,    // Arch, CachyOS, Manjaro, EndeavourOS
    Fedora,  // Fedora, Nobara, RHEL (Bazzite ships umu-run preinstalled)
    Debian,  // Ubuntu, Debian, Pop!_OS
    Suse,    // openSUSE
    Other,
}

fn detect_distro() -> Distro {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    // Check ID_LIKE first (broader family), then ID (specific distro).
    let id_like = extract_field(&os_release, "ID_LIKE").to_lowercase();
    let id = extract_field(&os_release, "ID").to_lowercase();

    // SteamOS reports ID_LIKE=arch, so match its specific ID before the family
    // loop — its read-only root means the generic Arch `paru` hint won't work.
    if id == "steamos" {
        return Distro::SteamOS;
    }

    for field in [id_like.as_str(), id.as_str()] {
        if field.contains("arch") || field.contains("cachyos") || field.contains("manjaro") {
            return Distro::Arch;
        }
        if field.contains("fedora") || field.contains("rhel") || field.contains("centos") {
            return Distro::Fedora;
        }
        if field.contains("debian") || field.contains("ubuntu") {
            return Distro::Debian;
        }
        if field.contains("suse") || field.contains("opensuse") {
            return Distro::Suse;
        }
    }
    Distro::Other
}

fn extract_field<'a>(content: &'a str, key: &str) -> &'a str {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(&format!("{key}=")) {
            return rest.trim_matches('"');
        }
    }
    ""
}

// ── Per-distro install hints ──────────────────────────────────────────────────

fn install_hint_umu(distro: &Distro) -> String {
    // umu-launcher's aarch64 support isn't usable yet: most distros package it
    // for x86_64 only, and the ARM handheld images that do ship a Proton build
    // (Armada and friends) have a read-only root anyway. Handing out a package
    // command that cannot work is worse than saying so, so short-circuit the
    // per-distro hints and let the docs link explain the state of play.
    if cfg!(target_arch = "aarch64") {
        return "# Windows games aren't supported on ARM yet — see the guide below".to_string();
    }
    match distro {
        // SteamOS root is read-only and AUR isn't available, so there's no
        // one-line package install — the docs link covers the home-dir build.
        Distro::SteamOS => "# SteamOS root is read-only — see the guide below".to_string(),
        Distro::Arch => "paru -S umu-launcher".to_string(),
        Distro::Fedora => "sudo dnf install umu-launcher".to_string(),
        Distro::Debian => "# Not in apt yet — see the guide below".to_string(),
        Distro::Suse => "sudo zypper install umu-launcher".to_string(),
        Distro::Other => "# See the guide below".to_string(),
    }
}
