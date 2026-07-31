//! Reading the undocumented inertial sensors on Apple Silicon MacBooks.
//!
//! Apple Silicon laptops (M2 and later, plus M1 Pro/Max) carry a Bosch IMU behind the
//! Sensor Processing Unit. It is exposed only as vendor-usage-page HID devices and has no
//! public API — CoreMotion does not see it. This crate opens the accelerometer and
//! gyroscope and streams both at their native ~805 Hz into lock-free ring buffers.
//!
//! ```no_run
//! # fn main() -> Result<(), spank_sensor::Error> {
//! let mut imu = spank_sensor::Imu::open()?;
//! while let Ok(sample) = imu.accel.pop() {
//!     println!("{:.4} g", sample.magnitude());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Not every Mac has this sensor: it is laptop-only, and the M1 MacBook Pro (2020) and
//! M1 Air lack it entirely, as do all desktops and all Intel Macs. [`Error::NoSensor`]
//! reports that case distinctly so callers can say so rather than hanging.

mod report;

#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "macos")]
mod timebase;

pub use report::{gap, parse, Sample, REPORT_LEN};

#[cfg(target_os = "macos")]
pub use iokit::{
    check_access, request_access, Access, Stats, DEFAULT_REPORT_INTERVAL_US,
    NATIVE_REPORT_INTERVAL_US,
};
#[cfg(target_os = "macos")]
pub use timebase::Timebase;

/// Default ring capacity: ~10 s of headroom at the native rate.
pub const DEFAULT_CAPACITY: usize = 8192;

/// Which of the two inertial sensors a stream came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorKind {
    /// Usage 3. Samples are in g.
    Accel,
    /// Usage 9. Samples are in deg/s.
    Gyro,
}

impl SensorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SensorKind::Accel => "accel",
            SensorKind::Gyro => "gyro",
        }
    }

    /// Physical unit of the axis values for this sensor.
    pub fn unit(self) -> &'static str {
        match self {
            SensorKind::Accel => "g",
            SensorKind::Gyro => "deg/s",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "no SPU inertial sensor found — this Mac does not have one. \
         It exists only on Apple Silicon laptops (M2+, and M1 Pro/Max); \
         desktops, the M1 MacBook Pro 2020, the M1 Air and all Intel Macs lack it"
    )]
    NoSensor,

    #[error(
        "could not open the {kind} sensor (IOReturn 0x{code:08x}). \
         If Input Monitoring is denied for this binary, grant it in \
         System Settings > Privacy & Security > Input Monitoring",
        kind = kind.as_str()
    )]
    Open { kind: SensorKind, code: i32 },

    #[error("IOKit error: {0}")]
    Iokit(String),

    #[error("this crate only supports macOS on Apple Silicon")]
    Unsupported,
}

/// Both inertial sensors, open and streaming.
#[cfg(target_os = "macos")]
pub struct Imu {
    /// Accelerometer samples, in g.
    pub accel: rtrb::Consumer<Sample>,
    /// Gyroscope samples, in deg/s. `None` if the gyro was not found — the accelerometer
    /// alone is enough to run, so a missing gyro degrades rather than fails.
    pub gyro: Option<rtrb::Consumer<Sample>>,
    accel_dev: iokit::OpenDevice,
    gyro_dev: Option<iokit::OpenDevice>,
}

#[cfg(target_os = "macos")]
impl Imu {
    /// Discover, wake and open both sensors at the native report rate.
    pub fn open() -> Result<Self, Error> {
        Self::open_with(DEFAULT_CAPACITY, NATIVE_REPORT_INTERVAL_US)
    }

    /// As [`Imu::open`], with an explicit ring capacity and requested report interval.
    ///
    /// `report_interval_us` is clamped to the range the hardware honours. Larger values
    /// mean fewer reports per second and proportionally less CPU, at the cost of missing
    /// the peak of short impacts — measured recall drops from 90% to 72% at half rate.
    pub fn open_with(capacity: usize, report_interval_us: u32) -> Result<Self, Error> {
        // Must happen before opening anything, and on the driver rather than the device.
        iokit::wake_sensors(report_interval_us);

        let found = iokit::find_devices()?;
        let accel_found = found
            .iter()
            .find(|d| d.kind == SensorKind::Accel)
            .ok_or(Error::NoSensor)?;

        let (accel_dev, accel) = iokit::open(accel_found, capacity)?;

        // A missing or unopenable gyro is not fatal; it only disables corroboration.
        let (gyro_dev, gyro) = match found.iter().find(|d| d.kind == SensorKind::Gyro) {
            Some(g) => match iokit::open(g, capacity) {
                Ok((dev, consumer)) => (Some(dev), Some(consumer)),
                Err(_) => (None, None),
            },
            None => (None, None),
        };

        Ok(Imu {
            accel,
            gyro,
            accel_dev,
            gyro_dev,
        })
    }

    /// Health counters for the accelerometer stream.
    pub fn accel_stats(&self) -> &std::sync::Arc<Stats> {
        self.accel_dev.stats()
    }

    /// Health counters for the gyroscope stream, if it is open.
    pub fn gyro_stats(&self) -> Option<&std::sync::Arc<Stats>> {
        self.gyro_dev.as_ref().map(|d| d.stats())
    }

    /// Whether the gyroscope is streaming.
    pub fn has_gyro(&self) -> bool {
        self.gyro_dev.is_some()
    }
}
