//! Privacy indicator component displaying active resource usage.

use crate::system_state::messages::SystemStateUpdate;
use agw_service::privacy::PrivacyUsage;
use catalyser::stdx::extension::str_extension::MultilineStr;
use gtk4::prelude::*;
use relm4::{
    ComponentParts,
    ComponentSender,
    RelmWidgetExt,
    SimpleComponent,
    gtk,
};

#[derive(Debug, Clone)]
pub enum PrivacyIndicatorInput {
    SystemStateUpdate(SystemStateUpdate),
}

pub struct PrivacyIndicator {
    usage: PrivacyUsage,
    /// Precomputed tooltip so `#[watch]` never rebuilds the markup — only
    /// genuine privacy changes (the only messages that reach `update`)
    /// recompute it.
    tooltip_markup: String,
}

#[relm4::component(pub)]
impl SimpleComponent for PrivacyIndicator {
    type Init = std::sync::mpsc::Receiver<SystemStateUpdate>;
    type Input = PrivacyIndicatorInput;
    type Output = ();

    view! {
        #[root]
        gtk::Box {
            add_css_class: "card",
            inline_css: "
            |color: var(--accent-color);
            |padding: 0 10px;
            ".trim_margin().as_str(),
            set_spacing: 8,

            #[watch]
            set_tooltip_markup: Some(&model.tooltip_markup),
            #[watch]
            set_visible: !model.usage_is_empty(),

            gtk::Image {
                set_icon_name: Some("camera-web-symbolic"),

                #[watch]
                set_visible: !model.usage.camera.is_empty(),
            },

            gtk::Image {
                set_icon_name: Some("audio-input-microphone-symbolic"),

                #[watch]
                set_visible: !model.usage.microphone.is_empty(),
            },

            gtk::Image {
                set_icon_name: Some("location-services-active-symbolic"),

                #[watch]
                set_visible: !model.usage.location.is_empty(),
            },

            gtk::Image {
                set_icon_name: Some("screen-shared-symbolic"),

                #[watch]
                set_visible: !model.usage.screencast.is_empty(),
            },
        }
    }

    fn init(system_state_receiver: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let model = PrivacyIndicator {
            usage: PrivacyUsage::default(),
            tooltip_markup: String::new(),
        };

        let input_sender = sender.input_sender().clone();
        std::thread::spawn(move || {
            while let Ok(update) = system_state_receiver.recv() {
                // Only privacy updates matter to this component: forwarding
                // every broadcast (battery, audio, network, ...) needlessly
                // re-ran every `#[watch]` and rebuilt the tooltip on the
                // GTK main thread several times per minute.
                if let SystemStateUpdate::Privacy(usage) = update {
                    input_sender
                        .send(PrivacyIndicatorInput::SystemStateUpdate(SystemStateUpdate::Privacy(usage)))
                        .ok();
                }
            }
        });

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>) {
        if let PrivacyIndicatorInput::SystemStateUpdate(SystemStateUpdate::Privacy(usage)) = message {
            self.tooltip_markup = Self::usage_to_tooltip_markup(&usage);
            self.usage = usage;
        }
    }
}

impl PrivacyIndicator {
    fn usage_is_empty(&self) -> bool {
        self.usage.camera.is_empty() && self.usage.microphone.is_empty() && self.usage.location.is_empty() && self.usage.screencast.is_empty()
    }

    fn usage_to_tooltip_markup(usage: &PrivacyUsage) -> String {
        let mut lines = Vec::new();

        if !usage.microphone.is_empty() {
            let mut apps: Vec<_> = usage.microphone.iter().cloned().collect();
            apps.sort_unstable();
            lines.push(format!("<b>Microphone:</b>\n{}", apps.join("\n")));
        }
        if !usage.camera.is_empty() {
            let mut apps: Vec<_> = usage.camera.iter().cloned().collect();
            apps.sort_unstable();
            lines.push(format!("<b>Camera:</b>\n{}", apps.join("\n")));
        }
        if !usage.location.is_empty() {
            let mut apps: Vec<_> = usage.location.iter().cloned().collect();
            apps.sort_unstable();
            lines.push(format!("<b>Location:</b>\n{}", apps.join("\n")));
        }
        if !usage.screencast.is_empty() {
            let mut apps: Vec<_> = usage.screencast.iter().cloned().collect();
            apps.sort_unstable();
            lines.push(format!("<b>Screen sharing:</b>\n{}", apps.join("\n")));
        }

        lines.join("\n\n")
    }
}
