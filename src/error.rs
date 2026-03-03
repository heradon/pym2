use thiserror::Error;

pub type Result<T> = std::result::Result<T, PyopsError>;

#[derive(Debug, Error)]
pub enum PyopsError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("config error: {0}")]
    Config(String),
    #[error("ipc error: {0}")]
    Ipc(String),
    #[error("supervisor error: {0}")]
    Supervisor(String),
}
