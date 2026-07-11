use askama::Template;
use axum::extract::{FromRef, FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use jmap_client::client::Client;
use serde::Deserialize;

use crate::state::{AppState, UserSession};
use crate::users::{Role, User};
use crate::webutil::{render, urlencode};

const COOKIE_NAME: &str = "jscal_sid";

/// Cookies never expire on their own; a signed-in user stays signed in
/// until they explicitly sign out (or an admin deletes their account).
const SESSION_COOKIE_DAYS: i64 = 3650;

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: Option<String>,
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
        username: demo
            .as_ref()
            .map(|d| d.username.clone())
            .unwrap_or_default(),
        password: demo.map(|d| d.password).unwrap_or_default(),
    })
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(req): Form<LoginRequest>,
) -> Response {
    match do_login(&state, req) {
        Ok(cookie) => {
            let jar = jar.add(cookie);
            (jar, Redirect::to("/app")).into_response()
        }
        Err(msg) => Redirect::to(&format!("/login?error={}", urlencode(&msg))).into_response(),
    }
}

fn do_login(state: &AppState, req: LoginRequest) -> Result<Cookie<'static>, String> {
    let user = state
        .users
        .find_by_username(&req.username)
        .ok_or("invalid username or password")?;
    if !crate::users::verify_password(&req.password, &user.password_hash) {
        return Err("invalid username or password".to_string());
    }

    let token = state.users.create_session(user.id);
    let mut cookie = Cookie::new(COOKIE_NAME, token);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(time::Duration::days(SESSION_COOKIE_DAYS));
    Ok(cookie)
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

/// Connects to a user's configured JMAP server and builds a `UserSession`
/// from the resulting session discovery.
async fn connect_jmap(user: &User) -> Result<UserSession, String> {
    let creds = user
        .jmap
        .credentials()
        .ok_or("no calendar server is configured for this account yet")?;
    let session_url = normalize_session_url(&user.jmap.server_url)?;
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
    Ok(UserSession {
        client,
        account_id,
        contacts_account_id,
    })
}

/// Resolves the cached JMAP client for `user`, connecting (and caching the
/// result) on first use. Cleared by the settings page whenever the user's
/// JMAP settings change, so a credential update takes effect on next use
/// without a server restart.
async fn get_or_create_jmap_client(
    app_state: &AppState,
    user: &User,
) -> Result<UserSession, String> {
    if let Some(session) = app_state.jmap_clients.get(&user.id) {
        return Ok(session.clone());
    }
    let session = connect_jmap(user).await?;
    app_state
        .jmap_clients
        .insert(user.id.clone(), session.clone());
    Ok(session)
}

pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    if let Some(cookie) = jar.get(COOKIE_NAME) {
        state.users.delete_session(cookie.value());
    }
    let jar = jar.remove(Cookie::from(COOKIE_NAME));
    (jar, Redirect::to("/login"))
}

/// Extractor that only requires a valid app login (no working JMAP
/// connection). Used by pages a brand-new user needs to reach before
/// they've configured their calendar server, like the settings page.
#[derive(Clone)]
pub struct AppUser {
    pub user_id: String,
    pub username: String,
    pub role: Role,
}

impl<S> FromRequestParts<S> for AppUser
where
    AppState: FromRef<S>,
    S: Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let jar = CookieJar::from_headers(&parts.headers);
        if let Some(sid) = jar.get(COOKIE_NAME).map(|c| c.value().to_string()) {
            if let Some(user) = app_state.users.user_for_session(&sid) {
                return Ok(AppUser {
                    user_id: user.id,
                    username: user.username,
                    role: user.role,
                });
            }
        }
        Err(redirect_to_login(&parts.headers))
    }
}

/// Extractor that requires both a valid app login and a working JMAP
/// connection. Redirects to the settings page (rather than the login page)
/// when the app login is valid but no calendar server is reachable yet,
/// since re-entering the app password wouldn't help.
#[derive(Clone)]
pub struct AuthedSession {
    pub client: Client,
    pub account_id: String,
    pub contacts_account_id: Option<String>,
    pub app_username: String,
    pub role: Role,
}

impl<S> FromRequestParts<S> for AuthedSession
where
    AppState: FromRef<S>,
    S: Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let app_user = AppUser::from_request_parts(parts, state).await?;
        let user = app_state
            .users
            .get_user(&app_user.user_id)
            .ok_or_else(|| redirect_to_login(&parts.headers))?;

        match get_or_create_jmap_client(&app_state, &user).await {
            Ok(session) => Ok(AuthedSession {
                client: session.client,
                account_id: session.account_id,
                contacts_account_id: session.contacts_account_id,
                app_username: app_user.username,
                role: app_user.role,
            }),
            Err(e) => {
                tracing::warn!(
                    "could not establish JMAP session for {}: {e}",
                    user.username
                );
                Err(crate::webutil::redirect("/app/settings", &parts.headers))
            }
        }
    }
}

/// Extractor that requires the caller to be signed in as an admin, for
/// gating `/admin/*` routes. Rejects with 403 (not a redirect) since a
/// non-admin being shown the login page again would be misleading — they
/// are signed in, just not authorized.
#[derive(Clone)]
pub struct AdminUser(pub AppUser);

impl<S> FromRequestParts<S> for AdminUser
where
    AppState: FromRef<S>,
    S: Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_user = AppUser::from_request_parts(parts, state).await?;
        if app_user.role != Role::Admin {
            return Err((StatusCode::FORBIDDEN, "admin access required").into_response());
        }
        Ok(AdminUser(app_user))
    }
}

fn redirect_to_login(headers: &HeaderMap) -> Response {
    crate::webutil::redirect("/login", headers)
}
