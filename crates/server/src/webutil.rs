use askama::Template;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};

pub fn is_hx(headers: &HeaderMap) -> bool {
    headers
        .get("HX-Request")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// For handlers that redirect after a POST/PATCH/DELETE: htmx doesn't
/// follow a normal 3xx on its fetch, so it needs the `HX-Redirect` header
/// instead; plain (no-JS) requests get a normal 303.
pub fn redirect(target: &str, headers: &HeaderMap) -> Response {
    if is_hx(headers) {
        (StatusCode::OK, [("HX-Redirect", target)]).into_response()
    } else {
        Redirect::to(target).into_response()
    }
}

pub fn render<T: Template>(tpl: &T) -> Response {
    match tpl.render() {
        Ok(body) => Html(body).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("template error: {err}")).into_response(),
    }
}
