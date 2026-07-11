use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
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
fn parse_german_region(s: &str) -> Option<holiday_de::GermanRegion> {
    use holiday_de::GermanRegion::*;
    Some(match s {
        "BadenWuerttemberg" => BadenWuerttemberg,
        "Bayern" => Bayern,
        "Berlin" => Berlin,
        "Brandenburg" => Brandenburg,
        "Bremen" => Bremen,
        "Hamburg" => Hamburg,
        "Hessen" => Hessen,
        "MechlenburgVorpommern" => MechlenburgVorpommern,
        "Niedersachsen" => Niedersachsen,
        "NordrheinWestfalen" => NordrheinWestfalen,
        "RheinlandPfalz" => RheinlandPfalz,
        "Saarland" => Saarland,
        "Sachsen" => Sachsen,
        "SachsenAnhalt" => SachsenAnhalt,
        "SchleswigHolstein" => SchleswigHolstein,
        "Thueringen" => Thueringen,
        _ => return None,
    })
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
        }
    }
}
