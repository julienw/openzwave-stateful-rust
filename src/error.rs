use openzwave;
use std::io;
use notify;

#[derive(Debug)]
pub enum Error {
    OpenzwaveError(openzwave::Error),
    NoDeviceFound,
    CannotReadDevice(String, io::Error),
    FsNotifyError(notify::Error),
}

pub type Result<T> = ::std::result::Result<T, Error>;

use std::fmt;
use std::error;

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::OpenzwaveError(ref cause) => write!(formatter, "{}", cause),
            Error::CannotReadDevice(ref message, ref cause) => write!(formatter, "The device {} is not readable: {}", message, cause),
            Error::FsNotifyError(ref cause) => write!(formatter, "Could not watch the device file: {}", cause),
            _ => write!(formatter, "{}", error::Error::description(self))
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::OpenzwaveError(ref cause) => cause.description(),
            Error::CannotReadDevice(_, _) => "Couldn't read the device",
            Error::FsNotifyError(_) => "Could not watch the device file",
            Error::NoDeviceFound => "No suitable device was found"
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::OpenzwaveError(ref cause) => Some(cause),
            Error::CannotReadDevice(_, ref cause) => Some(cause),
            Error::FsNotifyError(ref cause) => Some(cause),
            _ => None
        }
    }
}

impl From<openzwave::Error> for Error {
    fn from(error: openzwave::Error) -> Error {
        Error::OpenzwaveError(error)
    }
}

impl From<notify::Error> for Error {
    fn from(error: notify::Error) -> Error {
        Error::FsNotifyError(error)
    }
}
