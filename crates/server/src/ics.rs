//! Minimal RFC 5545 (iCalendar) `VEVENT` parser.
//!
//! Each parsed event is translated straight into a
//! `jmap_client::jscalendar::CalendarEvent` tagged with a synthetic
//! "ics:<subscription-id>" pseudo calendar id, so the existing
//! recurrence-expansion and rendering pipeline (`view::localize_events`,
//! `to_event_view`, the sidebar visibility toggle, …) applies to
//! subscribed calendars completely unchanged — the same trick already used
//! for the Birthdays pseudo-calendar.
//!
//! Deliberately narrow, covering what public "read-only" feeds (holiday
//! calendars, calendar exports) actually use: `SUMMARY`/`UID`/`DTSTART`/
//! `DTEND`/`DURATION`, a `RRULE` subset (`FREQ`/`INTERVAL`/`COUNT`/`UNTIL`/
//! `BYDAY`/`BYMONTHDAY`/`BYMONTH`), and `EXDATE`. `VTIMEZONE` blocks are
//! ignored — a `DTSTART` with a `TZID` parameter but no trailing `Z` is
//! treated as floating, matching how the rest of the app already displays
//! zoneless JSCalendar times. `VALARM`, `ATTENDEE`, and per-instance
//! `RECURRENCE-ID` overrides are skipped entirely.

use std::collections::BTreeMap;

use chrono::{NaiveDate, NaiveDateTime};
use jmap_client::duration::{format_duration, parse_duration};
use jmap_client::jscalendar::{
    CalendarEvent, Frequency, LocalDateTime, NDay, RecurrenceRule, Weekday,
};

/// Prefix marking a `ViewParams` calendar id as an ICS subscription rather
/// than a real JMAP calendar or the Birthdays pseudo-calendar.
pub const ICS_PSEUDO_PREFIX: &str = "ics:";

pub fn pseudo_calendar_id(subscription_id: &str) -> String {
    format!("{ICS_PSEUDO_PREFIX}{subscription_id}")
}

pub fn is_ics_pseudo_id(calendar_id: &str) -> bool {
    calendar_id.starts_with(ICS_PSEUDO_PREFIX)
}

/// Parses every `VEVENT` in an iCalendar document into `CalendarEvent`s
/// belonging to `subscription_id`'s pseudo calendar.
pub fn parse_events(ics_text: &str, subscription_id: &str) -> Vec<CalendarEvent> {
    let calendar_id = pseudo_calendar_id(subscription_id);
    let mut events = Vec::new();
    let mut current: Option<RawEvent> = None;

    for line in unfold(ics_text) {
        let Some((name, params, value)) = parse_line(&line) else {
            continue;
        };
        match name.as_str() {
            "BEGIN" if value == "VEVENT" => current = Some(RawEvent::default()),
            "END" if value == "VEVENT" => {
                if let Some(raw) = current.take() {
                    if let Some(event) = raw.into_event(&calendar_id) {
                        events.push(event);
                    }
                }
            }
            _ => {
                if let Some(raw) = current.as_mut() {
                    raw.set(&name, &params, &value);
                }
            }
        }
    }
    events
}

pub(crate) type Params = Vec<(String, String)>;

#[derive(Default)]
struct RawEvent {
    uid: Option<String>,
    summary: Option<String>,
    dtstart: Option<(String, Params)>,
    dtend: Option<(String, Params)>,
    duration: Option<String>,
    rrule: Option<String>,
    exdates: Vec<(String, Params)>,
}

impl RawEvent {
    fn set(&mut self, name: &str, params: &[(String, String)], value: &str) {
        match name {
            "UID" => self.uid = Some(unescape_text(value)),
            "SUMMARY" => self.summary = Some(unescape_text(value)),
            "DTSTART" => self.dtstart = Some((value.to_string(), params.to_vec())),
            "DTEND" => self.dtend = Some((value.to_string(), params.to_vec())),
            "DURATION" => self.duration = Some(value.to_string()),
            "RRULE" => self.rrule = Some(value.to_string()),
            "EXDATE" => {
                for v in value.split(',') {
                    self.exdates.push((v.to_string(), params.to_vec()));
                }
            }
            _ => {}
        }
    }

