//! Slap detection signal processing.
//!
//! Deliberately free of any platform or I/O dependency: it takes [`Frame`]s in and emits
//! [`Detection`]s out, so the whole detector can be replayed against recorded traces in a
//! unit test rather than only being testable by hitting a laptop.

pub mod config;
pub mod detector;
pub mod filters;
pub mod fixture;

pub use config::{Config, GyroMode, Thresholds, Tiers};
pub use detector::{intensity_for, Detection, Detector, Frame, Scores, Tier};
pub use fixture::Fixture;
