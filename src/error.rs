use core::fmt;

use flexi_logger::FlexiLoggerError;

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(err: String) -> Error {
        Error(err)
    }
}

impl From<&str> for Error {
    fn from(err: &str) -> Error {
        Error(err.to_string())
    }
}

impl From<FlexiLoggerError> for Error {
    fn from(err: FlexiLoggerError) -> Error {
        Error(err.to_string())
    }
}
