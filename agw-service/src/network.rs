//! Network monitoring via systemd-networkd/iwd (D-Bus + polling).
//!
//! State collection avoids subprocess forks: interface state comes from
//! sysfs (`/sys/class/net`), the default route from `/proc/net/route`, and
//! WiFi signal strength from `/proc/net/wireless` (with `iw` only as a
//! fallback when the wireless procfs entry is missing or stale). Combined
//! with change detection, an idle system performs zero forks.

use crate::{
    dbus_const::{
        DBUS_INTERFACE,
        DBUS_PATH,
        DBUS_PROPERTIES_CHANGED_MEMBER,
        DBUS_PROPERTIES_INTERFACE,
    },
    runtime,
};
use log::{
    debug,
    error,
    info,
    warn,
};
use std::{
    path::Path,
    process::Command,
    sync::{
        Arc,
        Mutex,
        atomic::{
            AtomicBool,
            Ordering,
        },
    },
};

/// Signal strength changes below this threshold (percent) do not trigger a
/// callback, so RSSI jitter stays silent.
const STRENGTH_HYSTERESIS: u8 = 5;

/// How often WiFi signal strength is sampled while a WiFi connection is up.
const WIFI_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub enum NetworkType {
    Wired,
    Wifi,
    None,
}

#[derive(Debug, Clone)]
pub struct NetworkState {
    pub network_type: NetworkType,
    pub connected: bool,
    pub wifi_strength: u8,
    pub interface: Option<String>,
}

/// NetworkService - Manages network state monitoring using systemd-networkd/iwd.
/// Uses `ip` and `iw` commands to detect network state.
pub struct NetworkService {
    network_type: Arc<Mutex<NetworkType>>,
    connected: Arc<Mutex<bool>>,
    wifi_strength: Arc<Mutex<u8>>,
    interface: Arc<Mutex<Option<String>>>,
}

impl NetworkService {
    /// Creates the service with a cheap initial probe.
    ///
    /// The probe is dominated by sysfs/procfs reads (no `ip`/`iw` fork on
    /// the common paths) and guarantees that subscribers get the real
    /// network state in their initial snapshot, even if they subscribe
    /// before the background monitor's first broadcast. The monitor still
    /// performs its own refresh on its thread and emits corrections.
    pub fn new() -> Self {
        let (network_type, connected, wifi_strength, interface) =
            match Self::probe_network_state() {
                Some(state) => (
                    state.network_type,
                    state.connected,
                    state.wifi_strength,
                    state.interface,
                ),
                None => (NetworkType::None, false, 0, None),
            };

        Self {
            network_type: Arc::new(Mutex::new(network_type)),
            connected: Arc::new(Mutex::new(connected)),
            wifi_strength: Arc::new(Mutex::new(wifi_strength)),
            interface: Arc::new(Mutex::new(interface)),
        }
    }

    /// Get current network state.
    pub fn get_state(&self) -> NetworkState {
        NetworkState {
            network_type: self.network_type.lock().unwrap().clone(),
            connected: *self.connected.lock().unwrap(),
            wifi_strength: *self.wifi_strength.lock().unwrap(),
            interface: self.interface.lock().unwrap().clone(),
        }
    }

