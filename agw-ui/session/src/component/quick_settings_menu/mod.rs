use gtk4::prelude::*;
use log::debug;
use relm4::{
    Component,
    ComponentController,
    ComponentParts,
    ComponentSender,
    RelmWidgetExt,
    SimpleComponent,
    adw,
    gtk,
};
use std::sync::mpsc;

pub mod icons;
pub mod popover;

use crate::system_state::messages::SystemStateUpdate;
use agw_service::{
    brightness::BrightnessService,
    network::NetworkType,
    power_mode::{
        PowerModeService,
        PowerProfile,
    },
};
use icons::{
    audio::{
        AudioIcon,
        AudioIconInput,
    },
    avatar::AvatarIcon,
    battery::icon::{
        BatteryIcon,
        BatteryIconInput,
    },
    bluetooth::icon::{
        BluetoothIcon,
        BluetoothIconInput,
    },
    brightness::icon::{
        BrightnessIcon,
        BrightnessIconInput,
    },
    dnd::{
        DoNotDisturbIcon,
        DoNotDisturbIconInput,
    },
    network::icon::{
        NetworkIcon,
        NetworkIconInput,
    },
    power_mode::icon::{
        PowerModeIcon,
        PowerModeIconInput,
    },
};
use popover::{
    QuickSettingsPopover,
    QuickSettingsPopoverInput,
    QuickSettingsPopoverOutput,
    toggles::ToggleButtonsOutput,
};

pub struct QuickSettingsMenu {
    brightness_icon: relm4::Controller<BrightnessIcon>,
    audio_icon: relm4::Controller<AudioIcon>,
    bluetooth_icon: relm4::Controller<BluetoothIcon>,
    network_icon: relm4::Controller<NetworkIcon>,
    dnd_icon: relm4::Controller<DoNotDisturbIcon>,
    power_mode_icon: relm4::Controller<PowerModeIcon>,
    battery_icon: relm4::Controller<BatteryIcon>,
    avatar_icon: relm4::Controller<AvatarIcon>,
    popover: relm4::Controller<QuickSettingsPopover>,
    popover_widget: gtk::glib::WeakRef<gtk::Popover>,
}

#[derive(Debug)]
pub enum QuickSettingsMenuInput {
    SystemStateUpdate(SystemStateUpdate),
    PowerProfileChanged(PowerProfile),
    ResetRevealers,
    ClosePopover,
}

#[relm4::component(pub)]
impl SimpleComponent for QuickSettingsMenu {
    type Init = (
        mpsc::Receiver<SystemStateUpdate>,
        std::sync::Arc<icons::audio::AudioService>,
        std::sync::Arc<BrightnessService>,
        Option<std::sync::Arc<PowerModeService>>,
    );
    type Input = QuickSettingsMenuInput;
    type Output = ();

