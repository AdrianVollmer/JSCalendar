use chrono::{Datelike, Duration as ChronoDuration, Months, NaiveDate, NaiveDateTime};
use jmap_client::duration::parse_duration;
use jmap_client::jscalendar::{Calendar, CalendarEvent};
use jmap_client::tz::{self, Tz};
use std::collections::HashSet;
use std::fmt;

pub const HOUR_HEIGHT_PX: f64 = 48.0;
const PALETTE: [&str; 8] = [
    "#6366f1", "#0ea5e9", "#10b981", "#f59e0b", "#ef4444", "#8b5cf6", "#ec4899", "#14b8a6",
];
pub const BIRTHDAY_COLOR: &str = "#f43f5e";
/// Synthetic calendar id used purely for the visibility-toggle machinery —
/// birthdays aren't a real JMAP calendar, but reusing the same "which ids
/// are visible" URL state means no separate toggle plumbing is needed.
pub const BIRTHDAY_PSEUDO_ID: &str = "birthdays";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Month,
    Week,
    Day,
    Agenda,
    Contacts,
}

impl ViewKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ViewKind::Month => "month",
            ViewKind::Week => "week",
            ViewKind::Day => "day",
            ViewKind::Agenda => "agenda",
            ViewKind::Contacts => "contacts",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "week" => ViewKind::Week,
            "day" => ViewKind::Day,
            "agenda" => ViewKind::Agenda,
            "contacts" => ViewKind::Contacts,
            _ => ViewKind::Month,
        }
    }

    /// Whether this view is anchored to `date` (so prev/next/today make
    /// sense) as opposed to a flat list like the contacts view.
    pub fn is_dated(&self) -> bool {
        !matches!(self, ViewKind::Contacts)
    }
}

impl fmt::Display for ViewKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// The parsed, defaulted query-string state that drives every calendar
/// view: which view, which date/window, and which calendars are visible.
/// Fully encoded in the URL so views are bookmarkable/shareable and no
/// server-side UI state is needed beyond the login session.
#[derive(Debug, Clone)]
pub struct ViewParams {
    pub view: ViewKind,
    pub date: NaiveDate,
    pub visible: HashSet<String>,
    pub all_calendar_ids: Vec<String>,
    /// Free-text contacts filter. Only meaningful for `ViewKind::Contacts`,
    /// but lives here so it round-trips through URLs the same way
    /// view/date/cal do.
    pub q: Option<String>,
}

impl ViewParams {
    pub fn query_string(&self, view: ViewKind, date: NaiveDate) -> String {
        let mut s = format!(
            "view={}&date={}&cal={}",
            view.as_str(),
            date.format("%Y-%m-%d"),
            self.cal_param()
        );
        if view == ViewKind::Contacts {
            if let Some(q) = self.q.as_ref().filter(|q| !q.is_empty()) {
                s.push_str(&format!("&q={}", crate::webutil::urlencode(q)));
            }
        }
        s
    }