    /// Start hybrid monitoring: D-Bus events + conditional WiFi strength polling.
    ///
    /// The initial state refresh and broadcast happen on the monitor's own
    /// thread so neither the caller nor the GTK main thread ever blocks on
    /// interface probing. Subsequent updates only invoke the callback when
    /// the state actually changed (with hysteresis for signal strength), so
    /// unchanged poll results no longer wake every subscriber.
    pub fn start_hybrid_monitor<F>(&self, callback: F)
    where
        F: Fn(NetworkType, bool, u8) + Send + 'static + Clone,
    {
        let service = self.clone_service();
        let callback = Arc::new(Mutex::new(callback));
        let initial_state = service.get_state();
        let wifi_polling_active = Arc::new(AtomicBool::new(
            initial_state.network_type == NetworkType::Wifi && initial_state.connected,
        ));
        let last_emitted: Arc<Mutex<Option<NetworkState>>> = Arc::new(Mutex::new(None));

        let service_dbus = service.clone_service();
        let callback_dbus = Arc::clone(&callback);
        let wifi_polling_ref = Arc::clone(&wifi_polling_active);
        let last_emitted_dbus = Arc::clone(&last_emitted);

        std::thread::spawn(move || {
            runtime::runtime().block_on(async move {
                use futures::StreamExt;
                use zbus::Connection;

                match Connection::system().await {
                    Ok(connection) => {
                        info!("Network D-Bus monitor connected");

                        let match_rule_str = format!(
                            "type='signal',interface='{interface}',member='{member}',path_namespace='/net/connman/iwd'",
                            interface = DBUS_PROPERTIES_INTERFACE,
                            member = DBUS_PROPERTIES_CHANGED_MEMBER
                        );

                        if let Err(e) = connection
                            .call_method(
                                Some(DBUS_INTERFACE),
                                DBUS_PATH,
                                Some(DBUS_INTERFACE),
                                "AddMatch",
                                &(match_rule_str),
                            )
                            .await
                        {
                            error!("Failed to subscribe to iwd signals: {}", e);
                            return;
                        }

                        info!("Subscribed to iwd PropertiesChanged signals");

                        // Initial refresh + broadcast: replaces the
                        // synchronous probe that used to run on the GTK
                        // main thread during startup.
                        emit_state_if_changed(
                            &service_dbus,
                            &callback_dbus,
                            &wifi_polling_ref,
                            &last_emitted_dbus,
                        );

                        let mut stream = zbus::MessageStream::from(&connection);

                        while let Some(msg) = stream.next().await {
                            if let Ok(msg) = msg {
                                let header = msg.header();

                                if let (Some(_path), Some(interface), Some(member)) = (header.path(), header.interface(), header.member()) {
                                    let interface_str = interface.as_str();
                                    let member_str = member.as_str();

                                    if interface_str == DBUS_PROPERTIES_INTERFACE && member_str == DBUS_PROPERTIES_CHANGED_MEMBER {
                                        use zbus::zvariant::OwnedValue;

                                        if let Ok((interface_name, changed_properties, _)) = msg.body().deserialize::<(
                                            String,
                                            std::collections::HashMap<String, OwnedValue>,
                                            Vec<String>,
                                        )>() {
                                            let is_station_state = interface_name == "net.connman.iwd.Station" && changed_properties.contains_key("State");
                                            let is_network_connected =
                                                interface_name == "net.connman.iwd.Network" && changed_properties.contains_key("Connected");
                                            let is_adapter_powered = interface_name == "net.connman.iwd.Adapter" && changed_properties.contains_key("Powered");

                                            if is_station_state || is_network_connected || is_adapter_powered {
                                                emit_state_if_changed(
                                                    &service_dbus,
                                                    &callback_dbus,
                                                    &wifi_polling_ref,
                                                    &last_emitted_dbus,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        info!("Network D-Bus monitor stream ended");
                    },
                    Err(e) => {
                        error!(
                            "Failed to connect to system bus for network monitoring: {}",
                            e
                        );
                    },
                }
            });
        });

        let service_poll = service.clone_service();
        let callback_poll = Arc::clone(&callback);
        let wifi_polling_poll = Arc::clone(&wifi_polling_active);
        let last_emitted_poll = Arc::clone(&last_emitted);

        std::thread::spawn(move || {
            // Initial sample: the D-Bus thread performs one too, but this
            // keeps the state correct on systems where the bus monitor is
            // unavailable. Change detection collapses the duplicate.
            emit_state_if_changed(
                &service_poll,
                &callback_poll,
                &wifi_polling_poll,
                &last_emitted_poll,
            );

            loop {
                std::thread::sleep(WIFI_POLL_INTERVAL);

                if wifi_polling_poll.load(Ordering::Relaxed) {
                    // Refresh only while a WiFi connection is up: this
                    // sample exists purely to track signal strength. Link
                    // and route changes arrive via the D-Bus monitor.
                    emit_state_if_changed(
                        &service_poll,
                        &callback_poll,
                        &wifi_polling_poll,
                        &last_emitted_poll,
                    );
                }
            }
        });

        info!("Network hybrid monitor started (D-Bus events + conditional WiFi polling)");
    }

    /// Create a monitor that checks for network state changes.
    #[allow(dead_code)] // Will be used in the future with networkd-manager
    pub fn monitor_network<F>(&self, callback: F) -> NetworkMonitor
    where
        F: Fn(NetworkType, bool, u8) + Send + 'static,
    {
        let state = self.get_state();
        NetworkMonitor {
            service: Arc::new(self.clone_service()),
            last_type: state.network_type,
            last_connected: state.connected,
            last_strength: state.wifi_strength,
            callback: Box::new(callback),
        }
    }

    /// Collect the current network state.
    ///
    /// Everything except the `iw` fallback is pure procfs/sysfs reads, so a
    /// refresh normally costs zero forks: interfaces come from
    /// `/sys/class/net/*/flags`, the default route from `/proc/net/route`,
    /// and WiFi signal strength from `/proc/net/wireless`.
    fn get_network_state(&self) -> Option<NetworkState> {
        Self::probe_network_state()
    }

    fn probe_network_state() -> Option<NetworkState> {
        let interfaces = Self::up_interfaces()?;
        let default_route_interfaces = Self::default_route_interfaces()?;
        let has_default_route = !default_route_interfaces.is_empty();

        let wired_interfaces: Vec<&str> = interfaces
            .iter()
            .map(String::as_str)
            .filter(|name| Self::is_wired_name(name))
            .collect();

        let wifi_interfaces: Vec<&str> = interfaces
            .iter()
            .map(String::as_str)
            .filter(|name| Self::is_wifi_name(name))
            .collect();

        if !has_default_route {
            if let Some(wifi_iface) = wifi_interfaces.first() {
                return Some(Self::wifi_state(wifi_iface));
            }

            if let Some(wired_iface) = wired_interfaces.first() {
                return Some(NetworkState {
                    network_type: NetworkType::Wired,
                    connected: false,
                    wifi_strength: 0,
                    interface: Some((*wired_iface).to_string()),
                });
            }
            return Some(NetworkState {
                network_type: NetworkType::None,
                connected: false,
                wifi_strength: 0,
                interface: None,
            });
        }

        let default_over_wired = default_route_interfaces
            .iter()
            .any(|iface| Self::is_wired_name(iface));

        if !wired_interfaces.is_empty() && default_over_wired {
            return Some(NetworkState {
                network_type: NetworkType::Wired,
                connected: true,
                wifi_strength: 0,
                interface: wired_interfaces.first().map(|s| (*s).to_string()),
            });
        }

        if let Some(wifi_iface) = wifi_interfaces.first() {
            return Some(Self::wifi_state(wifi_iface));
        }

        Some(NetworkState {
            network_type: NetworkType::None,
            connected: false,
            wifi_strength: 0,
            interface: None,
        })
    }

    fn is_wired_name(name: &str) -> bool {
        name.contains("eth") || name.contains("enp") || name.contains("eno")
    }

    fn is_wifi_name(name: &str) -> bool {
        name.contains("wlan") || name.contains("wlp")
    }

    /// Interfaces that are administratively UP, from sysfs (equivalent of
    /// `ip -br link show up` without the fork).
    fn up_interfaces() -> Option<Vec<String>> {
        let net_class = Path::new("/sys/class/net");
        let mut names = Vec::new();
        for entry in std::fs::read_dir(net_class).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "lo" {
                continue;
            }
            // IFF_UP is bit 0 of the flags bitmask.
            let up = std::fs::read_to_string(net_class.join(&name).join("flags"))
                .ok()
                .and_then(|flags| u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16).ok())
                .is_some_and(|flags| flags & 0x1 != 0);
            if up {
                names.push(name);
            }
        }
        Some(names)
    }

    /// Interfaces that own a default route, from `/proc/net/route`
    /// (equivalent of `ip route show default` without the fork).
    fn default_route_interfaces() -> Option<Vec<String>> {
        let route_table = std::fs::read_to_string("/proc/net/route").ok()?;
        let mut interfaces = Vec::new();
        for line in route_table.lines().skip(1) {
            let mut columns = line.split_whitespace();
            let (Some(iface), Some(destination), Some(flags)) =
                (columns.next(), columns.next(), columns.next())
            else {
                continue;
            };
            // Destination 00000000 = default route; RTF_UP (0x1) must be set.
            let is_default = destination == "00000000"
                && u32::from_str_radix(flags, 16)
                    .is_ok_and(|flags| flags & 0x1 != 0);
            if is_default && !interfaces.iter().any(|existing| existing == iface) {
                interfaces.push(iface.to_string());
            }
        }
        Some(interfaces)
    }

    /// WiFi state for one interface: connected status and signal strength.
    ///
    /// `/proc/net/wireless` is the fast path (no fork); `iw dev <iface> link`
    /// only runs when procfs has no usable level (stale wext data or an
    /// interface that is actually disconnected).
    fn wifi_state(interface: &str) -> NetworkState {
        let disconnected = NetworkState {
            network_type: NetworkType::Wifi,
            connected: false,
            wifi_strength: 0,
            interface: Some(interface.to_string()),
        };

        if let Some(signal_dbm) = Self::wireless_level_dbm(interface)
            && signal_dbm < 0
        {
            return NetworkState {
                network_type: NetworkType::Wifi,
                connected: true,
                wifi_strength: Self::strength_from_dbm(signal_dbm),
                interface: Some(interface.to_string()),
            };
        }
        // Level of 0 usually means "not associated" — but confirm via `iw`
        // because some drivers never update the procfs entry.

        match Command::new("iw")
            .args(["dev", interface, "link"])
            .output()
        {
            Ok(iw_output) => {
                let iw_stdout = String::from_utf8_lossy(&iw_output.stdout);

                if iw_stdout.contains("Not connected") {
                    return disconnected;
                }

                let strength = Self::parse_wifi_strength(&iw_stdout);
                NetworkState {
                    network_type: NetworkType::Wifi,
                    connected: true,
                    wifi_strength: strength,
                    interface: Some(interface.to_string()),
                }
            },
            Err(e) => {
                warn!("Failed to query iw for interface {}: {}", interface, e);
                NetworkState {
                    network_type: NetworkType::Wifi,
                    connected: true,
                    wifi_strength: 50,
                    interface: Some(interface.to_string()),
                }
            },
        }
    }

    /// Signal level in dBm from `/proc/net/wireless`, if the interface is
    /// listed there.
    fn wireless_level_dbm(interface: &str) -> Option<i32> {
        let wireless = std::fs::read_to_string("/proc/net/wireless").ok()?;
        let prefix = format!("{}:", interface);
        for line in wireless.lines().skip(2) {
            let mut columns = line.split_whitespace();
            let Some(face) = columns.next() else {
                continue;
            };
            if face != prefix {
                continue;
            }
            // Columns: face | status | link | level | noise | ...
            let _status = columns.next()?;
            let _link = columns.next()?;
            let level = columns.next()?;
            return level.trim_end_matches('.').parse::<i32>().ok();
        }
        None
    }

    fn refresh_state(&self) -> bool {
        if let Some(state) = self.get_network_state() {
            *self.network_type.lock().unwrap() = state.network_type;
            *self.connected.lock().unwrap() = state.connected;
            *self.wifi_strength.lock().unwrap() = state.wifi_strength;
            *self.interface.lock().unwrap() = state.interface;
            true
        } else {
            error!("Failed to get network state");
            false
        }
    }

    fn clone_service(&self) -> Self {
        Self {
            network_type: Arc::clone(&self.network_type),
            connected: Arc::clone(&self.connected),
            wifi_strength: Arc::clone(&self.wifi_strength),
            interface: Arc::clone(&self.interface),
        }
    }

    fn parse_wifi_strength(iw_output: &str) -> u8 {
        match Self::extract_signal_dbm(iw_output) {
            Some(signal_dbm) => Self::strength_from_dbm(signal_dbm),
            None => 50,
        }
    }

    /// Map a signal level in dBm onto the 0-100% scale (the same curve the
    /// `iw`-based path has always used: -90 dBm → 0%, -30 dBm → 100%).
    fn strength_from_dbm(signal_dbm: i32) -> u8 {
        let strength = ((signal_dbm + 90) as f64 / 60.0 * 100.0).clamp(0.0, 100.0);
        strength as u8
    }

    fn extract_signal_dbm(iw_output: &str) -> Option<i32> {
        for line in iw_output.lines() {
            if line.contains("signal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if *part == "signal:" && i + 1 < parts.len() {
                        if let Ok(dbm) = parts[i + 1].parse::<i32>() {
                            return Some(dbm);
                        }
                    }
                }
            }
        }
        None
    }
}

impl Default for NetworkService {
    fn default() -> Self {
        Self::new()
    }
}

/// Refresh the service state, update the WiFi polling gate and invoke the
/// callback — but only when something meaningful changed: the type, the
/// connectivity, or a signal-strength move beyond the hysteresis. This keeps
/// idle polls silent instead of waking every subscriber every few seconds.
fn emit_state_if_changed<F>(
    service: &NetworkService,
    callback: &Arc<Mutex<F>>,
    wifi_polling_active: &AtomicBool,
    last_emitted: &Mutex<Option<NetworkState>>,
) where
    F: Fn(NetworkType, bool, u8) + Send,
{
    if !service.refresh_state() {
        return;
    }

    let state = service.get_state();
    wifi_polling_active.store(
        state.network_type == NetworkType::Wifi && state.connected,
        Ordering::Relaxed,
    );

    let changed = match last_emitted.lock() {
        Ok(mut last) => {
            let is_new_state = match last.as_ref() {
                Some(previous) => {
                    previous.network_type != state.network_type
                        || previous.connected != state.connected
                        || state.wifi_strength.abs_diff(previous.wifi_strength) > STRENGTH_HYSTERESIS
                },
                None => true,
            };
            if is_new_state {
                *last = Some(state.clone());
            }
            is_new_state
        },
        Err(_) => true,
    };

    if changed {
        debug!(
            "Network state changed: type={:?}, connected={}, strength={}%",
            state.network_type, state.connected, state.wifi_strength
        );
        if let Ok(cb) = callback.lock() {
            cb(state.network_type.clone(), state.connected, state.wifi_strength);
        }
    }
}

/// Monitor for network state changes.
#[allow(dead_code)] // Some fields will be used in the future with networkd-manager
pub struct NetworkMonitor {
    service: Arc<NetworkService>,
    last_type: NetworkType,
    last_connected: bool,
    last_strength: u8,
    callback: Box<dyn Fn(NetworkType, bool, u8) + Send>,
}

impl NetworkMonitor {
    /// Check for network state changes and call callback if changed.
    #[allow(dead_code)] // Will be used in the future with networkd-manager
    pub fn check(&mut self) {
        if !self.service.refresh_state() {
            return;
        }

        let state = self.service.get_state();

        let type_changed = state.network_type != self.last_type;
        let connected_changed = state.connected != self.last_connected;
        let strength_changed = state.wifi_strength.abs_diff(self.last_strength) > 5;

        if type_changed || connected_changed || strength_changed {
            debug!(
                "Network state changed: type={:?}, connected={}, strength={}%",
                state.network_type, state.connected, state.wifi_strength
            );
            (self.callback)(
                state.network_type.clone(),
                state.connected,
                state.wifi_strength,
            );
            self.last_type = state.network_type;
            self.last_connected = state.connected;
            self.last_strength = state.wifi_strength;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_from_dbm_maps_and_clamps() {
        assert_eq!(NetworkService::strength_from_dbm(-30), 100);
        assert_eq!(NetworkService::strength_from_dbm(-60), 50);
        assert_eq!(NetworkService::strength_from_dbm(-90), 0);
        assert_eq!(NetworkService::strength_from_dbm(-120), 0);
    }

    #[test]
    fn probed_state_is_internally_consistent() {
        // The initial probe reflects the host, so only assert invariants.
        let service = NetworkService::default();
        let state = service.get_state();

        if state.network_type != NetworkType::Wifi {
            assert_eq!(state.wifi_strength, 0, "non-WiFi links carry no RSSI");
        }
        if !state.connected {
            assert_eq!(state.wifi_strength, 0, "disconnected links carry no RSSI");
            assert!(state.interface.is_some() || state.network_type == NetworkType::None);
        }
    }
}
