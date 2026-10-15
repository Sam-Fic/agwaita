//! Topbar auto-hide preference via GSettings.
//!
//! Ships a tiny custom schema (`data/gsettings/agwaita.gschema.xml`,
//! installed and compiled by `make.nu install`) so the preference gets a
//! real, validated home instead of squatting another app's key.

use gtk4::{
    gio,
    glib,
    prelude::*,
};
use log::{
    debug,
    warn,
};

pub struct BarModeService;

pub struct BarModeMonitor {
    _handler_id: glib::SignalHandlerId,
    settings: gio::Settings,
}

impl BarModeService {
    const SCHEMA: &'static str = "com.github.gouvinb.agwaita";
    const KEY: &'static str = "bar-auto-hide";

    /// Check if bar auto-hide is enabled. `default` applies when the
    /// schema is not installed (e.g. running from a build tree).
    pub fn is_auto_hide(default: bool) -> bool {
        match Self::settings() {
            Some(settings) => settings.boolean(Self::KEY),
            None => default,
        }
    }

    /// Enable auto-hide: park the bars above the screen edge.
    pub fn enable() -> Result<(), String> {
        debug!("Enabling bar auto-hide");
        Self::set_value(true)
    }

    /// Disable auto-hide: pin the bars.
    pub fn disable() -> Result<(), String> {
        debug!("Disabling bar auto-hide");
        Self::set_value(false)
    }

    /// Toggle auto-hide.
    pub fn toggle() -> Result<(), String> {
        if Self::is_auto_hide(false) {
            Self::disable()
        } else {
            Self::enable()
        }
    }

    /// Monitor auto-hide preference changes.
    ///
    /// Returns a handle that must be kept alive.
    pub fn monitor_bar_mode<F>(callback: F) -> Option<BarModeMonitor>
    where
        F: Fn(bool) + Send + 'static,
    {
        let settings = Self::settings()?;

        let handler_id = settings.connect_changed(Some(Self::KEY), move |settings, _| {
            callback(settings.boolean(Self::KEY));
        });

        Some(BarModeMonitor {
            _handler_id: handler_id,
            settings,
        })
    }

    /// Settings handle for our schema, or None when the schema is not
    /// installed. Constructing gio::Settings on a missing schema aborts, so
    /// the availability check up front is load-bearing.
    fn settings() -> Option<gio::Settings> {
        let source = gio::SettingsSchemaSource::default()?;
        if source.lookup(Self::SCHEMA, true).is_none() {
            warn!("Schema {} not installed; bar mode will not persist", Self::SCHEMA);
            return None;
        }
        Some(gio::Settings::new(Self::SCHEMA))
    }

    fn set_value(enabled: bool) -> Result<(), String> {
        let settings = Self::settings().ok_or_else(|| format!("schema {} unavailable", Self::SCHEMA))?;
        settings
            .set_boolean(Self::KEY, enabled)
            .map_err(|e| e.to_string())
    }
}
