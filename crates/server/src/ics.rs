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

/// Serializes a single event back to a standalone iCalendar document (one
/// `VEVENT`), for the "share this event" download/native-share button —
/// the inverse of `parse_events`, and just as narrow: only the first
/// recurrence rule is written (the event editor only ever creates one),
/// and — like the parser — no `VTIMEZONE` block, just a bare `TZID`
/// parameter, which every mainstream calendar app resolves against its own
/// IANA timezone database without complaint.
pub fn to_ics(event: &CalendarEvent) -> String {
    let mut lines: Vec<String> = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//JSCalendar//EN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}", escape_text(&event.uid)),
        format!("DTSTAMP:{}", chrono::Utc::now().format("%Y%m%dT%H%M%SZ")),
    ];

    let start = event.start.to_naive().unwrap_or_default();
    let duration = parse_duration(&event.duration).unwrap_or_default();
    let end = start + duration;
    if event.show_without_time {
        lines.push(format!("DTSTART;VALUE=DATE:{}", start.format("%Y%m%d")));
        if !duration.is_zero() {
            lines.push(format!("DTEND;VALUE=DATE:{}", end.format("%Y%m%d")));
        }
    } else {
        match event.time_zone.as_deref() {
            Some("UTC") | Some("Etc/UTC") => {
                lines.push(format!("DTSTART:{}", start.format("%Y%m%dT%H%M%SZ")));
                if !duration.is_zero() {
                    lines.push(format!("DTEND:{}", end.format("%Y%m%dT%H%M%SZ")));
                }
            }
            Some(tz) => {
                lines.push(format!(
                    "DTSTART;TZID={tz}:{}",
                    start.format("%Y%m%dT%H%M%S")
                ));
                if !duration.is_zero() {
                    lines.push(format!("DTEND;TZID={tz}:{}", end.format("%Y%m%dT%H%M%S")));
                }
            }
            None => {
                lines.push(format!("DTSTART:{}", start.format("%Y%m%dT%H%M%S")));
                if !duration.is_zero() {
                    lines.push(format!("DTEND:{}", end.format("%Y%m%dT%H%M%S")));
                }
            }
        }
    }

    if let Some(title) = event.title.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("SUMMARY:{}", escape_text(title)));
    }
    if let Some(desc) = event.description.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("DESCRIPTION:{}", escape_text(desc)));
    }
    if let Some(loc) = event
        .locations
        .as_ref()
        .and_then(|m| m.values().next())
        .and_then(|l| l.name.as_deref())
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("LOCATION:{}", escape_text(loc)));
    }
    if let Some(rule) = event.recurrence_rules.as_ref().and_then(|r| r.first()) {
        lines.push(format!("RRULE:{}", rrule_to_ics(rule)));
    }

    lines.push("END:VEVENT".to_string());
    lines.push("END:VCALENDAR".to_string());
    lines.iter().map(|l| fold_line(l)).collect()
}

fn rrule_to_ics(rule: &RecurrenceRule) -> String {
    let freq = match rule.frequency {
        Frequency::Daily => "DAILY",
        Frequency::Weekly => "WEEKLY",
        Frequency::Monthly => "MONTHLY",
        Frequency::Yearly => "YEARLY",
        Frequency::Hourly => "HOURLY",
        Frequency::Minutely => "MINUTELY",
        Frequency::Secondly => "SECONDLY",
    };
    let mut parts = vec![format!("FREQ={freq}")];
    if let Some(interval) = rule.interval.filter(|i| *i > 1) {
        parts.push(format!("INTERVAL={interval}"));
    }
    if let Some(count) = rule.count {
        parts.push(format!("COUNT={count}"));
    }
    if let Some(until) = rule.until.as_ref().and_then(|u| u.to_naive()) {
        parts.push(format!("UNTIL={}", until.format("%Y%m%dT%H%M%SZ")));
    }
    if !rule.by_day.is_empty() {
        let days: Vec<String> = rule
            .by_day
            .iter()
            .map(|nd| {
                let day = match nd.day {
                    Weekday::Monday => "MO",
                    Weekday::Tuesday => "TU",
                    Weekday::Wednesday => "WE",
                    Weekday::Thursday => "TH",
                    Weekday::Friday => "FR",
                    Weekday::Saturday => "SA",
                    Weekday::Sunday => "SU",
                };
                match nd.nth_of_period {
                    Some(n) => format!("{n}{day}"),
                    None => day.to_string(),
                }
            })
            .collect();
        parts.push(format!("BYDAY={}", days.join(",")));
    }
    if !rule.by_month_day.is_empty() {
        let days = rule
            .by_month_day
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("BYMONTHDAY={days}"));
    }
    if !rule.by_month.is_empty() {
        parts.push(format!("BYMONTH={}", rule.by_month.join(",")));
    }
    if !rule.by_set_position.is_empty() {
        let positions = rule
            .by_set_position
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("BYSETPOS={positions}"));
    }
    parts.join(";")
}

