//! Converting the kernel's report timestamps into seconds.
//!
//! IOKit stamps each report with `mach_absolute_time`, which counts in an abstract unit
//! that is not nanoseconds and whose scale differs between Apple Silicon and Intel. The
//! conversion ratio comes from `mach_timebase_info` and is fixed for the life of the boot.

#[repr(C)]
#[derive(Clone, Copy)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::kern_return_t;
}

/// Converts `mach_absolute_time` ticks to seconds, relative to a chosen origin.
#[derive(Debug, Clone, Copy)]
pub struct Timebase {
    /// Nanoseconds per tick.
    ns_per_tick: f64,
    origin: Option<u64>,
}

impl Timebase {
    pub fn new() -> Self {
        let mut info = MachTimebaseInfo { numer: 1, denom: 1 };
        // Only fails on a bad pointer, which cannot happen here.
        unsafe { mach_timebase_info(&mut info) };
        Timebase {
            ns_per_tick: f64::from(info.numer) / f64::from(info.denom),
            origin: None,
        }
    }

    /// Seconds since the first host time this instance was shown.
    ///
    /// The first call establishes the origin, so both sensor streams must share one
    /// `Timebase` for their timestamps to be comparable.
    #[inline]
    pub fn seconds_since_start(&mut self, host_time: u64) -> f64 {
        let origin = *self.origin.get_or_insert(host_time);
        // Guard against a report stamped fractionally before the origin.
        let ticks = host_time.saturating_sub(origin);
        ticks as f64 * self.ns_per_tick / 1e9
    }

    /// Whether an origin has been established yet.
    pub fn is_started(&self) -> bool {
        self.origin.is_some()
    }
}

impl Default for Timebase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_is_time_zero() {
        let mut tb = Timebase::new();
        assert!(!tb.is_started());
        assert_eq!(tb.seconds_since_start(1_000_000), 0.0);
        assert!(tb.is_started());
    }

    #[test]
    fn advances_monotonically_in_real_seconds() {
        let mut tb = Timebase::new();
        let t0 = unsafe { mach_absolute_time_now() };
        tb.seconds_since_start(t0);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let elapsed = tb.seconds_since_start(unsafe { mach_absolute_time_now() });
        assert!(
            (0.04..0.20).contains(&elapsed),
            "50 ms of sleep measured as {elapsed}s — timebase scaling is wrong"
        );
    }

    #[test]
    fn a_timestamp_before_the_origin_clamps_to_zero() {
        let mut tb = Timebase::new();
        tb.seconds_since_start(1_000_000);
        assert_eq!(tb.seconds_since_start(999_000), 0.0);
    }

    extern "C" {
        #[link_name = "mach_absolute_time"]
        fn mach_absolute_time_now() -> u64;
    }
}
