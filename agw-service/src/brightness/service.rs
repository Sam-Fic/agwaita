//! Brightness Service - Hybrid approach combining Agwaita + Ashell best practices
//!
//! Features:
//! - inotify monitoring (Agwaita) for brightness file changes (external tools)
//! - udev monitoring (Ashell) for device hotplug detection
//! - logind D-Bus for writing (from Ashell) to avoid permission issues
//! - Normalized 0.0-1.0 API (from Agwaita) for ergonomics
//! - Signal-based events (Agwaita pattern) for consistency
//!
//! Note: udev does NOT generate events for sysfs file modifications, only for
//! device add/remove. Therefore inotify is necessary for detecting external brightness changes.

use crate::signal::{
    Signal,
    SignalHandler,
};
use log::{
    error,
    warn,
};
use std::{
    fs,
    io::Error,
    path::{
        Path,
        PathBuf,
    },
    sync::{
        Arc,
        Mutex,
        mpsc::Sender,
    },
    thread,
    time::Instant,
};
use udev::Enumerator;
use zbus::{
    blocking::Connection,
    proxy,
};

/// D-Bus proxy for logind Session interface (used for setting brightness without root)
#[proxy(
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1/session/auto",
    interface = "org.freedesktop.login1.Session"
)]
trait BrightnessCtrl {
    fn set_brightness(&self, subsystem: &str, name: &str, value: u32) -> zbus::Result<()>;
}

pub struct BrightnessService {
    /// Path to backlight device in sysfs
    device_path: Option<PathBuf>,

    /// Maximum brightness value from device
    max_brightness: u32,

    /// Current brightness level (raw value)
    current_brightness: Arc<Mutex<u32>>,

    /// Signal emitted when brightness changes (normalized 0.0-1.0)
    brightness_changed: Signal<f64>,

    /// D-Bus connection for logind (Arc for clonability)
    conn: Option<Arc<Connection>>,

    /// Sender to the single brightness writer thread
    writer_tx: Option<Sender<u32>>,

    /// Timestamp of last set_brightness call (for debouncing inotify)
    last_set_time: Arc<Mutex<Option<Instant>>>,
}

impl BrightnessService {
    pub fn new() -> Self {
        let (device_path, max_brightness, current_brightness) = Self::init_device();

        let conn = match Connection::system() {
            Ok(conn) => Some(Arc::new(conn)),
            Err(e) => {
                error!("Failed to connect to D-Bus for brightness control: {}", e);
                None
            },
        };

        let writer_tx = conn
            .as_ref()
            .zip(device_path.as_ref())
            .map(|(conn, device_path)| {
                let conn = Arc::clone(conn);
                let device_name = device_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                Self::start_writer_thread(conn, device_name)
            });

        Self {
            device_path,
            max_brightness,
            current_brightness: Arc::new(Mutex::new(current_brightness)),
            brightness_changed: Signal::new(),
            conn,
            writer_tx,
            last_set_time: Arc::new(Mutex::new(None)),
        }
    }

    /// Spawn the single long-lived writer thread for logind calls.
    ///
    /// Dragging the slider used to spawn one thread (and one D-Bus proxy)
    /// per percentage step with no ordering guarantees, so tens of
    /// concurrent writes could race and make the backlight jump back and
    /// forth. This worker drains its queue and only forwards the newest
    /// value, reusing one proxy — a fast drag becomes one D-Bus call with
    /// the final value.
    fn start_writer_thread(conn: Arc<Connection>, device_name: String) -> Sender<u32> {
        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        thread::spawn(move || {
            // The proxy is created lazily and re-created on failure, so a
            // transient D-Bus problem at startup no longer permanently
            // disables brightness control.
            let mut proxy = BrightnessCtrlProxyBlocking::new(conn.as_ref()).ok();

            while let Ok(first_value) = rx.recv() {
                // Coalesce everything already queued, keeping only the
                // latest value.
                let mut value = first_value;
                while let Ok(newer) = rx.try_recv() {
                    value = newer;
                }

                if proxy.is_none() {
                    proxy = BrightnessCtrlProxyBlocking::new(conn.as_ref()).ok();
                }

                match proxy.as_ref() {
                    Some(writer) => {
                        if let Err(e) = writer.set_brightness("backlight", &device_name, value) {
                            error!("Failed to set brightness via logind: {}", e);
                            // Reconnect on the next request.
                            proxy = None;
                        }
                    },
                    None => {
                        error!("Failed to create brightness proxy");
                    },
                }
            }
        });
        tx
    }