    fn into_event(self, calendar_id: &str) -> Option<CalendarEvent> {
        let (dtstart_raw, dtstart_params) = self.dtstart?;
        let (start, all_day, tz) = parse_datetime(&dtstart_raw, &dtstart_params)?;
        let uid = self.uid.unwrap_or_else(|| {
            format!(
                "anon-{dtstart_raw}-{}",
                self.summary.clone().unwrap_or_default()
            )
        });

        let mut event = CalendarEvent::new(
            format!("{calendar_id}:{uid}"),
            LocalDateTime::from_naive(start),
        );
        event.title = self.summary;
        event.show_without_time = all_day;
        event.time_zone = tz;
        let mut calendar_ids = BTreeMap::new();
        calendar_ids.insert(calendar_id.to_string(), true);
        event.calendar_ids = Some(calendar_ids);

        let duration = if let Some(dur_str) = &self.duration {
            parse_duration(dur_str).unwrap_or_default()
        } else if let Some((dtend_raw, dtend_params)) = &self.dtend {
            parse_datetime(dtend_raw, dtend_params)
                .map(|(end, _, _)| end - start)
                .unwrap_or_default()
        } else if all_day {
            chrono::Duration::days(1)
        } else {
            chrono::Duration::zero()
        };
        event.duration = format_duration(duration);

        if let Some(rrule_str) = &self.rrule {
            // An unsupported RRULE shape leaves the event non-recurring
            // (shown once at DTSTART) rather than mis-expanding it.
            if let Some(rule) = parse_rrule(rrule_str) {
                event.recurrence_rules = Some(vec![rule]);
            }
        }

        if !self.exdates.is_empty() {
            let mut overrides = BTreeMap::new();
            for (raw, params) in &self.exdates {
                if let Some((d, _, _)) = parse_datetime(raw, params) {
                    let mut patch = BTreeMap::new();
                    patch.insert("excluded".to_string(), serde_json::json!(true));
                    overrides.insert(LocalDateTime::from_naive(d), patch);
                }
            }
            if !overrides.is_empty() {
                event.recurrence_overrides = Some(overrides);
            }
        }

        Some(event)
    }
}

/// Returns `(local wall-clock time, all_day, timeZone)` for a `DTSTART`,
/// `DTEND`, `EXDATE`, or `RRULE` `UNTIL` value.
fn parse_datetime(
    raw: &str,
    params: &[(String, String)],
) -> Option<(NaiveDateTime, bool, Option<String>)> {
    let is_date_value = params.iter().any(|(k, v)| k == "VALUE" && v == "DATE")
        || (raw.len() == 8 && !raw.contains('T'));
    if is_date_value {
        let date = NaiveDate::parse_from_str(raw, "%Y%m%d").ok()?;
        return Some((date.and_hms_opt(0, 0, 0)?, true, None));
    }
    let (raw_dt, is_utc) = match raw.strip_suffix('Z') {
        Some(stripped) => (stripped, true),
        None => (raw, false),
    };
    let naive = NaiveDateTime::parse_from_str(raw_dt, "%Y%m%dT%H%M%S").ok()?;
    let tz = is_utc.then(|| "UTC".to_string());
    Some((naive, false, tz))
}

fn parse_rrule(s: &str) -> Option<RecurrenceRule> {
    let mut frequency = None;
    let mut interval = None;
    let mut count = None;
    let mut until = None;
    let mut by_day = Vec::new();
    let mut by_month_day = Vec::new();
    let mut by_month = Vec::new();
    let mut unsupported = false;

    for part in s.split(';') {
        let mut kv = part.splitn(2, '=');
        let key = kv.next()?.to_uppercase();
        let val = kv.next()?;
        match key.as_str() {
            "FREQ" => {
                frequency = match val {
                    "DAILY" => Some(Frequency::Daily),
                    "WEEKLY" => Some(Frequency::Weekly),
                    "MONTHLY" => Some(Frequency::Monthly),
                    "YEARLY" => Some(Frequency::Yearly),
                    "HOURLY" => Some(Frequency::Hourly),
                    "MINUTELY" => Some(Frequency::Minutely),
                    "SECONDLY" => Some(Frequency::Secondly),
                    _ => None,
                }
            }
            "INTERVAL" => interval = val.parse().ok(),
            "COUNT" => count = val.parse().ok(),
            "UNTIL" => match parse_datetime(val, &[]) {
                Some((dt, _, _)) => until = Some(LocalDateTime::from_naive(dt)),
                None => unsupported = true,
            },
            "BYDAY" => {
                for item in val.split(',') {
                    match parse_byday(item) {
                        Some(nday) => by_day.push(nday),
                        None => unsupported = true,
                    }
                }
            }
            "BYMONTHDAY" => {
                for item in val.split(',') {
                    if let Ok(n) = item.parse::<i32>() {
                        by_month_day.push(n);
                    }
                }
            }
            "BYMONTH" => {
                for item in val.split(',') {
                    by_month.push(item.to_string());
                }
            }
            "BYSETPOS" | "BYWEEKNO" | "BYYEARDAY" | "BYHOUR" | "BYMINUTE" | "BYSECOND" => {
                unsupported = true;
            }
            _ => {}
        }
    }

    if unsupported {
        return None;
    }

    Some(RecurrenceRule {
        type_: "RecurrenceRule".to_string(),
        frequency: frequency?,
        interval,
        count,
        until,
        by_day,
        by_month_day,
        by_month,
        by_set_position: Vec::new(),
        extra: BTreeMap::new(),
    })
}

