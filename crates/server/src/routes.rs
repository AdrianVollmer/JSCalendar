use std::collections::{BTreeMap, HashSet};

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use futures_util::StreamExt;
use jmap_client::duration::{format_duration, parse_duration};
use jmap_client::jscalendar::{Calendar, CalendarEvent, Frequency, LocalDateTime, RecurrenceRule};
use jmap_client::jscontact::{
    AddressBook, Anniversary, AnniversaryDate, Card, EmailAddress, NameProperty,
};
use jmap_client::tz::Tz;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{AppUser, AuthedSession};
use crate::error::AppError;
use crate::state::AppState;
use crate::vcard;
use crate::view::{self, ViewKind, ViewParams};
use crate::webutil::{is_hx, render};

#[derive(Debug, Deserialize, Default)]
pub struct RawViewQuery {
    pub view: Option<String>,
    pub date: Option<String>,
    pub cal: Option<String>,
    pub q: Option<String>,
}

fn today_in(tz: Tz) -> NaiveDate {
    chrono::Utc::now().with_timezone(&tz).date_naive()
}

fn resolve_view_params(
    raw: &RawViewQuery,
    calendars: &[Calendar],
    today: NaiveDate,
    has_contacts: bool,
    ics_ids: &[String],
    has_holidays: bool,
) -> ViewParams {
    let view = raw
        .view
        .as_deref()
        .map(ViewKind::parse)
        .unwrap_or(ViewKind::Month);
    let date = raw
        .date
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or(today);
    let mut all_calendar_ids: Vec<String> = calendars.iter().filter_map(|c| c.id.clone()).collect();
    if has_contacts {
        all_calendar_ids.push(view::BIRTHDAY_PSEUDO_ID.to_string());
    }
    all_calendar_ids.extend(ics_ids.iter().cloned());
    if has_holidays {
        all_calendar_ids.push(view::HOLIDAYS_PSEUDO_ID.to_string());
    }
    let visible: HashSet<String> = match &raw.cal {
        Some(s) => s
            .split(',')
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect(),
        None => {
            let mut v: HashSet<String> = calendars
                .iter()
                .filter(|c| c.is_visible)
                .filter_map(|c| c.id.clone())
                .collect();
            if has_contacts {
                v.insert(view::BIRTHDAY_PSEUDO_ID.to_string());
            }
            v.extend(ics_ids.iter().cloned());
            // Not inserted into the default-visible set even when
            // configured — holidays are opt-in, shown only once the user
            // explicitly toggles them on.
            v
        }
    };
    ViewParams {
        view,
        date,
        visible,
        all_calendar_ids,
        q: raw.q.clone().filter(|s| !s.is_empty()),
    }
}

