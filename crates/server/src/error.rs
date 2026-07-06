use askama::Template;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorTemplate {
    status: u16,
    message: String,
}

pub struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let tpl = ErrorTemplate {
            status: self.status.as_u16(),
            message: self.message,
        };
        let body = tpl
            .render()
            .unwrap_or_else(|e| format!("template error: {e}"));
        (self.status, axum::response::Html(body)).into_response()
    }
}

impl From<jmap_client::Error> for AppError {
    fn from(err: jmap_client::Error) -> Self {
        let status = match err {
            jmap_client::Error::Unauthorized => StatusCode::UNAUTHORIZED,
            jmap_client::Error::NotFound => StatusCode::NOT_FOUND,
            jmap_client::Error::UnsupportedAccount(_) => StatusCode::BAD_GATEWAY,
            jmap_client::Error::Http(_)
            | jmap_client::Error::Json(_)
            | jmap_client::Error::Protocol(_) => StatusCode::BAD_GATEWAY,
        };
        AppError::new(status, err.to_string())
    }
}
