use crate::{
    desktop_entries_service::DesktopEntriesService,
    favorites::FavoritesService,
    model::DesktopEntry,
    search::AppSearcher,
};
use agw_lib_slide_bin::{
    SlideBin,
    SlideEdge,
};
use agw_service::wm::WMService;
use catalyser::stdx::extension::{
    scope_functions_extension::{
        Apply,
        TakeIf,
    },
    str_extension::MultilineStr,
};
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
use log::{
    debug,
    info,
};
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
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        Mutex,
    },
};

pub struct AppLauncherWindow {
    visible: bool,
    window: gtk::Window,
    desktop_entries_service: Arc<Mutex<Option<DesktopEntriesService>>>,
    searcher: AppSearcher,
    filtered_apps: Vec<DesktopEntry>,
    list_view: gtk::ListView,
    search_entry: gtk::SearchEntry,
    selection_model: gtk::SingleSelection,
    scrolled_window: gtk::ScrolledWindow,
    /// 空搜索结果时的占位浮层
    empty_label: gtk::Box,
    /// 滑动动画的载体：快照时按进度把卡片从屏幕底边外平移到位
    slide_bin: SlideBin,
    /// 当前滑动动画；方向切换时 pause 旧动画，从中途进度反向
    animation: RefCell<Option<adw::Animation>>,
    /// hide 动画完成后是否需要 unmap 层表面
    hiding: Rc<std::cell::Cell<bool>>,
    /// 打开时表面尚未 map（冷启动首次 configure 延迟）则挂起滑入动画
    slide_pending: std::cell::Cell<bool>,
    favorites_service: Option<Arc<Mutex<Option<FavoritesService>>>>,
    wm_service: Arc<WMService>,
}

#[derive(Debug, Clone)]
pub enum AppLauncherWindowInput {
    Toggle,
    Hide,
    WindowMapped,
    SearchChanged(String),
    NavigateDown,
    NavigateUp,
    LaunchSelected,
    ToggleFavorite(String), // app_id
    DesktopEntriesChanged,
}

#[derive(Clone)]
pub struct AppLauncherWindowConfig {
    pub desktop_entries_service: Arc<Mutex<Option<DesktopEntriesService>>>,
    pub favorites_service: Option<Arc<Mutex<Option<FavoritesService>>>>,
    pub wm_service: Arc<WMService>,
}

impl AppLauncherWindow {
    /// Slide durations; only the easing differs besides these (ease-out
    /// landing on show, ease-in leaving on hide). The show is longer so the
    /// card settles gently, while hide stays snappy.
    const SHOW_DURATION_MS: u32 = 320;
    const HIDE_DURATION_MS: u32 = 200;

    /// Animate the card between its resting position (progress 1) and fully
    /// below the bottom screen edge (progress 0). AdwAnimation drives the
    /// SlideBin transform; GtkRevealer was rejected because it snaps on
    /// freshly remapped layer surfaces (frame-rate readiness gate), while
    /// AdwAnimation only honors the gtk-enable-animations setting.
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
        debug!(
            "launcher slide: {:.2} -> {:.2} ({}ms)",
            from,
            to,
            duration
        );
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
                debug!("launcher slide done");
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
impl SimpleComponent for AppLauncherWindow {
    type Input = AppLauncherWindowInput;
    type Output = ();
    type Init = AppLauncherWindowConfig;

