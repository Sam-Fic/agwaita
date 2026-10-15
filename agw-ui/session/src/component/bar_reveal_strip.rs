//! A 2px-tall invisible strip pinned to the top edge of the screen. While
//! the topbar is parked off-screen in auto-hide mode this strip is the only
//! surface left under the pointer, so its motion-enter event is what
//! reveals the bar.
//!
//! Exclusive zone -1 claims no space AND makes the strip ignore the bar's
//! own reservation, so it stays pinned to the very top edge whether the
//! bar is revealed or parked off-screen.

use crate::{
    APP_SENDER,
    component::TopbarManagerInput,
};
use gtk4::{
    gdk,
    prelude::*,
};
use gtk4_layer_shell::{
    Edge,
    Layer,
    LayerShell,
};
use relm4::{
    ComponentParts,
    ComponentSender,
    RelmWidgetExt,
    SimpleComponent,
    gtk,
};

pub(crate) struct BarRevealStrip;

#[relm4::component(pub(crate))]
impl SimpleComponent for BarRevealStrip {
    type Init = gdk::Monitor;
    type Input = ();
    type Output = ();

    view! {
        #[root]
        gtk::Window {
            set_namespace: Some("agwaita-bar-reveal"),
            set_layer: Layer::Top,
            set_anchor: (Edge::Top, true),
            set_anchor: (Edge::Left, true),
            set_anchor: (Edge::Right, true),
            set_margin_all: 0,
            set_visible: false,

            gtk::Box {
                set_height_request: 2,
            },
        }
    }

    fn init(monitor: Self::Init, root: Self::Root, _sender: ComponentSender<Self>) -> ComponentParts<Self> {
        root.init_layer_shell();
        root.set_monitor(Some(&monitor));
        root.set_exclusive_zone(-1);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter(move |_, _, _| {
            if let Some(sender) = APP_SENDER.get() {
                sender.send(TopbarManagerInput::Reveal).ok();
            }
        });
        root.add_controller(motion);

        let model = BarRevealStrip;
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }
}
