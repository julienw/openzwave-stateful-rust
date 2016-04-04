use openzwave;

#[derive(Debug)]
pub enum Error {
    OpenzwaveError(openzwave::Error),
    NoDeviceFound
}

pub type Result<T> = ::std::result::Result<T, Error>;

use std::fmt;
use std::error;

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::OpenzwaveError(ref cause) => write!(formatter, "{}", cause),
            _ => write!(formatter, "{}", error::Error::description(self))
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::OpenzwaveError(ref cause) => cause.description(),
            Error::NoDeviceFound => "No suitable device was found"
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::OpenzwaveError(ref cause) => Some(cause),
            _ => None
        }
    }
}

impl From<openzwave::Error> for Error {
    fn from(error: openzwave::Error) -> Error {
        Error::OpenzwaveError(error)
    }
}
