use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{context}: {source}")]
    Io { context: String, source: io::Error },
    #[error("invalid image: {0}")]
    Format(String),
    #[error("{0}")]
    Usage(String),
    #[error("{0} failed: {1}")]
    Tool(&'static str, String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub trait IoContext<T> {
    fn ctx(self, context: impl FnOnce() -> String) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn ctx(self, context: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|source| Error::Io { context: context(), source })
    }
}

pub fn format_err(msg: impl Into<String>) -> Error {
    Error::Format(msg.into())
}