    pub fn cal_param(&self) -> String {
        self.all_calendar_ids
            .iter()
            .filter(|id| self.visible.contains(*id))
            .cloned()
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn toggle_href(&self, calendar_id: &str) -> String {
        let mut set = self.visible.clone();
        if set.contains(calendar_id) {
            set.remove(calendar_id);
        } else {
            set.insert(calendar_id.to_string());
        }
        let cal = self
            .all_calendar_ids
            .iter()
            .filter(|id| set.contains(*id))
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        let mut href = format!(
            "/app?view={}&date={}&cal={}",
            self.view.as_str(),
            self.date.format("%Y-%m-%d"),
            cal
        );
        if self.view == ViewKind::Contacts {
            if let Some(q) = self.q.as_ref().filter(|q| !q.is_empty()) {
                href.push_str(&format!("&q={}", crate::webutil::urlencode(q)));
            }
        }
        href
    }

    pub fn nav_href(&self, view: ViewKind, date: NaiveDate) -> String {
        format!("/app?{}", self.query_string(view, date))
    }

    pub fn prev_date(&self) -> NaiveDate {
        match self.view {
            ViewKind::Month => self
                .date
                .checked_sub_months(Months::new(1))
                .unwrap_or(self.date),
            ViewKind::Week => self.date - ChronoDuration::days(7),
            ViewKind::Day => self.date - ChronoDuration::days(1),
            ViewKind::Agenda => self.date - ChronoDuration::days(30),
            ViewKind::Contacts => self.date,
        }
    }

    pub fn next_date(&self) -> NaiveDate {
        match self.view {
            ViewKind::Month => self
                .date
                .checked_add_months(Months::new(1))
                .unwrap_or(self.date),
            ViewKind::Week => self.date + ChronoDuration::days(7),
            ViewKind::Day => self.date + ChronoDuration::days(1),
            ViewKind::Agenda => self.date + ChronoDuration::days(30),
            ViewKind::Contacts => self.date,
        }
    }

    pub fn title(&self, today: NaiveDate) -> String {
        match self.view {
            ViewKind::Month => self.date.format("%B %Y").to_string(),
            ViewKind::Week => {
                let start = week_start(self.date);
                let end = start + ChronoDuration::days(6);
                if start.month() == end.month() {
                    format!(
                        "{} {}\u{2013}{} {}",
                        start.format("%b"),
                        start.day(),
                        end.day(),
                        end.year()
                    )
                } else {
                    format!(
                        "{} \u{2013} {}",
                        start.format("%b %-d"),
                        end.format("%b %-d, %Y")
                    )
                }
            }
            ViewKind::Day => {
                if self.date == today {
                    format!("Today \u{2013} {}", self.date.format("%A, %B %-d"))
                } else {
                    self.date.format("%A, %B %-d, %Y").to_string()
                }
            }
            ViewKind::Agenda => "Agenda".to_string(),
            ViewKind::Contacts => "Contacts".to_string(),
        }
    }
}

/// The viewer-local wall-clock `[start, end)` window a given set of
/// `ViewParams` will display; used to size the upstream JMAP query before
/// the precise per-view layout logic runs.
pub fn display_range(params: &ViewParams) -> (NaiveDateTime, NaiveDateTime) {
    match params.view {
        ViewKind::Month => {
            let first_of_month = params.date.with_day(1).unwrap();
            let grid_start = week_start(first_of_month);
            let next_month = first_of_month.checked_add_months(Months::new(1)).unwrap();
            let mut grid_end = week_start(next_month);
            if grid_end < next_month {
                grid_end += ChronoDuration::days(7);
            }
            (
                grid_start.and_hms_opt(0, 0, 0).unwrap(),
                grid_end.and_hms_opt(0, 0, 0).unwrap(),
            )
        }
        ViewKind::Week => {
            let start = week_start(params.date);
            (
                start.and_hms_opt(0, 0, 0).unwrap(),
                (start + ChronoDuration::days(7))
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
        }
        ViewKind::Day => (
            params.date.and_hms_opt(0, 0, 0).unwrap(),
            (params.date + ChronoDuration::days(1))
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        ),
        ViewKind::Agenda => (
            params.date.and_hms_opt(0, 0, 0).unwrap(),
            (params.date + ChronoDuration::days(30))
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        ),
        // Not date-anchored; callers skip fetching events for this view.
        ViewKind::Contacts => (
            params.date.and_hms_opt(0, 0, 0).unwrap(),
            params.date.and_hms_opt(0, 0, 0).unwrap(),
        ),
    }
}

pub fn week_start(date: NaiveDate) -> NaiveDate {
    date - ChronoDuration::days(date.weekday().num_days_from_monday() as i64)
}

#[derive(Debug, Clone)]
pub struct EventView {
    pub id: String,
    pub title: String,
    pub color: String,
    pub all_day: bool,
    /// Birthdays are displayed alongside real events but aren't editable —
    /// they're derived from contact data, not a JMAP calendar object.
    pub is_birthday: bool,
    pub time_label: String,
    pub edit_href: String,
    pub top_px: f64,
    pub height_px: f64,
    pub left_pct: f64,
    pub width_pct: f64,
}

fn birthday_event_view(occ: &jmap_client::jscontact::BirthdayOccurrence) -> EventView {
    let title = match occ.turns {
        Some(age) => format!("\u{1f382} {} turns {}", occ.name, age),
        None => format!("\u{1f382} {}", occ.name),
    };
    EventView {
        id: format!("bday-{}", occ.uid),
        title,
        color: BIRTHDAY_COLOR.to_string(),
        all_day: true,
        is_birthday: true,
        time_label: "Birthday".to_string(),
        edit_href: String::new(),
        top_px: 0.0,
        height_px: HOUR_HEIGHT_PX,
        left_pct: 0.0,
        width_pct: 100.0,
    }
}

#[derive(Debug, Clone)]
pub struct DayCell {
    pub day_num: u32,
    pub in_month: bool,
    pub is_today: bool,
    pub iso: String,
    pub events: Vec<EventView>,
    pub more_count: usize,
}

#[derive(Debug, Clone)]
pub struct DayColumn {
    pub iso: String,
    pub weekday_label: String,
    pub is_today: bool,
    pub all_day_events: Vec<EventView>,
    pub timed_events: Vec<EventView>,
}

#[derive(Debug, Clone)]
pub struct HourRow {
    pub hour: u32,
    pub label: String,
}

pub fn hour_rows() -> Vec<HourRow> {
    (0..24)
        .map(|h| HourRow {
            hour: h,
            label: label_for_hour(h),
        })
        .collect()
}

fn label_for_hour(h: u32) -> String {
    if h == 0 {
        "12 AM".to_string()
    } else if h < 12 {
        format!("{h} AM")
    } else if h == 12 {
        "12 PM".to_string()
    } else {
        format!("{} PM", h - 12)
    }
}

pub fn calendar_color(calendar: Option<&Calendar>, calendar_id: &str) -> String {
    if let Some(c) = calendar.and_then(|c| c.color.clone()) {
        return c;
    }
    let idx = calendar_id.bytes().map(|b| b as usize).sum::<usize>() % PALETTE.len();
    PALETTE[idx].to_string()
}

/// One expanded, viewer-timezone-localized occurrence ready for display.
pub(crate) struct Localized {
    start: NaiveDateTime,
    end: NaiveDateTime,
    all_day: bool,
    event: CalendarEvent,
    calendar_id: String,
}

/// Expand + localize every event into the viewer's display timezone,
/// clipped to `[range_start, range_end)` (both given in viewer wall-clock).
pub fn localize_events(
    events: &[CalendarEvent],
    visible: &HashSet<String>,
    viewer_tz: Tz,
    range_start: NaiveDateTime,
    range_end: NaiveDateTime,
) -> Vec<Localized> {
    let mut out = Vec::new();
    for event in events {
        let calendar_id = event
            .calendar_ids
            .as_ref()
            .and_then(|m| m.keys().next())
            .cloned()
            .unwrap_or_default();
        if !visible.contains(&calendar_id) {
            continue;
        }
        let event_tz = event.time_zone.as_deref().and_then(tz::parse_tz);

        let range_start_event = match event_tz {
            Some(etz) => tz::convert(range_start, Some(viewer_tz), etz),
            None => range_start,
        };
        let range_end_event = match event_tz {
            Some(etz) => tz::convert(range_end, Some(viewer_tz), etz),
            None => range_end,
        };

        for occ in jmap_client::recurrence::expand(event, range_start_event, range_end_event) {
            let start_viewer = match event_tz {
                Some(etz) => tz::convert(occ.start, Some(etz), viewer_tz),
                None => occ.start,
            };
            let dur = parse_duration(&occ.event.duration).unwrap_or_default();
            let end_viewer = start_viewer + dur;
            out.push(Localized {
                start: start_viewer,
                end: end_viewer,
                all_day: occ.event.show_without_time,
                event: occ.event,
                calendar_id: calendar_id.clone(),
            });
        }
    }
    out.sort_by_key(|l| l.start);
    out
}

fn time_label(start: NaiveDateTime, end: NaiveDateTime) -> String {
    format!(
        "{}\u{2013}{}",
        start.format("%-I:%M %p"),
        end.format("%-I:%M %p")
    )
}

fn to_event_view(l: &Localized, calendars: &[Calendar]) -> EventView {
    let calendar = calendars
        .iter()
        .find(|c| c.id.as_deref() == Some(l.calendar_id.as_str()));
    let id = l.event.id.clone().unwrap_or_default();
    let day_start = l.start.date().and_hms_opt(0, 0, 0).unwrap();
    let minutes_from_midnight = (l.start - day_start).num_minutes().max(0) as f64;
    let duration_minutes = (l.end - l.start).num_minutes().max(15) as f64;
    EventView {
        id: id.clone(),
        title: l
            .event
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "(untitled)".to_string()),
        color: calendar_color(calendar, &l.calendar_id),
        all_day: l.all_day,
        is_birthday: false,
        time_label: if l.all_day {
            "All day".to_string()
        } else {
            time_label(l.start, l.end)
        },
        edit_href: format!(
            "/app/event/{}/edit{}",
            id,
            l.event
                .recurrence_id
                .as_ref()
                .map(|r| format!("?rid={}", urlencoding_light(&r.0)))
                .unwrap_or_default()
        ),
        top_px: (minutes_from_midnight / 60.0 * HOUR_HEIGHT_PX).round(),
        height_px: (duration_minutes / 60.0 * HOUR_HEIGHT_PX).max(22.0).round(),
        left_pct: 0.0,
        width_pct: 100.0,
    }
}

fn urlencoding_light(s: &str) -> String {
    s.replace(':', "%3A")
}

/// Greedily packs overlapping timed events into side-by-side lanes so
/// concurrent meetings don't visually collide.
fn layout_overlaps(events: &mut [EventView]) {
    let mut order: Vec<usize> = (0..events.len()).collect();
    order.sort_by(|&a, &b| events[a].top_px.partial_cmp(&events[b].top_px).unwrap());

    let mut lane_ends: Vec<f64> = Vec::new();
    let mut lanes_of: Vec<usize> = vec![0; events.len()];
    let mut cluster: Vec<usize> = Vec::new();
    let mut cluster_max_lane = 0usize;

    for &i in &order {
        let start = events[i].top_px;
        let end = events[i].top_px + events[i].height_px;
        // Free up lanes that have ended before this event starts.
        for end_ref in lane_ends.iter_mut() {
            if *end_ref <= start {
                *end_ref = f64::NEG_INFINITY;
            }
        }
        let lane = lane_ends
            .iter()
            .position(|e| *e == f64::NEG_INFINITY)
            .unwrap_or_else(|| {
                lane_ends.push(f64::NEG_INFINITY);
                lane_ends.len() - 1
            });
        lane_ends[lane] = end;
        lanes_of[i] = lane;
        cluster.push(i);
        cluster_max_lane = cluster_max_lane.max(lane);
    }

    let total_lanes = (cluster_max_lane + 1).max(1);
    for &i in &cluster {
        events[i].width_pct = ((100.0 / total_lanes as f64 - 1.0) * 100.0).round() / 100.0;
        events[i].left_pct =
            (lanes_of[i] as f64 * (100.0 / total_lanes as f64) * 100.0).round() / 100.0;
    }
}

pub struct BuildInputs<'a> {
    pub events: &'a [CalendarEvent],
    pub calendars: &'a [Calendar],
    pub birthdays: &'a [jmap_client::jscontact::BirthdayOccurrence],
    pub viewer_tz: Tz,
    pub params: &'a ViewParams,
    pub today: NaiveDate,
}

