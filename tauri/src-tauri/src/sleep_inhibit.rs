//! Sleep/idle inhibitor for long foreground jobs (Linux).
//!
//! Running a Windows installer through Proton — the guided `setup.exe` flow and
//! the "run an installer in this game's prefix" helper — can take many minutes
//! and is often interactive, so the machine shouldn't suspend or idle-sleep out
//! from under it. [`SleepInhibitor::acquire`] takes a systemd-logind `block`
//! inhibitor on `sleep:idle` and holds it for the lifetime of the returned
//! guard; dropping the guard releases it immediately.
//!
//! A watchdog task also releases the inhibitor after [`linux::MAX_INHIBIT`]
//! regardless, so a wedged `umu-run` can't block sleep indefinitely on the
//! tray-resident app (which never auto-quits).
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
    #[cfg(target_os = "linux")]
    _inner: Option<linux::Guard>,
}

impl SleepInhibitor {
    /// Take a best-effort `sleep:idle` block inhibitor labelled with `reason`
    /// (shown by desktop power UIs alongside the holder name "Spool").
    pub async fn acquire(reason: &str) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                _inner: linux::acquire(reason).await,
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
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;
    use zbus::zvariant::OwnedFd;
    use zbus::Connection;

    /// Ceiling on how long one inhibitor is held. A guided game install or an
    /// interactive patch installer can legitimately run a long time, so this is
    /// generous; its only job is to stop a wedged `umu-run` from blocking sleep
    /// indefinitely (Spool is tray-resident and never auto-quits).
    const MAX_INHIBIT: std::time::Duration = std::time::Duration::from_secs(3 * 60 * 60);

    /// Shared slot for the logind inhibitor fd. Dropping the fd (setting this to
    /// `None`) is what releases the inhibition. Both the guard's `Drop` and the
    /// watchdog task hold a handle; whichever fires first clears it.
    type FdSlot = Arc<Mutex<Option<OwnedFd>>>;

    pub(super) struct Guard {
        slot: FdSlot,
        /// Dropped by `Guard::drop`, which cancels the watchdog's timeout wait.
        _cancel: oneshot::Sender<()>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            // Release now (the common case: the installer finished). Clearing
            // the slot drops the fd; `_cancel` then drops too, ending the
            // watchdog task without it logging a timeout.
            if let Ok(mut g) = self.slot.lock() {
                *g = None;
            }
        }
    }

    pub(super) async fn acquire(reason: &str) -> Option<Guard> {
        let conn = match Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "sleep-inhibit: no system D-Bus — not inhibiting sleep");
                return None;
            }
        };
        let proxy = match crate::logind::manager_proxy(&conn).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "sleep-inhibit: logind proxy failed — not inhibiting sleep");
                return None;
            }
        };
        // "block" (not "delay") keeps the machine awake for the whole job;
        // "sleep:idle" covers both an explicit suspend and the idle auto-sleep
        // timer.
        let fd = crate::logind::inhibit(&proxy, "sleep:idle", "Spool", reason, "block").await?;
        tracing::info!(%reason, "sleep-inhibit: holding logind block inhibitor (sleep:idle)");

        let slot: FdSlot = Arc::new(Mutex::new(Some(fd)));
        let (cancel_tx, cancel_rx) = oneshot::channel();

        let watch_slot = slot.clone();
        let reason = reason.to_owned();
        tokio::spawn(async move {
            // Keep the bus connection alive for the inhibitor's lifetime.
            let _conn = conn;
            // Wait for the guard to drop (cancel_rx resolves) or the cap to
            // elapse. Only the cap path force-releases and logs.
            let capped = tokio::time::timeout(MAX_INHIBIT, cancel_rx).await.is_err();
            if capped && watch_slot.lock().ok().and_then(|mut g| g.take()).is_some() {
                tracing::warn!(
                    %reason,
                    "sleep-inhibit: max hold reached — releasing (installer still running?)"
                );
            }
        });

        Some(Guard {
            slot,
            _cancel: cancel_tx,
        })
    }
}
