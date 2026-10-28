//! Airplane mode control via rfkill.
//!
//! State is read from `/sys/class/rfkill` (no fork) and kept in a cache that
//! every `is_enabled()` consults, so the GTK main thread never spawns an
//! `rfkill` subprocess. External changes are detected through kernel rfkill
//! uevents (udev) with a low-frequency sysfs poll as a safety net, replacing
//! the old fork-every-2-seconds polling loop.

use log::{
    debug,
    error,
    warn,
};
use serde::Deserialize;
use std::{
    path::Path,
    sync::{
        Arc,
        Mutex,
        atomic::{
            AtomicBool,
            Ordering,
        },
    },
    thread,
    time::Duration,
};

/// Safety-net polling interval when udev uevents are the primary signal.
const FALLBACK_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Airplane Mode service - controls WiFi and Bluetooth via rfkill.
pub struct AirplaneModeService {
    /// Last known airplane mode state, updated by the sysfs reader and kept
    /// fresh by the monitor. Reads are fork-free.
    state: Arc<Mutex<bool>>,
}

#[derive(Debug, Deserialize)]
struct RfkillOutput {
    rfkilldevices: Vec<RfkillDevice>,
}

#[derive(Debug, Deserialize)]
struct RfkillDevice {
    #[serde(rename = "type")]
    device_type: String,
    device: String,
    soft: String,
    hard: String,
}

impl AirplaneModeService {
    pub fn new() -> Self {
        let initial_state = Self::read_state_from_sysfs().unwrap_or_else(Self::is_enabled_static);
        debug!(
            "AirplaneModeService initialized with state: {}",
            initial_state
        );
        Self {
            state: Arc::new(Mutex::new(initial_state)),
        }
    }

    /// Check if airplane mode is enabled (all rfkill devices blocked).
    ///
    /// Reads the cached state — no subprocess fork, safe on the GTK main
    /// thread.
    pub fn is_enabled(&self) -> bool {
        *self.state.lock().unwrap()
    }

    /// Re-read the state from sysfs and update the cache.
    pub fn refresh_state(&self) -> bool {
        if let Some(state) = Self::read_state_from_sysfs() {
            *self.state.lock().unwrap() = state;
            state
        } else {
            self.is_enabled()
        }
    }

    /// Read the airplane mode state directly from `/sys/class/rfkill`.
    ///
    /// Mirrors the `rfkill --json` semantics the forked version used:
    /// airplane mode is on when every rfkill device is soft- or hard-blocked
    /// (vacuously true when there are no devices). Returns `None` when sysfs
    /// cannot be read (e.g. non-Linux platforms).
    fn read_state_from_sysfs() -> Option<bool> {
        let rfkill_class = Path::new("/sys/class/rfkill");
        let entries = std::fs::read_dir(rfkill_class).ok()?;
        let mut any_device = false;
        let mut all_blocked = true;

        for entry in entries.flatten() {
            let dir = entry.path();
            let (Some(soft), Some(hard)) = (
                std::fs::read_to_string(dir.join("soft")).ok(),
                std::fs::read_to_string(dir.join("hard")).ok(),
            ) else {
                // Incomplete sysfs entry: treat as a blocking device so we
                // never report airplane mode on when in doubt.
                any_device = true;
                all_blocked = all_blocked && false;
                continue;
            };

            any_device = true;
            let blocked = soft.trim() == "1" || hard.trim() == "1";
            all_blocked = all_blocked && blocked;
        }

        // Preserve the `all(!enabled)` vacuous-truth semantics of the
        // rfkill-based reader when no devices exist.
        if !any_device {
            return Some(true);
        }
        Some(all_blocked)
    }

    /// Static method to check state without instance (spawns `rfkill`).
    ///
    /// Prefer [`Self::is_enabled`] on a service instance: it reads the
    /// fork-free cache. This static helper is the fallback for callers
    /// without a service.
    pub fn is_enabled_static() -> bool {
        match Self::get_status() {
            Ok(status) => status.iter().all(|device| !device.enabled),
            Err(e) => {
                error!("Failed to get airplane mode status: {}", e);
                false
            },
        }
    }

