use askama::Template;
use axum::extract::{FromRef, FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use jmap_client::client::{Client, Credentials};
use serde::Deserialize;
use uuid::Uuid;

use crate::state::{AppState, UserSession};
use crate::webutil::{render, urlencode};

const COOKIE_NAME: &str = "jscal_sid";

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: Option<String>,
    server_url: String,
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    error: Option<String>,
}

pub async fn login_form(State(state): State<AppState>, Query(q): Query<LoginQuery>) -> Response {
    let demo = state.demo_login.clone();
    render(&LoginTemplate {
        error: q.error,
        server_url: demo
            .as_ref()
            .map(|d| d.server_url.clone())
            .unwrap_or_default(),
        username: demo
            .as_ref()
            .map(|d| d.username.clone())
            .unwrap_or_default(),
        password: demo.map(|d| d.password).unwrap_or_default(),
    })
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Either a full session URL or just a host/domain; `/.well-known/jmap`
    /// is appended automatically when no path is given.
    pub server_url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// Bearer token, used instead of username/password when present.
    #[serde(default)]
    pub token: String,
}

fn normalize_session_url(input: &str) -> Result<url::Url, String> {
    let trimmed = input.trim();
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let mut url = url::Url::parse(&with_scheme).map_err(|e| format!("invalid server URL: {e}"))?;
    if matches!(url.path(), "" | "/") {
        url.set_path("/.well-known/jmap");
    }
    Ok(url)
}

pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(req): Form<LoginRequest>,
) -> Response {
    match do_login(state, req).await {
        Ok((cookie, _)) => {
            let jar = jar.add(cookie);
            (jar, Redirect::to("/app")).into_response()
        }
        Err(msg) => Redirect::to(&format!("/login?error={}", urlencode(&msg))).into_response(),
    }
}

/// Connects to `session_url` with `creds` and builds a `UserSession` from
/// the resulting JMAP session — shared by the login form and by
/// `get_or_create_auto_session` so there's exactly one place that knows how
/// to turn credentials into a working session.
async fn connect_jmap(session_url: url::Url, creds: Credentials) -> Result<UserSession, String> {
    let mut client = Client::new(session_url, creds);
    client
        .connect()
        .await
        .map_err(|e| format!("could not connect: {e}"))?;
    let session = client.session().expect("connect populates session");
    let account_id = session
        .calendars_account_id()
        .ok_or("server does not advertise JMAP Calendars support")?
        .to_string();
    let contacts_account_id = session.contacts_account_id().map(|s| s.to_string());
    let username = session.username.clone();
    Ok(UserSession {
        client,
        account_id,
        username,
        contacts_account_id,
    })
}

async fn do_login(state: AppState, req: LoginRequest) -> Result<(Cookie<'static>, String), String> {
    let session_url = normalize_session_url(&req.server_url)?;

    let creds = if !req.token.is_empty() {
        Credentials::Bearer(req.token)
    } else {
        if req.username.is_empty() || req.password.is_empty() {
            return Err("username and password are required".to_string());
        }
        Credentials::Basic {
            username: req.username,
            password: req.password,
        }
    };

    let user_session = connect_jmap(session_url, creds).await?;
    let username = user_session.username.clone();

    let sid = Uuid::new_v4().to_string();
    state.sessions.insert(sid.clone(), user_session);

    let mut cookie = Cookie::new(COOKIE_NAME, sid);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    Ok((cookie, username))
}

/// Resolves the shared session used to auto-authenticate cookie-less
/// visitors when `AppState::auto_login` is configured, connecting (and
/// caching the result) on first use.
async fn get_or_create_auto_session(app_state: &AppState) -> Option<UserSession> {
    if let Some(session) = app_state.auto_session.read().unwrap().clone() {
        return Some(session);
    }
    let cfg = app_state.auto_login.as_ref()?;
    let session_url = match normalize_session_url(&cfg.server_url) {
        Ok(url) => url,
        Err(e) => {
            tracing::warn!("auto-login: invalid JSCAL_SERVER_URL: {e}");
            return None;
        }
    };
    match connect_jmap(session_url, cfg.credentials.clone()).await {
        Ok(user_session) => {
            *app_state.auto_session.write().unwrap() = Some(user_session.clone());
            Some(user_session)
        }
        Err(e) => {
            tracing::warn!("auto-login: could not connect: {e}");
            None
        }
    }
}

pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    if let Some(cookie) = jar.get(COOKIE_NAME) {
        state.sessions.remove(cookie.value());
    }
    // Also drop the cached auto-login session, so "Sign out" actually does
    // something when auto-login is configured (otherwise the next visit to
    // /app would just re-authenticate transparently with the cached
    // client) — and so a credential rotation can take effect without a
    // server restart.
    *state.auto_session.write().unwrap() = None;
    let jar = jar.remove(Cookie::from(COOKIE_NAME));
    (jar, Redirect::to("/login"))
}

/// Extractor that resolves the caller's `UserSession` from the `jscal_sid`
/// cookie, redirecting to `/login` (303, or `HX-Redirect` under htmx) when
/// there is none.
#[derive(Clone)]
pub struct AuthedSession {
    pub client: Client,
    pub account_id: String,
    pub username: String,
    pub contacts_account_id: Option<String>,
}

impl<S> FromRequestParts<S> for AuthedSession
where
    AppState: FromRef<S>,
    S: Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let jar = CookieJar::from_headers(&parts.headers);
        if let Some(sid) = jar.get(COOKIE_NAME).map(|c| c.value().to_string()) {
            if let Some(session) = app_state.sessions.get(&sid) {
                return Ok(AuthedSession {
                    client: session.client.clone(),
                    account_id: session.account_id.clone(),
                    username: session.username.clone(),
                    contacts_account_id: session.contacts_account_id.clone(),
                });
            }
        }
        // No cookie session — fall back to the operator-supplied
        // credentials (JSCAL_SERVER_URL/...), if configured, instead of
        // requiring every visitor to log in themselves.
        if app_state.auto_login.is_some() {
            if let Some(session) = get_or_create_auto_session(&app_state).await {
                return Ok(AuthedSession {
                    client: session.client,
                    account_id: session.account_id,
                    username: session.username,
                    contacts_account_id: session.contacts_account_id,
                });
            }
        }
        Err(redirect_to_login(&parts.headers))
    }
}

fn redirect_to_login(headers: &HeaderMap) -> Response {
    crate::webutil::redirect("/login", headers)
}
