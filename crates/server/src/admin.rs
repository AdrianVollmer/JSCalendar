use askama::Template;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::auth::AdminUser;
use crate::routes::account_page;
use crate::state::AppState;
use crate::users::Role;

struct UserRow {
    id: String,
    username: String,
    role_label: &'static str,
    is_self: bool,
    jmap_configured: bool,
}

#[derive(Template)]
#[template(path = "admin_users.html")]
struct AdminUsersTemplate {
    users: Vec<UserRow>,
}

pub async fn list_users(State(state): State<AppState>, admin: AdminUser) -> Response {
    let mut users: Vec<UserRow> = state
        .users
        .list_users()
        .into_iter()
        .map(|u| UserRow {
            is_self: u.id == admin.0.user_id,
            jmap_configured: u.jmap.is_configured(),
            role_label: u.role.as_str(),
            id: u.id,
            username: u.username,
        })
        .collect();
    users.sort_by(|a, b| a.username.to_lowercase().cmp(&b.username.to_lowercase()));
    let body = AdminUsersTemplate { users }
        .render()
        .unwrap_or_else(|e| format!("template error: {e}"));
    account_page(&state, &admin.0, "users", "Users", body).await
}

#[derive(Template)]
#[template(path = "admin_user_form.html")]
struct AdminUserFormTemplate {
    editing: bool,
    user_id: String,
    username: String,
    role: &'static str,
    error: Option<String>,
}

pub async fn new_user_form(State(state): State<AppState>, admin: AdminUser) -> Response {
    let body = AdminUserFormTemplate {
        editing: false,
        user_id: String::new(),
        username: String::new(),
        role: Role::User.as_str(),
        error: None,
    }
    .render()
    .unwrap_or_else(|e| format!("template error: {e}"));
    account_page(&state, &admin.0, "users", "New user", body).await
}

pub async fn edit_user_form(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
) -> Response {
    let Some(user) = state.users.get_user(&id) else {
        return (axum::http::StatusCode::NOT_FOUND, "user not found").into_response();
    };
    let body = AdminUserFormTemplate {
        editing: true,
        user_id: user.id,
        username: user.username,
        role: user.role.as_str(),
        error: None,
    }
    .render()
    .unwrap_or_else(|e| format!("template error: {e}"));
    account_page(&state, &admin.0, "users", "Edit user", body).await
}

#[derive(Debug, Deserialize, Default)]
pub struct AdminUserFormBody {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub role: String,
}

async fn form_error(
    state: &AppState,
    admin: &crate::auth::AppUser,
    editing: bool,
    user_id: &str,
    username: &str,
    role: &str,
    msg: &str,
) -> Response {
    let body = AdminUserFormTemplate {
        editing,
        user_id: user_id.to_string(),
        username: username.to_string(),
        role: if role == "admin" { "admin" } else { "user" },
        error: Some(msg.to_string()),
    }
    .render()
    .unwrap_or_else(|e| format!("template error: {e}"));
    let title = if editing { "Edit user" } else { "New user" };
    account_page(state, admin, "users", title, body).await
}

pub async fn create_user(
    State(state): State<AppState>,
    admin: AdminUser,
    headers: HeaderMap,
    Form(form): Form<AdminUserFormBody>,
) -> Response {
    let username = form.username.trim().to_string();
    if username.is_empty() {
        return form_error(
            &state,
            &admin.0,
            false,
            "",
            &username,
            &form.role,
            "Username is required.",
        )
        .await;
    }
    if form.password.len() < 8 {
        return form_error(
            &state,
            &admin.0,
            false,
            "",
            &username,
            &form.role,
            "Password must be at least 8 characters.",
        )
        .await;
    }
    if state.users.username_taken(&username, None) {
        return form_error(
            &state,
            &admin.0,
            false,
            "",
            &username,
            &form.role,
            "That username is taken.",
        )
        .await;
    }
    let hash = crate::users::hash_password(&form.password);
    state.users.create_user(
        username,
        hash,
        Role::parse(&form.role),
        crate::users::JmapSettings::default(),
    );
    crate::webutil::redirect("/admin/users", &headers)
}

pub async fn update_user(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<AdminUserFormBody>,
) -> Response {
    let Some(existing) = state.users.get_user(&id) else {
        return (axum::http::StatusCode::NOT_FOUND, "user not found").into_response();
    };
    let username = form.username.trim().to_string();
    if username.is_empty() {
        return form_error(
            &state,
            &admin.0,
            true,
            &id,
            &username,
            &form.role,
            "Username is required.",
        )
        .await;
    }
    if state.users.username_taken(&username, Some(&id)) {
        return form_error(
            &state,
            &admin.0,
            true,
            &id,
            &username,
            &form.role,
            "That username is taken.",
        )
        .await;
    }
    let new_role = Role::parse(&form.role);
    if existing.role == Role::Admin && new_role == Role::User && id == admin.0.user_id {
        return form_error(
            &state,
            &admin.0,
            true,
            &id,
            &username,
            &form.role,
            "You can't demote yourself.",
        )
        .await;
    }
    if existing.role == Role::Admin && new_role == Role::User && state.users.admin_count() <= 1 {
        return form_error(
            &state,
            &admin.0,
            true,
            &id,
            &username,
            &form.role,
            "There must be at least one admin.",
        )
        .await;
    }
    if !form.password.is_empty() && form.password.len() < 8 {
        return form_error(
            &state,
            &admin.0,
            true,
            &id,
            &username,
            &form.role,
            "Password must be at least 8 characters.",
        )
        .await;
    }

    let new_hash = if form.password.is_empty() {
        None
    } else {
        Some(crate::users::hash_password(&form.password))
    };
    state.users.update_user(&id, |u| {
        u.username = username;
        u.role = new_role;
        if let Some(hash) = new_hash {
            u.password_hash = hash;
        }
    });
    crate::webutil::redirect("/admin/users", &headers)
}

pub async fn delete_user(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if id == admin.0.user_id {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "You can't delete your own account.",
        )
            .into_response();
    }
    if let Some(user) = state.users.get_user(&id) {
        if user.role == Role::Admin && state.users.admin_count() <= 1 {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "There must be at least one admin.",
            )
                .into_response();
        }
    }
    state.users.delete_user(&id);
    state.jmap_clients.remove(&id);
    crate::webutil::redirect("/admin/users", &headers)
}
