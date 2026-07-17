use serde::Serialize;

/// Error type crossing the Tauri command boundary. Messages are user-facing;
/// the API key must never appear in them (see `config::redact_key`).
#[derive(Debug, thiserror::Error)]
pub enum SallyError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("audio error: {0}")]
    Audio(String),
    #[error("gemini error: {0}")]
    Gemini(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("session error: {0}")]
    Session(String),
}

impl Serialize for SallyError {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, SallyError>;

impl From<std::io::Error> for SallyError {
    fn from(e: std::io::Error) -> Self {
        SallyError::Storage(e.to_string())
    }
}
