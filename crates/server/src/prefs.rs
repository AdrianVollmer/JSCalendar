//! Per-user display preferences (timezone, holidays region, time format),
//! overriding the server-wide `JSCAL_TIMEZONE`/`JSCAL_HOLIDAYS_REGION`/
//! `JSCAL_TIME_FORMAT` defaults.
//!
//! Set from the settings page (`/app/settings`) as a long-lived cookie —
//! deliberately separate from the login session cookie, so preferences
//! survive logging out and back in — and mirrored into `localStorage` by a
//! small script (see `settings.html`) so they also survive the cookie
//! being cleared independently. The cookie remains the thing the server
//! actually reads: it's what makes preferences apply to server-rendered
//! pages even with JavaScript disabled.
//!
//! `apply` folds a request's preferences onto a request-scoped `AppState`
//! clone, so every existing call site that already reads
//! `state.viewer_tz`/`state.holidays_region`/`state.time_format` picks up
//! the per-user override for free, with no further plumbing.

use axum::http::HeaderMap;
use axum_extra::extract::cookie::CookieJar;

use crate::state::{AppState, TimeFormat};

pub const PREFS_COOKIE: &str = "jscal_prefs";

#[derive(Debug, Clone, Default)]
pub struct UserPrefs {
    pub timezone: Option<jmap_client::tz::Tz>,
    /// `Some(None)` means "explicitly no holidays calendar", distinct from
    /// `None` ("no override — use the server default").
    pub holidays_region: Option<Option<holiday_de::GermanRegion>>,
    pub time_format: Option<TimeFormat>,
}

impl UserPrefs {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let jar = CookieJar::from_headers(headers);
        match jar.get(PREFS_COOKIE) {
            Some(cookie) => Self::parse(cookie.value()),
            None => Self::default(),
        }
    }

    fn parse(raw: &str) -> Self {
        let mut prefs = Self::default();
        for pair in raw.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "tz" => prefs.timezone = jmap_client::tz::parse_tz(value),
                "hr" => {
                    prefs.holidays_region = Some(if value == "none" {
                        None
                    } else {
                        crate::state::parse_german_region(value)
                    })
                }
                "tf" => prefs.time_format = TimeFormat::parse(value),
                _ => {}
            }
        }
        prefs
    }
}

/// Builds the cookie value the settings form saves. `holidays_region` is
/// `""` for "use the server default" or `"none"` for "explicitly off";
/// any other value must be a valid `ALL_GERMAN_REGIONS` name.
pub fn encode(timezone: &str, holidays_region: &str, time_format: &str) -> String {
    let mut parts = Vec::new();
    if !timezone.is_empty() {
        parts.push(format!("tz={timezone}"));
    }
    if !holidays_region.is_empty() {
        parts.push(format!("hr={holidays_region}"));
    }
    if !time_format.is_empty() {
        parts.push(format!("tf={time_format}"));
    }
    parts.join("&")
}

/// Returns a request-scoped `AppState` with `viewer_tz`/`holidays_region`/
/// `time_format` overridden by the caller's preferences cookie, if any.
/// Cheap: `AppState`'s fields are all `Arc`/`Copy`, so this is just a
/// shallow clone with a few fields swapped.
pub fn apply(state: AppState, headers: &HeaderMap) -> AppState {
    let prefs = UserPrefs::from_headers(headers);
    let mut state = state;
    if let Some(tz) = prefs.timezone {
        state.viewer_tz = tz;
    }
    if let Some(region) = prefs.holidays_region {
        state.holidays_region = region;
    }
    if let Some(tf) = prefs.time_format {
        state.time_format = tf;
    }
    state
}
