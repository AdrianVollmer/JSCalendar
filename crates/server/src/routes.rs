use std::collections::HashSet;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::{NaiveDate, NaiveDateTime};
use jmap_client::duration::{format_duration, parse_duration};
use jmap_client::jscalendar::{Calendar, CalendarEvent, Frequency, LocalDateTime, RecurrenceRule};
use jmap_client::tz::Tz;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::AuthedSession;
use crate::error::AppError;
use crate::state::AppState;
use crate::view::{self, ViewKind, ViewParams};
use crate::webutil::{is_hx, render};

#[derive(Debug, Deserialize, Default)]
pub struct RawViewQuery {
    pub view: Option<String>,
    pub date: Option<String>,
    pub cal: Option<String>,
}

fn today_in(tz: Tz) -> NaiveDate {
    chrono::Utc::now().with_timezone(&tz).date_naive()
}

fn resolve_view_params(
    raw: &RawViewQuery,
    calendars: &[Calendar],
    today: NaiveDate,
    has_contacts: bool,
) -> ViewParams {
    let view = raw.view.as_deref().map(ViewKind::parse).unwrap_or(ViewKind::Month);
    let date = raw
        .date
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or(today);
    let mut all_calendar_ids: Vec<String> = calendars.iter().filter_map(|c| c.id.clone()).collect();
    if has_contacts {
        all_calendar_ids.push(view::BIRTHDAY_PSEUDO_ID.to_string());
    }
    let visible: HashSet<String> = match &raw.cal {
        Some(s) => s.split(',').filter(|p| !p.is_empty()).map(|p| p.to_string()).collect(),
        None => {
            let mut v: HashSet<String> =
                calendars.iter().filter(|c| c.is_visible).filter_map(|c| c.id.clone()).collect();
            if has_contacts {
                v.insert(view::BIRTHDAY_PSEUDO_ID.to_string());
            }
            v
        }
    };
    ViewParams {
        view,
        date,
        visible,
        all_calendar_ids,
    }
}

async fn fetch_events(session: &AuthedSession, state: &AppState, params: &ViewParams) -> Result<Vec<CalendarEvent>, AppError> {
    let (range_start, range_end) = view::display_range(params);
    let start_utc = jmap_client::tz::convert(range_start, Some(state.viewer_tz), Tz::UTC);
    let end_utc = jmap_client::tz::convert(range_end, Some(state.viewer_tz), Tz::UTC);
    let after = start_utc.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let before = end_utc.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let events = session
        .client
        .query_events(&session.account_id, None, Some(&after), Some(&before))
        .await?;
    Ok(events)
}

async fn fetch_birthdays(
    session: &AuthedSession,
    params: &ViewParams,
) -> Result<Vec<jmap_client::jscontact::BirthdayOccurrence>, AppError> {
    if !params.visible.contains(view::BIRTHDAY_PSEUDO_ID) {
        return Ok(Vec::new());
    }
    let Some(contacts_account_id) = &session.contacts_account_id else {
        return Ok(Vec::new());
    };
    let (range_start, range_end) = view::display_range(params);
    let cards = session.client.get_contact_cards(contacts_account_id).await?;
    Ok(jmap_client::jscontact::expand_birthdays(&cards, range_start.date(), range_end.date()))
}

