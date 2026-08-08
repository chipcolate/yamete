//! Process-level errors for the `yamete` binary.
//!
//! Sensor failures stay as [`yamete_sensor::Error`]. Everything else — socket I/O,
//! launchctl, config parse, fixture load — used to be stuffed into
//! `yamete_sensor::Error::Iokit`, which made every CLI failure look like an HID problem.
//! This type keeps the two apart.

use std::fmt;

/// A failure of the CLI / daemon process, not necessarily of the sensor.
#[derive(Debug)]
pub enum Error {
    /// The IMU / IOKit path failed.
    Sensor(yamete_sensor::Error),
    /// Anything else: socket, launchd, filesystem, protocol, user error.
    Other(String),
}

impl Error {
    pub fn other(msg: impl fmt::Display) -> Self {
        Error::Other(msg.to_string())
    }

    pub fn is_no_sensor(&self) -> bool {
        matches!(self, Error::Sensor(yamete_sensor::Error::NoSensor))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sensor(e) => write!(f, "{e}"),
            Error::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sensor(e) => Some(e),
            Error::Other(_) => None,
        }
    }
}

impl From<yamete_sensor::Error> for Error {
    fn from(e: yamete_sensor::Error) -> Self {
        Error::Sensor(e)
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Other(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Other(s.into())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Other(e.to_string())
    }
}
