//! News calendar provider trait (P2.16 fix).
//!
//! The binding spec requires a live news calendar feed (red/orange
//! folders, per-symbol impact), configurable window, and honoring
//! `allow_closes`. The previous implementation hardcoded 5 events
//! (`builtin_events()`) with a 2-minute window — illustrative only.
//!
//! This module defines the [`NewsCalendarProvider`] trait so a
//! production deployment can swap in a real calendar feed (e.g.
//! FinancialJuice, ForexFactory, or an internal Bloomberg/Reuters
//! feed) without touching the rule itself.

use crate::core::types::Timestamp;

/// Impact level of a news event. Matches the binding spec's
/// `impact: red | orange | yellow | gray` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NewsImpact {
    /// Low impact — typically not tradable.
    Gray,
    /// Medium impact — caution.
    Yellow,
    /// High impact — restrict trading within the window.
    Orange,
    /// Critical impact — hard restrict trading within the window.
    /// (e.g. NFP, FOMC, CPI.)
    Red,
}

impl std::fmt::Display for NewsImpact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NewsImpact::Gray => write!(f, "gray"),
            NewsImpact::Yellow => write!(f, "yellow"),
            NewsImpact::Orange => write!(f, "orange"),
            NewsImpact::Red => write!(f, "red"),
        }
    }
}

/// A single news event from the calendar.
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    /// Event name (e.g. "Non-Farm Payrolls", "FOMC Rate Decision").
    pub name: String,
    /// ISO 4217 currency the event affects (e.g. "USD", "EUR").
    /// None means all-currencies (rare; e.g. geopolitical events).
    pub currency: Option<String>,
    /// Scheduled release time (UTC).
    pub at: Timestamp,
    /// Impact level.
    pub impact: NewsImpact,
    /// Actual value if released (e.g. "215K", "+0.4%"). None if
    /// upcoming/forecast only.
    pub actual: Option<String>,
    /// Forecast value.
    pub forecast: Option<String>,
    /// Previous value.
    pub previous: Option<String>,
    /// List of symbols most affected (e.g. ["EURUSD", "USDJPY"]).
    /// None means "all USD-quoted symbols".
    pub affected_symbols: Option<Vec<String>>,
}

impl CalendarEvent {
    /// Returns true if this event falls within `window_minutes` of `ts`
    /// (either side). Used by the news trading rule.
    pub fn within_window(&self, ts: Timestamp, window_minutes: i64) -> bool {
        let delta = (ts - self.at).num_minutes().abs();
        delta <= window_minutes
    }

    /// Returns true if `symbol` is in the affected-symbols list, OR
    /// if the affected list is None (all symbols).
    pub fn affects_symbol(&self, symbol: &str) -> bool {
        match &self.affected_symbols {
            None => true,
            Some(syms) => syms.iter().any(|s| s.eq_ignore_ascii_case(symbol)),
        }
    }
}

/// Trait for news calendar providers. Production implementations fetch
/// from a live feed (Bloomberg/Reuters/ForexFactory). The default
/// implementation is a small built-in static list — illustrative only.
pub trait NewsCalendarProvider: Send + Sync {
    /// Returns all events within `window_minutes` of `ts` (either side).
    /// Filtered by `impact >= min_impact`.
    fn events_within(&self, ts: Timestamp, window_minutes: i64, min_impact: NewsImpact) -> Vec<CalendarEvent>;

    /// Returns true if `symbol` is tradable at `ts` (i.e. no red/orange
    /// event affecting it within the window).
    fn is_tradable(&self, symbol: &str, ts: Timestamp, window_minutes: i64) -> bool {
        let events = self.events_within(ts, window_minutes, NewsImpact::Orange);
        !events.iter().any(|e| e.affects_symbol(symbol))
    }
}

/// Built-in static news calendar (illustrative only — not for production).
/// Production deployments should swap in a real provider.
#[derive(Debug, Clone, Default)]
pub struct BuiltinCalendar;

impl BuiltinCalendar {
    pub fn new() -> Self { Self }
}

impl NewsCalendarProvider for BuiltinCalendar {
    fn events_within(&self, ts: Timestamp, window_minutes: i64, min_impact: NewsImpact) -> Vec<CalendarEvent> {
        // Static illustrative list — production should fetch from a real feed.
        use chrono::{Datelike, TimeZone, Utc, Weekday, NaiveTime};
        let builtin: &[(&str, Option<&str>, Weekday, u32, u32, NewsImpact, &[&str])] = &[
            ("NFP",              Some("USD"), Weekday::Fri, 12, 30, NewsImpact::Red,    &["EURUSD", "USDJPY", "GBPUSD", "USDCAD"]),
            ("CPI",              Some("USD"), Weekday::Wed, 12, 30, NewsImpact::Red,    &["EURUSD", "USDJPY"]),
            ("FOMC",             Some("USD"), Weekday::Wed, 18, 0,  NewsImpact::Red,    &["EURUSD", "USDJPY", "GBPUSD"]),
            ("ECB Rate",         Some("EUR"), Weekday::Thu, 11, 45, NewsImpact::Red,    &["EURUSD", "EURJPY"]),
            ("BOE Rate",         Some("GBP"), Weekday::Thu, 11, 0,  NewsImpact::Red,    &["GBPUSD", "GBPJPY"]),
        ];
        let mut out: Vec<CalendarEvent> = Vec::new();
        for (name, ccy, weekday, hour, minute, impact, syms) in builtin {
            if *impact < min_impact { continue; }
            // Construct this week's instance.
            let naive_time = match NaiveTime::from_hms_opt(*hour, *minute, 0) {
                Some(t) => t,
                None => continue,
            };
            let today = ts.weekday();
            let event_offset = weekday.num_days_from_monday() as i64;
            let today_offset = today.num_days_from_monday() as i64;
            let day_diff = (event_offset - today_offset).rem_euclid(7);
            let event_date = (ts + chrono::Duration::days(day_diff)).date_naive();
            let event_dt = event_date.and_time(naive_time);
            let event_utc = Utc.from_utc_datetime(&event_dt);
            let delta = (ts - event_utc).num_minutes().abs();
            if delta <= window_minutes {
                out.push(CalendarEvent {
                    name: (*name).to_string(),
                    currency: ccy.map(|s| s.to_string()),
                    at: event_utc,
                    impact: *impact,
                    actual: None,
                    forecast: None,
                    previous: None,
                    affected_symbols: Some(syms.iter().map(|s| s.to_string()).collect()),
                });
            }
        }
        out
    }
}