fn parse_byday(item: &str) -> Option<NDay> {
    let idx = item.find(|c: char| c.is_ascii_alphabetic())?;
    let (num_part, day_part) = item.split_at(idx);
    let nth_of_period = if num_part.is_empty() {
        None
    } else {
        num_part.parse::<i32>().ok()
    };
    let day = match day_part {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    };
    Some(NDay {
        type_: "NDay".to_string(),
        day,
        nth_of_period,
    })
}

pub(crate) fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Un-folds RFC 5545 line continuations (a line starting with a single
/// space or tab is a continuation of the previous line) and normalizes
/// line endings.
pub(crate) fn unfold(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = Vec::new();
    for raw_line in normalized.split('\n') {
        if (raw_line.starts_with(' ') || raw_line.starts_with('\t')) && !lines.is_empty() {
            if let Some(last) = lines.last_mut() {
                last.push_str(&raw_line[1..]);
            }
        } else if !raw_line.is_empty() {
            lines.push(raw_line.to_string());
        }
    }
    lines
}

/// Splits a content line into `(NAME, params, VALUE)`, e.g.
/// `DTSTART;VALUE=DATE:20260101` -> `("DTSTART", [("VALUE","DATE")], "20260101")`.
pub(crate) fn parse_line(line: &str) -> Option<(String, Params, String)> {
    let mut in_quotes = false;
    let mut colon_idx = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon_idx = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon_idx = colon_idx?;
    let (head, value) = (&line[..colon_idx], &line[colon_idx + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.to_uppercase();
    let params = parts
        .filter_map(|p| {
            let mut kv = p.splitn(2, '=');
            let k = kv.next()?.to_uppercase();
            let v = kv.next().unwrap_or("").trim_matches('"').to_string();
            Some((k, v))
        })
        .collect();
    Some((name, params, value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_timed_event() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:evt1@example.com\r\nSUMMARY:Standup\r\nDTSTART:20260715T090000Z\r\nDTEND:20260715T091500Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let events = parse_events(ics, "sub1");
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.title.as_deref(), Some("Standup"));
        assert!(!e.show_without_time);
        assert_eq!(e.time_zone.as_deref(), Some("UTC"));
        assert_eq!(e.duration, "PT15M");
        assert_eq!(
            e.calendar_ids.as_ref().unwrap().keys().next().unwrap(),
            "ics:sub1"
        );
    }

    #[test]
    fn parses_all_day_event_with_folded_summary() {
        let ics = "BEGIN:VEVENT\r\nUID:holiday1\r\nSUMMARY:A very long holiday name that \r\n gets folded across lines\r\nDTSTART;VALUE=DATE:20261225\r\nEND:VEVENT\r\n";
        let events = parse_events(ics, "sub1");
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert!(e.show_without_time);
        assert_eq!(
            e.title.as_deref(),
            Some("A very long holiday name that gets folded across lines")
        );
        assert_eq!(e.duration, "P1D");
    }

    #[test]
    fn parses_weekly_rrule_with_byday() {
        let ics = "BEGIN:VEVENT\r\nUID:recur1\r\nSUMMARY:Weekly sync\r\nDTSTART:20260706T140000\r\nDTEND:20260706T150000\r\nRRULE:FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE;COUNT=5\r\nEND:VEVENT\r\n";
        let events = parse_events(ics, "sub1");
        let rules = events[0].recurrence_rules.as_ref().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].frequency, Frequency::Weekly);
        assert_eq!(rules[0].count, Some(5));
        assert_eq!(rules[0].by_day.len(), 2);
    }

    #[test]
    fn unsupported_rrule_shape_falls_back_to_single_occurrence() {
        let ics = "BEGIN:VEVENT\r\nUID:complex1\r\nSUMMARY:Odd rule\r\nDTSTART:20260706T140000\r\nRRULE:FREQ=MONTHLY;BYSETPOS=-1;BYDAY=SU\r\nEND:VEVENT\r\n";
        let events = parse_events(ics, "sub1");
        assert!(events[0].recurrence_rules.is_none());
    }

    #[test]
    fn exdate_becomes_an_excluded_override() {
        let ics = "BEGIN:VEVENT\r\nUID:recur2\r\nSUMMARY:Daily\r\nDTSTART:20260706T090000\r\nRRULE:FREQ=DAILY;COUNT=5\r\nEXDATE:20260708T090000\r\nEND:VEVENT\r\n";
        let events = parse_events(ics, "sub1");
        let overrides = events[0].recurrence_overrides.as_ref().unwrap();
        assert_eq!(overrides.len(), 1);
    }
}
