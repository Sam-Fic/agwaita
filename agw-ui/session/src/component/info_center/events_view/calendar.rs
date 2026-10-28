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
    cell::Cell,
    collections::HashSet,
    rc::Rc,
    str::FromStr,
    sync::Arc,
};

pub struct Calendar {
    /// 选中日期的标题:相对今天的说法(Today/Yesterday/Tomorrow)或 "星期几 日 月"
    selected_title: String,
    selected_naive_date: Option<NaiveDate>,
    is_selected_today: bool,
    /// 选中日期是否有日程,决定空状态占位是否显示
    has_selected_events: bool,
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
    /// 42 个常驻日期格(7x6):更新时只改属性,不再销毁重建控件
    day_cells: Vec<DayCell>,
    /// 选中日已渲染的日程,用于避免无变化时的列表重建
    rendered_events: Vec<EventData>,
    _day_handler: Option<SignalHandler>,
}

/// 一个常驻的日期格:按钮 + 日期文字 + 事件小圆点。
///
/// 旧实现每次刷新都销毁并重建 42 个 Button/Overlay/Label(外加 42 个点击
/// 闭包、42 次本地化 tooltip 格式化)。现在控件只创建一次,刷新只改文字、
/// CSS class 和圆点可见性,日历变更不会再造成整格子重建的卡顿。
struct DayCell {
    /// 当前显示的日期(点击时读取,避免每次重建闭包)
    date: Rc<Cell<NaiveDate>>,
    button: gtk::Button,
    label: gtk::Label,
    dot: gtk::Label,
}

impl DayCell {
    fn new() -> Self {
        let button = gtk::Button::new();
        button.set_size_request(38, 38);
        button.add_css_class("flat");
        button.add_css_class("circular");

        let overlay = gtk::Overlay::new();
        let label = gtk::Label::new(None);
        overlay.set_child(Some(&label));

        let dot = gtk::Label::new(None);
        dot.set_valign(gtk::Align::End);
        dot.set_margin_bottom(4);
        dot.set_size_request(4, 4);
        dot.add_css_class("calendar-event-dot");
        dot.set_visible(false);
        overlay.add_overlay(&dot);

        button.set_child(Some(&overlay));

        // Placeholder until the first refresh assigns the real date.
        let date = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();

        Self {
            date: Rc::new(Cell::new(date)),
            button,
            label,
            dot,
        }
    }
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
            // popover > contents 已有 12px padding,这里不再叠加,保证同心圆角链

