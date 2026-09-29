use crate::model::PowerMenuAction;
use agw_lib_slide_bin::{
    SlideBin,
    SlideEdge,
};
use catalyser::stdx::extension::str_extension::MultilineStr;
use gtk4::{
    gdk,
    glib,
    prelude::*,
};
use gtk4_layer_shell::{
    Edge,
    KeyboardMode,
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
};
use std::{
    cell::RefCell,
    rc::Rc,
};

pub struct PowerMenuWindow {
    visible: bool,
    actions: Vec<PowerMenuAction>,
    window: gtk::Window,
    selection_model: gtk::SingleSelection,
    /// 滑动动画的载体：快照时按进度把卡片平移出屏幕边缘
    slide_bin: SlideBin,
    /// 当前滑动动画；方向切换时 pause 旧动画，从中途进度反向
    animation: RefCell<Option<adw::Animation>>,
    /// hide 动画完成后是否需要 unmap 层表面
    hiding: Rc<std::cell::Cell<bool>>,
    /// 打开时表面尚未 map（冷启动首次 configure 延迟）则挂起滑入动画
    slide_pending: std::cell::Cell<bool>,
}

#[derive(Debug, Clone)]
pub enum PowerMenuWindowInput {
    Toggle,
    Hide,
    WindowMapped,
    ExecuteAction(usize),
    NavigateDown,
    NavigateUp,
    ActivateSelected,
}

#[derive(Debug, Clone, Default)]
pub struct PowerMenuWindowConfig;

impl PowerMenuWindow {
    /// Slide durations; only the easing differs besides these (ease-out
    /// landing on show, ease-in leaving on hide).
    const SHOW_DURATION_MS: u32 = 320;
    const HIDE_DURATION_MS: u32 = 200;

    /// Animate the card between its resting position (progress 1) and fully
    /// outside the bottom screen edge (progress 0).
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

    /// Slide the card in from the bottom edge of the screen.
    fn show_animated(&mut self) {
        self.hiding.set(false);
        self.visible = true;
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

    /// Slide the card back down and unmap the layer surface once the
    /// animation finishes (connect_done on the animation).
    fn hide_animated(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        if self.slide_bin.property::<f64>("progress") <= 0.0 {
            // Nothing to animate: the window is mapped with the card fully
            // below the screen edge, so unmap right away.
            self.window.set_visible(false);
            return;
        }
        self.hiding.set(true);
        self.start_slide(0.0);
    }
}

#[relm4::component(pub)]
impl SimpleComponent for PowerMenuWindow {
    type Input = PowerMenuWindowInput;
    type Output = ();
    type Init = PowerMenuWindowConfig;

    view! {
        #[root]
        gtk::Window {
            set_namespace: Some("agwaita-power-menu"),
            set_layer: Layer::Overlay,
            set_anchor: (Edge::Top, true),
            set_anchor: (Edge::Right, true),
            set_anchor: (Edge::Left, true),
            set_anchor: (Edge::Bottom, true),
            set_keyboard_mode: KeyboardMode::Exclusive,
            /* The dim lives in the SlideBin's snapshot (semi-transparent
               black fading with the slide progress); the window itself must
               paint nothing or the two would stack. */
            inline_css: "background: transparent;",
            set_visible: false,

            add_controller = gtk::EventControllerKey {
                connect_key_pressed[sender] => move |_, keyval, _, _| {
                    match keyval {
                        gdk::Key::Escape => {
                            sender.input(PowerMenuWindowInput::Hide);
                            glib::Propagation::Stop
                        }
                        gdk::Key::Down => {
                            sender.input(PowerMenuWindowInput::NavigateDown);
                            glib::Propagation::Stop
                        }
                        gdk::Key::Up => {
                            sender.input(PowerMenuWindowInput::NavigateUp);
                            glib::Propagation::Stop
                        }
                        gdk::Key::Return | gdk::Key::KP_Enter => {
                            sender.input(PowerMenuWindowInput::ActivateSelected);
                            glib::Propagation::Stop
                        }
                        _ => glib::Propagation::Proceed
                    }
                }
            },

            add_controller = gtk::GestureClick {
                connect_released[sender] => move |gesture, _, x, y| {
                    if let Some(widget) = gesture.widget() {
                        if let Some(window) = widget.downcast_ref::<gtk::Window>() {
                            // Check if click is on the window's transparent background:
                            // the card lives inside the fullscreen slide bin, so a
                            // click outside it picks either the bin or the window.
                            if let Some(picked) = window.pick(x, y, gtk::PickFlags::DEFAULT) {
                                if picked.is::<SlideBin>() || picked.is::<gtk::Window>() {
                                    sender.input(PowerMenuWindowInput::Hide);
                                }
                            }
                        }
                    }
                }
            },

            #[local_ref]
            slide_bin_widget -> SlideBin {
                set_hexpand: true,
                set_vexpand: true,

                // Each action is its own rounded tile (styled via the
                // .power-list rules in init()); there is no outer card.
                #[local_ref]
                action_list -> gtk::ListView {
                    add_css_class: "power-list",
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,
                    set_can_focus: true,
                }
            }
        }
    }

