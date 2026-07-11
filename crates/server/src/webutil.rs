use askama::Template;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};

/// Whether the original client connection was HTTPS, trusting
/// `X-Forwarded-Proto` from a reverse proxy (the standard way a
/// TLS-terminating proxy in front of this app — nginx, Caddy, Traefik, most
/// cloud load balancers — tells it the real scheme). Used only to decide
/// whether to mark cookies `Secure`; a client that spoofs this header
/// against a plain-HTTP deployment just breaks its own cookie (the browser
/// won't send a `Secure` cookie back over the HTTP connection it actually
/// has), not a security downgrade. Without a proxy in front, this is
/// always `false`, which keeps `cargo run`/`make demo` over plain
/// `http://127.0.0.1` working with no configuration.
pub fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("X-Forwarded-Proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

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
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("template error: {err}"),
        )
            .into_response(),
    }
}

/// Minimal `application/x-www-form-urlencoded`-style percent-encoding for
/// values embedded in hrefs we build ourselves (query params, redirects).
/// Encodes by UTF-8 byte, not by codepoint — a naive per-`char` `%XX` (using
/// the codepoint value directly) silently mangles any non-ASCII character
/// into the wrong bytes.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
