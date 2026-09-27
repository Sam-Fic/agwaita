use super::event_item::{
    EventData,
    EventItem,
};
use crate::system_state::global_service::GlobalSystemService;
use agw_service::{
    calendar::types::CalendarEvent,
    signal::SignalHandler,
    time::TimeUnit,
};
use catalyser::stdx::extension::str_extension::MultilineStr;
use chrono::{
    Datelike,
    Local,
    Locale,
    NaiveDate,
};
use gtk4::prelude::*;
use relm4::{
    ComponentParts,
    ComponentSender,
    RelmWidgetExt,
    SimpleComponent,
    factory::FactoryVecDeque,
    gtk,
};
use std::{
    collections::HashSet,
    str::FromStr,
    sync::Arc,
};

pub struct Calendar {
    day_week: String,
    date: String,
    selected_date: String,
    selected_naive_date: Option<NaiveDate>,
    is_selected_today: bool,
    today_events: FactoryVecDeque<EventItem>,
    selected_day_events: FactoryVecDeque<EventItem>,
    locale: Locale,
    global_service: Arc<GlobalSystemService>,
    /// 当前展示的年/月(导航用)
    display_year: i32,
    display_month: u32,
    /// 展示月份中有事件安排的日期
    marked_days: HashSet<u32>,
    /// 表头下方显示的年/月标签
    nav_month_label: gtk::Label,
    nav_year_label: gtk::Label,
    /// 日期网格容器
    days_grid: gtk::Grid,
    /// 重建网格时给日期按钮发送点击事件用
    sender_for_redraw: ComponentSender<Self>,
    _day_handler: Option<SignalHandler>,
}

#[derive(Debug)]
pub enum CalendarInput {
    /// 点击某个日期格
    DaySelected(NaiveDate),
    /// 月/年导航,参数为 +/-1
    NavMonth(i32),
    NavYear(i32),
    UpdateEvents(Vec<CalendarEvent>),
    ResetToToday,
    DayChanged,
}

#[relm4::component(pub)]
impl SimpleComponent for Calendar {
    type Init = Arc<GlobalSystemService>;
    type Input = CalendarInput;
    type Output = ();

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 10,
            set_margin_all: 12,

            // ===== 表头:今天 =====
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 2,

