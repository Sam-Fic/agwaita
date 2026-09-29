use crate::{
    components::NotificationWithContext,
    model::{
        NotificationEvent,
        NotificationVisibility,
    },
    service::NotificationStore,
};
use agw_lib_slide_bin::{
    SlideBin,
    SlideEdge,
};
use catalyser::stdx::extension::str_extension::MultilineStr;
use gtk4::{
    glib,
    prelude::{
        BoxExt,
        OrientableExt,
        WidgetExt,
    },
};
use gtk4::glib::prelude::*;
use gtk4_layer_shell::{
    Edge,
    Layer,
    LayerShell,
};
use log::debug;
use relm4::{
    ComponentParts,
    ComponentSender,
    RelmWidgetExt,
    SimpleComponent,
    adw::{
        self,
        prelude::*,
    },
    gtk,
    typed_view::list::TypedListView,
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::Arc,
};

pub struct NotificationPopup {
    store: Arc<NotificationStore>,
    list_view: TypedListView<NotificationWithContext, gtk::NoSelection>,
    visible: bool,
    dnd_enabled: bool,
    window: gtk::Window,
    /// 滑动动画的载体：快照时按进度把通知列表平移出屏幕顶边
    slide_bin: SlideBin,
    /// 当前滑动动画；方向切换时 pause 旧动画，从中途进度反向
    animation: RefCell<Option<adw::Animation>>,
    /// hide 动画完成后是否需要 unmap 层表面
    hiding: Rc<std::cell::Cell<bool>>,
    /// 打开时表面尚未 map（冷启动首次 configure 延迟）则挂起滑入动画
    slide_pending: std::cell::Cell<bool>,
}

#[derive(Debug, Clone)]
pub enum NotificationPopupInput {
    NotificationEvent(Box<NotificationEvent>),
    DndChanged(bool),
    WindowMapped,
}

#[derive(Debug, Clone)]
pub struct NotificationPopupConfig {
    pub store: Arc<NotificationStore>,
    pub dnd_enabled: bool,
}

#[relm4::component(pub)]
impl SimpleComponent for NotificationPopup {
    type Input = NotificationPopupInput;
    type Output = ();
    type Init = NotificationPopupConfig;

