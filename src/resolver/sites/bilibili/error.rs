use bpi_rs::BpiError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid Bilibili URL: {0}")]
    InvalidUrl(String),
    #[error("Bilibili API error: {0}")]
    Api(#[from] BpiError),
    #[error("Bilibili response shape is invalid: {0}")]
    Schema(String),
    #[error(transparent)]
    Request(#[from] reqwest_012::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
