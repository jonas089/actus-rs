//! Error type for the whole engine.

use crate::time::DateTime;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("invalid value for {term}: {value:?}")]
    InvalidTerm { term: String, value: String },
    #[error("missing required term {0}")]
    MissingTerm(String),
    #[error("unknown contract type {0:?}")]
    UnknownContractType(String),
    #[error("no observation of {id} at {time}")]
    MissingObservation { id: String, time: DateTime },
    #[error("arithmetic overflow or division by zero while evaluating {context}")]
    Arithmetic { context: String },
    #[error("{0}")]
    Unsupported(String),
    #[error("child contract error: {0}")]
    Child(String),
}

impl Error {
    pub fn invalid(term: &str, value: impl ToString) -> Self {
        Error::InvalidTerm { term: term.to_string(), value: value.to_string() }
    }
}
