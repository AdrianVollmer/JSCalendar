use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use jmap_client::jscalendar::CalendarEvent;
use jmap_client::jscontact::Card;
use jmap_client::tz::Tz;
use jmap_client::Client;

use crate::users::UserStore;

/// A connected JMAP client cached per app user (see `AppState::jmap_clients`),
/// established lazily from that user's stored `JmapSettings` and reused
/// across every request/session for as long as it's valid.
#[derive(Clone)]
pub struct UserSession {
    pub client: Client,
    pub account_id: String,
    /// `None` when the JMAP server doesn't advertise Contacts support —
    /// birthdays and the contacts list are simply hidden in that case.
    pub contacts_account_id: Option<String>,
}

/// Reads `var`, preferring `{var}_FILE` (a path to a file holding the
/// value) when set. This is the safer option for containers/orchestrators:
/// a mounted secret file isn't visible in `docker inspect`, `ps`, or
/// `/proc/[pid]/environ` the way a plain environment variable is.
pub(crate) fn read_secret(var: &str) -> Option<String> {
    if let Ok(path) = std::env::var(format!("{var}_FILE")) {
        return std::fs::read_to_string(&path)
            .map(|s| s.trim().to_string())
            .ok();
    }
    std::env::var(var).ok()
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
/// so (unlike user accounts, which now do persist) the subscription list
/// lives only in memory and is lost on restart.
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

/// Pre-fills the login form's username/password when `JSCAL_ADMIN_PASSWORD`
/// is set, so a demo/test instance needs zero typing to sign in as the
/// bootstrap admin. There's no default, so a normal deployment's login page
/// stays blank.
#[derive(Clone)]
pub struct DemoLogin {
    pub username: String,
    pub password: String,
}

/// Shared server state. User accounts and sessions persist to disk (see
/// `crate::users`); everything else here (JMAP data caches, ICS
/// subscriptions) stays in-memory and is lost on restart, since it's
/// either cheap to re-fetch from the JMAP server or, for ICS subscriptions,
/// already documented as ephemeral.
#[derive(Clone)]
pub struct AppState {
    pub users: Arc<UserStore>,
    /// A connected JMAP client per app user, keyed by user id and
    /// established lazily from that user's stored `JmapSettings`. Removed
    /// (forcing a reconnect) whenever a user's JMAP settings change.
    pub jmap_clients: Arc<DashMap<String, UserSession>>,
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
    /// Recent failed login attempts, keyed by lowercased username, used to
    /// lock an account out for a short window after too many failures in a
    /// row (see `auth::do_login`). Grows one entry per distinct username
    /// ever attempted; unbounded in principle, but each entry is a few
    /// bytes and this is an acceptable trade-off for a self-hosted app.
    pub login_attempts: Arc<DashMap<String, LoginAttemptState>>,
}

#[derive(Debug, Default)]
pub struct LoginAttemptState {
    pub failures: u32,
    pub last_failure: Option<Instant>,
}

impl AppState {
    pub fn new() -> Self {
        let viewer_tz = std::env::var("JSCAL_TIMEZONE")
            .ok()
            .and_then(|s| jmap_client::tz::parse_tz(&s))
            .unwrap_or(Tz::UTC);
        let holidays_region = std::env::var("JSCAL_HOLIDAYS_REGION")
            .ok()
            .and_then(|s| parse_german_region(&s));
        let time_format = std::env::var("JSCAL_TIME_FORMAT")
            .ok()
            .and_then(|s| TimeFormat::parse(&s))
            .unwrap_or_default();

        let data_dir = std::env::var("JSCAL_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
        let users = Arc::new(UserStore::load(PathBuf::from(data_dir).join("users.json")));
        crate::users::ensure_bootstrap_admin(&users);
        let demo_login = read_secret("JSCAL_ADMIN_PASSWORD").map(|password| DemoLogin {
            username: crate::users::BOOTSTRAP_ADMIN_USERNAME.to_string(),
            password,
        });

        Self {
            users,
            jmap_clients: Arc::new(DashMap::new()),
            viewer_tz,
            demo_login,
            contacts_cache: Arc::new(DashMap::new()),
            ics_subscriptions: Arc::new(DashMap::new()),
            ics_cache: Arc::new(DashMap::new()),
            // No redirects: this client only ever fetches user-supplied
            // ICS-subscription URLs (see `crate::netguard`), and a
            // followed redirect would land on a host we never ran the
            // private-address check against — a classic SSRF-via-redirect
            // bypass. A redirected feed just fails with a clear error
            // instead of being fetched blind.
            http_client: reqwest::Client::builder()
                .user_agent("jscalendar-server")
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            holidays_region,
            time_format,
            login_attempts: Arc::new(DashMap::new()),
        }
    }
}