async fn fetch_events(
    session: &AuthedSession,
    state: &AppState,
    params: &ViewParams,
) -> Result<Vec<CalendarEvent>, AppError> {
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

/// `ContactCard/get` has no filter for "changed" or "has a birthday in this
/// range", so every fetch pulls the whole address book — restricting to
/// just what the UI needs (no photos, phone numbers, addresses, etc.) keeps
/// each of those fetches as cheap as this protocol allows.
const CONTACT_PROPERTIES: &[&str] = &["id", "uid", "name", "emails", "anniversaries"];

fn contacts_cache_key(session: &AuthedSession, contacts_account_id: &str) -> String {
    let api_url = session
        .client
        .session()
        .map(|s| s.api_url.clone())
        .unwrap_or_default();
    format!("{api_url}#{contacts_account_id}")
}

/// Fetches every contact card for the session's account, reusing a recent
/// result across calendar-view renders instead of re-fetching on every one
/// (birthdays rarely change, so a full re-fetch per render is pure waste —
/// see `state::CONTACTS_CACHE_TTL_SECS`).
async fn get_contact_cards_cached(
    state: &AppState,
    session: &AuthedSession,
    contacts_account_id: &str,
) -> Result<Vec<jmap_client::jscontact::Card>, AppError> {
    let cache_key = contacts_cache_key(session, contacts_account_id);
    let ttl = std::time::Duration::from_secs(crate::state::CONTACTS_CACHE_TTL_SECS);

    if let Some(entry) = state.contacts_cache.get(&cache_key) {
        if entry.fetched_at.elapsed() < ttl {
            return Ok(entry.cards.clone());
        }
    }

    let cards = session
        .client
        .get_contact_cards(contacts_account_id, Some(CONTACT_PROPERTIES))
        .await?;
    state.contacts_cache.insert(
        cache_key,
        crate::state::CachedContacts {
            cards: cards.clone(),
            fetched_at: std::time::Instant::now(),
        },
    );
    Ok(cards)
}

/// Called after our own create/update/delete so the change shows up
/// immediately instead of waiting out the TTL.
fn invalidate_contacts_cache(state: &AppState, session: &AuthedSession, contacts_account_id: &str) {
    state
        .contacts_cache
        .remove(&contacts_cache_key(session, contacts_account_id));
}

async fn fetch_birthdays(
    state: &AppState,
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
    let cards = get_contact_cards_cached(state, session, contacts_account_id).await?;
    Ok(jmap_client::jscontact::expand_birthdays(
        &cards,
        range_start.date(),
        range_end.date(),
    ))
}

fn fetch_holidays(state: &AppState, params: &ViewParams) -> Vec<(NaiveDate, String)> {
    if !params.visible.contains(view::HOLIDAYS_PSEUDO_ID) {
        return Vec::new();
    }
    let Some(region) = state.holidays_region else {
        return Vec::new();
    };
    let (range_start, range_end) = view::display_range(params);
    let mut out = Vec::new();
    for year in range_start.year()..=range_end.year() {
        for (date, holiday) in region.holiday_dates_in_year(year) {
            if date >= range_start.date() && date < range_end.date() {
                out.push((date, holiday.description().to_string()));
            }
        }
    }
    out
}

fn ics_subscriptions_for(
    state: &AppState,
    session: &AuthedSession,
) -> Vec<crate::users::IcsSubscription> {
    state.users.list_ics_subscriptions(&session.user_id)
}

fn ics_subscription_ids(state: &AppState, session: &AuthedSession) -> Vec<String> {
    ics_subscriptions_for(state, session)
        .iter()
        .map(|s| crate::ics::pseudo_calendar_id(&s.id))
        .collect()
}

/// Feeds are trusted to be reasonably small calendar exports, not
/// arbitrary downloads; this just bounds how much memory one subscription
/// can make the server hold regardless of what the remote host sends.
const ICS_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

async fn fetch_and_parse_ics(
    state: &AppState,
    sub: &crate::users::IcsSubscription,
) -> Result<Vec<CalendarEvent>, String> {
    let parsed = url::Url::parse(&sub.url).map_err(|e| format!("invalid URL: {e}"))?;
    let host = parsed.host_str().ok_or("URL has no host")?;
    let port = parsed
        .port_or_known_default()
        .ok_or("URL has no known port")?;
    // Re-checked on every fetch, not just when the subscription was
    // created: a hostname that resolved publicly then could resolve
    // privately now (DNS changes, TTL expiry) — see `crate::netguard`.
    crate::netguard::ensure_resolves_publicly(host, port).await?;

    let resp = state
        .http_client
        .get(&sub.url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if body.len() + chunk.len() > ICS_MAX_RESPONSE_BYTES {
            return Err("feed response too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body);
    Ok(crate::ics::parse_events(&text, &sub.id))
}

/// Fetches + parses a subscription's `.ics` feed, reusing a recent result
/// (see `state::ICS_CACHE_TTL_SECS`) instead of re-fetching the URL on
/// every render. A fetch/parse failure keeps serving the last known-good
/// events rather than blanking the subscription out of the view.
async fn get_ics_events_cached(
    state: &AppState,
    sub: &crate::users::IcsSubscription,
) -> Vec<CalendarEvent> {
    let ttl = std::time::Duration::from_secs(crate::state::ICS_CACHE_TTL_SECS);
    if let Some(entry) = state.ics_cache.get(&sub.id) {
        if entry.fetched_at.elapsed() < ttl {
            return entry.events.clone();
        }
    }
    match fetch_and_parse_ics(state, sub).await {
        Ok(events) => {
            state.ics_cache.insert(
                sub.id.clone(),
                crate::state::CachedIcsEvents {
                    events: events.clone(),
                    fetched_at: std::time::Instant::now(),
                    error: None,
                },
            );
            events
        }
        Err(e) => {
            let stale = state
                .ics_cache
                .get(&sub.id)
                .map(|c| c.events.clone())
                .unwrap_or_default();
            state.ics_cache.insert(
                sub.id.clone(),
                crate::state::CachedIcsEvents {
                    events: stale.clone(),
                    fetched_at: std::time::Instant::now(),
                    error: Some(e),
                },
            );
            stale
        }
    }
}

async fn fetch_ics_events(
    state: &AppState,
    session: &AuthedSession,
    params: &ViewParams,
) -> Vec<CalendarEvent> {
    let subs = ics_subscriptions_for(state, session);
    let mut events = Vec::new();
    for sub in &subs {
        if !params
            .visible
            .contains(&crate::ics::pseudo_calendar_id(&sub.id))
        {
            continue;
        }
        events.extend(get_ics_events_cached(state, sub).await);
    }
    events
}

fn ics_colors_for(
    state: &AppState,
    session: &AuthedSession,
) -> std::collections::HashMap<String, String> {
    ics_subscriptions_for(state, session)
        .into_iter()
        .map(|s| (crate::ics::pseudo_calendar_id(&s.id), s.color))
        .collect()
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
    id: String,
    name: String,
    initial: String,
    email: Option<String>,
    birthday_label: Option<String>,
    edit_href: String,
}

#[derive(Template)]
#[template(path = "view_contacts.html")]
struct ContactsTemplate {
    contacts: Vec<ContactRow>,
    q: String,
    cal_param: String,
}

fn birthday_label(card: &jmap_client::jscontact::Card) -> Option<String> {
    let anniversaries = card.anniversaries.as_ref()?;
    let birth = anniversaries
        .values()
        .find(|a| a.kind.as_deref() == Some("birth"))?;
    let (month, day, year) = birth.date.month_day_year()?;
    let date = NaiveDate::from_ymd_opt(year.unwrap_or(2000), month, day)?;
    Some(match year {
        Some(y) => format!("{} {}", date.format("%B %-d"), y),
        None => date.format("%B %-d").to_string(),
    })
}

fn matches_filter(card: &jmap_client::jscontact::Card, q: &str) -> bool {
    let q = q.to_lowercase();
    card.display_name().to_lowercase().contains(&q)
        || card
            .primary_email()
            .is_some_and(|e| e.to_lowercase().contains(&q))
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
            Some(id) => get_contact_cards_cached(state, session, id).await?,
            None => Vec::new(),
        };
        let mut contacts: Vec<ContactRow> = cards
            .iter()
            .filter(|c| params.q.as_deref().is_none_or(|q| matches_filter(c, q)))
            .map(|c| {
                let name = c.display_name();
                let initial = name
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string();
                let id = c.id.clone().unwrap_or_default();
                ContactRow {
                    edit_href: format!(
                        "/app/contact/{id}/edit?{}",
                        params.query_string(ViewKind::Contacts, params.date)
                    ),
                    id,
                    initial,
                    name,
                    email: c.primary_email().map(|s| s.to_string()),
                    birthday_label: birthday_label(c),
                }
            })
            .collect();
        contacts.sort_by(|a, b| a.name.cmp(&b.name));
        return ContactsTemplate {
            contacts,
            q: params.q.clone().unwrap_or_default(),
            cal_param: params.cal_param(),
        }
        .render()
        .map_err(|e| AppError::bad_request(format!("template error: {e}")));
    }

    let mut events = fetch_events(session, state, params).await?;
    events.extend(fetch_ics_events(state, session, params).await);
    let birthdays = fetch_birthdays(state, session, params).await?;
    let holidays = fetch_holidays(state, params);
    let ics_colors = ics_colors_for(state, session);
    let inputs = view::BuildInputs {
        events: &events,
        calendars,
        birthdays: &birthdays,
        holidays: &holidays,
        ics_colors: &ics_colors,
        viewer_tz: state.viewer_tz,
        time_format: state.time_format,
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
            DayTemplate {
                day: d.day,
                hours: d.hours,
            }
            .render()
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
    edit_href: Option<String>,
    /// Set when this is an ICS subscription whose last fetch failed —
    /// shown as a small warning badge so a broken feed URL doesn't fail
    /// silently.
    error: Option<String>,
}

fn sidebar_calendars(
    calendars: &[Calendar],
    params: &ViewParams,
    has_contacts: bool,
    has_holidays: bool,
) -> Vec<SidebarCalendarVM> {
    let back_qs = params.query_string(params.view, params.date);
    let mut items: Vec<SidebarCalendarVM> = calendars
        .iter()
        .filter_map(|c| {
            let id = c.id.clone()?;
            Some(SidebarCalendarVM {
                color: view::calendar_color(Some(c), &id),
                visible: params.visible.contains(&id),
                toggle_href: params.toggle_href(&id),
                edit_href: Some(format!("/app/calendar/{id}/edit?{back_qs}")),
                name: c.name.clone(),
                error: None,
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
            edit_href: None,
            error: None,
        });
    }
    if has_holidays {
        items.push(SidebarCalendarVM {
            id: view::HOLIDAYS_PSEUDO_ID.to_string(),
            name: "Public Holidays".to_string(),
            color: view::HOLIDAYS_COLOR.to_string(),
            visible: params.visible.contains(view::HOLIDAYS_PSEUDO_ID),
            toggle_href: params.toggle_href(view::HOLIDAYS_PSEUDO_ID),
            edit_href: None,
            error: None,
        });
    }
    items
}

/// Reuses `SidebarCalendarVM` (id/name/color/visible/toggle_href/edit_href
/// are all it needs) for ICS-subscription rows in the sidebar's
/// "Subscriptions" section.
fn sidebar_ics_subscriptions(
    state: &AppState,
    subs: &[crate::users::IcsSubscription],
    params: &ViewParams,
) -> Vec<SidebarCalendarVM> {
    let back_qs = params.query_string(params.view, params.date);
    let mut items: Vec<SidebarCalendarVM> = subs
        .iter()
        .map(|s| {
            let id = crate::ics::pseudo_calendar_id(&s.id);
            let error = state.ics_cache.get(&s.id).and_then(|c| c.error.clone());
            SidebarCalendarVM {
                visible: params.visible.contains(&id),
                toggle_href: params.toggle_href(&id),
                edit_href: Some(format!("/app/ics/{}/edit?{back_qs}", s.id)),
                color: s.color.clone(),
                name: s.name.clone(),
                error,
                id,
            }
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    items
}

#[derive(Template)]
#[template(path = "calendar_list.html")]
struct CalendarListTemplate {
    calendars: Vec<SidebarCalendarVM>,
}

#[derive(Template)]
#[template(path = "calendar_list.html")]
struct IcsListTemplate {
    calendars: Vec<SidebarCalendarVM>,
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
    is_admin: bool,
    is_dated_view: bool,
    calendars: Vec<SidebarCalendarVM>,
    ics_subscriptions: Vec<SidebarCalendarVM>,
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
    is_admin: bool,
    is_dated_view: bool,
    calendars: Vec<SidebarCalendarVM>,
    ics_subscriptions: Vec<SidebarCalendarVM>,
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
    is_admin: bool,
    is_dated_view: bool,
    calendars: Vec<SidebarCalendarVM>,
    ics_subscriptions: Vec<SidebarCalendarVM>,
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
            is_admin: self.is_admin,
            is_dated_view: self.is_dated_view,
            calendars: self.calendars,
            ics_subscriptions: self.ics_subscriptions,
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
            is_admin: self.is_admin,
            is_dated_view: self.is_dated_view,
            calendars: self.calendars,
            ics_subscriptions: self.ics_subscriptions,
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
        username: session.app_username.clone(),
        is_admin: session.role == crate::users::Role::Admin,
        is_dated_view: params.view.is_dated(),
        calendars: sidebar_calendars(
            calendars,
            params,
            has_contacts,
            state.holidays_region.is_some(),
        ),
        ics_subscriptions: sidebar_ics_subscriptions(
            state,
            &ics_subscriptions_for(state, session),
            params,
        ),
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

/// Serves the service worker with its shell-asset URLs already substituted
/// with the current content hashes (see `crate::assets`), so a stale
/// service-worker install can never pin an old `app.css`/`app.js` version
/// forever — the precache list itself changes whenever those files do.
pub async fn service_worker() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        crate::assets::service_worker_body(),
    )
}

pub async fn app_view(
    State(state): State<AppState>,
    session: AuthedSession,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );

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

fn blank_form(
    calendars: &[Calendar],
    params: &ViewParams,
    viewer_tz: Tz,
    error: Option<String>,
) -> EventFormTemplate {
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
    let calendar_id = event
        .calendar_ids
        .as_ref()
        .and_then(|m| m.keys().next())
        .cloned();

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
                    (
                        "until",
                        u.date()
                            .map(|d| d.format("%Y-%m-%d").to_string())
                            .unwrap_or_default(),
                        5,
                    )
                } else if let Some(c) = rule.count {
                    ("count", params.date.format("%Y-%m-%d").to_string(), c)
                } else {
                    ("never", params.date.format("%Y-%m-%d").to_string(), 5)
                };
                (
                    freq.to_string(),
                    rule.interval.unwrap_or(1),
                    mode.to_string(),
                    until,
                    count,
                )
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
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let form = blank_form(&calendars, &params, state.viewer_tz, None);
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

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
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let events = session
        .client
        .get_events(&session.account_id, &[id])
        .await?;
    let event = events.first().ok_or(jmap_client::Error::NotFound)?;
    let form = event_to_form(event, &calendars, &params, None);
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

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
    let time = if all_day {
        "00:00"
    } else {
        form.start_time.as_str()
    };
    let start = parse_dt(&form.start_date, time).ok_or("invalid start date/time")?;

    let duration = if all_day {
        let start_date = NaiveDate::parse_from_str(&form.start_date, "%Y-%m-%d")
            .map_err(|_| "invalid start date")?;
        let end_date = NaiveDate::parse_from_str(&form.end_date, "%Y-%m-%d")
            .map_err(|_| "invalid end date")?;
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
                let until_date = NaiveDate::parse_from_str(&form.repeat_until, "%Y-%m-%d")
                    .map_err(|_| "invalid repeat end date")?;
                rule.until = Some(LocalDateTime::from_naive(
                    until_date.and_hms_opt(23, 59, 59).unwrap(),
                ));
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
    back_q: Option<&str>,
) -> Result<Response, AppError> {
    if is_hx(headers) {
        let today = today_in(state.viewer_tz);
        let calendars = session.client.get_calendars(&session.account_id).await?;
        let raw = RawViewQuery {
            view: Some(back_view.to_string()),
            date: Some(back_date.to_string()),
            cal: Some(back_cal.to_string()),
            q: back_q.map(|s| s.to_string()),
        };
        let params = resolve_view_params(
            &raw,
            &calendars,
            today,
            session.contacts_account_id.is_some(),
            &ics_subscription_ids(state, session),
            state.holidays_region.is_some(),
        );
        let fragment = render_fragment(session, state, &calendars, &params, today).await?;
        let body =
            format!(r#"<div id="view" class="view-container" hx-swap-oob="true">{fragment}</div>"#);
        Ok(Html(body).into_response())
    } else {
        let q_suffix = back_q
            .filter(|q| !q.is_empty())
            .map(|q| format!("&q={}", crate::webutil::urlencode(q)))
            .unwrap_or_default();
        Ok(Redirect::to(&format!(
            "/app?view={back_view}&date={back_date}&cal={back_cal}{q_suffix}"
        ))
        .into_response())
    }
}

pub async fn event_create(
    State(state): State<AppState>,
    session: AuthedSession,
    headers: HeaderMap,
    Form(form): Form<EventFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let uid = Uuid::new_v4().to_string();
    let event = match build_event_from_form(&form, uid) {
        Ok(e) => e,
        Err(msg) => return render_form_error(&state, &session, &form, false, None, msg).await,
    };
    session
        .client
        .create_event(&session.account_id, &event)
        .await?;
    mutation_response(
        &state,
        &session,
        &headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
        None,
    )
    .await
}

pub async fn event_update(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<EventFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let event = match build_event_from_form(&form, String::new()) {
        Ok(e) => e,
        Err(msg) => return render_form_error(&state, &session, &form, true, Some(id), msg).await,
    };
    let mut patch =
        serde_json::to_value(&event).map_err(|e| AppError::bad_request(e.to_string()))?;
    if let Some(obj) = patch.as_object_mut() {
        obj.remove("id");
        obj.remove("uid");
    }
    session
        .client
        .update_event(&session.account_id, &id, patch)
        .await?;
    mutation_response(
        &state,
        &session,
        &headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
        None,
    )
    .await
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
        q: None,
    };
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(state, session),
        state.holidays_region.is_some(),
    );
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
    let body = tpl
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Html(body).into_response())
}

#[derive(Debug, Deserialize)]
pub struct DeleteFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
}

pub async fn event_delete_hx(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(form): Query<DeleteFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    event_delete_inner(&state, &session, &id, &headers, &form).await
}

pub async fn event_delete_post(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<DeleteFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    event_delete_inner(&state, &session, &id, &headers, &form).await
}

async fn event_delete_inner(
    state: &AppState,
    session: &AuthedSession,
    id: &str,
    headers: &HeaderMap,
    form: &DeleteFormBody,
) -> Result<Response, AppError> {
    session
        .client
        .destroy_event(&session.account_id, id)
        .await?;
    mutation_response(
        state,
        session,
        headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
        None,
    )
    .await
}

// ---- Contact create/edit form ------------------------------------------

struct AddressBookOption {
    id: String,
    name: String,
    selected: bool,
}

fn address_book_options(
    address_books: &[AddressBook],
    selected: Option<&str>,
) -> Vec<AddressBookOption> {
    let selected = selected.or_else(|| address_books.first().and_then(|a| a.id.as_deref()));
    address_books
        .iter()
        .filter_map(|a| {
            let id = a.id.clone()?;
            let is_selected = Some(id.as_str()) == selected;
            Some(AddressBookOption {
                name: a.name.clone(),
                selected: is_selected,
                id,
            })
        })
        .collect()
}

#[derive(Template)]
#[template(path = "contact_form.html")]
struct ContactFormTemplate {
    is_edit: bool,
    contact_id: String,
    back_view: String,
    back_date: String,
    back_cal: String,
    back_q: String,
    address_books: Vec<AddressBookOption>,
    name: String,
    email: String,
    birthday_date: String,
    hide_birth_year: bool,
    error: Option<String>,
}

fn blank_contact_form(
    address_books: &[AddressBook],
    params: &ViewParams,
    error: Option<String>,
) -> ContactFormTemplate {
    ContactFormTemplate {
        is_edit: false,
        contact_id: String::new(),
        back_view: ViewKind::Contacts.as_str().to_string(),
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        back_q: params.q.clone().unwrap_or_default(),
        address_books: address_book_options(address_books, None),
        name: String::new(),
        email: String::new(),
        birthday_date: String::new(),
        hide_birth_year: false,
        error,
    }
}

fn contact_to_form(
    card: &Card,
    address_books: &[AddressBook],
    params: &ViewParams,
    error: Option<String>,
) -> ContactFormTemplate {
    let address_book_id = card
        .address_book_ids
        .as_ref()
        .and_then(|m| m.keys().next())
        .cloned();
    let (birthday_date, hide_birth_year) = card
        .anniversaries
        .as_ref()
        .and_then(|m| m.values().find(|a| a.kind.as_deref() == Some("birth")))
        .and_then(|a| a.date.month_day_year())
        .and_then(|(month, day, year)| {
            let date = NaiveDate::from_ymd_opt(year.unwrap_or(2000), month, day)?;
            Some((date.format("%Y-%m-%d").to_string(), year.is_none()))
        })
        .unwrap_or_default();

    ContactFormTemplate {
        is_edit: true,
        contact_id: card.id.clone().unwrap_or_default(),
        back_view: ViewKind::Contacts.as_str().to_string(),
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        back_q: params.q.clone().unwrap_or_default(),
        address_books: address_book_options(address_books, address_book_id.as_deref()),
        name: card
            .name
            .as_ref()
            .and_then(|n| n.full.clone())
            .unwrap_or_default(),
        email: card.primary_email().unwrap_or_default().to_string(),
        birthday_date,
        hide_birth_year,
        error,
    }
}

pub async fn contact_new_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let address_books = match &session.contacts_account_id {
        Some(id) => session.client.get_address_books(id).await?,
        None => Vec::new(),
    };
    let form = blank_contact_form(&address_books, &params, None);
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

pub async fn contact_edit_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let contacts_account_id = session
        .contacts_account_id
        .clone()
        .ok_or(jmap_client::Error::NotFound)?;
    let address_books = session
        .client
        .get_address_books(&contacts_account_id)
        .await?;
    let cards = get_contact_cards_cached(&state, &session, &contacts_account_id).await?;
    let card = cards
        .iter()
        .find(|c| c.id.as_deref() == Some(id.as_str()))
        .ok_or(jmap_client::Error::NotFound)?;
    let form = contact_to_form(card, &address_books, &params, None);
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

// ---- Contact bulk import --------------------------------------------------

struct ImportResult {
    imported: usize,
    skipped: usize,
}

#[derive(Template)]
#[template(path = "contact_import_form.html")]
struct ContactImportTemplate {
    back_date: String,
    back_cal: String,
    error: Option<String>,
    result: Option<ImportResult>,
}

pub async fn contact_import_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let form = ContactImportTemplate {
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        error: None,
        result: None,
    };
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

fn card_from_parsed_contact(c: &vcard::ParsedContact, uid: String, address_book_id: &str) -> Card {
    let mut card = Card::new(uid);
    card.name = Some(NameProperty {
        full: Some(c.name.clone()),
        ..Default::default()
    });
    if let Some(email) = &c.email {
        let mut emails = BTreeMap::new();
        emails.insert(
            "e1".to_string(),
            EmailAddress {
                address: email.clone(),
                ..Default::default()
            },
        );
        card.emails = Some(emails);
    }
    if let Some((date, has_year)) = c.birthday {
        let date_value = if has_year {
            AnniversaryDate::Timestamp {
                utc: format!("{}T00:00:00Z", date.format("%Y-%m-%d")),
            }
        } else {
            AnniversaryDate::PartialDate {
                year: None,
                month: Some(date.month()),
                day: Some(date.day()),
            }
        };
        let mut anniversaries = BTreeMap::new();
        anniversaries.insert(
            "bday".to_string(),
            Anniversary {
                type_: "Anniversary".to_string(),
                kind: Some("birth".to_string()),
                date: date_value,
                extra: BTreeMap::new(),
            },
        );
        card.anniversaries = Some(anniversaries);
    }
    let mut address_book_ids = BTreeMap::new();
    address_book_ids.insert(address_book_id.to_string(), true);
    card.address_book_ids = Some(address_book_ids);
    card
}

/// A single upload could otherwise turn into thousands of sequential
/// `ContactCard/set` calls; asking the user to split an unreasonably large
/// export is simpler than a background job for what's meant to be a quick
/// one-off import.
const MAX_IMPORT_CONTACTS: usize = 500;

pub async fn contact_import(
    State(state): State<AppState>,
    session: AuthedSession,
    headers: HeaderMap,
    mut multipart: axum::extract::Multipart,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let mut back_date = String::new();
    let mut back_cal = String::new();
    let mut file_text: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?
    {
        match field.name().unwrap_or("") {
            "back_date" => back_date = field.text().await.unwrap_or_default(),
            "back_cal" => back_cal = field.text().await.unwrap_or_default(),
            "file" => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::bad_request(e.to_string()))?;
                file_text = Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            _ => {}
        }
    }

    let render_error = |msg: &str| ContactImportTemplate {
        back_date: back_date.clone(),
        back_cal: back_cal.clone(),
        error: Some(msg.to_string()),
        result: None,
    };

    let Some(contacts_account_id) = session.contacts_account_id.clone() else {
        let body = render_error("this server does not support contacts")
            .render()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        return Ok(Html(body).into_response());
    };
    let Some(text) = file_text else {
        let body = render_error("choose a .vcf file to import")
            .render()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        return Ok(Html(body).into_response());
    };

    let parsed = vcard::parse_vcards(&text);
    if parsed.is_empty() {
        let body = render_error("no contacts found in that file")
            .render()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        return Ok(Html(body).into_response());
    }
    if parsed.len() > MAX_IMPORT_CONTACTS {
        let body = render_error(&format!(
            "that file has {} contacts; please split it into batches of {MAX_IMPORT_CONTACTS} or fewer",
            parsed.len()
        ))
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;
        return Ok(Html(body).into_response());
    }

    let address_books = session
        .client
        .get_address_books(&contacts_account_id)
        .await?;
    let Some(address_book_id) = address_books.first().and_then(|a| a.id.clone()) else {
        let body = render_error("no address book to import into")
            .render()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        return Ok(Html(body).into_response());
    };

    let mut imported = 0usize;
    let mut skipped = 0usize;
    for contact in &parsed {
        let uid = Uuid::new_v4().to_string();
        let card = card_from_parsed_contact(contact, uid, &address_book_id);
        match session
            .client
            .create_contact_card(&contacts_account_id, &card)
            .await
        {
            Ok(_) => imported += 1,
            Err(_) => skipped += 1,
        }
    }
    invalidate_contacts_cache(&state, &session, &contacts_account_id);

    let result_tpl = ContactImportTemplate {
        back_date: back_date.clone(),
        back_cal: back_cal.clone(),
        error: None,
        result: Some(ImportResult { imported, skipped }),
    };
    let result_html = result_tpl
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        let today = today_in(state.viewer_tz);
        let calendars = session.client.get_calendars(&session.account_id).await?;
        let raw = RawViewQuery {
            view: Some(ViewKind::Contacts.as_str().to_string()),
            date: Some(back_date.clone()),
            cal: Some(back_cal.clone()),
            q: None,
        };
        let params = resolve_view_params(
            &raw,
            &calendars,
            today,
            true,
            &ics_subscription_ids(&state, &session),
            state.holidays_region.is_some(),
        );
        let fragment = render_fragment(&session, &state, &calendars, &params, today).await?;
        let body = format!(
            r#"{result_html}<div id="view" class="view-container" hx-swap-oob="true">{fragment}</div>"#
        );
        Ok(Html(body).into_response())
    } else {
        let today = today_in(state.viewer_tz);
        let calendars = session.client.get_calendars(&session.account_id).await?;
        let raw = RawViewQuery {
            view: Some(ViewKind::Contacts.as_str().to_string()),
            date: Some(back_date),
            cal: Some(back_cal),
            q: None,
        };
        let params = resolve_view_params(
            &raw,
            &calendars,
            today,
            true,
            &ics_subscription_ids(&state, &session),
            state.holidays_region.is_some(),
        );
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(result_html));
        Ok(render(&ctx))
    }
}

