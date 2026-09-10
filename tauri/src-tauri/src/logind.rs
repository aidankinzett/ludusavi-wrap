//! Shared systemd-logind (`org.freedesktop.login1`) D-Bus plumbing.
//!
//! Two features take logind inhibitor locks and differ only in the arguments:
//! the suspend watcher (`suspend.rs`) holds a `delay` inhibitor on `sleep` so it
//! can update the session marker before the freeze, and the installer
//! sleep-blocker (`sleep_inhibit.rs`) holds a `block` inhibitor on `sleep:idle`
//! for the lifetime of a Proton installer run. Both go through the `Inhibit`
//! method on logind's Manager interface, wired up here once.
//!
//! Linux-only — the module is empty on other targets.

#[cfg(target_os = "linux")]
mod imp {
    use zbus::zvariant::OwnedFd;
    use zbus::{Connection, Proxy};

    pub const DEST: &str = "org.freedesktop.login1";
    pub const PATH: &str = "/org/freedesktop/login1";
    pub const MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";

    /// A proxy onto logind's Manager interface on the system bus.
    pub async fn manager_proxy(conn: &Connection) -> zbus::Result<Proxy<'static>> {
        Proxy::new(conn, DEST, PATH, MANAGER_IFACE).await
    }

    /// Call `Manager.Inhibit(what, who, why, mode)` and return the inhibitor fd.
    /// logind keeps the inhibition active while any copy of that fd stays open,
    /// so hold the returned handle for as long as the inhibition should last and
    /// drop it to release. `None` on any D-Bus error — callers treat an
    /// inhibitor as best-effort and carry on without one.
    pub async fn inhibit(
        proxy: &Proxy<'_>,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> Option<OwnedFd> {
        match proxy
            .call::<_, _, OwnedFd>("Inhibit", &(what, who, why, mode))
            .await
        {
            Ok(fd) => Some(fd),
            Err(e) => {
                tracing::warn!(what, mode, error = %e, "logind: Inhibit call failed");
                None
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::{inhibit, manager_proxy};
