use std::sync::{Arc, RwLock};
use std::time::Instant;

use dashmap::DashMap;
use jmap_client::client::Credentials;
use jmap_client::jscalendar::CalendarEvent;
use jmap_client::jscontact::Card;
use jmap_client::tz::Tz;
use jmap_client::Client;

#[derive(Clone)]
pub struct UserSession {
    pub client: Client,
    pub account_id: String,
    pub username: String,
    /// `None` when the JMAP server doesn't advertise Contacts support —
    /// birthdays and the contacts list are simply hidden in that case.
    pub contacts_account_id: Option<String>,
}

/// Configuration for transparently authenticating every visitor who
/// doesn't already have their own session, instead of showing the login
/// page — for single-tenant deployments where the operator supplies the
/// one JMAP account up front (`JSCAL_SERVER_URL` + `JSCAL_USERNAME`/
/// `JSCAL_PASSWORD` or `JSCAL_TOKEN`).
#[derive(Clone)]
pub struct AutoLoginConfig {
    pub server_url: String,
    pub credentials: Credentials,
}

/// Reads `var`, preferring `{var}_FILE` (a path to a file holding the
/// value) when set. This is the safer option for containers/orchestrators:
/// a mounted secret file isn't visible in `docker inspect`, `ps`, or
/// `/proc/[pid]/environ` the way a plain environment variable is.
fn read_secret(var: &str) -> Option<String> {
    if let Ok(path) = std::env::var(format!("{var}_FILE")) {
        return std::fs::read_to_string(&path)
            .map(|s| s.trim().to_string())
            .ok();
    }
    std::env::var(var).ok()
}

fn read_auto_login() -> Option<AutoLoginConfig> {
    let server_url = std::env::var("JSCAL_SERVER_URL").ok()?;
    let token = read_secret("JSCAL_TOKEN").filter(|t| !t.is_empty());
    let credentials = if let Some(token) = token {
        Credentials::Bearer(token)
    } else {
        let username = std::env::var("JSCAL_USERNAME").ok();
        let password = read_secret("JSCAL_PASSWORD");
        match (username, password) {
            (Some(username), Some(password)) => Credentials::Basic { username, password },
            _ => {
                tracing::warn!(
                    "JSCAL_SERVER_URL is set but neither JSCAL_TOKEN nor \
                     JSCAL_USERNAME+JSCAL_PASSWORD are — auto-login disabled"
                );
                return None;
            }
        }
    };
    Some(AutoLoginConfig {
        server_url,
        credentials,
    })
}

/// How long a fetched contact list is trusted before re-fetching. There's
/// no JMAP filter for "has a birthday in this range" — `ContactCard/get`
/// always returns the whole address book — so the address book is cached
/// rather than re-fetched on every render. Contact data changes rarely, so
/// this trades a little staleness (an edit made from another client can
/// take up to this long to show up here) for turning "every calendar view
/// render" into "at most once per this interval."
pub const CONTACTS_CACHE_TTL_SECS: u64 = 15 * 60;

pub struct CachedContacts {
    pub cards: Vec<Card>,
    pub fetched_at: Instant,
}

/// A subscribed read-only calendar sourced from an external `.ics` URL.
/// Not a real JMAP calendar — there's no upstream server to store this on,
/// so (matching this app's "no persistence by design" stance for anything
/// that isn't the JMAP server's own data) the subscription list lives only
/// in memory and is lost on restart.
#[derive(Debug, Clone)]
pub struct IcsSubscription {
    pub id: String,
    pub name: String,
    pub color: String,
    pub url: String,
}

/// How long a fetched-and-parsed `.ics` feed is trusted before re-fetching
/// — the "regularly update by fetching" refresh, done lazily on read rather
/// than via a background scheduler (same lazy-TTL shape as the contacts
/// cache above).
pub const ICS_CACHE_TTL_SECS: u64 = 30 * 60;

pub struct CachedIcsEvents {
    pub events: Vec<CalendarEvent>,
    pub fetched_at: Instant,
    /// Set when the last fetch/parse failed, so the view can show a subtle
    /// indicator instead of silently showing stale or empty data forever.
    pub error: Option<String>,
}

/// Parses `JSCAL_HOLIDAYS_REGION`'s value into a `holiday_de::GermanRegion`
/// variant name (e.g. `BadenWuerttemberg`, `Bayern`, `NordrheinWestfalen`).
/// Unset or unrecognized means no holidays pseudo-calendar at all — this is
/// opt-in per deployment, not a default, since public holidays are specific
/// to wherever the server operator actually is.
pub(crate) fn parse_german_region(s: &str) -> Option<holiday_de::GermanRegion> {
    ALL_GERMAN_REGIONS
        .iter()
        .find(|(name, _)| *name == s)
        .map(|(_, region)| *region)
}

/// The inverse of `parse_german_region`.
pub(crate) fn german_region_name(region: holiday_de::GermanRegion) -> &'static str {
    ALL_GERMAN_REGIONS
        .iter()
        .find(|(_, r)| *r == region)
        .map(|(name, _)| *name)
        .unwrap_or("")
}

