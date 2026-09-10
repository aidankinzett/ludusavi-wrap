//! Sleep/idle inhibitor for long foreground jobs (Linux).
//!
//! Running a Windows installer through Proton — the guided `setup.exe` flow and
//! the "run an installer in this game's prefix" helper — can take many minutes
//! and is often interactive, so the machine shouldn't suspend or idle-sleep out
//! from under it. [`SleepInhibitor::acquire`] takes a systemd-logind `block`
//! inhibitor on `sleep:idle` and holds it for the lifetime of the returned
//! guard; dropping the guard closes the file descriptor and logind releases the
//! inhibitor.
//!
//! Desktop power managers surface logind block inhibitors to the user — KDE's
//! battery applet lists the holder ("Spool") and reason as blocking sleep — so
//! it's visible why the system is staying awake.
//!
//! Best effort: if the system bus or logind isn't reachable the guard is still
//! returned, just without an inhibitor, and the caller proceeds. Non-Linux
//! targets get a zero-sized no-op guard.

/// RAII guard for a held sleep/idle inhibitor. Keep it in scope for as long as
/// the system should stay awake; drop it to release.
pub struct SleepInhibitor {
    /// The inhibitor fd handed back by logind. logind keeps the inhibitor
    /// active while any copy of this fd is open, so dropping the guard (closing
    /// the fd) releases it. `None` when the inhibitor couldn't be taken.
    #[cfg(target_os = "linux")]
    _fd: Option<zbus::zvariant::OwnedFd>,
}

impl SleepInhibitor {
    /// Take a best-effort `sleep:idle` block inhibitor labelled with `reason`
    /// (shown by desktop power UIs alongside the holder name "Spool").
    pub async fn acquire(reason: &str) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                _fd: linux::acquire(reason).await,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = reason;
            Self {}
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use zbus::zvariant::OwnedFd;
    use zbus::{Connection, Proxy};

    const LOGIND_DEST: &str = "org.freedesktop.login1";
    const LOGIND_PATH: &str = "/org/freedesktop/login1";
    const LOGIND_IFACE: &str = "org.freedesktop.login1.Manager";

    pub(super) async fn acquire(reason: &str) -> Option<OwnedFd> {
        let conn = match Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "sleep-inhibit: no system D-Bus — not inhibiting sleep");
                return None;
            }
        };
        let proxy = match Proxy::new(&conn, LOGIND_DEST, LOGIND_PATH, LOGIND_IFACE).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "sleep-inhibit: logind proxy failed — not inhibiting sleep");
                return None;
            }
        };
        // Inhibit(what, who, why, mode). "block" (not "delay") keeps the machine
        // awake for the whole job; "sleep:idle" covers both an explicit suspend
        // and the idle auto-sleep timer.
        match proxy
            .call::<_, _, OwnedFd>("Inhibit", &("sleep:idle", "Spool", reason, "block"))
            .await
        {
            Ok(fd) => {
                tracing::info!(%reason, "sleep-inhibit: holding logind block inhibitor (sleep:idle)");
                Some(fd)
            }
            Err(e) => {
                tracing::warn!(error = %e, "sleep-inhibit: logind Inhibit failed — not inhibiting sleep");
                None
            }
        }
    }
}