    /// Get detailed status of all rfkill devices (spawns `rfkill --json`).
    pub fn get_status() -> Result<Vec<DeviceStatus>, String> {
        let output = std::process::Command::new("rfkill")
            .arg("--json")
            .env("LANG", "C")
            .output()
            .map_err(|e| format!("Failed to execute rfkill: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "rfkill command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        let rfkill_output: RfkillOutput = serde_json::from_str(&json_str).map_err(|e| format!("Failed to parse rfkill output: {}", e))?;

        Ok(rfkill_output
            .rfkilldevices
            .into_iter()
            .map(|device| DeviceStatus {
                device_type: device.device_type,
                device_name: device.device,
                enabled: device.soft == "unblocked" && device.hard == "unblocked",
            })
            .collect())
    }

    /// Enable airplane mode (block wifi and bluetooth).
    pub fn enable() -> Result<(), String> {
        debug!("Enabling airplane mode (blocking wifi and bluetooth)");

        Self::block_device("wifi")?;
        Self::block_device("bluetooth")?;

        Ok(())
    }

    /// Disable airplane mode (unblock wifi and bluetooth).
    pub fn disable() -> Result<(), String> {
        debug!("Disabling airplane mode (unblocking wifi and bluetooth)");

        Self::unblock_device("wifi")?;
        Self::unblock_device("bluetooth")?;

        Ok(())
    }

    /// Toggle airplane mode.
    ///
    /// The `rfkill` invocations run on a background thread so the caller
    /// (the GTK main thread) never blocks on subprocesses. The cache is
    /// updated optimistically so UI reads stay in sync immediately, and the
    /// uevent monitor corrects it if the underlying operation failed.
    pub fn toggle(&self) -> Result<(), String> {
        let target = !self.is_enabled();
        *self.state.lock().unwrap() = target;
        debug!("Toggling airplane mode to {} (background)", target);

        let state = Arc::clone(&self.state);
        thread::spawn(move || {
            let result = if target {
                Self::enable()
            } else {
                Self::disable()
            };
            if let Err(e) = result {
                error!("Failed to toggle airplane mode: {}", e);
            }
            // Adopt whatever state the system actually reached.
            if let Some(actual) = Self::read_state_from_sysfs() {
                *state.lock().unwrap() = actual;
            }
        });

        Ok(())
    }

    /// Create a monitor that tracks airplane mode state changes.
    ///
    /// Primary signal: kernel rfkill uevents (udev) — external tools
    /// flipping rfkill state emit a "change" uevent, so no polling is
    /// needed to notice them. A low-frequency sysfs poll acts as a safety
    /// net in case a uevent is missed. Neither path forks a subprocess.
    pub fn monitor_airplane_mode<F>(&self, callback: F) -> AirplaneModeMonitor
    where
        F: Fn(bool) + Send + Sync + 'static,
    {
        let state = Arc::clone(&self.state);
        let last_emitted = Arc::new(Mutex::new(self.is_enabled()));
        let callback = Arc::new(callback);
        let stop = Arc::new(AtomicBool::new(false));

        // udev listener: reacts to rfkill state changes the moment they
        // happen. This thread blocks on the udev socket and is
        // intentionally detached — it lives for the process lifetime.
        {
            let state = Arc::clone(&state);
            let last_emitted = Arc::clone(&last_emitted);
            let callback = Arc::clone(&callback);
            thread::spawn(move || match udev::MonitorBuilder::new()
                .and_then(|builder| builder.match_subsystem("rfkill"))
                .and_then(|builder| builder.listen())
            {
                Ok(socket) => {
                    debug!("Airplane mode udev monitor started");
                    for _ in socket.iter().filter(|event| event.event_type() == udev::EventType::Change) {
                        emit_if_changed(&state, &last_emitted, &callback);
                    }
                },
                Err(e) => {
                    warn!("Airplane mode udev monitor unavailable: {}", e);
                },
            });
        }

        // Fallback poll: cheap sysfs reads that only act when they disagree
        // with the last emitted state.
        let stop_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                thread::sleep(FALLBACK_POLL_INTERVAL);
                if stop_thread.load(Ordering::SeqCst) {
                    break;
                }
                emit_if_changed(&state, &last_emitted, &callback);
            }
            debug!("Airplane mode fallback poll stopped");
        });

        AirplaneModeMonitor {
            stop,
            handle: Some(handle),
        }
    }

    /// Block a specific device type.
    fn block_device(device_type: &str) -> Result<(), String> {
        let output = std::process::Command::new("rfkill")
            .arg("block")
            .arg(device_type)
            .output()
            .map_err(|e| format!("Failed to block {}: {}", device_type, e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to block {}: {}",
                device_type,
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        debug!("Blocked {}", device_type);
        Ok(())
    }

    /// Unblock a specific device type.
    fn unblock_device(device_type: &str) -> Result<(), String> {
        let output = std::process::Command::new("rfkill")
            .arg("unblock")
            .arg(device_type)
            .output()
            .map_err(|e| format!("Failed to unblock {}: {}", device_type, e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to unblock {}: {}",
                device_type,
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        debug!("Unblocked {}", device_type);
        Ok(())
    }
}

/// Re-read sysfs, update the cache, and invoke the callback when the state
/// actually changed (also fixing an optimistic cache after a failed toggle).
fn emit_if_changed(
    state: &Arc<Mutex<bool>>,
    last_emitted: &Arc<Mutex<bool>>,
    callback: &Arc<impl Fn(bool)>,
) {
    let Some(current) = AirplaneModeService::read_state_from_sysfs() else {
        return;
    };

    let changed = {
        let mut cached = state.lock().unwrap();
        let mut last = last_emitted.lock().unwrap();
        // Emit when the system state differs from what listeners last saw,
        // or when the cache was written optimistically (toggle) and the
        // system landed elsewhere — so a failed toggle still triggers a
        // correcting broadcast instead of leaving the UI out of sync.
        let changed = *last != current || *cached != current;
        *cached = current;
        *last = current;
        changed
    };

    if changed {
        debug!("Airplane mode state changed: {}", current);
        callback(current);
    }
}

impl Default for AirplaneModeService {
    fn default() -> Self {
        Self::new()
    }
}

/// Status of a single rfkill device.
#[derive(Debug, Clone)]
pub struct DeviceStatus {
    pub device_type: String,
    pub device_name: String,
    pub enabled: bool,
}

/// Monitor for airplane mode state changes (uevent-driven + fallback poll).
pub struct AirplaneModeMonitor {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for AirplaneModeMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
            debug!("Airplane mode monitor stopped");
        }
    }
}