// ---- Contact mutations --------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ContactFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
    pub back_q: String,
    pub address_book_id: String,
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub birthday_date: String,
    #[serde(default)]
    pub hide_birth_year: Option<String>,
}

fn build_card_from_form(form: &ContactFormBody, uid: String) -> Result<Card, String> {
    let name = form.name.trim().to_string();
    if name.is_empty() {
        return Err("name is required".to_string());
    }

    let mut card = Card::new(uid);
    card.name = Some(NameProperty {
        full: Some(name),
        ..Default::default()
    });
    if !form.email.is_empty() {
        let mut emails = BTreeMap::new();
        emails.insert(
            "e1".to_string(),
            EmailAddress {
                address: form.email.clone(),
                ..Default::default()
            },
        );
        card.emails = Some(emails);
    }
    if !form.birthday_date.is_empty() {
        let date = NaiveDate::parse_from_str(&form.birthday_date, "%Y-%m-%d")
            .map_err(|_| "invalid birthday date")?;
        let date_value = if form.hide_birth_year.is_some() {
            AnniversaryDate::PartialDate {
                year: None,
                month: Some(date.month()),
                day: Some(date.day()),
            }
        } else {
            AnniversaryDate::Timestamp {
                utc: format!("{}T00:00:00Z", date.format("%Y-%m-%d")),
            }
        };
        let mut anniversaries = BTreeMap::new();
        anniversaries.insert(
            "bday".to_string(),
            Anniversary {
                type_: "Anniversary".to_string(),
                kind: Some("birth".to_string()),
                date: date_value,
                extra: BTreeMap::new(),
            },
        );
        card.anniversaries = Some(anniversaries);
    }
    let mut address_book_ids = BTreeMap::new();
    address_book_ids.insert(form.address_book_id.clone(), true);
    card.address_book_ids = Some(address_book_ids);

    Ok(card)
}

