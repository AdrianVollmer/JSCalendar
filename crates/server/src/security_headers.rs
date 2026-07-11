//! Response headers that don't depend on anything per-request — applied to
//! every response as a blanket hardening layer.
//!
//! The CSP's `script-src 'self'` (no `unsafe-inline`) is enforceable
//! because every template-driven `<script>` is external (app.js/
//! settings.js/htmx.min.js, all served from `/static/`) and there are no
//! inline event-handler attributes (`onclick=`, etc.) anywhere in the
//! templates — those were removed in favor of delegated listeners in
//! app.js specifically so this policy could be strict. `style-src` does
//! need `unsafe-inline`: per-event/per-calendar colors and the time-grid's
//! computed positioning are set via inline `style="..."` attributes
//! throughout the calendar views, and moving all of those to CSS classes
//! would be a much larger change than this hardening pass warrants.

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;

const CSP: &str = "default-src 'self'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data:; \
     connect-src 'self'; \
     object-src 'none'; \
     base-uri 'self'; \
     form-action 'self'; \
     frame-ancestors 'none'";

pub async fn add_security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("Content-Security-Policy", HeaderValue::from_static(CSP));
    headers.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    headers.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "Referrer-Policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response
}