            // ===== 表头:选中日期(相对今天) =====
            gtk::Label {
                add_css_class: "title-5",
                inline_css: "
                |font-weight: bold;
                ".trim_margin().as_str(),

                #[watch]
                set_label: &model.selected_title,
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

            // ===== 日程列表(选中日期) =====
            gtk::ScrolledWindow {
                set_propagate_natural_width: true,
                set_propagate_natural_height: true,
                set_hexpand: true,
                set_vexpand: true,
                set_width_request: 320 - 24,
                set_max_content_height: 320,
                // 与上方日历数字字形对齐(数字在 38px 格内居中,字形起点约在
                // 格缘右 20px 处),左右 20px、上方 12px 留出呼吸感
                set_margin_start: 20,
                set_margin_end: 20,
                set_margin_top: 12,

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

                    // 空状态占位
                    gtk::Label {
                        add_css_class: "dim-label",
                        set_label: "No events",
                        set_halign: gtk::Align::Center,
                        set_margin_top: 24,

                        #[watch]
                        set_visible: !model.has_selected_events,
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
        }
    }

    fn init(global_service: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let now = Local::now();

        let selected_day_events = FactoryVecDeque::builder()
            .launch(gtk::Box::default())
            .detach();

        let locale = Self::get_system_locale();

        let model = Calendar {
            selected_title: "Today".to_string(),
            selected_naive_date: Some(now.date_naive()),
            is_selected_today: true,
            has_selected_events: false,
            selected_day_events,
            locale,
            global_service,
            display_year: now.year(),
            display_month: now.month(),
            marked_days: HashSet::new(),
            nav_month_label: gtk::Label::default(),
            nav_year_label: gtk::Label::default(),
            days_grid: gtk::Grid::new(),
            day_cells: Vec::new(),
            rendered_events: Vec::new(),
            _day_handler: None,
        };

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

        // 42 个日期格只在此处创建一次,后续刷新都是原地更新
        Self::build_day_cells(&mut model, &sender);
        model.refresh_marks();
        model.refresh_days(true);

        // Load initial events and mark days
        sender.input(CalendarInput::UpdateEvents(Vec::new()));

        // Subscribe to calendar events updates
        let receiver = model.global_service.subscribe();
        let sender_clone = sender.clone();
        std::thread::spawn(move || {
            while let Ok(update) = receiver.recv() {
                if let crate::system_state::messages::SystemStateUpdate::CalendarEvents(events) = update {
                    // send() instead of input(): input() panics when the
                    // component has been dropped (monitor unplugged), while
                    // this forwarding thread may still see one more event.
                    let _ = sender_clone
                        .input_sender()
                        .send(CalendarInput::UpdateEvents(events));
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
                let previous = self.selected_naive_date;
                self.selected_naive_date = Some(date);
                self.selected_title = Self::relative_title(date, self.locale);
                let today = Local::now().date_naive();
                self.is_selected_today = date == today;

                // Only the two affected cells change style.
                for cell in &self.day_cells {
                    let cell_date = cell.date.get();
                    if Some(cell_date) == previous || cell_date == date {
                        self.style_cell(cell, cell_date, today);
                    }
                }

                self.update_selected_day_events();
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
                self.refresh_days(true);
            },
            CalendarInput::NavYear(delta) => {
                self.display_year += delta as i32;
                self.refresh_marks();
                self.refresh_days(true);
            },
            CalendarInput::UpdateEvents(_events) => {
                // EDS signals can arrive in bursts; only touch the grid when
                // the marked-day set actually changed, and only rebuild the
                // event list when its content changed.
                let marks = self
                    .global_service
                    .calendar_service()
                    .get_days_with_events(self.display_year, self.display_month);
                if marks != self.marked_days {
                    self.marked_days = marks;
                    self.refresh_day_marks();
                }
                self.update_selected_day_events();
            },
            CalendarInput::ResetToToday => {
                let today = Local::now();
                self.display_year = today.year();
                self.display_month = today.month();
                self.selected_title = "Today".to_string();
                self.selected_naive_date = Some(today.date_naive());
                self.is_selected_today = true;

                self.refresh_marks();
                self.refresh_days(true);
                self.update_selected_day_events();
            },
            CalendarInput::DayChanged => {
                let today = Local::now();
                let today_date = today.date_naive();

                if self.is_selected_today {
                    self.selected_title = "Today".to_string();
                    self.selected_naive_date = Some(today_date);
                    self.is_selected_today = true;
                    self.display_year = today.year();
                    self.display_month = today.month();
                } else {
                    self.is_selected_today = self.selected_naive_date == Some(today_date);
                    if self.is_selected_today {
                        self.selected_title = "Today".to_string();
                    }
                }

                self.refresh_marks();
                self.refresh_days(true);
                self.update_selected_day_events();
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

    /// 选中日期的标题:今天/昨天/明天用相对说法,其余用 "星期几 日 月"
    fn relative_title(date: NaiveDate, locale: Locale) -> String {
        let today = Local::now().date_naive();
        match (date - today).num_days() {
            0 => "Today".to_string(),
            1 => "Tomorrow".to_string(),
            -1 => "Yesterday".to_string(),
            _ => Self::capitalize_first(
                date.format_localized("%A %d %B", locale).to_string(),
            ),
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
            .get_days_with_events(self.display_year, self.display_month);
    }

    /// 创建 42 个常驻日期格(仅初始化时调用一次)
    fn build_day_cells(model: &mut Self, sender: &ComponentSender<Self>) {
        for i in 0..42 {
            let cell = DayCell::new();
            let sender = sender.clone();
            let date = Rc::clone(&cell.date);
            cell.button.connect_clicked(move |_| {
                sender.input(CalendarInput::DaySelected(date.get()));
            });

            let col = (i % 7) as i32;
            let row = (i / 7) as i32;
            model.days_grid.attach(&cell.button, col, row, 1, 1);
            model.day_cells.push(cell);
        }
    }

    /// 刷新 42 个日期格的日期文字/状态。
    ///
    /// `dates_changed` 为 true 时(换月/跨天)才重新生成本地化 tooltip。
    fn refresh_days(&self, dates_changed: bool) {
        let Some(first_of_month) = NaiveDate::from_ymd_opt(self.display_year, self.display_month, 1) else {
            return;
        };
        let today = Local::now().date_naive();
        // 网格从周日开始
        let grid_start = first_of_month - chrono::Duration::days(first_of_month.weekday().num_days_from_sunday() as i64);

        for (i, cell) in self.day_cells.iter().enumerate() {
            let date = grid_start + chrono::Duration::days(i as i64);
            cell.date.set(date);

            if dates_changed {
                cell.button.set_tooltip_text(Some(&date.format_localized("%x", self.locale).to_string()));
            }

            self.style_cell(cell, date, today);
        }
    }

    /// 只更新"有事件"小圆点的可见性(不碰文字和 tooltip)
    fn refresh_day_marks(&self) {
        for cell in &self.day_cells {
            let date = cell.date.get();
            let in_month = date.year() == self.display_year && date.month() == self.display_month;
            cell.dot
                .set_visible(in_month && self.marked_days.contains(&date.day()));
        }
    }

    /// 把一个日期格的文字、选中/今天样式和圆点同步到当前模型状态。
    fn style_cell(&self, cell: &DayCell, date: NaiveDate, today: NaiveDate) {
        let day = date.day();
        let in_month = date.year() == self.display_year && date.month() == self.display_month;
        let selected = Some(date) == self.selected_naive_date;
        let is_today = date == today;

        if selected {
            cell.label
                .set_markup(&format!("<b><span foreground=\"#ffffff\">{}</span></b>", day));
        } else if is_today {
            cell.label.set_markup(&format!("<b>{}</b>", day));
        } else {
            cell.label.set_text(&day.to_string());
        }

        if selected {
            cell.button.remove_css_class("flat");
            cell.button.add_css_class("accent");
        } else {
            cell.button.add_css_class("flat");
            cell.button.remove_css_class("accent");
        }

        if is_today {
            cell.button.add_css_class("calendar-today");
        } else {
            cell.button.remove_css_class("calendar-today");
        }

        cell.button.set_opacity(if in_month { 1.0 } else { 0.35 });
        cell.dot
            .set_visible(in_month && self.marked_days.contains(&day));
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
        // `get_events_for_date` already filters by the selected date and
        // sorts all-day events first — no second filter pass needed.
        let rendered: Vec<EventData> = events.iter().map(Self::convert_event).collect();

        self.has_selected_events = !rendered.is_empty();

        // Factory reconstruction is the expensive part (several widgets per
        // row + inline CSS parses). Skip it when the list is unchanged,
        // which is the common case for bursts of calendar signals.
        if rendered == self.rendered_events {
            return;
        }
        self.rendered_events = rendered.clone();

        let mut events_guard = self.selected_day_events.guard();
        events_guard.clear();

        for event in rendered {
            events_guard.push_back(event);
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
