//! Systemd user unit failure monitoring.
//!
//! Monitors failed systemd user units and provides notifications. The
//! 5-second check talks to the systemd user manager over D-Bus (no
//! subprocess fork) with `systemctl` only as a fallback, and only reports
//! when the failed set actually changed.

use log::{
    debug,
    warn,
};
use std::{
    process::Command,
    sync::{
        Arc,
        Mutex,
        mpsc::Sender,
    },
    thread,
    time::Duration,
};
use serde::Deserialize;
use zbus::zvariant::Type;

/// Systemd unit failure information.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemdFailedUnits {
    pub units: Vec<String>,
    pub count: usize,
}

/// Systemd failure monitoring service.
#[derive(Clone)]
pub struct SystemdFailedService {
    failed_units: Arc<Mutex<SystemdFailedUnits>>,
}

impl SystemdFailedService {
    pub fn new() -> Self {
        SystemdFailedService {
            failed_units: Arc::new(Mutex::new(SystemdFailedUnits::default())),
        }
    }

    pub fn get_failed_units(&self) -> SystemdFailedUnits {
        self.failed_units.lock().unwrap().clone()
    }

    /// Update the cached failed set and return it only when it changed, so
    /// unchanged polls never wake any subscriber.
    fn update_failed_units(&self, units: Vec<String>) -> Option<SystemdFailedUnits> {
        let mut failed = self.failed_units.lock().unwrap();
        if failed.units == units {
            return None;
        }
        failed.units = units;
        failed.count = failed.units.len();
        Some(failed.clone())
    }
}

impl Default for SystemdFailedService {
    fn default() -> Self {
        Self::new()
    }
}

/// Proxy for the systemd user manager's Manager interface.
#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    /// Returns units matching the given states — `("failed",)` lists the
    /// failed set exactly like `systemctl --user --failed`.
    fn list_units_by_patterns(&self, states: &[&str], patterns: &[&str]) -> zbus::Result<Vec<UnitInfo>>;
}

/// One entry of systemd's `ListUnitsByPatterns` reply.
#[allow(dead_code)] // deserialized fields beyond the unit name are not needed yet
#[derive(Debug, Deserialize, Type)]
struct UnitInfo(
    String,        // name
    String,        // description
    String,        // load state
    String,        // active state
    String,        // sub state
    String,        // following
    zbus::zvariant::OwnedObjectPath, // unit path
    u32,           // job id
    String,        // job type
    zbus::zvariant::OwnedObjectPath, // job path
);

/// Systemd failure monitor.
pub struct SystemdFailedMonitor {
    _thread_handle: Option<thread::JoinHandle<()>>,
}

impl SystemdFailedMonitor {
    pub fn new(service: SystemdFailedService, sender: Sender<crate::system_state::messages::SystemStateUpdate>) -> Self {
        // Prime the cache synchronously so the initial snapshot handed to
        // subscribers already reflects reality (the change detection below
        // only broadcasts on differences, so a stale empty cache would
        // otherwise hide existing failures until the next change).
        let mut proxy: Option<SystemdManagerProxyBlocking<'static>> = None;
        if let Ok(units) = check_failed_units(&mut proxy) {
            service.update_failed_units(units);
        }

        let thread_handle = thread::spawn(move || {
            debug!("Systemd failed units monitor started");

            let mut proxy = proxy;

            loop {
                match check_failed_units(&mut proxy) {
                    Ok(units) => {
                        if let Some(failed) = service.update_failed_units(units) {
                            debug!("Failed units changed: {} unit(s)", failed.count);
                            sender
                                .send(crate::system_state::messages::SystemStateUpdate::SystemdFailed(failed))
                                .ok();
                        }
                    },
                    Err(e) => {
                        warn!("Failed to check systemd units: {}", e);
                    },
                }

                thread::sleep(Duration::from_secs(5));
            }
        });

        SystemdFailedMonitor {
            _thread_handle: Some(thread_handle),
        }
    }
}

/// Query the failed unit list, preferring the systemd user manager D-Bus
/// API (no fork) and falling back to `systemctl` when D-Bus is unavailable.
fn check_failed_units(
    proxy: &mut Option<SystemdManagerProxyBlocking<'static>>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    if proxy.is_none()
        && let Ok(conn) = zbus::blocking::Connection::session()
    {
        *proxy = SystemdManagerProxyBlocking::builder(&conn).build().ok();
    }

    if let Some(manager) = proxy.as_ref() {
        match manager.list_units_by_patterns(&["failed"], &[]) {
            Ok(units) => {
                return Ok(units
                    .into_iter()
                    .filter(|unit| unit.3 == "failed")
                    .map(|unit| unit.0)
                    .collect());
            },
            Err(e) => {
                warn!("systemd D-Bus query failed, falling back to systemctl: {}", e);
            },
        }
    }

    // Fallback (and repair the proxy for the next round): the old
    // subprocess path, kept for environments without the user manager bus.
    *proxy = None;
    check_failed_units_via_systemctl()
}

fn check_failed_units_via_systemctl() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use std::process::Stdio;

    let output = Command::new("systemctl")
        .args(["--user", "--failed", "--no-legend"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let units: Vec<String> = output_str
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && (line.starts_with('●') || line.contains("failed")))
        .map(|line| line.to_string())
        .collect();

    Ok(units)
}
