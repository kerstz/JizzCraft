use std::fmt;

/// Four ways for things to go wrong. Reality has found more; these are the
/// ones we admit to.
#[derive(Debug)]
pub enum Error {
    Db(rusqlite::Error),
    Io(std::io::Error),
    Invalid(String),
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Db(e) => write!(f, "database error: {e}"),
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Invalid(s) => write!(f, "invalid input: {s}"),
            Error::NotFound(s) => write!(f, "not found: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_kind_and_keep_the_detail() {
        assert_eq!(Error::Invalid("x".into()).to_string(), "invalid input: x");
        assert_eq!(Error::NotFound("y".into()).to_string(), "not found: y");
        let io: Error = std::io::Error::from(std::io::ErrorKind::NotFound).into();
        assert!(io.to_string().starts_with("I/O error"));
        let db: Error = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(db.to_string().starts_with("database error"));
    }
}