async fn render_contact_form_error(
    state: &AppState,
    session: &AuthedSession,
    form: &ContactFormBody,
    is_edit: bool,
    contact_id: Option<String>,
    message: String,
) -> Result<Response, AppError> {
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let today = today_in(state.viewer_tz);
    let raw = RawViewQuery {
        view: Some(form.back_view.clone()),
        date: Some(form.back_date.clone()),
        cal: Some(form.back_cal.clone()),
        q: Some(form.back_q.clone()),
    };
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(state, session),
        state.holidays_region.is_some(),
    );
    let address_books = match &session.contacts_account_id {
        Some(id) => session.client.get_address_books(id).await?,
        None => Vec::new(),
    };
    let mut tpl = blank_contact_form(&address_books, &params, Some(message));
    tpl.is_edit = is_edit;
    tpl.contact_id = contact_id.unwrap_or_default();
    tpl.address_books = address_book_options(&address_books, Some(&form.address_book_id));
    tpl.name = form.name.clone();
    tpl.email = form.email.clone();
    tpl.birthday_date = form.birthday_date.clone();
    tpl.hide_birth_year = form.hide_birth_year.is_some();
    let body = tpl
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Html(body).into_response())
}

pub async fn contact_create(
    State(state): State<AppState>,
    session: AuthedSession,
    headers: HeaderMap,
    Form(form): Form<ContactFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let Some(contacts_account_id) = session.contacts_account_id.clone() else {
        return Err(AppError::bad_request(
            "this server does not support contacts",
        ));
    };
    let uid = Uuid::new_v4().to_string();
    let card = match build_card_from_form(&form, uid) {
        Ok(c) => c,
        Err(msg) => {
            return render_contact_form_error(&state, &session, &form, false, None, msg).await
        }
    };
    session
        .client
        .create_contact_card(&contacts_account_id, &card)
        .await?;
    invalidate_contacts_cache(&state, &session, &contacts_account_id);
    mutation_response(
        &state,
        &session,
        &headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
        Some(&form.back_q),
    )
    .await
}

