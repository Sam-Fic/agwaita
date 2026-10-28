use crate::{
    runtime,
    signal::{
        Signal,
        SignalHandler,
    },
};
use chrono::{
    DateTime,
    Datelike,
    Local,
    NaiveDate,
    TimeZone,
    Timelike,
};
use std::{
    sync::{
        Arc,
        Mutex,
        atomic::{
            AtomicBool,
            Ordering,
        },
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

#[derive(Clone)]
struct TimeSignals {
    second_changed: Signal<DateTime<Local>>,
    minute_changed: Signal<DateTime<Local>>,
    hour_changed: Signal<DateTime<Local>>,
    day_changed: Signal<DateTime<Local>>,
    week_changed: Signal<DateTime<Local>>,
    month_changed: Signal<DateTime<Local>>,
    year_changed: Signal<DateTime<Local>>,
}

impl TimeSignals {
    fn new() -> Self {
        Self {
            second_changed: Signal::new(),
            minute_changed: Signal::new(),
            hour_changed: Signal::new(),
            day_changed: Signal::new(),
            week_changed: Signal::new(),
            month_changed: Signal::new(),
            year_changed: Signal::new(),
        }
    }
}

struct TimeState {
    second: u32,
    minute: u32,
    hour: u32,
    day: NaiveDate,
    week_year: i32,
    week: u32,
    month_year: i32,
    month: u32,
    year: i32,
}

pub struct TimeService {
    started: AtomicBool,
    signals: TimeSignals,
    state: Arc<Mutex<TimeState>>,
}

impl TimeService {
    pub fn new() -> Self {
        let now = Local::now();
        let iso_week = now.iso_week();
        Self {
            started: AtomicBool::new(false),
            signals: TimeSignals::new(),
            state: Arc::new(Mutex::new(TimeState {
                second: now.second(),
                minute: now.minute(),
                hour: now.hour(),
                day: now.date_naive(),
                week_year: iso_week.year(),
                week: iso_week.week(),
                month_year: now.year(),
                month: now.month(),
                year: now.year(),
            })),
        }
    }

    pub fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let signals = self.signals.clone();
        let state = Arc::clone(&self.state);

        runtime::spawn(async move {
            loop {
                // Sleep only until the next boundary of the finest
                // granularity anyone actually subscribes to. The old fixed
                // 100ms tick woke the process ten times per second around
                // the clock; a shell with a minute-precision clock and a
                // midnight subscriber now sleeps in whole-minute (or
                // whole-day) chunks instead, keeping the CPU idle.
                let now = Local::now();
                let target_ts = next_boundary(&signals, &now);
                let remaining_ns = (target_ts - now.timestamp()).max(1) * 1_000_000_000
                    - i64::from(now.timestamp_subsec_nanos());
                tokio::time::sleep(std::time::Duration::from_nanos(
                    remaining_ns.max(1_000_000) as u64,
                ))
                .await;

                let now = Local::now();
                let iso_week = now.iso_week();

                let second = now.second();
                let minute = now.minute();
                let hour = now.hour();
                let day = now.date_naive();
                let week_year = iso_week.year();
                let week = iso_week.week();
                let month_year = now.year();
                let month = now.month();
                let year = now.year();

                let (mut second_changed, mut minute_changed, mut hour_changed) = (false, false, false);
                let (mut day_changed, mut week_changed, mut month_changed, mut year_changed) = (false, false, false, false);

                {
                    let mut state = state.lock().unwrap();

                    if second != state.second {
                        state.second = second;
                        second_changed = true;
                    }
                    if minute != state.minute {
                        state.minute = minute;
                        minute_changed = true;
                    }
                    if hour != state.hour {
                        state.hour = hour;
                        hour_changed = true;
                    }
                    if day != state.day {
                        state.day = day;
                        day_changed = true;
                    }
                    if week_year != state.week_year || week != state.week {
                        state.week_year = week_year;
                        state.week = week;
                        week_changed = true;
                    }
                    if month_year != state.month_year || month != state.month {
                        state.month_year = month_year;
                        state.month = month;
                        month_changed = true;
                    }
                    if year != state.year {
                        state.year = year;
                        year_changed = true;
                    }
                }

                if second_changed && signals.second_changed.handler_count() > 0 {
                    signals.second_changed.emit_sync(now.clone());
                }
                if minute_changed && signals.minute_changed.handler_count() > 0 {
                    signals.minute_changed.emit_sync(now.clone());
                }
                if hour_changed && signals.hour_changed.handler_count() > 0 {
                    signals.hour_changed.emit_sync(now.clone());
                }
                if day_changed && signals.day_changed.handler_count() > 0 {
                    signals.day_changed.emit_sync(now.clone());
                }
                if week_changed && signals.week_changed.handler_count() > 0 {
                    signals.week_changed.emit_sync(now.clone());
                }
                if month_changed && signals.month_changed.handler_count() > 0 {
                    signals.month_changed.emit_sync(now.clone());
                }
                if year_changed && signals.year_changed.handler_count() > 0 {
                    signals.year_changed.emit_sync(now);
                }
            }
        });
    }

    pub fn subscribe<F>(&self, unit: TimeUnit, callback: F) -> SignalHandler
    where
        F: Fn(DateTime<Local>) + Send + 'static,
    {
        match unit {
            TimeUnit::Second => self.signals.second_changed.connect(callback),
            TimeUnit::Minute => self.signals.minute_changed.connect(callback),
            TimeUnit::Hour => self.signals.hour_changed.connect(callback),
            TimeUnit::Day => self.signals.day_changed.connect(callback),
            TimeUnit::Week => self.signals.week_changed.connect(callback),
            TimeUnit::Month => self.signals.month_changed.connect(callback),
            TimeUnit::Year => self.signals.year_changed.connect(callback),
        }
    }
}

/// Unix timestamp of the next granularity boundary the service must wake
/// on, derived from the finest active subscription: second subscribers need
/// a tick every second, minute subscribers every (local) minute, and day /
/// week / month / year subscribers all resolve at the next local midnight.
fn next_boundary(signals: &TimeSignals, now: &DateTime<Local>) -> i64 {
    if signals.second_changed.handler_count() > 0 {
        now.timestamp() + 1
    } else if signals.minute_changed.handler_count() > 0 {
        let seconds_into_minute = i64::from(now.timestamp() % 60);
        now.timestamp() + (60 - seconds_into_minute)
    } else if signals.hour_changed.handler_count() > 0 {
        // Local hour boundary: the offset only shifts the epoch by whole
        // minutes, so counting seconds within the hour is exact.
        let seconds_into_hour = i64::from(now.minute()) * 60 + i64::from(now.second());
        now.timestamp() + (3600 - seconds_into_hour)
    } else {
        // Day-granularity wake covers day, week, month and year changes.
        let tomorrow = now
            .date_naive()
            .succ_opt()
            .unwrap_or_else(|| now.date_naive());
        Local
            .with_ymd_and_hms(tomorrow.year(), tomorrow.month(), tomorrow.day(), 0, 0, 0)
            .earliest()
            .map(|midnight| midnight.timestamp())
            .unwrap_or_else(|| now.timestamp() + 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32, minute: u32, second: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 10, 26, hour, minute, second)
            .single()
            .expect("valid local time")
    }

    #[test]
    fn next_boundary_tracks_finest_subscription() {
        let service = TimeService::new();
        let now = at(9, 40, 30);

        // No subscribers: wake at the next local midnight.
        let midnight = Local
            .with_ymd_and_hms(2026, 10, 27, 0, 0, 0)
            .single()
            .unwrap();
        assert_eq!(next_boundary(&service.signals, &now), midnight.timestamp());

        let _minute = service.signals.minute_changed.connect(|_| {});
        let next_minute = at(9, 41, 0);
        assert_eq!(
            next_boundary(&service.signals, &now),
            next_minute.timestamp()
        );

        let _second = service.signals.second_changed.connect(|_| {});
        assert_eq!(
            next_boundary(&service.signals, &now),
            now.timestamp() + 1
        );
    }

    #[test]
    fn hour_boundary_uses_local_hour() {
        let service = TimeService::new();
        let _hour = service.signals.hour_changed.connect(|_| {});
        let now = at(9, 40, 30);
        let next_hour = at(10, 0, 0);
        assert_eq!(
            next_boundary(&service.signals, &now),
            next_hour.timestamp()
        );
    }
}