/// RFC 5545 TEXT escaping: backslash, comma, and semicolon are structural
/// (used for list separators / parameter delimiters), and newlines have no
/// literal representation in a single content line. Shared with
/// `crate::vcard`'s writer — vCard (RFC 6350 §3.4) escapes the same way.
pub(crate) fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

/// Folds a content line to RFC 5545's 75-octet limit: continuation lines
/// are joined with CRLF followed by a single leading space, which readers
/// strip back out (see `unfold`) to reconstruct the original line. Shared
/// with `crate::vcard`'s writer — vCard folds lines the same way.
pub(crate) fn fold_line(line: &str) -> String {
    const LIMIT: usize = 75;
    if line.len() <= LIMIT {
        return format!("{line}\r\n");
    }
    let mut out = String::new();
    let mut chunk_start = 0;
    let mut chunk_len = 0;
    let mut first = true;
    for (i, ch) in line.char_indices() {
        let budget = if first { LIMIT } else { LIMIT - 1 };
        if chunk_len + ch.len_utf8() > budget && i > chunk_start {
            out.push_str(&line[chunk_start..i]);
            out.push_str("\r\n ");
            chunk_start = i;
            chunk_len = 0;
            first = false;
        }
        chunk_len += ch.len_utf8();
    }
    out.push_str(&line[chunk_start..]);
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jmap_client::jscalendar::Location;

    #[test]
    fn to_ics_writes_summary_location_and_timed_range() {
        let mut event = CalendarEvent::new(
            "evt1@example.com",
            LocalDateTime::from_naive(
                NaiveDateTime::parse_from_str("2026-07-15T09:00:00", "%Y-%m-%dT%H:%M:%S").unwrap(),
            ),
        );
        event.title = Some("Standup".to_string());
        event.description = Some("Daily sync".to_string());
        event.time_zone = Some("Europe/Berlin".to_string());
        event.duration = "PT15M".to_string();
        let mut locations = BTreeMap::new();
        locations.insert(
            "loc1".to_string(),
            Location {
                name: Some("Room, 2nd floor".to_string()),
                ..Default::default()
            },
        );
        event.locations = Some(locations);

        let ics = to_ics(&event);
        assert!(ics.contains("BEGIN:VCALENDAR"));
        assert!(ics.contains("UID:evt1@example.com"));
        assert!(ics.contains("SUMMARY:Standup"));
        assert!(ics.contains("DESCRIPTION:Daily sync"));
        assert!(ics.contains("LOCATION:Room\\, 2nd floor"));
        assert!(ics.contains("DTSTART;TZID=Europe/Berlin:20260715T090000"));
        assert!(ics.contains("DTEND;TZID=Europe/Berlin:20260715T091500"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
    }

    #[test]
    fn to_ics_round_trips_through_parse_events() {
        let mut event = CalendarEvent::new(
            "roundtrip1",
            LocalDateTime::from_naive(
                NaiveDateTime::parse_from_str("2026-03-01T14:00:00", "%Y-%m-%dT%H:%M:%S").unwrap(),
            ),
        );
        event.title = Some("Planning".to_string());
        event.duration = "PT30M".to_string();

        let ics = to_ics(&event);
        let parsed = parse_events(&ics, "sub1");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title.as_deref(), Some("Planning"));
        assert_eq!(parsed[0].duration, "PT30M");
    }

    #[test]
    fn to_ics_writes_all_day_and_recurrence() {
        let mut event = CalendarEvent::new(
            "allday1",
            LocalDateTime::from_naive(
                NaiveDateTime::parse_from_str("2026-12-25T00:00:00", "%Y-%m-%dT%H:%M:%S").unwrap(),
            ),
        );
        event.title = Some("Holiday".to_string());
        event.show_without_time = true;
        event.duration = "P1D".to_string();
        event.recurrence_rules = Some(vec![RecurrenceRule {
            type_: "RecurrenceRule".to_string(),
            frequency: Frequency::Yearly,
            interval: None,
            count: None,
            until: None,
            by_day: vec![],
            by_month_day: vec![],
            by_month: vec![],
            by_set_position: vec![],
            extra: BTreeMap::new(),
        }]);

        let ics = to_ics(&event);
        assert!(ics.contains("DTSTART;VALUE=DATE:20261225"));
        // A single-day all-day event still gets an explicit (exclusive)
        // DTEND rather than relying on the implicit one-day default.
        assert!(ics.contains("DTEND;VALUE=DATE:20261226"));
        assert!(ics.contains("RRULE:FREQ=YEARLY"));
    }

    #[test]
    fn escape_text_escapes_special_characters() {
        assert_eq!(escape_text("a, b; c\\d\ne"), "a\\, b\\; c\\\\d\\ne");
    }

    #[test]
    fn fold_line_wraps_long_lines_and_unfolds_cleanly() {
        let long = format!("DESCRIPTION:{}", "x".repeat(200));
        let folded = fold_line(&long);
        assert!(folded.contains("\r\n "));
        let unfolded = unfold(&folded);
        assert_eq!(unfolded, vec![long]);
    }

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