    view! {
        #[root]
        gtk::Window {
            set_namespace: Some("agwaita-app-launcher"),
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
                    if keyval == gdk::Key::Escape {
                        sender.input(AppLauncherWindowInput::Hide);
                        glib::Propagation::Stop
                    } else {
                        glib::Propagation::Proceed
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
                                    sender.input(AppLauncherWindowInput::Hide);
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

                adw::Clamp {
                    set_maximum_size: 640,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,

                    gtk::Box {
                        add_css_class: "card",
                    inline_css: "
                    |background-color: @window_bg_color;
                    |border-radius: 20px;
                    |padding: 12px;
                    ".trim_margin().as_str(),
                    set_orientation: gtk::Orientation::Vertical,
                    set_width_request: 640,
                    set_height_request: 720,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_hexpand: true,
                        set_vexpand: true,

                        // Search input
                        #[name = "search_entry"]
                        gtk::SearchEntry {
                            add_css_class: "card",
                            inline_css: "
                            |border: none;
                            |border-radius: 8px;
                            ".trim_margin().as_str(),
                            set_placeholder_text: Some("Search applications..."),
                            set_hexpand: true,

                            connect_changed[sender] => move |entry| {
                                sender.input(AppLauncherWindowInput::SearchChanged(entry.text().to_string()));
                            },

                            connect_activate[sender] => move |_| {
                                sender.input(AppLauncherWindowInput::LaunchSelected);
                            },

                            add_controller = gtk::EventControllerKey {
                                connect_key_pressed[sender] => move |_, keyval, _, _| {
                                    match keyval {
                                        gdk::Key::Down => {
                                            sender.input(AppLauncherWindowInput::NavigateDown);
                                            glib::Propagation::Stop
                                        }
                                        gdk::Key::Up => {
                                            sender.input(AppLauncherWindowInput::NavigateUp);
                                            glib::Propagation::Stop
                                        }
                                        _ => glib::Propagation::Proceed
                                    }
                                }
                            }
                        },

                        // Results list
                        #[name = "scrolled_window"]
                        gtk::ScrolledWindow {
                            set_hexpand: true,
                            set_vexpand: true,
                            set_can_focus: true,
                            set_vscrollbar_policy: gtk::PolicyType::Automatic,
                            /* The rounded clip must live on the scrolled
                               window: its bounds are the fixed viewport, so
                               rows get cut by rounded corners at the visible
                               top/bottom edges. On the ListView the clip
                               would travel with the scrolled content. Note
                               "overflow" is a widget property, not a GTK CSS
                               property — the CSS declaration would be ignored. */
                            set_overflow: gtk::Overflow::Hidden,
                            inline_css: "border-radius: 8px;",

                            #[local_ref]
                            app_list_view -> gtk::ListView {
                                add_css_class: "card",
                                set_can_focus: true,
                            }
                        }
                    }
                }
                }
            }
        }
    }

    fn init(config: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        root.init_layer_shell();

        let slide_bin_widget = SlideBin::new(SlideEdge::Bottom);

        // Favorite stars are toggle buttons, and even flat toggles paint a
        // checked backdrop disc. Favoriting is conveyed by the filled star
        // icon alone, so strip that disc. Scoped via a dedicated class so no
        // other flat circular toggle in the process is affected.
        let fav_css = gtk::CssProvider::new();
        fav_css.load_from_string(
            "button.fav-toggle:checked { background: none; }\
             button.fav-toggle:checked:hover { background-image: linear-gradient(color-mix(in srgb, white 25%, transparent), color-mix(in srgb, white 25%, transparent)); }",
        );
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &fav_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }

        let desktop_entries_service = config.desktop_entries_service.clone();

        // Get entries from service
        let entries = if let Some(ref service) = *desktop_entries_service.lock().unwrap() {
            service.get_entries()
        } else {
            Vec::new()
        };

        // Subscribe to desktop entries updates in a background thread
        let service_clone = desktop_entries_service.clone();
        let sender_clone = sender.clone();
        std::thread::spawn(move || {
            let receiver = if let Some(ref service) = *service_clone.lock().unwrap() {
                service.subscribe()
            } else {
                return;
            };

            // Block waiting for updates from the service
            while receiver.recv().is_ok() {
                info!("Desktop entries changed, refreshing launcher");
                let sender_for_idle = sender_clone.clone();
                glib::idle_add_once(move || {
                    sender_for_idle.input(AppLauncherWindowInput::DesktopEntriesChanged);
                });
            }
        });

        // Create list model
        let list_store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
        for entry in entries.iter() {
            list_store.append(&glib::BoxedAnyObject::new(entry.clone()));
        }

