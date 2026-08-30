use thiserror::Error;

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid source project: {0}")]
    InvalidSource(String),
    #[error("invalid compiled project: {0}")]
    InvalidProject(String),
    #[error("unknown tag: {0}")]
    UnknownTag(String),
    #[error("tag is not writable: {0}")]
    NotWritable(String),
    #[error("type mismatch for tag: {0}")]
    TypeMismatch(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("engine process error: {0}")]
    Process(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("data output error: {0}")]
    Output(String),
    #[error("engine error {code}: {message}")]
    Remote { code: i32, message: String },
}

impl EngineError {
    pub fn code(&self) -> i32 {
        match self {
            Self::Io(_) => 1001,
            Self::InvalidSource(_) => 1101,
            Self::InvalidProject(_) => 1102,
            Self::UnknownTag(_) => 1201,
            Self::NotWritable(_) => 1202,
            Self::TypeMismatch(_) => 1203,
            Self::InvalidRequest(_) => 1204,
            Self::Process(_) => 1301,
            Self::Protocol(_) => 1302,
            Self::Output(_) => 1401,
            Self::Remote { code, .. } => *code,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_stable_integer_code() {
        let errors = [
            EngineError::Io(std::io::Error::other("io")),
            EngineError::InvalidSource("source".into()),
            EngineError::InvalidProject("project".into()),
            EngineError::UnknownTag("tag".into()),
            EngineError::NotWritable("tag".into()),
            EngineError::TypeMismatch("tag".into()),
            EngineError::InvalidRequest("request".into()),
            EngineError::Process("process".into()),
            EngineError::Protocol("protocol".into()),
            EngineError::Output("output".into()),
            EngineError::Remote {
                code: 999,
                message: "remote".into(),
            },
        ];
        assert_eq!(
            errors.iter().map(EngineError::code).collect::<Vec<_>>(),
            [1001, 1101, 1102, 1201, 1202, 1203, 1204, 1301, 1302, 1401, 999]
        );
        assert!(errors.iter().all(|error| !error.to_string().is_empty()));
    }
}
