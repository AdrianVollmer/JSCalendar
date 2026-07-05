#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("jmap protocol error: {0}")]
    Protocol(String),
    #[error("authentication failed")]
    Unauthorized,
    #[error("account does not support {0}")]
    UnsupportedAccount(&'static str),
    #[error("not found")]
    NotFound,
}
