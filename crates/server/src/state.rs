use std::sync::Arc;

use dashmap::DashMap;
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
}

impl AppState {
    pub fn new() -> Self {
        let viewer_tz = std::env::var("JSCAL_TIMEZONE")
            .ok()
            .and_then(|s| jmap_client::tz::parse_tz(&s))
            .unwrap_or(Tz::UTC);
        let demo_login = std::env::var("JSCAL_DEMO_SERVER_URL").ok().map(|server_url| DemoLogin {
            server_url,
            username: std::env::var("JSCAL_DEMO_USERNAME").unwrap_or_else(|_| "demo".to_string()),
            password: std::env::var("JSCAL_DEMO_PASSWORD").unwrap_or_else(|_| "demo".to_string()),
        });
        Self {
            sessions: Arc::new(DashMap::new()),
            viewer_tz,
            demo_login,
        }
    }
}
