mod auth;
mod error;
mod routes;
mod state;
mod view;
mod webutil;

use std::path::PathBuf;

use axum::routing::{get, patch, post};
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jscalendar_server=info,tower_http=info".into()),
        )
        .init();

    let state = AppState::new();
    let static_dir = static_dir();

    let app = Router::new()
        .route("/", get(routes::root))
        .route("/login", get(auth::login_form).post(auth::login))
        .route("/logout", post(auth::logout))
        .route("/app", get(routes::app_view))
        .route("/app/event/new", get(routes::event_new_form))
        .route("/app/event", post(routes::event_create))
        .route(
            "/app/event/{id}/edit",
            get(routes::event_edit_form).post(routes::event_update),
        )
        .route("/app/event/{id}/delete", post(routes::event_delete_post))
        .route(
            "/app/event/{id}",
            patch(routes::event_update).delete(routes::event_delete_hx),
        )
        .route("/app/contact/new", get(routes::contact_new_form))
        .route("/app/contact", post(routes::contact_create))
        .route(
            "/app/contact/{id}/edit",
            get(routes::contact_edit_form).post(routes::contact_update),
        )
        .route(
            "/app/contact/{id}/delete",
            post(routes::contact_delete_post),
        )
        .route(
            "/app/contact/{id}",
            patch(routes::contact_update).delete(routes::contact_delete_hx),
        )
        // Served at the root so its default scope covers the whole origin.
        .route_service("/sw.js", ServeFile::new(static_dir.join("sw.js")))
        .nest_service("/static", ServeDir::new(&static_dir))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("failed to bind listener");
    tracing::info!(
        "serving on http://0.0.0.0:{port} (static: {})",
        static_dir.display()
    );
    axum::serve(listener, app).await.expect("server error");
}

fn static_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("JSCAL_STATIC_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static")
}
