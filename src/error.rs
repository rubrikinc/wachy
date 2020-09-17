use core::fmt;
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
