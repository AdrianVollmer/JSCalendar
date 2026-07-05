//! Expands a JSCalendar `Event`'s `recurrenceRules` (RFC 8984 §4.3.3) into
//! concrete occurrences intersecting a display range, applying
//! `recurrenceOverrides` patches along the way.
//!
//! This covers the recurrence patterns that show up in practice — plain
//! daily/weekly/monthly/yearly intervals, `byDay` weekday lists, `byDay`
//! with `nthOfPeriod` ("2nd Tuesday"), `byMonthDay`, and `byMonth` — rather
//! than the full RFC 5545-derived grammar (no `bySetPosition` combined with
//! multiple `byDay` rules, no `byWeekNo`/`byYearDay`).

use chrono::{Datelike, Months, NaiveDate, NaiveDateTime, Weekday};

use crate::duration::parse_duration;
use crate::jscalendar::{CalendarEvent, Frequency, LocalDateTime, NDay};

const MAX_OCCURRENCES: usize = 2000;

pub struct Occurrence {
    pub start: NaiveDateTime,
    pub recurrence_id: Option<LocalDateTime>,
    pub event: CalendarEvent,
}

pub fn expand(
    event: &CalendarEvent,
    range_start: NaiveDateTime,
    range_end: NaiveDateTime,
) -> Vec<Occurrence> {
    let Some(dtstart) = event.start.to_naive() else {
        return Vec::new();
    };
    let duration = parse_duration(&event.duration).unwrap_or_default();

    let mut base_starts: Vec<NaiveDateTime> = match &event.recurrence_rules {
        None => {
            if overlaps(dtstart, duration, range_start, range_end) {
                vec![dtstart]
            } else {
                vec![]
            }
        }
        Some(rules) => {
            let mut all = Vec::new();
            for rule in rules {
                all.extend(expand_rule(rule, dtstart, range_start, range_end));
            }
            all.sort();
            all.dedup();
            all
        }
    };

    let overrides = event.recurrence_overrides.clone().unwrap_or_default();

    // Ad-hoc extra instances declared purely via an override (not produced
    // by any rule) also count, unless they are the `excluded` marker.
    for key in overrides.keys() {
        if let Some(d) = key.to_naive() {
            if !base_starts.contains(&d) && d >= range_start && d < range_end {
                let is_excluded_only = overrides
                    .get(key)
                    .and_then(|p| p.get("excluded"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !is_excluded_only {
                    base_starts.push(d);
                }
            }
        }
    }
    base_starts.sort();

    let mut out = Vec::new();
    for start in base_starts {
        let key = LocalDateTime::from_naive(start);
        let patch = overrides.get(&key);

        if let Some(p) = patch {
            if p.get("excluded").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
        }

        let effective_start = patch
            .and_then(|p| p.get("start"))
            .and_then(|v| v.as_str())
            .map(|s| LocalDateTime(s.to_string()))
            .unwrap_or_else(|| LocalDateTime::from_naive(start));
        let Some(effective_naive) = effective_start.to_naive() else {
            continue;
        };
        let effective_duration = patch
            .and_then(|p| p.get("duration"))
            .and_then(|v| v.as_str())
            .and_then(parse_duration)
            .unwrap_or(duration);

        if !overlaps(effective_naive, effective_duration, range_start, range_end) {
            continue;
        }

        let mut occ_event = event.clone();
        occ_event.start = effective_start.clone();
        occ_event.recurrence_id = Some(key.clone());
        occ_event.recurrence_rules = None;
        occ_event.recurrence_overrides = None;
        if let Some(p) = patch {
            if let Some(title) = p.get("title").and_then(|v| v.as_str()) {
                occ_event.title = Some(title.to_string());
            }
            if let Some(desc) = p.get("description").and_then(|v| v.as_str()) {
                occ_event.description = Some(desc.to_string());
            }
            if let Some(status) = p.get("status").and_then(|v| v.as_str()) {
                occ_event.status = Some(status.to_string());
            }
            if let Some(color) = p.get("color").and_then(|v| v.as_str()) {
                occ_event.color = Some(color.to_string());
            }
        }
        occ_event.duration = crate::duration::format_duration(effective_duration);

        out.push(Occurrence {
            start: effective_naive,
            recurrence_id: Some(key),
            event: occ_event,
        });
    }
    out
}

fn overlaps(
    start: NaiveDateTime,
    duration: chrono::Duration,
    range_start: NaiveDateTime,
    range_end: NaiveDateTime,
) -> bool {
    let end = start + duration.max(chrono::Duration::zero());
    start < range_end && end > range_start
}

fn expand_rule(
    rule: &crate::jscalendar::RecurrenceRule,
    dtstart: NaiveDateTime,
    range_start: NaiveDateTime,
    range_end: NaiveDateTime,
) -> Vec<NaiveDateTime> {
    let interval = rule.interval.unwrap_or(1).max(1) as i32;
    let until = rule.until.as_ref().and_then(|u| u.to_naive());
    let count = rule.count;
    let time = dtstart.time();

    let mut candidates: Box<dyn Iterator<Item = NaiveDate>> = match rule.frequency {
        Frequency::Daily => Box::new(daily_dates(dtstart.date(), interval)),
        Frequency::Weekly => Box::new(weekly_dates(dtstart.date(), interval, &rule.by_day)),
        Frequency::Monthly => Box::new(monthly_dates(
            dtstart.date(),
            interval,
            &rule.by_day,
            &rule.by_month_day,
        )),
        Frequency::Yearly => Box::new(yearly_dates(
            dtstart.date(),
            interval,
            &rule.by_month,
            &rule.by_month_day,
            &rule.by_day,
        )),
        // Sub-daily frequencies are rare for calendar UIs; treat as a no-op
        // beyond the single dtstart occurrence.
        Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => {
            Box::new(std::iter::once(dtstart.date()))
        }
    };

    let mut out = Vec::new();
    let mut produced = 0usize;
    for date in candidates.by_ref() {
        if date < dtstart.date() {
            continue;
        }
        let dt = date.and_time(time);
        if let Some(u) = until {
            if dt > u {
                break;
            }
        }
        produced += 1;
        if let Some(c) = count {
            if produced > c as usize {
                break;
            }
        }
        if dt >= range_start && dt < range_end {
            out.push(dt);
        }
        if dt >= range_end && until.is_none() && count.is_none() {
            break;
        }
        if out.len() >= MAX_OCCURRENCES || produced >= MAX_OCCURRENCES {
            break;
        }
    }
    out
}

fn daily_dates(start: NaiveDate, interval: i32) -> impl Iterator<Item = NaiveDate> {
    (0i64..).map(move |n| start + chrono::Duration::days(n * interval as i64))
}

fn weekly_dates(start: NaiveDate, interval: i32, by_day: &[NDay]) -> impl Iterator<Item = NaiveDate> {
    let days: Vec<Weekday> = if by_day.is_empty() {
        vec![start.weekday()]
    } else {
        by_day.iter().map(|d| to_chrono_weekday(d.day)).collect()
    };
    let week_start = start - chrono::Duration::days(start.weekday().num_days_from_monday() as i64);
    (0i64..).flat_map(move |week| {
        let base = week_start + chrono::Duration::weeks(week * interval as i64);
        let mut dates: Vec<NaiveDate> = days
            .iter()
            .map(|wd| base + chrono::Duration::days(wd.num_days_from_monday() as i64))
            .collect();
        dates.sort();
        dates.into_iter()
    })
}

fn monthly_dates(
    start: NaiveDate,
    interval: i32,
    by_day: &[NDay],
    by_month_day: &[i32],
) -> impl Iterator<Item = NaiveDate> {
    let by_day = by_day.to_vec();
    let by_month_day = by_month_day.to_vec();
    let start_day = start.day();
    (0u32..).filter_map(move |n| {
        let month_date = start.checked_add_months(Months::new(n * interval as u32))?;
        let year = month_date.year();
        let month = month_date.month();
        if !by_day.is_empty() {
            // Only single-nth-per-rule is supported; if multiple entries
            // are given, emit all of them for that month.
            return None.or_else(|| {
                let dates: Vec<NaiveDate> = by_day
                    .iter()
                    .filter_map(|nd| {
                        nth_weekday_of_month(year, month, to_chrono_weekday(nd.day), nd.nth_of_period.unwrap_or(1))
                    })
                    .collect();
                Some(dates)
            });
        }
        if !by_month_day.is_empty() {
            let dates: Vec<NaiveDate> = by_month_day
                .iter()
                .filter_map(|&d| nth_day_of_month(year, month, d))
                .collect();
            return Some(dates);
        }
        Some(NaiveDate::from_ymd_opt(year, month, start_day).into_iter().collect())
    })
    .flat_map(|v| v.into_iter())
}

fn yearly_dates(
    start: NaiveDate,
    interval: i32,
    by_month: &[String],
    by_month_day: &[i32],
    by_day: &[NDay],
) -> impl Iterator<Item = NaiveDate> {
    let by_month = by_month.to_vec();
    let by_month_day = by_month_day.to_vec();
    let by_day = by_day.to_vec();
    let start_month = start.month();
    let start_day = start.day();
    (0u32..).flat_map(move |n| {
        let year = start.year() + (n as i32) * interval;
        let months: Vec<u32> = if by_month.is_empty() {
            vec![start_month]
        } else {
            by_month.iter().filter_map(|m| m.parse::<u32>().ok()).collect()
        };
        let mut dates = Vec::new();
        for month in months {
            if !by_day.is_empty() {
                for nd in &by_day {
                    if let Some(d) =
                        nth_weekday_of_month(year, month, to_chrono_weekday(nd.day), nd.nth_of_period.unwrap_or(1))
                    {
                        dates.push(d);
                    }
                }
            } else if !by_month_day.is_empty() {
                for &d in &by_month_day {
                    if let Some(date) = nth_day_of_month(year, month, d) {
                        dates.push(date);
                    }
                }
            } else if let Some(d) = NaiveDate::from_ymd_opt(year, month, start_day) {
                dates.push(d);
            }
        }
        dates.into_iter()
    })
}

fn nth_day_of_month(year: i32, month: u32, day: i32) -> Option<NaiveDate> {
    if day > 0 {
        NaiveDate::from_ymd_opt(year, month, day as u32)
    } else {
        let first_next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let last = first_next.pred_opt()?;
        let offset = (-day - 1) as i64;
        Some(last - chrono::Duration::days(offset))
    }
}

fn nth_weekday_of_month(year: i32, month: u32, weekday: Weekday, nth: i32) -> Option<NaiveDate> {
    if nth > 0 {
        let first = NaiveDate::from_ymd_opt(year, month, 1)?;
        let offset = (7 + weekday.num_days_from_monday() as i64 - first.weekday().num_days_from_monday() as i64) % 7;
        let candidate = first + chrono::Duration::days(offset + 7 * (nth as i64 - 1));
        if candidate.month() == month {
            Some(candidate)
        } else {
            None
        }
    } else {
        let first_next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let last = first_next.pred_opt()?;
        let offset = (7 + last.weekday().num_days_from_monday() as i64 - weekday.num_days_from_monday() as i64) % 7;
        let candidate = last - chrono::Duration::days(offset + 7 * (-nth as i64 - 1));
        if candidate.month() == month {
            Some(candidate)
        } else {
            None
        }
    }
}

fn to_chrono_weekday(w: crate::jscalendar::Weekday) -> Weekday {
    use crate::jscalendar::Weekday as J;
    match w {
        J::Monday => Weekday::Mon,
        J::Tuesday => Weekday::Tue,
        J::Wednesday => Weekday::Wed,
        J::Thursday => Weekday::Thu,
        J::Friday => Weekday::Fri,
        J::Saturday => Weekday::Sat,
        J::Sunday => Weekday::Sun,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jscalendar::RecurrenceRule;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap()
    }

    #[test]
    fn non_recurring_single_occurrence() {
        let event = CalendarEvent::new("u1", LocalDateTime::from_naive(dt("2026-07-06T09:00:00")));
        let occs = expand(&event, dt("2026-07-01T00:00:00"), dt("2026-08-01T00:00:00"));
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].start, dt("2026-07-06T09:00:00"));
    }

    #[test]
    fn daily_interval_two_with_count() {
        let mut event = CalendarEvent::new("u1", LocalDateTime::from_naive(dt("2026-07-01T09:00:00")));
        event.recurrence_rules = Some(vec![RecurrenceRule {
            frequency: Frequency::Daily,
            interval: Some(2),
            count: Some(5),
            ..Default::default()
        }]);
        let occs = expand(&event, dt("2026-07-01T00:00:00"), dt("2026-07-31T00:00:00"));
        let dates: Vec<_> = occs.iter().map(|o| o.start.date()).collect();
        assert_eq!(
            dates,
            vec![
                dt("2026-07-01T00:00:00").date(),
                dt("2026-07-03T00:00:00").date(),
                dt("2026-07-05T00:00:00").date(),
                dt("2026-07-07T00:00:00").date(),
                dt("2026-07-09T00:00:00").date(),
            ]
        );
    }

    #[test]
    fn weekly_by_day() {
        use crate::jscalendar::Weekday as J;
        // 2026-07-06 is a Monday.
        let mut event = CalendarEvent::new("u1", LocalDateTime::from_naive(dt("2026-07-06T10:00:00")));
        event.recurrence_rules = Some(vec![RecurrenceRule {
            frequency: Frequency::Weekly,
            by_day: vec![
                NDay { type_: "NDay".into(), day: J::Monday, nth_of_period: None },
                NDay { type_: "NDay".into(), day: J::Wednesday, nth_of_period: None },
                NDay { type_: "NDay".into(), day: J::Friday, nth_of_period: None },
            ],
            ..Default::default()
        }]);
        let occs = expand(&event, dt("2026-07-06T00:00:00"), dt("2026-07-20T00:00:00"));
        assert_eq!(occs.len(), 6);
        assert_eq!(occs[0].start, dt("2026-07-06T10:00:00"));
        assert_eq!(occs[1].start, dt("2026-07-08T10:00:00"));
        assert_eq!(occs[2].start, dt("2026-07-10T10:00:00"));
    }

    #[test]
    fn monthly_nth_weekday() {
        use crate::jscalendar::Weekday as J;
        // 2nd Tuesday of each month, starting 2026-01-13 (which is the 2nd Tuesday of Jan 2026).
        let mut event = CalendarEvent::new("u1", LocalDateTime::from_naive(dt("2026-01-13T14:00:00")));
        event.recurrence_rules = Some(vec![RecurrenceRule {
            frequency: Frequency::Monthly,
            by_day: vec![NDay { type_: "NDay".into(), day: J::Tuesday, nth_of_period: Some(2) }],
            count: Some(3),
            ..Default::default()
        }]);
        let occs = expand(&event, dt("2026-01-01T00:00:00"), dt("2026-05-01T00:00:00"));
        let dates: Vec<_> = occs.iter().map(|o| o.start.date()).collect();
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 1, 13).unwrap(),
                NaiveDate::from_ymd_opt(2026, 2, 10).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            ]
        );
    }

    #[test]
    fn recurrence_override_excludes_and_reschedules() {
        let mut event = CalendarEvent::new("u1", LocalDateTime::from_naive(dt("2026-07-06T09:00:00")));
        event.recurrence_rules = Some(vec![RecurrenceRule {
            frequency: Frequency::Daily,
            count: Some(3),
            ..Default::default()
        }]);
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert(
            LocalDateTime("2026-07-07T09:00:00".into()),
            [("excluded".to_string(), serde_json::json!(true))].into_iter().collect(),
        );
        overrides.insert(
            LocalDateTime("2026-07-08T09:00:00".into()),
            [("start".to_string(), serde_json::json!("2026-07-08T15:00:00"))].into_iter().collect(),
        );
        event.recurrence_overrides = Some(overrides);

        let occs = expand(&event, dt("2026-07-01T00:00:00"), dt("2026-07-31T00:00:00"));
        let starts: Vec<_> = occs.iter().map(|o| o.start).collect();
        assert_eq!(
            starts,
            vec![dt("2026-07-06T09:00:00"), dt("2026-07-08T15:00:00")]
        );
    }
}
