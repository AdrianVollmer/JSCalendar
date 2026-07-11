//! Content-hashed `/static/*` URLs for `app.css`/`app.js`/`htmx.min.js`.
//!
//! The service worker (`static/sw.js`) caches `/static/*` responses
//! cache-first with no revalidation, so once a URL is cached it's served
//! forever until that exact URL changes — a plain deploy that only changes
//! file *contents* at a fixed URL is invisible to it. Appending a content
//! hash as a `?v=` query string means any edit to one of these files
//! produces a brand new URL automatically, so both the browser cache and
//! the service worker fetch it fresh instead of serving something stale.
//! (`ServeDir` ignores the query string, so the same file is still served —
//! this only needs the URL to change, not any new routing.)

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

const HASHED_ASSETS: &[&str] = &["app.css", "app.js", "htmx.min.js", "settings.js"];

static VERSIONS: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
static SW_BODY: OnceLock<String> = OnceLock::new();

/// Hashes each entry in `HASHED_ASSETS` and pre-renders `sw.js`'s
/// placeholders with the results. Must run once at startup, before any
/// template renders or `/sw.js` is served.
pub fn init(static_dir: &Path) {
    let mut map = HashMap::new();
    for &rel in HASHED_ASSETS {
        let hash = std::fs::read(static_dir.join(rel))
            .map(|bytes| fnv1a_hex(&bytes))
            .unwrap_or_else(|_| "0".to_string());
        map.insert(rel, hash);
    }
    let _ = VERSIONS.set(map);

    let raw = std::fs::read_to_string(static_dir.join("sw.js")).unwrap_or_default();
    let body = raw
        .replace("__APP_CSS_VERSION__", version("app.css"))
        .replace("__APP_JS_VERSION__", version("app.js"))
        .replace("__HTMX_VERSION__", version("htmx.min.js"));
    let _ = SW_BODY.set(body);
}

fn version(rel: &str) -> &str {
    VERSIONS
        .get()
        .and_then(|m| m.get(rel))
        .map(|s| s.as_str())
        .unwrap_or("0")
}

fn url(rel: &str) -> String {
    format!("/static/{rel}?v={}", version(rel))
}

pub fn css_url() -> String {
    url("app.css")
}

pub fn js_url() -> String {
    url("app.js")
}

pub fn htmx_url() -> String {
    url("htmx.min.js")
}

pub fn settings_js_url() -> String {
    url("settings.js")
}

/// The pre-rendered `sw.js` body, with its shell-asset URLs already
/// substituted with the current content hashes.
pub fn service_worker_body() -> &'static str {
    SW_BODY.get().map(|s| s.as_str()).unwrap_or("")
}

fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (hash >> 32) as u32 ^ hash as u32)
}