        // Create selection model
        let selection_model = gtk::SingleSelection::new(Some(list_store.clone()));

        // Create factory for list items
        let factory = gtk::SignalListItemFactory::new();

        // Pre-load all icons into cache to avoid memory leaks from repeated loading
        let icon_cache: Rc<RefCell<HashMap<String, gdk::Paintable>>> = Rc::new(RefCell::new(HashMap::new()));
        let icon_theme = gtk::IconTheme::for_display(&gdk::Display::default().unwrap());

        for entry in entries.iter() {
            let icon_name = entry
                .icon
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("application-x-executable");
            if !icon_cache.borrow().contains_key(icon_name) {
                let paintable = if icon_name.starts_with('/') {
                    // Handle absolute paths
                    if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(icon_name, 32, 32) {
                        Some(
                            #[allow(deprecated)]
                            gdk::Texture::for_pixbuf(&pixbuf).upcast::<gdk::Paintable>(),
                        )
                    } else {
                        // Fallback icon if absolute path fails
                        let fallback = icon_theme.lookup_icon(
                            "application-x-executable",
                            &[],
                            32,
                            1,
                            gtk::TextDirection::Ltr,
                            gtk::IconLookupFlags::empty(),
                        );
                        Some(fallback.upcast::<gdk::Paintable>())
                    }
                } else if icon_theme.has_icon(icon_name) {
                    // Handle icon names from theme
                    let icon_paintable = icon_theme.lookup_icon(
                        icon_name,
                        &[],
                        32,
                        1,
                        gtk::TextDirection::Ltr,
                        gtk::IconLookupFlags::empty(),
                    );
                    Some(icon_paintable.upcast::<gdk::Paintable>())
                } else {
                    // Fallback icon if not found
                    let fallback = icon_theme.lookup_icon(
                        "application-x-executable",
                        &[],
                        32,
                        1,
                        gtk::TextDirection::Ltr,
                        gtk::IconLookupFlags::empty(),
                    );
                    Some(fallback.upcast::<gdk::Paintable>())
                };

                if let Some(p) = paintable {
                    icon_cache.borrow_mut().insert(icon_name.to_string(), p);
                }
            }
        }