    view! {
        #[root]
        gtk::MenuButton {
            add_css_class: "flat",
            add_css_class: "qs-button",
            set_direction: gtk::ArrowType::Down,
            // Fill the 40px topbar so the padding-free button centers its
            // 26px cells. No end margin: the avatar circle then sits at the
            // bar's uniform 4px edge margin, symmetric with the left end.
            set_valign: gtk::Align::Fill,
            // CenterBox over-allocates its end third; snap back to natural
            // width so the trailing slack doesn't sit between the last icon
            // and the bar edge.
            set_halign: gtk::Align::End,

            #[wrap(Some)]
            set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 4,

                // Square 26px cells keep every icon's allotted area square
                // (icon 16px centered, matching the tray's 34px cells).
                // The avatar gets a 20px cell so its circle sits flush.
                gtk::Box {
                    set_width_request: 26,
                    set_height_request: 26,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                    // Outer 10px so the hover plate's left edge clears the
                    // icon as much as its top edge does.
                    set_margin_start: 10,

                    model.brightness_icon.widget(),
                },
                gtk::Box {
                    set_width_request: 26,
                    set_height_request: 26,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,

                    model.audio_icon.widget(),
                },
                gtk::Box {
                    set_width_request: 26,
                    set_height_request: 26,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,

                    model.bluetooth_icon.widget(),
                },
                gtk::Box {
                    set_width_request: 26,
                    set_height_request: 26,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,

                    model.network_icon.widget(),
                },
                gtk::Box {
                    set_width_request: 26,
                    set_height_request: 26,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,

                    model.dnd_icon.widget(),
                },
                gtk::Box {
                    set_width_request: 26,
                    set_height_request: 26,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,

                    model.power_mode_icon.widget(),
                },
                model.battery_icon.widget(),
                gtk::Box {
                    set_width_request: 20,
                    set_height_request: 20,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                    // Matches the left outer margin of the hover plate.
                    set_margin_end: 10,
                    model.avatar_icon.widget(),
                },
            },

            #[wrap(Some)]
            #[name = "popover_widget"]
            set_popover = &gtk::Popover {
                adw::Clamp {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_maximum_size: 360,
                    set_width_request: 360,

                    model.popover.widget(),
                },
            }
        }
    }

    fn init(init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let (receiver, audio_service, brightness_service, power_mode_service) = init;
        debug!("Initializing QuickSettingsMenu (GlobalSystemService version)");

        // Initialize icon components with default values
        // They'll be updated when the first system state update arrives
        let brightness_icon = BrightnessIcon::builder()
            .launch(0.5)
            .forward(sender.input_sender(), |_| unreachable!());

        let audio_icon = AudioIcon::builder()
            .launch((0.5, false))
            .forward(sender.input_sender(), |_| unreachable!());

        let bluetooth_icon = BluetoothIcon::builder()
            .launch((false, 0))
            .forward(sender.input_sender(), |_| unreachable!());

        let network_icon = NetworkIcon::builder()
            .launch((NetworkType::None, false, 0))
            .forward(sender.input_sender(), |_| unreachable!());

        let dnd_icon = DoNotDisturbIcon::builder()
            .launch(false)
            .forward(sender.input_sender(), |_| unreachable!());

        let initial_power_profile = power_mode_service
            .as_ref()
            .map(|service| service.get_active_profile())
            .unwrap_or(PowerProfile::Balanced);

        let power_mode_icon = PowerModeIcon::builder()
            .launch(initial_power_profile)
            .forward(sender.input_sender(), |_| unreachable!());

        let battery_icon = BatteryIcon::builder()
            .launch((0.0, false, true))
            .forward(sender.input_sender(), |_| unreachable!());

        let avatar_icon = AvatarIcon::builder()
            .launch(())
            .forward(sender.input_sender(), |_| unreachable!());

        // Services are now received from GlobalSystemService (no longer created here)

        // Initialize popover with default battery state and services
        let popover = QuickSettingsPopover::builder()
            .launch((
                (0.0, false, true),
                audio_service,
                brightness_service,
                None,
                power_mode_service,
            )) // (battery state, audio service, brightness service, airplane mode service, power mode service)
            .forward(sender.input_sender(), |msg| {
                // Forward ToggleButtons output to QuickSettingsMenu input
                match msg {
                    QuickSettingsPopoverOutput::ToggleButtons(ToggleButtonsOutput::PowerProfileChanged(profile)) => {
                        QuickSettingsMenuInput::PowerProfileChanged(profile)
                    },
                    QuickSettingsPopoverOutput::ToggleButtons(ToggleButtonsOutput::ClosePopover) | QuickSettingsPopoverOutput::ClosePopover => {
                        QuickSettingsMenuInput::ClosePopover
                    },
                }
            });

        let mut model = QuickSettingsMenu {
            brightness_icon,
            audio_icon,
            bluetooth_icon,
            network_icon,
            dnd_icon,
            power_mode_icon,
            battery_icon,
            avatar_icon,
            popover,
            popover_widget: gtk::glib::WeakRef::new(), // Temporary, will be updated after view_output
        };

        // Spawn a task to listen for system state updates from the receiver
        let input_sender = sender.input_sender().clone();
        std::thread::spawn(move || {
            debug!("QuickSettingsMenu: Listening for system state updates");
            while let Ok(update) = receiver.recv() {
                // Forward the update to our component's update method
                input_sender
                    .send(QuickSettingsMenuInput::SystemStateUpdate(update))
                    .ok();
            }
            debug!("QuickSettingsMenu: System state update receiver closed");
        });

        let widgets = view_output!();

        // Store weak reference to popover widget
        model.popover_widget = widgets.popover_widget.downgrade();

        // Connect to popover closed signal to reset revealers
        let popover_sender = sender.input_sender().clone();
        widgets.popover_widget.connect_closed(move |_| {
            debug!("Popover closed, resetting revealers");
            popover_sender
                .send(QuickSettingsMenuInput::ResetRevealers)
                .ok();
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, #[allow(unused_variables)] sender: ComponentSender<Self>) {
        match message {
            QuickSettingsMenuInput::SystemStateUpdate(update) => match update {
                SystemStateUpdate::Brightness(level) => {
                    // Update both the icon and the popover
                    self.brightness_icon
                        .emit(BrightnessIconInput::UpdateLevel(level));
                    self.popover
                        .emit(QuickSettingsPopoverInput::UpdateBrightness(level));
                },
                SystemStateUpdate::Audio(volume, muted) => {
                    // Update both the icon and the popover
                    self.audio_icon.emit(AudioIconInput::UpdateVolume(volume));
                    self.audio_icon.emit(AudioIconInput::UpdateMuted(muted));
                    self.popover
                        .emit(QuickSettingsPopoverInput::UpdateAudio(volume, muted));
                },
                SystemStateUpdate::Bluetooth(powered, connected_count) => {
                    self.bluetooth_icon
                        .emit(BluetoothIconInput::UpdateState(powered, connected_count));
                },
                SystemStateUpdate::Network(network_type, connected, wifi_strength) => {
                    self.network_icon.emit(NetworkIconInput::UpdateState(
                        network_type,
                        connected,
                        wifi_strength,
                    ));
                },
                SystemStateUpdate::Dnd(dont_disturb) => {
                    self.dnd_icon
                        .emit(DoNotDisturbIconInput::UpdateState(dont_disturb));
                    self.popover
                        .emit(QuickSettingsPopoverInput::UpdateDnd(dont_disturb));
                },
                SystemStateUpdate::DarkMode(enabled) => {
                    self.popover
                        .emit(QuickSettingsPopoverInput::UpdateDarkMode(enabled));
                },
                SystemStateUpdate::AirplaneMode(_enabled) => {
                    // Airplane mode state - forward to popover toggle
                    self.popover
                        .emit(QuickSettingsPopoverInput::UpdateAirplaneMode(_enabled));
                },
                SystemStateUpdate::PowerProfile(profile) => {
                    self.power_mode_icon
                        .emit(PowerModeIconInput::UpdateProfile(profile.clone()));
                    self.popover
                        .emit(QuickSettingsPopoverInput::UpdatePowerProfile(profile));
                },
                SystemStateUpdate::PowerModeServiceReady(service) => {
                    // Power mode service is ready - pass it to the popover
                    self.popover
                        .emit(QuickSettingsPopoverInput::PowerModeServiceReady(service));
                },
                SystemStateUpdate::AccentColor(color) => {
                    // Accent color changed - forward to popover
                    self.popover
                        .emit(QuickSettingsPopoverInput::UpdateAccentColor(color));
                },
                SystemStateUpdate::Privacy(_usage) => {
                    // Privacy indicator - handled by separate component
                },
                SystemStateUpdate::SystemdFailed(_units) => {
                    // Systemd failed indicator - handled by separate component
                },
                SystemStateUpdate::Battery(percentage, charging, present) => {
                    // Update both the icon and the popover
                    self.battery_icon.emit(BatteryIconInput::UpdateBattery(
                        percentage, charging, present,
                    ));
                    self.popover.emit(QuickSettingsPopoverInput::UpdateBattery(
                        percentage, charging, present,
                    ));
                },
                SystemStateUpdate::CalendarEvents(_events) => {
                    // Calendar events - handled by InfoCenter calendar view
                },
            },
            QuickSettingsMenuInput::PowerProfileChanged(profile) => {
                // User changed power profile - update icon immediately
                self.power_mode_icon
                    .emit(PowerModeIconInput::UpdateProfile(profile));
            },
            QuickSettingsMenuInput::ResetRevealers => {
                // Popover closed - reset all revealers
                self.popover.emit(QuickSettingsPopoverInput::ResetRevealers);
            },
            QuickSettingsMenuInput::ClosePopover => {
                // Close the popover
                if let Some(popover) = self.popover_widget.upgrade() {
                    popover.popdown();
                }
            },
        }
    }
}