pub async fn contact_update(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<ContactFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let Some(contacts_account_id) = session.contacts_account_id.clone() else {
        return Err(AppError::bad_request(
            "this server does not support contacts",
        ));
    };
    let card = match build_card_from_form(&form, String::new()) {
        Ok(c) => c,
        Err(msg) => {
            return render_contact_form_error(&state, &session, &form, true, Some(id), msg).await
        }
    };
    let mut patch =
        serde_json::to_value(&card).map_err(|e| AppError::bad_request(e.to_string()))?;
    if let Some(obj) = patch.as_object_mut() {
        obj.remove("id");
        obj.remove("uid");
    }
    session
        .client
        .update_contact_card(&contacts_account_id, &id, patch)
        .await?;
    invalidate_contacts_cache(&state, &session, &contacts_account_id);
    mutation_response(
        &state,
        &session,
        &headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
        Some(&form.back_q),
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct ContactDeleteFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
    pub back_q: String,
}

pub async fn contact_delete_hx(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(form): Query<ContactDeleteFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    contact_delete_inner(&state, &session, &id, &headers, &form).await
}

pub async fn contact_delete_post(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<ContactDeleteFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    contact_delete_inner(&state, &session, &id, &headers, &form).await
}

async fn contact_delete_inner(
    state: &AppState,
    session: &AuthedSession,
    id: &str,
    headers: &HeaderMap,
    form: &ContactDeleteFormBody,
) -> Result<Response, AppError> {
    let Some(contacts_account_id) = session.contacts_account_id.clone() else {
        return Err(AppError::bad_request(
            "this server does not support contacts",
        ));
    };
    session
        .client
        .destroy_contact_card(&contacts_account_id, id)
        .await?;
    invalidate_contacts_cache(state, session, &contacts_account_id);
    mutation_response(
        state,
        session,
        headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
        Some(&form.back_q),
    )
    .await
}

// ---- Calendar create/edit form ------------------------------------------

const DEFAULT_CALENDAR_COLOR: &str = "#3b82f6";

#[derive(Template)]
#[template(path = "calendar_form.html")]
struct CalendarFormTemplate {
    is_edit: bool,
    calendar_id: String,
    back_view: String,
    back_date: String,
    back_cal: String,
    back_q: String,
    name: String,
    color: String,
    description: String,
    error: Option<String>,
}

fn blank_calendar_form(params: &ViewParams, error: Option<String>) -> CalendarFormTemplate {
    CalendarFormTemplate {
        is_edit: false,
        calendar_id: String::new(),
        back_view: params.view.as_str().to_string(),
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        back_q: params.q.clone().unwrap_or_default(),
        name: String::new(),
        color: DEFAULT_CALENDAR_COLOR.to_string(),
        description: String::new(),
        error,
    }
}

fn calendar_to_form(
    calendar: &Calendar,
    params: &ViewParams,
    error: Option<String>,
) -> CalendarFormTemplate {
    CalendarFormTemplate {
        is_edit: true,
        calendar_id: calendar.id.clone().unwrap_or_default(),
        back_view: params.view.as_str().to_string(),
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        back_q: params.q.clone().unwrap_or_default(),
        name: calendar.name.clone(),
        color: calendar
            .color
            .clone()
            .unwrap_or_else(|| DEFAULT_CALENDAR_COLOR.to_string()),
        description: calendar.description.clone().unwrap_or_default(),
        error,
    }
}

pub async fn calendar_new_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let form = blank_calendar_form(&params, None);
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

pub async fn calendar_edit_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let calendar = calendars
        .iter()
        .find(|c| c.id.as_deref() == Some(id.as_str()))
        .ok_or(jmap_client::Error::NotFound)?;
    let form = calendar_to_form(calendar, &params, None);
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

// ---- Calendar mutations --------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CalendarFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
    #[serde(default)]
    pub back_q: String,
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub description: String,
}

