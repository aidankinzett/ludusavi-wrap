//! System-suspend watcher for the unsynced-session marker.
//!
//! When a device sleeps mid-session — most importantly a Steam Deck that
//! suspends while a game is running — every userspace process freezes, so the
//! session heartbeat (`rclone::start_heartbeat`) stops rewriting the marker.
//! Without intervention the marker would age out to "stale" and a peer might
//! treat the session as abandoned, even though it's merely asleep.
//!
//! This watcher subscribes to systemd-logind's `PrepareForSleep` D-Bus signal,
//! which fires *before* the freeze. On the way down it marks the session marker
//! suspended (a suspended marker never goes stale, so a peer sees an *unsynced
//! session* rather than the marker being reclaimed); on resume it re-asserts an
//! awake marker, and warns if a peer took the session over while we slept.
//!
//! To guarantee the suspend write actually lands before the system freezes, the
//! watcher holds a logind *delay* inhibitor lock and only releases it once the
//! marker write has been fired — the standard "react before sleep" pattern.
//!
//! Non-Linux targets get a no-op watcher: the returned handle is an
//! already-finished task, so the caller's unconditional `.abort()` is harmless.

use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::task::JoinHandle;

/// Total seconds the system spent suspended during a session. The watcher adds
/// each resume−sleep interval to this; the runner subtracts it from the
/// wall-clock session length so time asleep isn't counted as play time.
pub type SuspendedSecs = Arc<AtomicI64>;

/// Starts a per-session task that marks the play-state lock suspended when the
/// system sleeps. The returned handle is aborted by the runner when the session
/// ends (see `run_workflow`). No-op when sync is unconfigured — the inner calls
/// short-circuit on a missing server/device id.
///
/// `suspended_secs` accumulates the total time spent asleep across the session
/// (Linux only); on other platforms it stays zero.
pub fn start_suspend_watcher(
    app: AppHandle,
    game_name: String,
    suspended_secs: SuspendedSecs,
) -> JoinHandle<()> {
    #[cfg(target_os = "linux")]
    {
        tokio::spawn(linux::watch(app, game_name, suspended_secs))
    }
    #[cfg(not(target_os = "linux"))]
    {
        // No suspend integration off Linux; hand back a completed task so the
        // caller's `.abort()` is a harmless no-op.
        let _ = (&app, &game_name, &suspended_secs);
        tokio::spawn(async {})
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::rclone;
    use chrono::Utc;
    use std::sync::atomic::Ordering;
    use zbus::zvariant::OwnedFd;
    use zbus::{Connection, Proxy};

    /// Take a `delay` inhibitor on `sleep`. logind blocks the actual suspend
    /// until this fd is dropped (up to `InhibitDelayMaxSec`), giving us a window
    /// to mark the lock suspended before everything freezes.
    async fn take_delay_inhibitor(proxy: &Proxy<'_>) -> Option<OwnedFd> {
        crate::logind::inhibit(
            proxy,
            "sleep",
            "Spool",
            "Marking game session lock as suspended",
            "delay",
        )
        .await
    }

    pub async fn watch(app: AppHandle, game_name: String, suspended_secs: SuspendedSecs) {
        let conn = match Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "suspend: no system D-Bus — suspend handling disabled");
                return;
            }
        };
        let proxy = match crate::logind::manager_proxy(&conn).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "suspend: logind proxy failed — suspend handling disabled");
                return;
            }
        };

        let mut signal = match proxy.receive_signal("PrepareForSleep").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "suspend: PrepareForSleep subscribe failed");
                return;
            }
        };

        // Hold a delay inhibitor while awake so we get a window to react to the
        // next suspend. Re-taken after each resume.
        let mut inhibitor = take_delay_inhibitor(&proxy).await;

        // When we last went to sleep, so resume can accumulate the asleep span.
        let mut slept_at = None;

        use futures_util::StreamExt;
        while let Some(msg) = signal.next().await {
            // PrepareForSleep carries a single bool: true = about to sleep,
            // false = just resumed.
            let about_to_sleep: bool = match msg.body().deserialize() {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e, "suspend: bad PrepareForSleep payload");
                    continue;
                }
            };

            if about_to_sleep {
                tracing::info!(game = %game_name, "suspend: system sleeping — marking session suspended");
                slept_at = Some(Utc::now());
                rclone::mark_session_suspended(&app, &game_name).await;
                // Release the inhibitor so the suspend can proceed now that the
                // marker is updated. Re-taken on resume.
                inhibitor = None;
            } else {
                // Add the span we just spent asleep so the runner can subtract
                // it from the session's wall-clock play time.
                if let Some(start) = slept_at.take() {
                    let secs = (Utc::now() - start).num_seconds().max(0);
                    // fetch_add returns the previous total; new total = prev + secs.
                    let total = suspended_secs.fetch_add(secs, Ordering::Relaxed) + secs;
                    // Persist the running total to the session record so it survives
                    // a Game-Mode force-kill — the forced-close backup runs in a
                    // separate process and reads this instead of our in-memory tally.
                    crate::session::record_suspended_secs(&game_name, total);
                    tracing::info!(game = %game_name, asleep_secs = secs, "suspend: system resumed — refreshing session marker");
                } else {
                    tracing::info!(game = %game_name, "suspend: system resumed — refreshing session marker");
                }
                // Re-assert our marker immediately rather than waiting up to
                // 60s for the next heartbeat. If another device took over the
                // session while we slept, warn the user; the local game keeps
                // running regardless.
                if let Some(device_name) = rclone::resume_session(&app, &game_name).await {
                    warn_lock_taken(&app, &game_name, &device_name);
                }
                inhibitor = take_delay_inhibitor(&proxy).await;
            }
        }

        // Keep the inhibitor fd alive for the whole loop; dropping here on task
        // abort releases it cleanly.
        drop(inhibitor);
    }

    /// Surface a best-effort warning when our suspended lock was taken over by
    /// another device while we slept. Emits an app event (toast on the library
    /// window) and an OS notification since the window is usually hidden during
    /// a Game-Mode session.
    fn warn_lock_taken(app: &AppHandle, game_name: &str, device_name: &str) {
        use tauri::Emitter;
        tracing::warn!(game = %game_name, %device_name, "suspend: lock taken over while suspended");
        let msg = format!(
            "While this device slept, {device_name} started playing {game_name}. \
             Your unsynced progress here may be overwritten."
        );
        let _ = app.emit("sync:lock-taken", &msg);
        crate::runner::os_toast_if_hidden(app, "Spool: session taken over", &msg);
    }
}