        let sender_for_setup = sender.clone();
        let selection_model_for_setup = selection_model.clone();
        // 每行的星星按钮状态:悬停或被选中时显示
        let row_states: std::rc::Rc<
            std::cell::RefCell<std::collections::HashMap<usize, std::rc::Rc<LauncherRowState>>>,
        > = std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
        {
            let row_states = row_states.clone();
            selection_model.connect_selection_changed(move |model, _, _| {
                let selected = model.selected();
                for state in row_states.borrow().values() {
                    state.sync_visibility(selected);
                }
            });
        }
        let row_states_for_setup = row_states.clone();
        factory.connect_setup(move |_, list_item| {
            let list_item_ref = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = adw::ActionRow::new();

            // Create icon widget once and store reference
            let icon = gtk::Image::new();
            icon.set_pixel_size(32);
            row.add_prefix(&icon);

            // Create favorite toggle button
            let favorite_button = gtk::ToggleButton::new();
            favorite_button.set_icon_name("non-starred-symbolic");
            // Flat circular action button: no chrome at rest, fully round
            // hover/active backdrop, sized down so it floats inside the row.
            // Hidden until the row is hovered or selected (see row_states).
            favorite_button.add_css_class("flat");
            favorite_button.add_css_class("circular");
            // Neutralize the checked-state backdrop so hover shows the same
            // shade circle as unfavorited rows (rule in the global provider).
            favorite_button.add_css_class("star-reveal");
            // Checked backdrop disc removed via the .fav-toggle rule in init().
            favorite_button.add_css_class("fav-toggle");
            favorite_button.set_opacity(0.0);

            // Star reveal: show when hovered, when the row is selected, or both.
            let row_state = std::rc::Rc::new(LauncherRowState {
                item: list_item_ref.clone(),
                hovered: std::cell::Cell::new(false),
                favorited: std::cell::Cell::new(false),
                button: favorite_button.clone(),
            });
            row_states_for_setup
                .borrow_mut()
                .insert(list_item_ref.as_ptr() as usize, row_state.clone());
            let motion = gtk::EventControllerMotion::new();
            {
                let state = row_state.clone();
                let model = selection_model_for_setup.clone();
                motion.connect_enter(move |_, _, _| {
                    state.hovered.set(true);
                    state.sync_visibility(model.selected());
                });
                let state = row_state.clone();
                let model = selection_model_for_setup.clone();
                motion.connect_leave(move |_| {
                    state.hovered.set(false);
                    state.sync_visibility(model.selected());
                });
            }
            row.add_controller(motion);

            let aspect_frame = gtk::AspectFrame::builder()
                .ratio(1.0)
                .xalign(0.5)
                .yalign(0.5)
                .margin_top(6)
                .margin_bottom(6)
                .margin_end(6)
                .child(&favorite_button)
                .build();

            // Connect toggle signal and store handler ID
            let sender_clone = sender_for_setup.clone();
            let signal_handler_id = favorite_button.connect_toggled(move |button| {
                // Get app_id from button data
                unsafe {
                    if let Some(app_id) = button.data::<String>("app-id") {
                        sender_clone.input(AppLauncherWindowInput::ToggleFavorite(
                            app_id.as_ref().clone(),
                        ));
                    }
                }
            });

            row.add_suffix(&aspect_frame);

            // Add click handler to launch app
            let gesture = gtk::GestureClick::new();
            gesture.set_button(gdk::BUTTON_PRIMARY);
            let sender_for_click = sender_for_setup.clone();
            let selection_model_clone = selection_model_for_setup.clone();
            let list_item_for_click = list_item_ref.clone();
            gesture.connect_released(move |_, _, _, _| {
                let position = list_item_for_click.position();
                selection_model_clone.set_selected(position);
                sender_for_click.input(AppLauncherWindowInput::LaunchSelected);
            });
            row.add_controller(gesture);

            // Store widgets and signal handler in ListItem for easy access in bind
            unsafe {
                list_item_ref.set_data("app-icon", icon);
                list_item_ref.set_data("favorite-button", favorite_button);
                list_item_ref.set_data("favorite-signal-id", signal_handler_id);
            }

            list_item_ref.set_child(Some(&row));
        });

