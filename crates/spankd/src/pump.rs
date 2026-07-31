//! Draining the sensor rings into detector frames.
//!
//! The two sensors are independent HID devices with independent rings, but the detector
//! needs their samples interleaved in time so gyroscope corroboration lines up with the
//! accelerometer trigger it is corroborating. A single shared [`Timebase`] gives both
//! streams a common origin; this module merges them in timestamp order.

use spank_dsp::Frame;
use spank_sensor::{Imu, Sample, Timebase};

/// Which sensor a merged frame came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Accel,
    Gyro,
}

/// Merges the two sensor streams into one time-ordered sequence of frames.
pub struct Pump {
    timebase: Timebase,
    /// A gyro sample already popped but not yet emitted, because the accelerometer had an
    /// earlier one waiting.
    pending_gyro: Option<Sample>,
    pending_accel: Option<Sample>,
}

impl Pump {
    pub fn new() -> Self {
        Pump {
            timebase: Timebase::new(),
            pending_gyro: None,
            pending_accel: None,
        }
    }

    /// Drain whatever is currently buffered, calling `on_frame` in timestamp order.
    ///
    /// Returns the number of frames emitted. This never blocks: it processes what has
    /// arrived and returns, so the caller controls the polling cadence.
    pub fn drain<F>(&mut self, imu: &mut Imu, mut on_frame: F) -> usize
    where
        F: FnMut(Source, Frame),
    {
        let mut emitted = 0;
        loop {
            if self.pending_accel.is_none() {
                self.pending_accel = imu.accel.pop().ok();
            }
            if self.pending_gyro.is_none() {
                self.pending_gyro = imu.gyro.as_mut().and_then(|g| g.pop().ok());
            }

            // Emit whichever waiting sample is older. When only one stream has data we
            // must not drain it ahead of the other, or corroboration windows desync —
            // but we also cannot block waiting for a gyro sample that may never come,
            // so a starved stream simply stops contributing.
            let take = match (&self.pending_accel, &self.pending_gyro) {
                (Some(a), Some(g)) => {
                    if a.host_time <= g.host_time {
                        Source::Accel
                    } else {
                        Source::Gyro
                    }
                }
                (Some(_), None) => Source::Accel,
                (None, Some(_)) => Source::Gyro,
                (None, None) => break,
            };

            let sample = match take {
                Source::Accel => self.pending_accel.take(),
                Source::Gyro => self.pending_gyro.take(),
            }
            .expect("match arm guarantees the slot is occupied");

            on_frame(take, self.to_frame(sample));
            emitted += 1;
        }
        emitted
    }

    fn to_frame(&mut self, s: Sample) -> Frame {
        Frame {
            t: self.timebase.seconds_since_start(s.host_time),
            x: s.x,
            y: s.y,
            z: s.z,
        }
    }
}

impl Default for Pump {
    fn default() -> Self {
        Self::new()
    }
}