    /// Initialize brightness device from sysfs
    fn init_device() -> (Option<PathBuf>, u32, u32) {
        match Self::enumerate_backlight_devices() {
            Ok(devices) => {
                if let Some(device) = devices.into_iter().next() {
                    let device_path = device.syspath().to_path_buf();

                    let max = Self::read_brightness_value(&device_path.join("max_brightness")).unwrap_or(100);
                    let current = Self::read_brightness_value(&device_path.join("brightness")).unwrap_or(max / 2);

                    return (Some(device_path), max, current);
                }
            },
            Err(e) => {
                error!("Failed to enumerate backlight devices: {}", e);
            },
        }

        error!("No backlight device found");
        (None, 100, 50)
    }

    /// Enumerate backlight devices using udev (Ashell approach)
    fn enumerate_backlight_devices() -> Result<Vec<udev::Device>, Error> {
        let mut enumerator = Enumerator::new()?;
        enumerator.match_subsystem("backlight")?;
        Ok(enumerator.scan_devices()?.collect())
    }

    /// Read brightness value from sysfs file
    fn read_brightness_value(path: &Path) -> Option<u32> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
    }

    /// Get current brightness (normalized 0.0-1.0)
    pub fn get_brightness(&self) -> f64 {
        let current = *self.current_brightness.lock().unwrap();
        current as f64 / self.max_brightness as f64
    }

    /// Set brightness (normalized 0.0-1.0) via logind D-Bus
    pub fn set_brightness(&self, value: f64) {
        let clamped = value.clamp(0.0, 1.0);
        let raw_value = (clamped * self.max_brightness as f64).round() as u32;

        *self.last_set_time.lock().unwrap() = Some(Instant::now());

        if let Some(tx) = &self.writer_tx {
            // The writer thread coalesles bursts, so a slider drag results
            // in exactly one logind call with the final value.
            let _ = tx.send(raw_value);
        } else {
            warn!("D-Bus connection or device path not available for brightness control");
        }

        *self.current_brightness.lock().unwrap() = raw_value;

        // Note: Signal is NOT emitted here to avoid duplicate events.
        // The inotify monitor will detect the change and emit the signal.
    }

    /// Connect a callback to brightness changes
    pub fn connect_brightness_changed<F>(&self, callback: F) -> SignalHandler
    where
        F: Fn(f64) + Send + 'static,
    {
        self.brightness_changed.connect(callback)
    }

    /// Disconnect a signal handler
    pub fn disconnect(&self, handler: SignalHandler) {
        self.brightness_changed.disconnect(handler);
    }

    /// Start monitoring brightness changes
    pub fn start_monitoring(&self) {
        if let Some(ref device_path) = self.device_path {
            let device_path = device_path.clone();
            let max_brightness = self.max_brightness;
            let current_brightness = Arc::clone(&self.current_brightness);
            let signal = self.brightness_changed.clone();
            let last_set_time = Arc::clone(&self.last_set_time);

            super::monitor::start_udev_monitor(
                device_path,
                max_brightness,
                current_brightness,
                signal,
                last_set_time,
            );
        } else {
            warn!("No device path available, brightness monitoring disabled");
        }
    }
}

impl Default for BrightnessService {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for BrightnessService {
    fn clone(&self) -> Self {
        Self {
            device_path: self.device_path.clone(),
            max_brightness: self.max_brightness,
            current_brightness: Arc::clone(&self.current_brightness),
            brightness_changed: self.brightness_changed.clone(),
            conn: self.conn.as_ref().map(Arc::clone),
            writer_tx: self.writer_tx.clone(),
            last_set_time: Arc::clone(&self.last_set_time),
        }
    }
}
