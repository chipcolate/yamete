//! Parsing of the 22-byte `AppleSPUHIDDevice` input report.
//!
//! The HID report descriptor for these devices is an opaque blob — `Report Size 8,
//! Report Count 22, Input(Data,Var,Abs)` with no report ID and no field structure — so
//! the layout below is empirical, decoded by capturing live reports from both the
//! accelerometer (usage 3) and gyroscope (usage 9) simultaneously:
//!
//! ```text
//! off  size  field
//! 0    2     u16 LE  sequence counter, +1 per report, wraps at 0xFFFF
//! 2    4     always zero (likely the high bits of a u48 counter)
//! 6    4     i32 LE  X  IOFixed 16.16
//! 10   4     i32 LE  Y
//! 14   4     i32 LE  Z
//! 18   4     i32 LE  die temperature °C, IOFixed 16.16
//! ```
//!
//! Bytes 18..22 are *not* a timestamp: the value is non-monotonic and is bit-identical
//! between the accel and gyro devices sampled at the same instant, which is what you'd
//! expect from a shared die-temperature reading.

/// Every report from these devices is exactly this long.
pub const REPORT_LEN: usize = 22;

/// Apple's `IOFixed` is Q16.16 fixed point, so the divisor is 2^16.
const IOFIXED_SCALE: f32 = 65536.0;

/// One decoded sensor report.
///
/// Units are g for the accelerometer and deg/s for the gyroscope — the wire format is
/// identical, only the interpretation differs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Hardware sequence counter. Increments by exactly 1 per report and wraps at
    /// `u16::MAX`; a gap means the consumer fell behind and reports were dropped.
    pub seq: u16,
    /// `mach_absolute_time` at which the kernel timestamped the report.
    pub host_time: u64,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Die temperature in °C.
    pub temp_c: f32,
}

impl Sample {
    /// Euclidean magnitude of the three axes.
    #[inline]
    pub fn magnitude(&self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }
}

#[inline]
fn fixed16_16(bytes: &[u8]) -> f32 {
    let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    raw as f32 / IOFIXED_SCALE
}

/// Decode a raw input report. Returns `None` if the report is not the expected length.
#[inline]
pub fn parse(buf: &[u8], host_time: u64) -> Option<Sample> {
    if buf.len() != REPORT_LEN {
        return None;
    }
    Some(Sample {
        seq: u16::from_le_bytes([buf[0], buf[1]]),
        host_time,
        x: fixed16_16(&buf[6..10]),
        y: fixed16_16(&buf[10..14]),
        z: fixed16_16(&buf[14..18]),
        temp_c: fixed16_16(&buf[18..22]),
    })
}

/// Number of reports lost between two sequence counter values, accounting for wraparound.
#[inline]
pub fn gap(prev: u16, next: u16) -> u32 {
    u32::from(next.wrapping_sub(prev).wrapping_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three consecutive reports captured from a Mac16,5 lying flat on a desk.
    const FLAT: [[u8; REPORT_LEN]; 3] = [
        [
            0x04, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x4c, 0x01, 0x00, 0x00, 0x8f, 0x01, 0x00, 0x00,
            0xad, 0x00, 0xff, 0xff, 0x80, 0xed, 0x2a, 0x00,
        ],
        [
            0x05, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x53, 0x01, 0x00, 0x00, 0x8a, 0x01, 0x00, 0x00,
            0xba, 0x00, 0xff, 0xff, 0x80, 0xed, 0x2a, 0x00,
        ],
        [
            0x06, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x57, 0x01, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00,
            0xcc, 0x00, 0xff, 0xff, 0x80, 0xed, 0x2a, 0x00,
        ],
    ];

    #[test]
    fn decodes_a_captured_report() {
        let s = parse(&FLAT[0], 42).unwrap();
        assert_eq!(s.seq, 0x5d04);
        assert_eq!(s.host_time, 42);
        // Flat on a desk: x and y near zero, z near -1 g.
        assert!((s.x - 0.0051).abs() < 1e-3, "x = {}", s.x);
        assert!((s.y - 0.0061).abs() < 1e-3, "y = {}", s.y);
        assert!((s.z + 0.9974).abs() < 1e-3, "z = {}", s.z);
        // Magnitude at rest must be ~1 g, which is the load-bearing sanity check on
        // both the byte offsets and the 65536 divisor.
        assert!(
            (s.magnitude() - 1.0).abs() < 0.01,
            "|a| = {}",
            s.magnitude()
        );
    }

    #[test]
    fn sign_extends_negative_axes() {
        // z = ad 00 ff ff = -0.9974 g; a naive u32 read would give +65535.something.
        let s = parse(&FLAT[0], 0).unwrap();
        assert!(s.z < 0.0);
    }

    #[test]
    fn temperature_is_plausible_and_stable() {
        for raw in &FLAT {
            let s = parse(raw, 0).unwrap();
            assert!((40.0..50.0).contains(&s.temp_c), "temp = {}", s.temp_c);
        }
    }

    #[test]
    fn sequence_counter_is_contiguous_across_captures() {
        let a = parse(&FLAT[0], 0).unwrap();
        let b = parse(&FLAT[1], 0).unwrap();
        let c = parse(&FLAT[2], 0).unwrap();
        assert_eq!(gap(a.seq, b.seq), 0);
        assert_eq!(gap(b.seq, c.seq), 0);
    }

    #[test]
    fn gap_handles_wraparound() {
        assert_eq!(gap(10, 11), 0);
        assert_eq!(gap(10, 14), 3);
        assert_eq!(gap(u16::MAX, 0), 0);
        assert_eq!(gap(u16::MAX - 1, 2), 3);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(parse(&[0u8; 21], 0).is_none());
        assert!(parse(&[0u8; 23], 0).is_none());
        assert!(parse(&[], 0).is_none());
    }
}
