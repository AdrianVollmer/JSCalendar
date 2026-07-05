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
}

impl AppState {
    pub fn new() -> Self {
        let viewer_tz = std::env::var("JSCAL_TIMEZONE")
            .ok()
            .and_then(|s| jmap_client::tz::parse_tz(&s))
            .unwrap_or(Tz::UTC);
        Self {
            sessions: Arc::new(DashMap::new()),
            viewer_tz,
        }
    }
}