                gtk::Label {
                    add_css_class: "title-5",
                    inline_css: "
                    |font-weight: bold;
                    ".trim_margin().as_str(),

                    #[watch]
                    set_label: &model.day_week,
                },
                gtk::Label {
                    add_css_class: "title-4",

                    #[watch]
                    set_label: &model.date,
                },
            },

            // ===== 月/年导航 =====
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_halign: gtk::Align::Center,
                set_spacing: 16,

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 2,

                    gtk::Button {
                        add_css_class: "flat",
                        add_css_class: "circular",
                        set_icon_name: "pan-start-symbolic",

                        connect_clicked[sender] => move |_| {
                            sender.input(CalendarInput::NavMonth(-1));
                        },
                    },
                    #[name = "nav_month_label"]
                    gtk::Label {
                        set_width_request: 86,
                        set_xalign: 0.5,
                    },
                    gtk::Button {
                        add_css_class: "flat",
                        add_css_class: "circular",
                        set_icon_name: "pan-end-symbolic",

                        connect_clicked[sender] => move |_| {
                            sender.input(CalendarInput::NavMonth(1));
                        },
                    },
                },
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 2,

                    gtk::Button {
                        add_css_class: "flat",
                        add_css_class: "circular",
                        set_icon_name: "pan-start-symbolic",

                        connect_clicked[sender] => move |_| {
                            sender.input(CalendarInput::NavYear(-1));
                        },
                    },
                    #[name = "nav_year_label"]
                    gtk::Label {
                        set_width_request: 52,
                        set_xalign: 0.5,
                    },
                    gtk::Button {
                        add_css_class: "flat",
                        add_css_class: "circular",
                        set_icon_name: "pan-end-symbolic",

                        connect_clicked[sender] => move |_| {
                            sender.input(CalendarInput::NavYear(1));
                        },
                    },
                },
            },

            // ===== 星期表头 =====
            #[name = "weekday_row"]
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_halign: gtk::Align::Center,
                set_spacing: 2,
            },

            // ===== 日期网格 =====
            #[name = "days_grid"]
            gtk::Grid {
                set_halign: gtk::Align::Center,
                set_row_spacing: 2,
                set_column_spacing: 2,
            },

            gtk::Separator {
                set_margin_top: 4,
            },

            // ===== 日程列表 =====
            gtk::ScrolledWindow {
                set_propagate_natural_width: true,
                set_propagate_natural_height: true,
                set_hexpand: true,
                set_vexpand: true,
                set_width_request: 320 - 24,
                set_max_content_height: 320,

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

                    // Today events
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 8,

                        gtk::Label {
                            add_css_class: "title-5",
                            inline_css: "
                            |font-weight: bold;
                            ".trim_margin().as_str(),
                            set_halign: gtk::Align::Start,

                            set_label: "Today",
                        },
                        #[local_ref]
                        today_events_box -> gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 8,
                        },
                    },
                    gtk::Box {
                        set_margin_top: 8,
                    },
                    // Selected day events
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 8,

                        #[watch]
                        set_visible: !model.is_selected_today,

                        gtk::Label {
                            add_css_class: "title-5",
                            inline_css: "
                            |font-weight: bold;
                            ".trim_margin().as_str(),
                            set_halign: gtk::Align::Start,

                            #[watch]
                            set_label: &model.selected_date,
                        },
                        #[local_ref]
                        selected_events_box -> gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_width_request: 320 - 24,
                            set_can_focus: false,
                            set_focusable: false,
                            set_spacing: 8,
                        },

                    },
                },
            },
        }
    }

    fn init(global_service: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let now = Local::now();

        let today_events = FactoryVecDeque::builder()
            .launch(gtk::Box::default())
            .detach();

        let selected_day_events = FactoryVecDeque::builder()
            .launch(gtk::Box::default())
            .detach();

        let locale = Self::get_system_locale();

        let model = Calendar {
            day_week: Self::capitalize_first(now.format_localized("%A", locale).to_string()),
            date: Self::capitalize_first(now.format_localized("%d %B %Y", locale).to_string()),
            selected_date: Self::capitalize_first(now.format_localized("%d %B %Y", locale).to_string()),
            selected_naive_date: Some(now.date_naive()),
            is_selected_today: true,
            today_events,
            selected_day_events,
            locale,
            global_service,
            display_year: now.year(),
            display_month: now.month(),
            marked_days: HashSet::new(),
            nav_month_label: gtk::Label::default(),
            nav_year_label: gtk::Label::default(),
            days_grid: gtk::Grid::new(),
            sender_for_redraw: sender.clone(),
            _day_handler: None,
        };

        let today_events_box = model.today_events.widget();
        let selected_events_box = model.selected_day_events.widget();

        let widgets = view_output!();

        let mut model = Calendar {
            nav_month_label: widgets.nav_month_label.clone(),
            nav_year_label: widgets.nav_year_label.clone(),
            days_grid: widgets.days_grid.clone(),
            ..model
        };

        // 星期表头(周日起,与日期网格一致)
        Self::fill_weekday_row(&widgets.weekday_row, model.locale);

        // Load initial events and mark days
        sender.input(CalendarInput::UpdateEvents(Vec::new()));

        // Subscribe to calendar events updates
        let receiver = model.global_service.subscribe();
        let sender_clone = sender.clone();
        std::thread::spawn(move || {
            while let Ok(update) = receiver.recv() {
                if let crate::system_state::messages::SystemStateUpdate::CalendarEvents(events) = update {
                    sender_clone.input(CalendarInput::UpdateEvents(events));
                }
            }
        });

        // Subscribe to day changes (midnight rollover).
        // Store the handler in the model so it's disconnected when this component is dropped.
        let day_sender = sender.clone();
        model._day_handler = Some(
            model
                .global_service
                .time_service()
                .subscribe(TimeUnit::Day, move |_| {
                    day_sender.input(CalendarInput::DayChanged);
                }),
        );

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, #[allow(unused_variables)] sender: ComponentSender<Self>) {
        match message {
            CalendarInput::DaySelected(date) => {
                self.selected_naive_date = Some(date);
                self.selected_date = Self::capitalize_first(
                    date.format_localized("%A %d %B %Y", self.locale).to_string(),
                );

                let today = Local::now().date_naive();
                self.is_selected_today = date == today;

                self.update_selected_day_events();
                self.redraw_days();
            },
            CalendarInput::NavMonth(delta) => {
                let (mut year, mut month) = (self.display_year, self.display_month as i32 - 1 + delta);
                if month < 0 {
                    month += 12;
                    year -= 1;
                } else if month > 11 {
                    month -= 12;
                    year += 1;
                }
                self.display_year = year;
                self.display_month = month as u32 + 1;
                self.refresh_marks();
                self.redraw_days();
            },
            CalendarInput::NavYear(delta) => {
                self.display_year += delta as i32;
                self.refresh_marks();
                self.redraw_days();
            },
            CalendarInput::UpdateEvents(_events) => {
                self.update_today_events_from_service();
                self.update_selected_day_events();
                self.refresh_marks();
                self.redraw_days();
            },
            CalendarInput::ResetToToday => {
                let today = Local::now();
                self.display_year = today.year();
                self.display_month = today.month();
                self.day_week = Self::capitalize_first(today.format_localized("%A", self.locale).to_string());
                self.date = Self::capitalize_first(today.format_localized("%d %B %Y", self.locale).to_string());
                self.selected_date = Self::capitalize_first(today.format_localized("%d %B %Y", self.locale).to_string());
                self.selected_naive_date = Some(today.date_naive());
                self.is_selected_today = true;

                self.update_today_events_from_service();
                self.update_selected_day_events();
                self.refresh_marks();
                self.redraw_days();
            },
            CalendarInput::DayChanged => {
                let today = Local::now();
                let today_date = today.date_naive();

                self.day_week = Self::capitalize_first(today.format_localized("%A", self.locale).to_string());
                self.date = Self::capitalize_first(today.format_localized("%d %B %Y", self.locale).to_string());

                if self.is_selected_today {
                    self.selected_naive_date = Some(today_date);
                    self.selected_date = Self::capitalize_first(
                        today_date
                            .format_localized("%A %d %B %Y", self.locale)
                            .to_string(),
                    );
                    self.is_selected_today = true;
                    self.display_year = today.year();
                    self.display_month = today.month();
                } else {
                    self.is_selected_today = self.selected_naive_date == Some(today_date);
                }

                self.update_today_events_from_service();
                self.update_selected_day_events();
                self.refresh_marks();
                self.redraw_days();
            },
        }
    }
}