fn build_calendar_from_form(form: &CalendarFormBody) -> Result<Calendar, String> {
    let name = form.name.trim().to_string();
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    let mut calendar = Calendar::new(name);
    if !form.color.is_empty() {
        calendar.color = Some(validate_color(&form.color)?);
    }
    if !form.description.is_empty() {
        calendar.description = Some(form.description.clone());
    }
    Ok(calendar)
}

async fn render_calendar_form_error(
    state: &AppState,
    session: &AuthedSession,
    form: &CalendarFormBody,
    is_edit: bool,
    calendar_id: Option<String>,
    message: String,
) -> Result<Response, AppError> {
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let today = today_in(state.viewer_tz);
    let raw = RawViewQuery {
        view: Some(form.back_view.clone()),
        date: Some(form.back_date.clone()),
        cal: Some(form.back_cal.clone()),
        q: Some(form.back_q.clone()),
    };
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(state, session),
        state.holidays_region.is_some(),
    );
    let mut tpl = blank_calendar_form(&params, Some(message));
    tpl.is_edit = is_edit;
    tpl.calendar_id = calendar_id.unwrap_or_default();
    tpl.name = form.name.clone();
    tpl.color = form.color.clone();
    tpl.description = form.description.clone();
    let body = tpl
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Html(body).into_response())
}

/// Unlike `mutation_response`, a calendar create/edit/delete can change the
/// sidebar's calendar list itself (name, color, or membership), so the htmx
/// path also OOB-swaps `#calendar-list` alongside the usual `#view` refresh.
async fn calendar_mutation_response(
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
            q: None,
        };
        let has_contacts = session.contacts_account_id.is_some();
        let params = resolve_view_params(
            &raw,
            &calendars,
            today,
            has_contacts,
            &ics_subscription_ids(state, session),
            state.holidays_region.is_some(),
        );
        let fragment = render_fragment(session, state, &calendars, &params, today).await?;
        let list_tpl = CalendarListTemplate {
            calendars: sidebar_calendars(
                &calendars,
                &params,
                has_contacts,
                state.holidays_region.is_some(),
            ),
        };
        let list_html = list_tpl
            .render()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        let body = format!(
            r#"<div id="view" class="view-container" hx-swap-oob="true">{fragment}</div><ul id="calendar-list" class="calendar-list" hx-swap-oob="true">{list_html}</ul>"#
        );
        Ok(Html(body).into_response())
    } else {
        Ok(Redirect::to(&format!(
            "/app?view={back_view}&date={back_date}&cal={back_cal}"
        ))
        .into_response())
    }
}

pub async fn calendar_create(
    State(state): State<AppState>,
    session: AuthedSession,
    headers: HeaderMap,
    Form(form): Form<CalendarFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let calendar = match build_calendar_from_form(&form) {
        Ok(c) => c,
        Err(msg) => {
            return render_calendar_form_error(&state, &session, &form, false, None, msg).await
        }
    };
    let created = session
        .client
        .create_calendar(&session.account_id, &calendar)
        .await?;
    let mut back_cal = form.back_cal.clone();
    if let Some(new_id) = &created.id {
        if !back_cal.split(',').any(|p| p == new_id) {
            if !back_cal.is_empty() {
                back_cal.push(',');
            }
            back_cal.push_str(new_id);
        }
    }
    calendar_mutation_response(
        &state,
        &session,
        &headers,
        &form.back_view,
        &form.back_date,
        &back_cal,
    )
    .await
}

pub async fn calendar_update(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<CalendarFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let calendar = match build_calendar_from_form(&form) {
        Ok(c) => c,
        Err(msg) => {
            return render_calendar_form_error(&state, &session, &form, true, Some(id), msg).await
        }
    };
    let mut patch =
        serde_json::to_value(&calendar).map_err(|e| AppError::bad_request(e.to_string()))?;
    if let Some(obj) = patch.as_object_mut() {
        obj.remove("id");
    }
    session
        .client
        .update_calendar(&session.account_id, &id, patch)
        .await?;
    calendar_mutation_response(
        &state,
        &session,
        &headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct CalendarDeleteFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
}

pub async fn calendar_delete_hx(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(form): Query<CalendarDeleteFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    calendar_delete_inner(&state, &session, &id, &headers, &form).await
}

pub async fn calendar_delete_post(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<CalendarDeleteFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    calendar_delete_inner(&state, &session, &id, &headers, &form).await
}

async fn calendar_delete_inner(
    state: &AppState,
    session: &AuthedSession,
    id: &str,
    headers: &HeaderMap,
    form: &CalendarDeleteFormBody,
) -> Result<Response, AppError> {
    session
        .client
        .destroy_calendar(&session.account_id, id)
        .await?;
    calendar_mutation_response(
        state,
        session,
        headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
    )
    .await
}

// ---- ICS-URL calendar subscriptions --------------------------------------

const DEFAULT_ICS_COLOR: &str = "#0ea5e9";

#[derive(Template)]
#[template(path = "ics_form.html")]
struct IcsFormTemplate {
    is_edit: bool,
    subscription_id: String,
    back_view: String,
    back_date: String,
    back_cal: String,
    back_q: String,
    name: String,
    color: String,
    url: String,
    error: Option<String>,
}

fn blank_ics_form(params: &ViewParams, error: Option<String>) -> IcsFormTemplate {
    IcsFormTemplate {
        is_edit: false,
        subscription_id: String::new(),
        back_view: params.view.as_str().to_string(),
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        back_q: params.q.clone().unwrap_or_default(),
        name: String::new(),
        color: DEFAULT_ICS_COLOR.to_string(),
        url: String::new(),
        error,
    }
}

fn ics_sub_to_form(
    sub: &crate::users::IcsSubscription,
    params: &ViewParams,
    error: Option<String>,
) -> IcsFormTemplate {
    IcsFormTemplate {
        is_edit: true,
        subscription_id: sub.id.clone(),
        back_view: params.view.as_str().to_string(),
        back_date: params.date.format("%Y-%m-%d").to_string(),
        back_cal: params.cal_param(),
        back_q: params.q.clone().unwrap_or_default(),
        name: sub.name.clone(),
        color: sub.color.clone(),
        url: sub.url.clone(),
        error,
    }
}

pub async fn ics_new_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let form = blank_ics_form(&params, None);
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

pub async fn ics_edit_form(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    Query(raw): Query<RawViewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let today = today_in(state.viewer_tz);
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(&state, &session),
        state.holidays_region.is_some(),
    );
    let sub = state
        .users
        .get_ics_subscription(&session.user_id, &id)
        .ok_or(jmap_client::Error::NotFound)?;
    let form = ics_sub_to_form(&sub, &params, None);
    let modal_html = form
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    if is_hx(&headers) {
        Ok(Html(modal_html).into_response())
    } else {
        let parts = build_shell_parts(&session, &state, &calendars, &params, today).await?;
        let ctx = parts.into_shell(Some(modal_html));
        Ok(render(&ctx))
    }
}

