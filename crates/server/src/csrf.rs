//! CSRF defense-in-depth via `Origin`/`Referer` verification.
//!
//! Every mutating route in this app already only accepts POST/PATCH/DELETE
//! (never GET), and every cookie is `SameSite=Lax`, which together already
//! block the classic cross-site auto-submitting-form CSRF attack in every
//! current browser. This middleware is a second, independent layer that
//! doesn't rely on cookie attributes at all: browsers set `Origin` (and,
//! failing that, `Referer`) on every POST/PUT/PATCH/DELETE request and
//! refuse to let a page forge either to a different origin, so comparing
//! it against the request's own `Host` catches a cross-site mutation even
//! if `SameSite` were ever stripped by a misconfigured proxy or downgraded
//! by a browser bug — and it applies uniformly to every route with no
//! per-handler or per-template wiring, so a new mutating route can't
//! accidentally ship unprotected.
//!
//! Compares hostnames only (not port/scheme): a reverse proxy in front of
//! this app commonly terminates TLS and forwards to a different internal
//! port, which would otherwise make a legitimate same-site request look
//! like a port mismatch.

use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub async fn verify_same_origin(request: Request, next: Next) -> Response {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return next.run(request).await;
    }

    let headers = request.headers();
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(hostname_only);

    let source = headers
        .get(axum::http::header::ORIGIN)
        .or_else(|| headers.get(axum::http::header::REFERER))
        .and_then(|v| v.to_str().ok())
        .and_then(hostname_only);

    match (host, source) {
        (Some(host), Some(source)) if host == source => next.run(request).await,
        _ => (
            StatusCode::FORBIDDEN,
            "cross-origin request rejected (Origin/Referer did not match Host)",
        )
            .into_response(),
    }
}

/// Extracts just the hostname from a `Host` header value (`example.com` or
/// `example.com:8080`) or an `Origin`/`Referer` value
/// (`https://example.com` or `https://example.com:8080/some/path`).
fn hostname_only(s: &str) -> Option<String> {
    let without_scheme = s.split_once("://").map(|(_, rest)| rest).unwrap_or(s);
    let authority = without_scheme.split('/').next().unwrap_or("");
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_hostname_from_various_shapes() {
        assert_eq!(hostname_only("example.com").as_deref(), Some("example.com"));
        assert_eq!(
            hostname_only("example.com:8080").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            hostname_only("https://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            hostname_only("https://Example.COM:8080/path?q=1").as_deref(),
            Some("example.com")
        );
        assert_eq!(hostname_only("").as_deref(), None);
    }
}