/// Every `GermanRegion` paired with the exact string `parse_german_region`
/// accepts for it — used both there and to populate the settings page's
/// region picker without duplicating the list.
pub(crate) const ALL_GERMAN_REGIONS: &[(&str, holiday_de::GermanRegion)] = {
    use holiday_de::GermanRegion::*;
    &[
        ("BadenWuerttemberg", BadenWuerttemberg),
        ("Bayern", Bayern),
        ("Berlin", Berlin),
        ("Brandenburg", Brandenburg),
        ("Bremen", Bremen),
        ("Hamburg", Hamburg),
        ("Hessen", Hessen),
        ("MechlenburgVorpommern", MechlenburgVorpommern),
        ("Niedersachsen", Niedersachsen),
        ("NordrheinWestfalen", NordrheinWestfalen),
        ("RheinlandPfalz", RheinlandPfalz),
        ("Saarland", Saarland),
        ("Sachsen", Sachsen),
        ("SachsenAnhalt", SachsenAnhalt),
        ("SchleswigHolstein", SchleswigHolstein),
        ("Thueringen", Thueringen),
    ]
};

/// Which clock style to render event/hour times in (`JSCAL_TIME_FORMAT` env
/// var, default 12-hour) — overridable per-user from the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeFormat {
    #[default]
    Twelve,
    TwentyFour,
}

impl TimeFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            TimeFormat::Twelve => "12h",
            TimeFormat::TwentyFour => "24h",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "12h" => Some(TimeFormat::Twelve),
            "24h" => Some(TimeFormat::TwentyFour),
            _ => None,
        }
    }
}

/// Pre-fills the login form so a demo/test instance (e.g. wired up to
/// `mock-jmap-server`) needs zero typing to sign in. Only set via
/// `JSCAL_DEMO_SERVER_URL` — there's no default, so a normal deployment's
/// login page stays blank.
#[derive(Clone)]
pub struct DemoLogin {
    pub server_url: String,
    pub username: String,
    pub password: String,
}

/// Shared server state: an in-memory table of logged-in sessions, keyed by
/// an opaque cookie value. There is no persistence by design — restarting
/// the server simply signs everyone out, and credentials never touch disk.
#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<DashMap<String, UserSession>>,
    /// The IANA zone all views are rendered in (`JSCAL_TIMEZONE` env var,
    /// default UTC). JSCalendar events carry their own zone; this is only
    /// the *display* zone since a single-user server has no per-request
    /// notion of the browser's zone without client-side JS.
    pub viewer_tz: Tz,
    pub demo_login: Option<DemoLogin>,
    /// Keyed by `{api_url}#{account_id}` (not just account_id, since that's
    /// only unique within one JMAP server) so two different accounts never
    /// collide even if their ids happen to match.
    pub contacts_cache: Arc<DashMap<String, CachedContacts>>,
    /// Subscribed iCal-URL calendars, keyed the same way as `contacts_cache`.
    pub ics_subscriptions: Arc<DashMap<String, Vec<IcsSubscription>>>,
    /// Parsed events from each subscription's last successful fetch, keyed
    /// by subscription id (globally unique, no account-key prefix needed).
    pub ics_cache: Arc<DashMap<String, CachedIcsEvents>>,
    pub http_client: reqwest::Client,
    /// The German federal state to show public holidays for (`JSCAL_HOLIDAYS_REGION`
    /// env var), or `None` to not offer a holidays pseudo-calendar at all.
    pub holidays_region: Option<holiday_de::GermanRegion>,
    pub time_format: TimeFormat,
    /// Set from `JSCAL_SERVER_URL` + credentials; when present, visitors
    /// without their own session cookie are transparently authenticated
    /// using it instead of being shown the login page.
    pub auto_login: Option<AutoLoginConfig>,
    /// The shared session established from `auto_login`, connected lazily
    /// on first use and reused by every cookie-less visitor after that.
    pub auto_session: Arc<RwLock<Option<UserSession>>>,
}

impl AppState {
    pub fn new() -> Self {
        let viewer_tz = std::env::var("JSCAL_TIMEZONE")
            .ok()
            .and_then(|s| jmap_client::tz::parse_tz(&s))
            .unwrap_or(Tz::UTC);
        let demo_login = std::env::var("JSCAL_DEMO_SERVER_URL")
            .ok()
            .map(|server_url| DemoLogin {
                server_url,
                username: std::env::var("JSCAL_DEMO_USERNAME")
                    .unwrap_or_else(|_| "demo".to_string()),
                password: std::env::var("JSCAL_DEMO_PASSWORD")
                    .unwrap_or_else(|_| "demo".to_string()),
            });
        let holidays_region = std::env::var("JSCAL_HOLIDAYS_REGION")
            .ok()
            .and_then(|s| parse_german_region(&s));
        let time_format = std::env::var("JSCAL_TIME_FORMAT")
            .ok()
            .and_then(|s| TimeFormat::parse(&s))
            .unwrap_or_default();
        let auto_login = read_auto_login();
        Self {
            sessions: Arc::new(DashMap::new()),
            viewer_tz,
            demo_login,
            contacts_cache: Arc::new(DashMap::new()),
            ics_subscriptions: Arc::new(DashMap::new()),
            ics_cache: Arc::new(DashMap::new()),
            http_client: reqwest::Client::builder()
                .user_agent("jscalendar-server")
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            holidays_region,
            time_format,
            auto_login,
            auto_session: Arc::new(RwLock::new(None)),
        }
    }
}