        let selection_model_clone = selection_model.clone();
        let icon_cache_clone = icon_cache.clone();
        let icon_theme_clone = icon_theme.clone();
        let row_states_for_bind = row_states.clone();
        factory.connect_bind(move |_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = list_item
                .child()
                .unwrap()
                .downcast::<adw::ActionRow>()
                .unwrap();

            if let Some(obj) = list_item.item() {
                let boxed = obj.downcast_ref::<glib::BoxedAnyObject>().unwrap();
                let entry: DesktopEntry = boxed.borrow::<DesktopEntry>().clone();

                row.set_subtitle_lines(1);
                row.set_title(glib::markup_escape_text(&entry.name).as_str());
                row.set_subtitle(
                    glib::markup_escape_text(
                        &entry
                            .comment
                            .unwrap_or_default()
                            .take_if(|it| !it.trim().is_empty())
                            .unwrap_or(Self::factorise_exec_str(&entry.exec)),
                    )
                    .as_str(),
                );

                // Get icon from ListItem data and update from cache
                unsafe {
                    if let Some(icon) = list_item.data::<gtk::Image>("app-icon") {
                        let icon_name = entry
                            .icon
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("application-x-executable");
                        // Check cache first, if not found, load and cache on the fly
                        let paintable = if let Some(p) = icon_cache_clone.borrow().get(icon_name) {
                            Some(p.clone())
                        } else {
                            let p = if icon_name.starts_with('/') {
                                // Handle absolute paths
                                if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(icon_name, 32, 32) {
                                    Some(
                                        #[allow(deprecated)]
                                        gdk::Texture::for_pixbuf(&pixbuf).upcast::<gdk::Paintable>(),
                                    )
                                } else {
                                    // Fallback icon if absolute path fails
                                    let fallback = icon_theme_clone.lookup_icon(
                                        "application-x-executable",
                                        &[],
                                        32,
                                        1,
                                        gtk::TextDirection::Ltr,
                                        gtk::IconLookupFlags::empty(),
                                    );
                                    Some(fallback.upcast::<gdk::Paintable>())
                                }
                            } else if icon_theme_clone.has_icon(icon_name) {
                                // Handle icon names from theme
                                let icon_paintable = icon_theme_clone.lookup_icon(
                                    icon_name,
                                    &[],
                                    32,
                                    1,
                                    gtk::TextDirection::Ltr,
                                    gtk::IconLookupFlags::empty(),
                                );
                                Some(icon_paintable.upcast::<gdk::Paintable>())
                            } else {
                                // Fallback icon if not found
                                let fallback = icon_theme_clone.lookup_icon(
                                    "application-x-executable",
                                    &[],
                                    32,
                                    1,
                                    gtk::TextDirection::Ltr,
                                    gtk::IconLookupFlags::empty(),
                                );
                                Some(fallback.upcast::<gdk::Paintable>())
                            };

                            if let Some(ref p_val) = p {
                                icon_cache_clone
                                    .borrow_mut()
                                    .insert(icon_name.to_string(), p_val.clone());
                            }
                            p
                        };

                        if let Some(ref p) = paintable {
                            // Only set paintable if it's different to avoid memory leaks
                            let current_paintable = icon.as_ref().paintable();
                            let needs_update = current_paintable
                                .as_ref()
                                .map(|curr_p| !curr_p.eq(p))
                                .unwrap_or(true);

                            if needs_update {
                                icon.as_ref().set_paintable(Some(p));
                            }
                            icon.as_ref().set_visible(true);
                        } else {
                            icon.as_ref().set_visible(false);
                        }
                    }

                    // Update favorite button
                    if let Some(favorite_button) = list_item.data::<gtk::ToggleButton>("favorite-button") {
                        let button = favorite_button.as_ref();

                        // Get signal handler ID to block/unblock
                        if let Some(signal_id) = list_item.data::<glib::SignalHandlerId>("favorite-signal-id") {
                            // Block signal handler to avoid triggering on programmatic change
                            button.block_signal(signal_id.as_ref());

                            // Update toggle state and icon
                            button.set_active(entry.is_favorite);
                            if entry.is_favorite {
                                button.set_icon_name("starred-symbolic");
                            } else {
                                button.set_icon_name("non-starred-symbolic");
                            }

                            // 重新绑定行时同步星星可见性(已收藏/选中则常显)
                            if let Some(state) =
                                row_states_for_bind.borrow().get(&(list_item.as_ptr() as usize))
                            {
                                state.favorited.set(entry.is_favorite);
                                state.sync_visibility(selection_model_clone.selected());
                            }

                            // Store app_id in button for toggle handler
                            button.set_data("app-id", entry.id.clone());

                            // Unblock signal handler
                            button.unblock_signal(signal_id.as_ref());
                        }
                    }
                }

                // Apply concentric border-radius logic: the list card is 8px,
                // rows sit flush (gap 0), so the first row carries an 8px top
                // radius, the last row an 8px bottom radius, middle rows 0.
                let position = list_item.position();
                let n_items = selection_model_clone.n_items();

                let mut css = String::from("outline: none;");
                if position < n_items - 1 {
                    css.push_str(
                        "
                        |border-bottom-left-radius: 0px;
                        |border-bottom-right-radius: 0px;
                        "
                        .trim_margin()
                        .as_str(),
                    );
                } else {
                    css.push_str(
                        "
                        |border-bottom-left-radius: 8px;
                        |border-bottom-right-radius: 8px;
                        "
                        .trim_margin()
                        .as_str(),
                    );
                }
                if position > 0 {
                    css.push_str(
                        "
                        |border-top-left-radius: 0px;
                        |border-top-right-radius: 0px;
                        "
                        .trim_margin()
                        .as_str(),
                    );
                } else {
                    css.push_str(
                        "
                        |border-top-left-radius: 8px;
                        |border-top-right-radius: 8px;
                        "
                        .trim_margin()
                        .as_str(),
                    );
                }
                row.inline_css(&css);
            }
        });

        let list_view = gtk::ListView::new(Some(selection_model.clone()), Some(factory));

        let app_list_view = &list_view;
        let widgets = view_output!();

        // Surface mapping can lag the show request (layer-shell configure on
        // cold start); the deferred slide resumes here.
        widgets.slide_bin_widget.connect_map({
            let sender = sender.input_sender().clone();
            move |_| {
                sender.send(AppLauncherWindowInput::WindowMapped).ok();
            }
        });

        // Store references
        let search_entry_ref = widgets.search_entry.clone();
        let scrolled_window_ref = widgets.scrolled_window.clone();

        // Empty-state placeholder: floats over the list card while a search
        // yields nothing.
        let empty_icon = gtk::Image::builder()
            .icon_name("system-search-symbolic")
            .pixel_size(48)
            .build();
        let empty_text = gtk::Label::new(Some("No results"));
        empty_text.add_css_class("title-4");
        empty_text.add_css_class("dim-label");
        let empty_label = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        empty_label.append(&empty_icon);
        empty_label.append(&empty_text);
        empty_label.set_visible(false);

        // Wrap the list view in an overlay so the placeholder can float
        // above the (possibly empty) list card.
        let list_overlay = gtk::Overlay::new();
        scrolled_window_ref.set_child(None::<&gtk::Widget>);
        list_overlay.set_child(Some(&list_view));
        list_overlay.add_overlay(&empty_label);
        scrolled_window_ref.set_child(Some(&list_overlay));

        // Initialize fuzzy searcher with all entries
        let mut searcher = AppSearcher::new();
        searcher.set_entries(entries.clone().apply(|this: &mut Vec<DesktopEntry>| {
            this.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        }));

        let model = Self {
            visible: false,
            window: root,
            desktop_entries_service,
            searcher,
            filtered_apps: entries,
            list_view,
            search_entry: search_entry_ref,
            selection_model,
            scrolled_window: scrolled_window_ref,
            empty_label,
            slide_bin: slide_bin_widget.clone(),
            animation: RefCell::new(None),
            hiding: Rc::new(std::cell::Cell::new(false)),
            slide_pending: std::cell::Cell::new(false),
            favorites_service: config.favorites_service,
            wm_service: config.wm_service,
        };

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppLauncherWindowInput::Toggle => {
                if self.visible {
                    self.hide_animated();
                    debug!("App launcher toggled: visible=false");
                } else {
                    self.show_animated();
                    debug!("App launcher toggled: visible=true");

                    // Clear search and reset list when showing
                    self.search_entry.set_text("");
                    let entries = if let Some(ref service) = *self.desktop_entries_service.lock().unwrap() {
                        service.get_entries()
                    } else {
                        Vec::new()
                    };
                    self.searcher.set_entries(entries.clone());
                    self.filtered_apps = entries;
                    self.update_list_view();
                    self.selection_model.set_selected(0);

                    // Focus search entry (keep focus on entry for typing)
                    self.search_entry.grab_focus();
                }
            },
            AppLauncherWindowInput::Hide => {
                self.hide_animated();

                // Clear state when hiding
                self.search_entry.set_text("");
                let entries = if let Some(ref service) = *self.desktop_entries_service.lock().unwrap() {
                    service.get_entries()
                } else {
                    Vec::new()
                };
                self.searcher.set_entries(entries.clone());
                self.filtered_apps = entries;
                self.update_list_view();
                self.selection_model.set_selected(0);

                debug!("App launcher hidden");
            },
            AppLauncherWindowInput::WindowMapped => {
                if self.visible && self.slide_pending.replace(false) {
                    self.start_slide(1.0);
                }
            },
            AppLauncherWindowInput::SearchChanged(query) => {
                debug!("Search query: {}", query);

                self.filtered_apps = self.searcher.search(&query);

                // Update list view and select first item
                self.update_list_view();
                self.selection_model.set_selected(0);
            },
            AppLauncherWindowInput::NavigateDown => {
                let current = self.selection_model.selected();
                let max = self.filtered_apps.len().saturating_sub(1) as u32;
                if current < max {
                    self.selection_model.set_selected(current + 1);
                    self.scroll_to_selected();
                }
            },
            AppLauncherWindowInput::NavigateUp => {
                let current = self.selection_model.selected();
                if current > 0 {
                    self.selection_model.set_selected(current - 1);
                    self.scroll_to_selected();
                }
            },
            AppLauncherWindowInput::LaunchSelected => {
                let idx = self.selection_model.selected() as usize;
                if let Some(app) = self.filtered_apps.get(idx).cloned() {
                    debug!("Launching app: {}", app.name);

                    self.hide_animated();

                    // Launch via WMService asynchronously
                    let wm_service = self.wm_service.clone();
                    glib::spawn_future_local(async move {
                        if let Err(e) = app.launch(&wm_service).await {
                            log::error!("Failed to launch {}: {}", app.name, e);
                        }
                    });
                }
            },
            AppLauncherWindowInput::ToggleFavorite(app_id) => {
                debug!("Toggling favorite for app: {}", app_id);

                // Toggle favorite in GSettings and update service
                if let Some(ref favorites_service_arc) = self.favorites_service {
                    if let Ok(guard) = favorites_service_arc.lock() {
                        if let Some(ref service) = *guard {
                            // Toggle in GSettings
                            if let Err(e) = service.toggle_favorite(&app_id) {
                                log::error!("Failed to toggle favorite in GSettings: {}", e);
                                return;
                            }

                            // Get updated favorites and update desktop entries service
                            let favorites = service.get_favorites();
                            let favorites_vec: Vec<String> = favorites.into_iter().collect();

                            if let Some(ref de_service) = *self.desktop_entries_service.lock().unwrap() {
                                de_service.set_favorites(favorites_vec);
                            }

                            debug!("Favorite toggled successfully for {}", app_id);
                        }
                    }
                }

                // Reload entries with updated favorites and preserve search
                let current_query = self.search_entry.text().to_string();
                let all_entries = if let Some(ref service) = *self.desktop_entries_service.lock().unwrap() {
                    service.get_entries()
                } else {
                    Vec::new()
                };

                if current_query.is_empty() {
                    self.filtered_apps = all_entries;
                } else {
                    // Update searcher with new entries including updated favorites
                    self.searcher.set_entries(all_entries);
                    self.filtered_apps = self.searcher.search(&current_query);
                }
                self.update_list_view();
            },
            AppLauncherWindowInput::DesktopEntriesChanged => {
                debug!("Desktop entries changed, reloading list");

                // Get updated entries from service
                let all_entries = if let Some(ref service) = *self.desktop_entries_service.lock().unwrap() {
                    service.get_entries()
                } else {
                    Vec::new()
                };

                // Update searcher and filtered list based on current search
                self.searcher.set_entries(all_entries.clone());
                let current_query = self.search_entry.text().to_string();

                if current_query.is_empty() {
                    self.filtered_apps = all_entries;
                } else {
                    self.filtered_apps = self.searcher.search(&current_query);
                }

                self.update_list_view();
                info!(
                    "Launcher list updated with {} entries",
                    self.filtered_apps.len()
                );
            },
        }
    }
}