// ---- ICS mutations --------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct IcsFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
    #[serde(default)]
    pub back_q: String,
    pub name: String,
    #[serde(default)]
    pub color: String,
    pub url: String,
}

async fn validate_ics_form(form: &IcsFormBody) -> Result<(String, String, String), String> {
    let name = form.name.trim().to_string();
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    let url = form.url.trim().to_string();
    let parsed = url::Url::parse(&url).map_err(|_| "not a valid URL".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("URL must be http:// or https://".to_string());
    }
    let host = parsed.host_str().ok_or("URL has no host")?;
    let port = parsed
        .port_or_known_default()
        .ok_or("URL has no known port")?;
    crate::netguard::ensure_resolves_publicly(host, port).await?;
    let color = if form.color.is_empty() {
        DEFAULT_ICS_COLOR.to_string()
    } else {
        validate_color(&form.color)?
    };
    Ok((name, color, url))
}

/// A `#rgb` or `#rrggbb` hex color, matching what the `<input type="color">`
/// picker actually submits. Rejects anything else so a hand-crafted form
/// post can't smuggle extra CSS declarations into the `style="--cal-color:
/// ..."` attribute these values are later rendered into.
fn validate_color(s: &str) -> Result<String, String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    let valid_len = hex.len() == 3 || hex.len() == 6;
    if valid_len && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(format!("#{hex}"))
    } else {
        Err("color must be a hex value like #6366f1".to_string())
    }
}

async fn render_ics_form_error(
    state: &AppState,
    session: &AuthedSession,
    form: &IcsFormBody,
    is_edit: bool,
    subscription_id: Option<String>,
    message: String,
) -> Result<Response, AppError> {
    let calendars = session.client.get_calendars(&session.account_id).await?;
    let today = today_in(state.viewer_tz);
    let raw = RawViewQuery {
        view: Some(form.back_view.clone()),
        date: Some(form.back_date.clone()),
        cal: Some(form.back_cal.clone()),
        q: Some(form.back_q.clone()),
    };
    let params = resolve_view_params(
        &raw,
        &calendars,
        today,
        session.contacts_account_id.is_some(),
        &ics_subscription_ids(state, session),
        state.holidays_region.is_some(),
    );
    let mut tpl = blank_ics_form(&params, Some(message));
    tpl.is_edit = is_edit;
    tpl.subscription_id = subscription_id.unwrap_or_default();
    tpl.name = form.name.clone();
    tpl.color = form.color.clone();
    tpl.url = form.url.clone();
    let body = tpl
        .render()
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Html(body).into_response())
}

/// Like `calendar_mutation_response`: an ICS subscription create/edit/
/// delete can change the sidebar's "Subscriptions" list itself, so the htmx
/// path also OOB-swaps `#ics-subscription-list` alongside the usual `#view`
/// refresh.
async fn ics_mutation_response(
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
            q: None,
        };
        let has_contacts = session.contacts_account_id.is_some();
        let ics_ids = ics_subscription_ids(state, session);
        let params = resolve_view_params(
            &raw,
            &calendars,
            today,
            has_contacts,
            &ics_ids,
            state.holidays_region.is_some(),
        );
        let fragment = render_fragment(session, state, &calendars, &params, today).await?;
        let list_tpl = IcsListTemplate {
            calendars: sidebar_ics_subscriptions(
                state,
                &ics_subscriptions_for(state, session),
                &params,
            ),
        };
        let list_html = list_tpl
            .render()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        let body = format!(
            r#"<div id="view" class="view-container" hx-swap-oob="true">{fragment}</div><ul id="ics-subscription-list" class="calendar-list" hx-swap-oob="true">{list_html}</ul>"#
        );
        Ok(Html(body).into_response())
    } else {
        Ok(Redirect::to(&format!(
            "/app?view={back_view}&date={back_date}&cal={back_cal}"
        ))
        .into_response())
    }
}

pub async fn ics_create(
    State(state): State<AppState>,
    session: AuthedSession,
    headers: HeaderMap,
    Form(form): Form<IcsFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let (name, color, url) = match validate_ics_form(&form).await {
        Ok(v) => v,
        Err(msg) => return render_ics_form_error(&state, &session, &form, false, None, msg).await,
    };
    let sub = state
        .users
        .create_ics_subscription(&session.user_id, name, color, url);
    let new_id = sub.id.clone();

    let mut back_cal = form.back_cal.clone();
    let pseudo_id = crate::ics::pseudo_calendar_id(&new_id);
    if !back_cal.split(',').any(|p| p == pseudo_id) {
        if !back_cal.is_empty() {
            back_cal.push(',');
        }
        back_cal.push_str(&pseudo_id);
    }

    ics_mutation_response(
        &state,
        &session,
        &headers,
        &form.back_view,
        &form.back_date,
        &back_cal,
    )
    .await
}