    view! {
        #[root]
        gtk::Window {
            set_namespace: Some("agwaita-notifications"),
            set_layer: Layer::Overlay,
            set_anchor: (Edge::Top, true),
            set_anchor: (Edge::Right, true),
            set_anchor: (Edge::Bottom, true),
            inline_css: "
            |background: linear-gradient(to right, transparent, var(--shade-color));
            ".trim_margin().as_str(),
            set_margin_vertical: 0,
            set_margin_end: 0,
            set_visible: false,

            // The cards slide down from the top edge (the popup is anchored
            // top-right); the window's gradient shade stays static, so the
            // bin paints no backdrop of its own.
            #[local_ref]
            slide_bin_widget -> SlideBin {
                set_hexpand: true,
                set_vexpand: true,

                adw::Clamp {
                    set_maximum_size: 320,

                    gtk::ScrolledWindow {
                        set_hscrollbar_policy: gtk::PolicyType::Never,
                        set_propagate_natural_width: true,
                        set_propagate_natural_height: true,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 8,
                            set_margin_top: 12,
                            set_margin_bottom: 12,
                            set_margin_start: 12,
                            set_margin_end: 12,

                            #[local_ref]
                            notification_list_view -> gtk::ListView {
                                inline_css: "
                                |background: transparent;
                                ".trim_margin().as_str(),
                                set_hexpand: true,
                                set_can_focus: false,
                                set_focusable: false,
                            },
                        }
                    }
                }
            }
        }
    }

    fn init(config: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        // Setup layer shell
        root.init_layer_shell();

        // Initialize global store for notification items to access
        crate::components::init_notification_store(config.store.clone());

        // Create list view (start empty, only show new notifications)
        let list_view: TypedListView<NotificationWithContext, gtk::NoSelection> = TypedListView::new();

        let notification_list_view = &list_view.view;
        let slide_bin_widget = SlideBin::new(SlideEdge::Top);
        // The window paints its own gradient shade, so the bin adds no dim.
        slide_bin_widget.set_property("backdrop-opacity", 0.0);
        let widgets = view_output!();

        // Surface mapping can lag the show request (layer-shell configure on
        // cold start); the deferred slide resumes here.
        widgets.slide_bin_widget.connect_map({
            let sender = sender.input_sender().clone();
            move |_| {
                sender.send(NotificationPopupInput::WindowMapped).ok();
            }
        });

        let model = NotificationPopup {
            store: config.store.clone(),
            list_view,
            visible: false,
            dnd_enabled: config.dnd_enabled,
            window: root.clone(),
            slide_bin: slide_bin_widget,
            animation: RefCell::new(None),
            hiding: Rc::new(std::cell::Cell::new(false)),
            slide_pending: std::cell::Cell::new(false),
        };

        // Subscribe to notification events after component is built
        let store_clone = config.store.clone();
        let sender_clone = sender.input_sender().clone();
        std::thread::spawn(move || {
            debug!("Popup: Event listener started");
            let receiver = store_clone.subscribe();
            loop {
                match receiver.recv() {
                    Ok(event) => {
                        debug!("Popup: Received event from store: {:?}", event);
                        if sender_clone
                            .send(NotificationPopupInput::NotificationEvent(Box::new(event)))
                            .is_err()
                        {
                            debug!("Popup: Component disconnected, stopping event listener");
                            break;
                        }
                    },
                    Err(_) => {
                        debug!("Popup: Store disconnected, stopping event listener");
                        break;
                    },
                }
            }
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>) {
        match message {
            NotificationPopupInput::NotificationEvent(event) => match event.as_ref() {
                NotificationEvent::Added(notification) => {
                    // Only show visible notifications and only if DND is disabled
                    if notification.visibility == NotificationVisibility::Visible && !self.dnd_enabled {
                        debug!(
                            "Popup: Adding notification id={}, summary={} (DND={})",
                            notification.id, notification.summary, self.dnd_enabled
                        );

                        // Add at the beginning (most recent first)
                        self.list_view
                            .insert(0, NotificationWithContext::new(notification.clone(), true));

                        // Schedule timeout to hide the notification
                        if let Some(duration) = notification.get_timeout_duration() {
                            let store = self.store.clone();
                            let id = notification.id;
                            glib::timeout_add_local_once(duration, move || {
                                debug!("Popup: Hiding notification id={} after timeout", id);
                                store.hide(id);
                            });
                        } else {
                            // Default 5 second timeout for popup
                            let store = self.store.clone();
                            let id = notification.id;
                            glib::timeout_add_local_once(std::time::Duration::from_secs(5), move || {
                                debug!(
                                    "Popup: Hiding notification id={} after default 5s timeout",
                                    id
                                );
                                store.hide(id);
                            });
                        }
                    }
                },
                NotificationEvent::Updated(notification) => {
                    if let Some(pos) = self
                        .list_view
                        .iter()
                        .position(|n| n.borrow().notification.id == notification.id)
                    {
                        // If notification became hidden or closed, remove from popup
                        if notification.visibility != NotificationVisibility::Visible {
                            debug!(
                                "Popup: Removing notification id={} (visibility changed to {:?})",
                                notification.id, notification.visibility
                            );
                            self.list_view.remove(pos as u32);
                        } else {
                            // Update in place
                            self.list_view.remove(pos as u32);
                            self.list_view.insert(
                                pos as u32,
                                NotificationWithContext::new(notification.clone(), true),
                            );
                        }
                    }
                },
                NotificationEvent::Closed(id) => {
                    if let Some(pos) = self
                        .list_view
                        .iter()
                        .position(|n| n.borrow().notification.id == *id)
                    {
                        debug!("Popup: Removing closed notification id={}", id);
                        self.list_view.remove(pos as u32);
                    }
                },
                NotificationEvent::ActionInvoked(id, action_id) => {
                    debug!(
                        "Popup: Action invoked on notification id={}, action={}",
                        id, action_id
                    );
                    // The notification will be closed by the store, which will trigger Closed event
                },
            },
            NotificationPopupInput::DndChanged(enabled) => {
                debug!(
                    "Popup: DND changed to {} (was {})",
                    enabled, self.dnd_enabled
                );
                self.dnd_enabled = enabled;

                // If DND is being enabled, clear all notifications from popup
                if enabled {
                    debug!(
                        "Popup: Clearing {} notifications due to DND activation",
                        self.list_view.len()
                    );
                    self.list_view.clear();
                }

                self.update_visibility();
            },
            NotificationPopupInput::WindowMapped => {
                if self.visible && self.slide_pending.replace(false) {
                    self.start_slide(1.0);
                }
            },
        }

        // Update window visibility based on notification count
        self.update_visibility();
    }
}

impl NotificationPopup {
    /// Slide durations; only the easing differs besides these (ease-out
    /// landing on show, ease-in leaving on hide).
    const SHOW_DURATION_MS: u32 = 280;
    const HIDE_DURATION_MS: u32 = 180;

    /// Animate the card stack between its resting position (progress 1) and
    /// fully outside the top screen edge (progress 0).
    fn start_slide(&self, to: f64) {
        if let Some(old) = self.animation.borrow_mut().take() {
            // Stop without emitting done, so a mid-flight turn-around never
            // triggers the hide-completed unmap.
            old.pause();
        }
        let from = self.slide_bin.property::<f64>("progress");
        if (to - from).abs() < f64::EPSILON {
            return;
        }
        let showing = to > from;
        let duration = if showing {
            Self::SHOW_DURATION_MS
        } else {
            Self::HIDE_DURATION_MS
        };
        let target = adw::CallbackAnimationTarget::new({
            let slide_bin = self.slide_bin.clone();
            move |value| {
                slide_bin.set_property("progress", value);
            }
        });
        let timed = adw::TimedAnimation::new(
            &self.slide_bin,
            from,
            to,
            duration,
            target,
        );
        timed.set_easing(if showing {
            adw::Easing::EaseOutCubic
        } else {
            adw::Easing::EaseInCubic
        });
        let anim: adw::Animation = timed.upcast();
        anim.connect_done({
            let window = self.window.clone();
            let hiding = Rc::clone(&self.hiding);
            move |_| {
                if hiding.get() {
                    window.set_visible(false);
                }
            }
        });
        anim.play();
        self.animation.replace(Some(anim));
    }

    /// Slide the card stack in from the top edge of the screen.
    fn show_animated(&mut self) {
        self.hiding.set(false);
        self.window.set_visible(true);
        if self.slide_bin.is_mapped() {
            self.start_slide(1.0);
        } else {
            // AdwAnimation skips straight to its end value when played on an
            // unmapped widget, and the first cold-start map waits for the
            // compositor's layer configure — defer to the map signal.
            self.slide_pending.set(true);
        }
    }

    /// Slide the card stack back up and unmap the layer surface once the
    /// animation finishes (connect_done on the animation).
    fn hide_animated(&mut self) {
        if self.slide_bin.property::<f64>("progress") <= 0.0 {
            // Nothing to animate: unmap right away (also covers a pending
            // show that never mapped).
            self.window.set_visible(false);
            return;
        }
        self.hiding.set(true);
        self.start_slide(0.0);
    }

    fn update_visibility(&mut self) {
        let should_be_visible = !self.list_view.is_empty() && !self.dnd_enabled;

        if self.visible != should_be_visible {
            debug!(
                "Popup: Changing visibility from {} to {} (notifications={}, dnd={})",
                self.visible,
                should_be_visible,
                self.list_view.len(),
                self.dnd_enabled
            );
            self.visible = should_be_visible;
            if should_be_visible {
                self.show_animated();
            } else {
                self.hide_animated();
            }
        }
    }
}