impl AppLauncherWindow {
    fn update_list_view(&self) {
        if let Some(model) = self.list_view.model() {
            if let Some(selection) = model.downcast_ref::<gtk::SingleSelection>() {
                if let Some(list_store) = selection
                    .model()
                    .and_then(|m| m.downcast::<gtk::gio::ListStore>().ok())
                {
                    list_store.remove_all();
                    for app in self.filtered_apps.iter() {
                        list_store.append(&glib::BoxedAnyObject::new(app.clone()));
                    }
                }
            }
        }

        // Empty search results -> show the "No results" placeholder.
        self.empty_label.set_visible(self.filtered_apps.is_empty());
    }

    fn scroll_to_selected(&self) {
        let selected = self.selection_model.selected();

        // Get the adjustment (not Option in GTK4)
        let vadjustment = self.scrolled_window.vadjustment();
        let page_size = vadjustment.page_size();
        let upper = vadjustment.upper();
        let n_items = self.filtered_apps.len() as f64;

        if n_items > 0.0 {
            // Calculate approximate item height
            let item_height = upper / n_items;
            let target_pos = selected as f64 * item_height;

            // Scroll to make selected item visible
            let current_pos = vadjustment.value();

            // If item is below visible area
            if target_pos + item_height > current_pos + page_size {
                vadjustment.set_value(target_pos + item_height - page_size);
            }
            // If item is above visible area
            else if target_pos < current_pos {
                vadjustment.set_value(target_pos);
            }
        }
    }