pub async fn ics_update(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<IcsFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    let (name, color, url) = match validate_ics_form(&form).await {
        Ok(v) => v,
        Err(msg) => {
            return render_ics_form_error(&state, &session, &form, true, Some(id), msg).await
        }
    };
    let found = state
        .users
        .update_ics_subscription(&session.user_id, &id, name, color, url);
    if !found {
        return Err(jmap_client::Error::NotFound.into());
    }
    // The feed URL may have changed — drop the cached parse so the new URL
    // is fetched on the next render instead of waiting out the TTL.
    state.ics_cache.remove(&id);

    ics_mutation_response(
        &state,
        &session,
        &headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct IcsDeleteFormBody {
    pub back_view: String,
    pub back_date: String,
    pub back_cal: String,
}

pub async fn ics_delete_hx(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(form): Query<IcsDeleteFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    ics_delete_inner(&state, &session, &id, &headers, &form).await
}

pub async fn ics_delete_post(
    State(state): State<AppState>,
    session: AuthedSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<IcsDeleteFormBody>,
) -> Result<Response, AppError> {
    let state = crate::state::apply_display_prefs(state, &session.prefs);
    ics_delete_inner(&state, &session, &id, &headers, &form).await
}

async fn ics_delete_inner(
    state: &AppState,
    session: &AuthedSession,
    id: &str,
    headers: &HeaderMap,
    form: &IcsDeleteFormBody,
) -> Result<Response, AppError> {
    state.users.delete_ics_subscription(&session.user_id, id);
    state.ics_cache.remove(id);
    ics_mutation_response(
        state,
        session,
        headers,
        &form.back_view,
        &form.back_date,
        &form.back_cal,
    )
    .await
}

// ---- Settings page ---------------------------------------------------------

/// English label for each `state::ALL_GERMAN_REGIONS` entry — the internal
/// names are the exact Rust enum variant names, which read fine in code but
/// aren't great UI copy (e.g. "MechlenburgVorpommern").
const GERMAN_REGION_LABELS: &[(&str, &str)] = &[
    ("BadenWuerttemberg", "Baden-Württemberg"),
    ("Bayern", "Bavaria (Bayern)"),
    ("Berlin", "Berlin"),
    ("Brandenburg", "Brandenburg"),
    ("Bremen", "Bremen"),
    ("Hamburg", "Hamburg"),
    ("Hessen", "Hesse (Hessen)"),
    ("MechlenburgVorpommern", "Mecklenburg-Vorpommern"),
    ("Niedersachsen", "Lower Saxony (Niedersachsen)"),
    ("NordrheinWestfalen", "North Rhine-Westphalia (NRW)"),
    ("RheinlandPfalz", "Rhineland-Palatinate"),
    ("Saarland", "Saarland"),
    ("Sachsen", "Saxony (Sachsen)"),
    ("SachsenAnhalt", "Saxony-Anhalt"),
    ("SchleswigHolstein", "Schleswig-Holstein"),
    ("Thueringen", "Thuringia (Thüringen)"),
];

struct SelectOption {
    value: String,
    label: String,
    selected: bool,
}

fn holiday_region_options(selected: &str) -> Vec<SelectOption> {
    let mut opts = vec![
        SelectOption {
            value: String::new(),
            label: "Use server default".to_string(),
            selected: selected.is_empty(),
        },
        SelectOption {
            value: "none".to_string(),
            label: "None (disabled)".to_string(),
            selected: selected == "none",
        },
    ];
    for &(value, label) in GERMAN_REGION_LABELS {
        opts.push(SelectOption {
            selected: selected == value,
            value: value.to_string(),
            label: label.to_string(),
        });
    }
    opts
}

fn display_tz_options(selected: &str) -> Vec<TzOption> {
    let mut opts = vec![TzOption {
        value: String::new(),
        label: "Use server default".to_string(),
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

fn time_format_options(selected: &str) -> Vec<SelectOption> {
    vec![
        SelectOption {
            value: String::new(),
            label: "Use server default".to_string(),
            selected: selected.is_empty(),
        },
        SelectOption {
            value: "12h".to_string(),
            label: "12-hour (3:45 PM)".to_string(),
            selected: selected == "12h",
        },
        SelectOption {
            value: "24h".to_string(),
            label: "24-hour (15:45)".to_string(),
            selected: selected == "24h",
        },
    ]
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    timezones: Vec<TzOption>,
    holiday_regions: Vec<SelectOption>,
    time_formats: Vec<SelectOption>,
    server_default_timezone: String,
    server_default_holidays: String,
    server_default_time_format: String,
    saved: bool,
    jmap_server_url: String,
    jmap_username: String,
    jmap_password: String,
    jmap_token: String,
    password_error: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct SettingsQuery {
    #[serde(default)]
    saved: bool,
}

fn server_default_labels(state: &AppState) -> (String, String, String) {
    let holidays = state
        .holidays_region
        .map(crate::state::german_region_name)
        .map(|name| {
            GERMAN_REGION_LABELS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, label)| label.to_string())
                .unwrap_or_else(|| name.to_string())
        })
        .unwrap_or_else(|| "None".to_string());
    let time_format = match state.time_format {
        crate::state::TimeFormat::Twelve => "12-hour".to_string(),
        crate::state::TimeFormat::TwentyFour => "24-hour".to_string(),
    };
    (state.viewer_tz.to_string(), holidays, time_format)
}

pub async fn settings_form(
    State(state): State<AppState>,
    user: AppUser,
    Query(q): Query<SettingsQuery>,
) -> Response {
    let jmap = state
        .users
        .get_user(&user.user_id)
        .map(|u| u.jmap)
        .unwrap_or_default();
    let (server_default_timezone, server_default_holidays, server_default_time_format) =
        server_default_labels(&state);
    render(&SettingsTemplate {
        timezones: display_tz_options(&user.prefs.timezone),
        holiday_regions: holiday_region_options(&user.prefs.holidays_region),
        time_formats: time_format_options(&user.prefs.time_format),
        server_default_timezone,
        server_default_holidays,
        server_default_time_format,
        saved: q.saved,
        jmap_server_url: jmap.server_url,
        jmap_username: jmap.username,
        jmap_password: jmap.password,
        jmap_token: jmap.token,
        password_error: None,
    })
    .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct SettingsFormBody {
    #[serde(default)]
    pub timezone: String,
    #[serde(default)]
    pub holidays_region: String,
    #[serde(default)]
    pub time_format: String,
    #[serde(default)]
    pub jmap_server_url: String,
    #[serde(default)]
    pub jmap_username: String,
    #[serde(default)]
    pub jmap_password: String,
    #[serde(default)]
    pub jmap_token: String,
    #[serde(default)]
    pub new_password: String,
    #[serde(default)]
    pub new_password_confirm: String,
}

pub async fn settings_save(
    State(state): State<AppState>,
    user: AppUser,
    headers: HeaderMap,
    Form(form): Form<SettingsFormBody>,
) -> Response {
    if !form.new_password.is_empty() || !form.new_password_confirm.is_empty() {
        if form.new_password != form.new_password_confirm {
            return password_change_error(&state, &user, "Passwords do not match.");
        }
        if form.new_password.len() < 8 {
            return password_change_error(&state, &user, "Password must be at least 8 characters.");
        }
        let hash = crate::users::hash_password(&form.new_password);
        state
            .users
            .update_user(&user.user_id, |u| u.password_hash = hash);
    }

    let jmap = crate::users::JmapSettings {
        server_url: form.jmap_server_url.trim().to_string(),
        username: form.jmap_username.trim().to_string(),
        password: form.jmap_password,
        token: form.jmap_token.trim().to_string(),
    };
    let prefs = crate::users::DisplayPrefs {
        timezone: form.timezone.clone(),
        time_format: form.time_format.clone(),
        holidays_region: form.holidays_region.clone(),
    };
    state.users.update_user(&user.user_id, |u| {
        u.jmap = jmap;
        u.prefs = prefs;
    });
    state.jmap_clients.remove(&user.user_id);

    crate::webutil::redirect("/app/settings?saved=true", &headers)
}

fn password_change_error(state: &AppState, user: &AppUser, msg: &str) -> Response {
    let jmap = state
        .users
        .get_user(&user.user_id)
        .map(|u| u.jmap)
        .unwrap_or_default();
    let (server_default_timezone, server_default_holidays, server_default_time_format) =
        server_default_labels(state);
    render(&SettingsTemplate {
        timezones: display_tz_options(&user.prefs.timezone),
        holiday_regions: holiday_region_options(&user.prefs.holidays_region),
        time_formats: time_format_options(&user.prefs.time_format),
        server_default_timezone,
        server_default_holidays,
        server_default_time_format,
        saved: false,
        jmap_server_url: jmap.server_url,
        jmap_username: jmap.username,
        jmap_password: jmap.password,
        jmap_token: jmap.token,
        password_error: Some(msg.to_string()),
    })
    .into_response()
}