impl Calendar {
    /// 填充星期表头(周日起,与日期网格一致)
    fn fill_weekday_row(row: &gtk::Box, locale: Locale) {
        let today = Local::now().date_naive();
        let sunday = today - chrono::Duration::days(today.weekday().num_days_from_sunday() as i64);

        for i in 0..7 {
            let day = sunday + chrono::Duration::days(i);
            let label = gtk::Label::new(Some(&day.format_localized("%a", locale).to_string()));
            label.set_width_request(38);
            label.set_halign(gtk::Align::Center);
            label.add_css_class("caption");
            label.set_opacity(0.65);
            row.append(&label);
        }
    }

    /// 刷新展示月份的事件日期标记并同步导航标签
    fn refresh_marks(&mut self) {
        self.nav_month_label.set_text(
            &Self::capitalize_first(
                NaiveDate::from_ymd_opt(self.display_year, self.display_month, 1)
                    .unwrap()
                    .format_localized("%B", self.locale)
                    .to_string(),
            ),
        );
        self.nav_year_label.set_text(&self.display_year.to_string());

        self.marked_days = self
            .global_service
            .calendar_service()
            .get_days_with_events(self.display_year, self.display_month)
            .into_iter()
            .collect();
    }

    /// 重建日期网格:7 列 x 6 行,38px 圆形单元格
    fn redraw_days(&self) {
        while let Some(child) = self.days_grid.first_child() {
            self.days_grid.remove(&child);
        }

        let Some(first_of_month) = NaiveDate::from_ymd_opt(self.display_year, self.display_month, 1) else {
            return;
        };
        let today = Local::now().date_naive();
        // 网格从周日开始
        let grid_start = first_of_month - chrono::Duration::days(first_of_month.weekday().num_days_from_sunday() as i64);

        for i in 0..42 {
            let date = grid_start + chrono::Duration::days(i);
            let day = date.day();
            let in_month = date.year() == self.display_year && date.month() == self.display_month;

            let button = gtk::Button::new();
            button.set_size_request(38, 38);
            button.add_css_class("flat");
            button.add_css_class("circular");

            let overlay = gtk::Overlay::new();
            let label = gtk::Label::new(Some(&day.to_string()));

            if date == today {
                label.set_markup(&format!("<b>{}</b>", day));
                button.inline_css("border: 2px solid @accent_color;");
            }

            if Some(date) == self.selected_naive_date {
                button.remove_css_class("flat");
                button.add_css_class("accent");
                label.set_markup(&format!(
                    "<b><span foreground=\"#ffffff\">{}</span></b>",
                    day
                ));
            }

            if !in_month {
                button.set_opacity(0.35);
            }

            overlay.set_child(Some(&label));

            // 有事件的日期:底部小圆点
            if in_month && self.marked_days.contains(&day) {
                let dot = gtk::Label::new(None);
                dot.set_valign(gtk::Align::End);
                dot.set_margin_bottom(4);
                dot.set_size_request(4, 4);
                dot.inline_css("background: @accent_color; border-radius: 2px; min-width: 4px; min-height: 4px;");
                overlay.add_overlay(&dot);
            }

            button.set_child(Some(&overlay));
            button.set_tooltip_text(Some(&date.format_localized("%x", self.locale).to_string()));

            let sender = self.sender_for_redraw.clone();
            button.connect_clicked(move |_| {
                sender.input(CalendarInput::DaySelected(date));
            });

            let col = (i % 7) as i32;
            let row = (i / 7) as i32;
            self.days_grid.attach(&button, col, row, 1, 1);
        }
    }