    fn factorise_exec_str(exec: &str) -> String {
        let mut factorized_exec = exec.to_string();

        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            factorized_exec = factorized_exec.replace(&tmpdir, "$TMPDIR");
        }
        if let Ok(xdg_runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            factorized_exec = factorized_exec.replace(&xdg_runtime_dir, "$XDG_RUNTIME_DIR");
        }
        if let Ok(xdg_cache_home) = std::env::var("XDG_CACHE_HOME") {
            factorized_exec = factorized_exec.replace(&xdg_cache_home, "$XDG_CACHE_HOME");
        }
        if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
            factorized_exec = factorized_exec.replace(&xdg_config_home, "$XDG_CONFIG_HOME");
        }
        if let Ok(xdg_bin_home) = std::env::var("XDG_BIN_HOME") {
            factorized_exec = factorized_exec.replace(&xdg_bin_home, "$XDG_BIN_HOME");
        }
        if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
            factorized_exec = factorized_exec.replace(&xdg_data_home, "$XDG_DATA_HOME");
        }
        if let Ok(xdg_lib_home) = std::env::var("XDG_LIB_HOME") {
            factorized_exec = factorized_exec.replace(&xdg_lib_home, "$XDG_LIB_HOME");
        }
        if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
            factorized_exec = factorized_exec.replace(&xdg_state_home, "$XDG_STATE_HOME");
        }
        if let Ok(xdg_data_dirs) = std::env::var("XDG_DATA_DIRS") {
            factorized_exec = factorized_exec.replace(&xdg_data_dirs, "$XDG_DATA_DIRS");
        }
        if let Ok(xdg_current_desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
            factorized_exec = factorized_exec.replace(&xdg_current_desktop, "$XDG_CURRENT_DESKTOP");
        }
        if let Ok(home) = std::env::var("HOME") {
            factorized_exec = factorized_exec.replace(&home, "$HOME");
        }

        factorized_exec
            .replace("/usr/bin/", "")
            .replace("/usr/sbin/", "")
            .replace("/usr/local/bin/", "")
            .replace("/usr/local/sbin/", "")
    }
}

/// Per-row favorite-star reveal state: the button is visible while the row
/// is hovered or selected, hidden otherwise.
struct LauncherRowState {
    item: gtk::ListItem,
    hovered: std::cell::Cell<bool>,
    favorited: std::cell::Cell<bool>,
    button: gtk::ToggleButton,
}

impl LauncherRowState {
    fn sync_visibility(&self, selected: u32) {
        let visible =
            self.hovered.get() || self.favorited.get() || self.item.position() == selected;
        self.button.set_opacity(if visible { 1.0 } else { 0.0 });
    }
}