#[derive(Template)]
#[template(path = "view_month.html")]
struct MonthTemplateGrid {
    weekday_labels: [&'static str; 7],
    weeks: Vec<Vec<view::DayCell>>,
    cal_param: String,
}

#[derive(Template)]
#[template(path = "view_week.html")]
struct WeekTemplate {
    days: Vec<view::DayColumn>,
    hours: Vec<view::HourRow>,
    cal_param: String,
}

#[derive(Template)]
#[template(path = "view_day.html")]
struct DayTemplate {
    day: view::DayColumn,
    hours: Vec<view::HourRow>,
}

#[derive(Template)]
#[template(path = "view_agenda.html")]
struct AgendaTemplate {
    groups: Vec<view::AgendaGroup>,
}

struct ContactRow {
    name: String,
    initial: String,
    email: Option<String>,
    birthday_label: Option<String>,
}

#[derive(Template)]
#[template(path = "view_contacts.html")]
struct ContactsTemplate {
    contacts: Vec<ContactRow>,
}

fn birthday_label(card: &jmap_client::jscontact::Card) -> Option<String> {
    let anniversaries = card.anniversaries.as_ref()?;
    let birth = anniversaries.values().find(|a| a.kind.as_deref() == Some("birth"))?;
    let (month, day, year) = birth.date.month_day_year()?;
    let date = NaiveDate::from_ymd_opt(year.unwrap_or(2000), month, day)?;
    Some(match year {
        Some(y) => format!("{} {}", date.format("%B %-d"), y),
        None => date.format("%B %-d").to_string(),
    })
}

async fn render_fragment(
    session: &AuthedSession,
    state: &AppState,
    calendars: &[Calendar],
    params: &ViewParams,
    today: NaiveDate,
) -> Result<String, AppError> {
    if params.view == ViewKind::Contacts {
        let cards = match &session.contacts_account_id {
            Some(id) => session.client.get_contact_cards(id).await?,
            None => Vec::new(),
        };
        let mut contacts: Vec<ContactRow> = cards
            .iter()
            .map(|c| {
                let name = c.display_name();
                let initial = name.chars().next().unwrap_or('?').to_uppercase().to_string();
                ContactRow {
                    initial,
                    name,
                    email: c.primary_email().map(|s| s.to_string()),
                    birthday_label: birthday_label(c),
                }
            })
            .collect();
        contacts.sort_by(|a, b| a.name.cmp(&b.name));
        return ContactsTemplate { contacts }
            .render()
            .map_err(|e| AppError::bad_request(format!("template error: {e}")));
    }

    let events = fetch_events(session, state, params).await?;
    let birthdays = fetch_birthdays(session, params).await?;
    let inputs = view::BuildInputs {
        events: &events,
        calendars,
        birthdays: &birthdays,
        viewer_tz: state.viewer_tz,
        params,
        today,
    };
    let html = match params.view {
        ViewKind::Month => {
            let m = view::build_month(&inputs);
            MonthTemplateGrid {
                weekday_labels: m.weekday_labels,
                weeks: m.weeks,
                cal_param: params.cal_param(),
            }
            .render()
        }
        ViewKind::Week => {
            let w = view::build_week(&inputs);
            WeekTemplate {
                days: w.days,
                hours: w.hours,
                cal_param: params.cal_param(),
            }
            .render()
        }
        ViewKind::Day => {
            let d = view::build_day(&inputs);
            DayTemplate { day: d.day, hours: d.hours }.render()
        }
        ViewKind::Agenda => {
            let a = view::build_agenda(&inputs);
            AgendaTemplate { groups: a.groups }.render()
        }
        ViewKind::Contacts => unreachable!("handled above"),
    };
    html.map_err(|e| AppError::bad_request(format!("template error: {e}")))
}

struct ViewLink {
    label: &'static str,
    href: String,
    active: bool,
}

struct SidebarCalendarVM {
    id: String,
    name: String,
    color: String,
    visible: bool,
    toggle_href: String,
}

fn sidebar_calendars(calendars: &[Calendar], params: &ViewParams, has_contacts: bool) -> Vec<SidebarCalendarVM> {
    let mut items: Vec<SidebarCalendarVM> = calendars
        .iter()
        .filter_map(|c| {
            let id = c.id.clone()?;
            Some(SidebarCalendarVM {
                color: view::calendar_color(Some(c), &id),
                visible: params.visible.contains(&id),
                toggle_href: params.toggle_href(&id),
                name: c.name.clone(),
                id,
            })
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if has_contacts {
        items.push(SidebarCalendarVM {
            id: view::BIRTHDAY_PSEUDO_ID.to_string(),
            name: "Birthdays".to_string(),
            color: view::BIRTHDAY_COLOR.to_string(),
            visible: params.visible.contains(view::BIRTHDAY_PSEUDO_ID),
            toggle_href: params.toggle_href(view::BIRTHDAY_PSEUDO_ID),
        });
    }
    items
}

fn view_links(params: &ViewParams, has_contacts: bool) -> Vec<ViewLink> {
    let mut kinds = vec![
        (ViewKind::Month, "Month"),
        (ViewKind::Week, "Week"),
        (ViewKind::Day, "Day"),
        (ViewKind::Agenda, "Agenda"),
    ];
    if has_contacts {
        kinds.push((ViewKind::Contacts, "Contacts"));
    }
    kinds
        .into_iter()
        .map(|(kind, label)| ViewLink {
            label,
            href: params.nav_href(kind, params.date),
            active: kind == params.view,
        })
        .collect()
}

/// The sidebar/topbar/view region re-rendered on *every* navigation (view
/// switch, prev/next/today, calendar toggle, day drill-down) — not just the
/// grid — since the title, active nav link, and back-context links all
/// depend on the current view/date/cal state too.
struct ShellParts {
    title: String,
    username: String,
    is_dated_view: bool,
    calendars: Vec<SidebarCalendarVM>,
    view_links: Vec<ViewLink>,
    prev_href: String,
    next_href: String,
    today_href: String,
    back_qs: String,
    body: String,
}

#[derive(Template)]
#[template(path = "app_shell.html")]
struct AppShellTemplate {
    title: String,
    username: String,
    is_dated_view: bool,
    calendars: Vec<SidebarCalendarVM>,
    view_links: Vec<ViewLink>,
    prev_href: String,
    next_href: String,
    today_href: String,
    back_qs: String,
    body: String,
    modal: Option<String>,
}

#[derive(Template)]
#[template(path = "app_inner.html")]
struct AppInnerTemplate {
    title: String,
    username: String,
    is_dated_view: bool,
    calendars: Vec<SidebarCalendarVM>,
    view_links: Vec<ViewLink>,
    prev_href: String,
    next_href: String,
    today_href: String,
    back_qs: String,
    body: String,
}

impl ShellParts {
    fn into_shell(self, modal: Option<String>) -> AppShellTemplate {
        AppShellTemplate {
            title: self.title,
            username: self.username,
            is_dated_view: self.is_dated_view,
            calendars: self.calendars,
            view_links: self.view_links,
            prev_href: self.prev_href,
            next_href: self.next_href,
            today_href: self.today_href,
            back_qs: self.back_qs,
            body: self.body,
            modal,
        }
    }

    fn into_inner(self) -> AppInnerTemplate {
        AppInnerTemplate {
            title: self.title,
            username: self.username,
            is_dated_view: self.is_dated_view,
            calendars: self.calendars,
            view_links: self.view_links,
            prev_href: self.prev_href,
            next_href: self.next_href,
            today_href: self.today_href,
            back_qs: self.back_qs,
            body: self.body,
        }
    }
}

async fn build_shell_parts(
    session: &AuthedSession,
    state: &AppState,
    calendars: &[Calendar],
    params: &ViewParams,
    today: NaiveDate,
) -> Result<ShellParts, AppError> {
    let has_contacts = session.contacts_account_id.is_some();
    let body = render_fragment(session, state, calendars, params, today).await?;
    Ok(ShellParts {
        title: params.title(today),
        username: session.username.clone(),
        is_dated_view: params.view.is_dated(),
        calendars: sidebar_calendars(calendars, params, has_contacts),
        view_links: view_links(params, has_contacts),
        prev_href: params.nav_href(params.view, params.prev_date()),
        next_href: params.nav_href(params.view, params.next_date()),
        today_href: params.nav_href(params.view, today),
        back_qs: params.query_string(params.view, params.date),
        body,
    })
}

pub async fn root() -> Redirect {
    Redirect::to("/app")
}

pub async fn app_view(
    State(state): State<AppState>,
    session: AuthedSession,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(&raw, &calendars, today, session.contacts_account_id.is_some());

    let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
    if is_hx(&headers) {
        let tpl = parts.into_inner();
        Ok(render(&tpl))
    } else {
        let tpl = parts.into_shell(None);
        Ok(render(&tpl))
    }
}

// ---- Event create/edit form -------------------------------------------

struct CalendarOption {
    id: String,
    name: String,
    selected: bool,
}

struct TzOption {
    value: String,
    label: String,
    selected: bool,
}

const COMMON_ZONES: &[&str] = &[
    "UTC",
    "Europe/London",
    "Europe/Berlin",
    "Europe/Paris",
    "Europe/Moscow",
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "America/Sao_Paulo",
    "Asia/Tokyo",
    "Asia/Shanghai",
    "Asia/Kolkata",
    "Asia/Dubai",
    "Australia/Sydney",
];

fn tz_options(selected: &str) -> Vec<TzOption> {
    let mut opts = vec![TzOption {
        value: String::new(),
        label: "Floating (no time zone)".to_string(),
        selected: selected.is_empty(),
    }];
    let mut seen_selected = selected.is_empty();
    for &z in COMMON_ZONES {
        if z == selected {
            seen_selected = true;
        }
        opts.push(TzOption {
            value: z.to_string(),
            label: z.replace('_', " "),
            selected: z == selected,
        });
    }
    if !seen_selected && !selected.is_empty() {
        opts.push(TzOption {
            value: selected.to_string(),
            label: selected.replace('_', " "),
            selected: true,
        });
    }
    opts
}

#[derive(Template)]
#[template(path = "event_form.html")]
struct EventFormTemplate {
    is_edit: bool,
    event_id: String,
    back_view: String,
    back_date: String,
    back_cal: String,
    calendars: Vec<CalendarOption>,
    title: String,
    description: String,
    location: String,
    all_day: bool,
    start_date: String,
    start_time: String,
    end_date: String,
    end_time: String,
    timezones: Vec<TzOption>,
    repeat_freq: String,
    repeat_interval: u32,
    repeat_end_mode: String,
    repeat_until: String,
    repeat_count: u32,
    error: Option<String>,
}

fn calendar_options(calendars: &[Calendar], selected: Option<&str>) -> Vec<CalendarOption> {
    let selected = selected.or_else(|| calendars.first().and_then(|c| c.id.as_deref()));
    calendars
        .iter()
        .filter_map(|c| {
            let id = c.id.clone()?;
            let is_selected = Some(id.as_str()) == selected;
            Some(CalendarOption {
                name: c.name.clone(),
                selected: is_selected,
                id,
            })
        })
        .collect()
}

fn blank_form(calendars: &[Calendar], params: &ViewParams, viewer_tz: Tz, error: Option<String>) -> EventFormTemplate {
    EventFormTemplate {
        is_edit: false,
        event_id: String::new(),
        back_view: params.view.as_str().to_string(),
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        calendars: calendar_options(calendars, None),
        title: String::new(),
        description: String::new(),
        location: String::new(),
        all_day: false,
        start_date: params.date.format("%Y-%m-%d").to_string(),
        start_time: "09:00".to_string(),
        end_date: params.date.format("%Y-%m-%d").to_string(),
        end_time: "10:00".to_string(),
        timezones: tz_options(&viewer_tz.to_string()),
        repeat_freq: "none".to_string(),
        repeat_interval: 1,
        repeat_end_mode: "never".to_string(),
        repeat_until: params.date.format("%Y-%m-%d").to_string(),
        repeat_count: 5,
        error,
    }
}

fn event_to_form(
    event: &CalendarEvent,
    calendars: &[Calendar],
    params: &ViewParams,
    error: Option<String>,
) -> EventFormTemplate {
    let start = event.start.to_naive().unwrap_or_default();
    let duration = parse_duration(&event.duration).unwrap_or_default();
    let end = start + duration;
    let calendar_id = event.calendar_ids.as_ref().and_then(|m| m.keys().next()).cloned();

    let (repeat_freq, repeat_interval, repeat_end_mode, repeat_until, repeat_count) =
        match event.recurrence_rules.as_ref().and_then(|r| r.first()) {
            Some(rule) => {
                let freq = match rule.frequency {
                    Frequency::Daily => "daily",
                    Frequency::Weekly => "weekly",
                    Frequency::Monthly => "monthly",
                    Frequency::Yearly => "yearly",
                    _ => "none",
                };
                let (mode, until, count) = if let Some(u) = &rule.until {
                    ("until", u.date().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(), 5)
                } else if let Some(c) = rule.count {
                    ("count", params.date.format("%Y-%m-%d").to_string(), c)
                } else {
                    ("never", params.date.format("%Y-%m-%d").to_string(), 5)
                };
                (freq.to_string(), rule.interval.unwrap_or(1), mode.to_string(), until, count)
            }
            None => (
                "none".to_string(),
                1,
                "never".to_string(),
                params.date.format("%Y-%m-%d").to_string(),
                5,
            ),
        };

    EventFormTemplate {
        is_edit: true,
        event_id: event.id.clone().unwrap_or_default(),
        back_view: params.view.as_str().to_string(),
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        calendars: calendar_options(calendars, calendar_id.as_deref()),
        title: event.title.clone().unwrap_or_default(),
        description: event.description.clone().unwrap_or_default(),
        location: event
            .locations
            .as_ref()
            .and_then(|m| m.values().next())
            .and_then(|l| l.name.clone())
            .unwrap_or_default(),
        all_day: event.show_without_time,
        start_date: start.format("%Y-%m-%d").to_string(),
        start_time: start.format("%H:%M").to_string(),
        end_date: end.format("%Y-%m-%d").to_string(),
        end_time: end.format("%H:%M").to_string(),
        timezones: tz_options(event.time_zone.as_deref().unwrap_or("")),
        repeat_freq,
        repeat_interval,
        repeat_end_mode,
        repeat_until,
        repeat_count,
        error,
    }
}

pub async fn event_new_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(&raw, &calendars, today, session.contacts_account_id.is_some());
    let form = blank_form(&calendars, &params, state.viewer_tz, None);
    let modal_html = form.render().map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

pub async fn event_edit_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(&raw, &calendars, today, session.contacts_account_id.is_some());
    let events = session.client.get_events(&session.account_id, &[id]).await?;
    let event = events.first().ok_or(jmap_client::Error::NotFound)?;
    let form = event_to_form(event, &calendars, &params, None);
    let modal_html = form.render().map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

// ---- Event mutations ---------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct EventFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
    pub title: String,
    pub calendar_id: String,
    #[serde(default)]
    pub all_day: Option<String>,
    pub start_date: String,
    #[serde(default)]
    pub start_time: String,
    pub end_date: String,
    #[serde(default)]
    pub end_time: String,
    #[serde(default)]
    pub timezone: String,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub description: String,
    pub repeat_freq: String,
    #[serde(default = "one")]
    pub repeat_interval: u32,
    pub repeat_end_mode: String,
    #[serde(default = "five")]
    pub repeat_count: u32,
    #[serde(default)]
    pub repeat_until: String,
}

fn one() -> u32 {
    1
}
fn five() -> u32 {
    5
}

fn parse_dt(date: &str, time: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(&format!("{date}T{time}:00"), "%Y-%m-%dT%H:%M:%S").ok()
}

fn build_event_from_form(form: &EventFormBody, uid: String) -> Result<CalendarEvent, String> {
    let all_day = form.all_day.is_some();
    let time = if all_day { "00:00" } else { form.start_time.as_str() };
    let start = parse_dt(&form.start_date, time).ok_or("invalid start date/time")?;

    let duration = if all_day {
        let start_date = NaiveDate::parse_from_str(&form.start_date, "%Y-%m-%d").map_err(|_| "invalid start date")?;
        let end_date = NaiveDate::parse_from_str(&form.end_date, "%Y-%m-%d").map_err(|_| "invalid end date")?;
        let days = (end_date - start_date).num_days().max(1);
        chrono::Duration::days(days)
    } else {
        let end = parse_dt(&form.end_date, &form.end_time).ok_or("invalid end date/time")?;
        let d = end - start;
        if d.num_minutes() <= 0 {
            return Err("end must be after start".to_string());
        }
        d
    };

    let mut event = CalendarEvent::new(uid, LocalDateTime::from_naive(start));
    event.title = Some(form.title.trim().to_string()).filter(|t| !t.is_empty());
    event.description = Some(form.description.clone()).filter(|d| !d.is_empty());
    event.show_without_time = all_day;
    event.time_zone = Some(form.timezone.clone()).filter(|t| !t.is_empty());
    event.duration = format_duration(duration);
    let mut calendar_ids = std::collections::BTreeMap::new();
    calendar_ids.insert(form.calendar_id.clone(), true);
    event.calendar_ids = Some(calendar_ids);
    if !form.location.is_empty() {
        let mut locations = std::collections::BTreeMap::new();
        locations.insert(
            "loc1".to_string(),
            jmap_client::jscalendar::Location {
                name: Some(form.location.clone()),
                ..Default::default()
            },
        );
        event.locations = Some(locations);
    }

    if form.repeat_freq != "none" {
        let frequency = match form.repeat_freq.as_str() {
            "daily" => Frequency::Daily,
            "weekly" => Frequency::Weekly,
            "monthly" => Frequency::Monthly,
            "yearly" => Frequency::Yearly,
            _ => Frequency::Daily,
        };
        let mut rule = RecurrenceRule {
            frequency,
            interval: Some(form.repeat_interval.max(1)),
            ..Default::default()
        };
        match form.repeat_end_mode.as_str() {
            "count" => rule.count = Some(form.repeat_count.max(1)),
            "until" => {
                let until_date =
                    NaiveDate::parse_from_str(&form.repeat_until, "%Y-%m-%d").map_err(|_| "invalid repeat end date")?;
                rule.until = Some(LocalDateTime::from_naive(until_date.and_hms_opt(23, 59, 59).unwrap()));
            }
            _ => {}
        }
        event.recurrence_rules = Some(vec![rule]);
    }

    Ok(event)
}

async fn mutation_response(
    state: &AppState,
    session: &AuthedSession,
    headers: &HeaderMap,
    back_view: &str,
    back_date: &str,
    back_cal: &str,
) -> Result<Response, AppError> {
    if is_hx(headers) {
        let today = today_in(state.viewer_tz);
        let calendars = session.client.get_calendars(&session.account_id).await?;
        let raw = RawViewQuery {
            view: Some(back_view.to_string()),
            date: Some(back_date.to_string()),
            cal: Some(back_cal.to_string()),
        };
        let params = resolve_view_params(&raw, &calendars, today, session.contacts_account_id.is_some());
        let fragment = render_fragment(session, state, &calendars, &params, today).await?;
        let body = format!(r#"<div id="view" class="view-container" hx-swap-oob="true">{fragment}</div>"#);
        Ok(Html(body).into_response())
    } else {
        Ok(Redirect::to(&format!("/app?view={back_view}&date={back_date}&cal={back_cal}")).into_response())
    }
}

pub async fn event_create(
    State(state): State<AppState>,
    session: AuthedSession,
    headers: HeaderMap,
    Form(form): Form<EventFormBody>,
) -> Result<Response, AppError> {
    let uid = Uuid::new_v4().to_string();
    let event = match build_event_from_form(&form, uid) {
        Ok(e) => e,
        Err(msg) => return render_form_error(&state, &session, &form, false, None, msg).await,
    };
    session.client.create_event(&session.account_id, &event).await?;
    mutation_response(&state, &session, &headers, &form.back_view, &form.back_date, &form.back_cal).await
}

pub async fn event_update(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<EventFormBody>,
) -> Result<Response, AppError> {
    let event = match build_event_from_form(&form, String::new()) {
        Ok(e) => e,
        Err(msg) => return render_form_error(&state, &session, &form, true, Some(id), msg).await,
    };
    let mut patch = serde_json::to_value(&event).map_err(|e| AppError::bad_request(e.to_string()))?;
    if let Some(obj) = patch.as_object_mut() {
        obj.remove("id");
        obj.remove("uid");
    }
    session.client.update_event(&session.account_id, &id, patch).await?;
    mutation_response(&state, &session, &headers, &form.back_view, &form.back_date, &form.back_cal).await
}

async fn render_form_error(
    state: &AppState,
    session: &AuthedSession,
    form: &EventFormBody,
    is_edit: bool,
    event_id: Option<String>,
    message: String,
) -> Result<Response, AppError> {
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let today = today_in(state.viewer_tz);
    let raw = RawViewQuery {
        view: Some(form.back_view.clone()),
        date: Some(form.back_date.clone()),
        cal: Some(form.back_cal.clone()),
    };
    let params = resolve_view_params(&raw, &calendars, today, session.contacts_account_id.is_some());
    let mut tpl = blank_form(&calendars, &params, state.viewer_tz, Some(message));
    tpl.is_edit = is_edit;
    tpl.event_id = event_id.unwrap_or_default();
    tpl.calendars = calendar_options(&calendars, Some(&form.calendar_id));
    tpl.title = form.title.clone();
    tpl.description = form.description.clone();
    tpl.location = form.location.clone();
    tpl.all_day = form.all_day.is_some();
    tpl.start_date = form.start_date.clone();
    tpl.start_time = form.start_time.clone();
    tpl.end_date = form.end_date.clone();
    tpl.end_time = form.end_time.clone();
    tpl.timezones = tz_options(&form.timezone);
    tpl.repeat_freq = form.repeat_freq.clone();
    tpl.repeat_interval = form.repeat_interval;
    tpl.repeat_end_mode = form.repeat_end_mode.clone();
    tpl.repeat_until = form.repeat_until.clone();
    tpl.repeat_count = form.repeat_count;
    let body = tpl.render().map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Html(body).into_response())
}

#[derive(Debug, Deserialize)]
pub struct DeleteFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
}

pub async fn event_delete(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<DeleteFormBody>,
) -> Result<Response, AppError> {
    session.client.destroy_event(&session.account_id, &id).await?;
    mutation_response(&state, &session, &headers, &form.back_view, &form.back_date, &form.back_cal).await
}