    fn init(_config: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        root.init_layer_shell();

        let slide_bin_widget = SlideBin::new(SlideEdge::Bottom);

        // Tile styling for the action list: each row is its own rounded
        // button floating over the dim, with hover and selected states kept
        // rounded as well. Scoped under .power-list so no other list in the
        // process is affected.
        let tile_css = gtk::CssProvider::new();
        tile_css.load_from_string(
            "listview.power-list { background: none; }\n\
             listview.power-list > row {\n\
             \x20 background-color: @window_bg_color;\n\
             \x20 border-radius: 8px;\n\
             \x20 margin-bottom: 8px;\n\
             }\n\
             listview.power-list > row:hover {\n\
             \x20 background-color: color-mix(in srgb, @window_fg_color 8%, @window_bg_color);\n\
             }\n\
             listview.power-list > row:selected {\n\
             \x20 background-color: @accent_bg_color;\n\
             \x20 color: @accent_fg_color;\n\
             }",
        );
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &tile_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }

        let actions = PowerMenuAction::ALL_ACTIONS.to_vec();

        let list_store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
        for action in actions.iter() {
            list_store.append(&glib::BoxedAnyObject::new(action.clone()));
        }

        let selection_model = gtk::SingleSelection::new(Some(list_store.clone()));
        selection_model.set_autoselect(false);
        selection_model.set_can_unselect(true);

        let factory = gtk::SignalListItemFactory::new();

        let sender_for_setup = sender.clone();
        let selection_model_for_setup = selection_model.clone();
        factory.connect_setup(move |_, list_item| {
            let list_item_ref = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = adw::ActionRow::new();

            // Add click handler
            let gesture = gtk::GestureClick::new();
            gesture.set_button(gdk::BUTTON_PRIMARY);
            let sender_for_click = sender_for_setup.clone();
            let selection_model_clone = selection_model_for_setup.clone();
            let list_item_for_click = list_item_ref.clone();
            gesture.connect_released(move |_, _, _, _| {
                let position = list_item_for_click.position();
                selection_model_clone.set_selected(position);
                sender_for_click.input(PowerMenuWindowInput::ExecuteAction(position as usize));
            });
            row.add_controller(gesture);

            list_item_ref.set_child(Some(&row));
        });

        factory.connect_bind(move |_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = list_item
                .child()
                .unwrap()
                .downcast::<adw::ActionRow>()
                .unwrap();

            if let Some(obj) = list_item.item() {
                let boxed = obj.downcast_ref::<glib::BoxedAnyObject>().unwrap();
                let action: PowerMenuAction = boxed.borrow::<PowerMenuAction>().clone();

                row.set_title(action.title);
                row.add_prefix(&gtk::Image::from_icon_name(action.icon_name));
            }
        });

        let action_list = gtk::ListView::new(Some(selection_model.clone()), Some(factory));

        let widgets = view_output!();

        // Surface mapping can lag the show request (layer-shell configure on
        // cold start); the deferred slide resumes here.
        widgets.slide_bin_widget.connect_map({
            let sender = sender.input_sender().clone();
            move |_| {
                sender.send(PowerMenuWindowInput::WindowMapped).ok();
            }
        });

        let model = Self {
            visible: false,
            actions,
            window: root,
            selection_model,
            slide_bin: slide_bin_widget,
            animation: RefCell::new(None),
            hiding: Rc::new(std::cell::Cell::new(false)),
            slide_pending: std::cell::Cell::new(false),
        };

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            PowerMenuWindowInput::Toggle => {
                if self.visible {
                    self.hide_animated();
                } else {
                    self.show_animated();
                    // Select first item when opening
                    self.selection_model.set_selected(0);
                }

                debug!("Power menu toggled: visible={}", self.visible);
            },
            PowerMenuWindowInput::Hide => {
                self.hide_animated();
                debug!("Power menu hidden");
            },
            PowerMenuWindowInput::WindowMapped => {
                if self.visible && self.slide_pending.replace(false) {
                    self.start_slide(1.0);
                }
            },
            PowerMenuWindowInput::ExecuteAction(idx) => {
                if let Some(action) = self.actions.get(idx) {
                    debug!("Executing power menu action: {}", action.title);
                    sender.input(PowerMenuWindowInput::Hide);

                    if let Err(e) = action.call() {
                        log::error!("Power menu action '{}' failed: {}", action.title, e);
                    }
                }
            },
            PowerMenuWindowInput::NavigateDown => {
                let current = self.selection_model.selected();
                let max = self.actions.len().saturating_sub(1) as u32;
                if current < max {
                    self.selection_model.set_selected(current + 1);
                }
            },
            PowerMenuWindowInput::NavigateUp => {
                let current = self.selection_model.selected();
                if current > 0 {
                    self.selection_model.set_selected(current - 1);
                }
            },
            PowerMenuWindowInput::ActivateSelected => {
                let idx = self.selection_model.selected() as usize;
                sender.input(PowerMenuWindowInput::ExecuteAction(idx));
            },
        }
    }
}