fn birthdays_on(inputs: &BuildInputs, date: NaiveDate) -> Vec<EventView> {
    inputs
        .birthdays
        .iter()
        .filter(|b| b.date == date)
        .map(birthday_event_view)
        .collect()
}

pub struct MonthView {
    pub weekday_labels: [&'static str; 7],
    pub weeks: Vec<Vec<DayCell>>,
}

pub fn build_month(inputs: &BuildInputs) -> MonthView {
    let first_of_month = inputs.params.date.with_day(1).unwrap();
    let grid_start = week_start(first_of_month);
    let (range_start, range_end) = display_range(inputs.params);
    let grid_end = range_end.date();
    let localized = localize_events(
        inputs.events,
        &inputs.params.visible,
        inputs.viewer_tz,
        range_start,
        range_end,
    );

    let mut weeks = Vec::new();
    let mut cursor = grid_start;
    while cursor < grid_end {
        let mut week = Vec::new();
        for _ in 0..7 {
            let mut day_events: Vec<EventView> = birthdays_on(inputs, cursor);
            day_events.extend(
                localized
                    .iter()
                    .filter(|l| l.start.date() == cursor)
                    .map(|l| to_event_view(l, inputs.calendars)),
            );
            let more_count = day_events.len().saturating_sub(4);
            week.push(DayCell {
                day_num: cursor.day(),
                in_month: cursor.month() == first_of_month.month(),
                is_today: cursor == inputs.today,
                iso: cursor.format("%Y-%m-%d").to_string(),
                events: day_events.into_iter().take(4).collect(),
                more_count,
            });
            cursor += ChronoDuration::days(1);
        }
        weeks.push(week);
    }

    MonthView {
        weekday_labels: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
        weeks,
    }
}

pub struct WeekView {
    pub days: Vec<DayColumn>,
    pub hours: Vec<HourRow>,
}

pub fn build_week(inputs: &BuildInputs) -> WeekView {
    let start = week_start(inputs.params.date);
    let range_start = start.and_hms_opt(0, 0, 0).unwrap();
    let range_end = (start + ChronoDuration::days(7))
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let localized = localize_events(
        inputs.events,
        &inputs.params.visible,
        inputs.viewer_tz,
        range_start,
        range_end,
    );

    let mut days = Vec::new();
    for i in 0..7 {
        let date = start + ChronoDuration::days(i);
        let mut all_day: Vec<EventView> = birthdays_on(inputs, date);
        let mut timed: Vec<EventView> = Vec::new();
        for l in localized.iter().filter(|l| l.start.date() == date) {
            let ev = to_event_view(l, inputs.calendars);
            if l.all_day {
                all_day.push(ev);
            } else {
                timed.push(ev);
            }
        }
        layout_overlaps(&mut timed);
        days.push(DayColumn {
            iso: date.format("%Y-%m-%d").to_string(),
            weekday_label: date.format("%a %-d").to_string(),
            is_today: date == inputs.today,
            all_day_events: all_day,
            timed_events: timed,
        });
    }

    WeekView {
        days,
        hours: hour_rows(),
    }
}

pub struct DayViewModel {
    pub day: DayColumn,
    pub hours: Vec<HourRow>,
}

pub fn build_day(inputs: &BuildInputs) -> DayViewModel {
    let date = inputs.params.date;
    let range_start = date.and_hms_opt(0, 0, 0).unwrap();
    let range_end = (date + ChronoDuration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let localized = localize_events(
        inputs.events,
        &inputs.params.visible,
        inputs.viewer_tz,
        range_start,
        range_end,
    );

    let mut all_day: Vec<EventView> = birthdays_on(inputs, date);
    let mut timed: Vec<EventView> = Vec::new();
    for l in &localized {
        let ev = to_event_view(l, inputs.calendars);
        if l.all_day {
            all_day.push(ev);
        } else {
            timed.push(ev);
        }
    }
    layout_overlaps(&mut timed);

    DayViewModel {
        day: DayColumn {
            iso: date.format("%Y-%m-%d").to_string(),
            weekday_label: date.format("%A, %-d %B").to_string(),
            is_today: date == inputs.today,
            all_day_events: all_day,
            timed_events: timed,
        },
        hours: hour_rows(),
    }
}

pub struct AgendaGroup {
    pub date: NaiveDate,
    pub label: String,
    pub events: Vec<EventView>,
}

pub struct AgendaView {
    pub groups: Vec<AgendaGroup>,
}

pub fn build_agenda(inputs: &BuildInputs) -> AgendaView {
    let start = inputs.params.date;
    let range_start = start.and_hms_opt(0, 0, 0).unwrap();
    let range_end = (start + ChronoDuration::days(30))
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let localized = localize_events(
        inputs.events,
        &inputs.params.visible,
        inputs.viewer_tz,
        range_start,
        range_end,
    );

    // Birthdays are listed first among that day's events (stable sort keeps
    // them ahead of same-date real events).
    let mut combined: Vec<(NaiveDate, EventView)> = inputs
        .birthdays
        .iter()
        .map(|b| (b.date, birthday_event_view(b)))
        .collect();
    combined.extend(
        localized
            .iter()
            .map(|l| (l.start.date(), to_event_view(l, inputs.calendars))),
    );
    combined.sort_by_key(|(date, _)| *date);

    let mut groups: Vec<AgendaGroup> = Vec::new();
    for (date, ev) in combined {
        if let Some(last) = groups.last_mut() {
            if last.date == date {
                last.events.push(ev);
                continue;
            }
        }
        let label = if date == inputs.today {
            "Today".to_string()
        } else if date == inputs.today + ChronoDuration::days(1) {
            "Tomorrow".to_string()
        } else {
            date.format("%A, %B %-d").to_string()
        };
        groups.push(AgendaGroup {
            date,
            label,
            events: vec![ev],
        });
    }

    AgendaView { groups }
}