    fn update_today_events_from_service(&mut self) {
        let today = Local::now().date_naive();
        let events = self
            .global_service
            .calendar_service()
            .get_events_for_date(today);

        let mut today_events_guard = self.today_events.guard();
        today_events_guard.clear();

        for event in &events {
            today_events_guard.push_back(Self::convert_event(event));
        }
    }

    fn update_selected_day_events(&mut self) {
        // Get events for selected day using new lazy API
        if let Some(naive_date) = self.selected_naive_date {
            let events = self
                .global_service
                .calendar_service()
                .get_events_for_date(naive_date);
            self.update_selected_day_events_from_list(&events);
        }
    }

    fn update_selected_day_events_from_list(&mut self, events: &[CalendarEvent]) {
        let Some(selected_date) = self.selected_naive_date else {
            return;
        };

        let mut events_guard = self.selected_day_events.guard();
        events_guard.clear();

        for event in events {
            if event.start.date_naive() == selected_date {
                events_guard.push_back(Self::convert_event(event));
            }
        }
    }

    fn convert_event(event: &CalendarEvent) -> EventData {
        EventData {
            title: event.summary.clone(),
            description: event.description.clone(),
            start: event.start,
            end: event.end,
            color: event
                .color
                .clone()
                .unwrap_or_else(|| "@accent_color".to_string()),
            is_all_day: event.is_all_day,
        }
    }
}

impl Calendar {
    fn get_system_locale() -> Locale {
        std::env::var("LANG")
            .or_else(|_| std::env::var("LC_TIME"))
            .or_else(|_| std::env::var("LC_ALL"))
            .ok()
            .and_then(|lang| Locale::from_str(&lang).ok())
            .unwrap_or(Locale::default())
    }

    fn capitalize_first(s: String) -> String {
        let mut chars = s.chars();
        match chars.next() {
            None => s,
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        }
    }
}
