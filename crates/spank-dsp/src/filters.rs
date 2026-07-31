//! Streaming primitives shared by the detectors.
//!
//! Everything here is O(1) per sample with no allocation, because it all runs on every
//! one of the ~805 samples per second per sensor.

/// First-order IIR high-pass, used to strip gravity from the accelerometer.
///
/// `y[n] = a·(y[n-1] + x[n] − x[n-1])`, the standard DC-blocker. At rest this drives the
/// output to zero regardless of orientation, which is exactly what we want: a slap is a
/// transient, and the 1 g DC term carries no information about it.
#[derive(Debug, Clone)]
pub struct HighPass {
    a: f32,
    prev_in: f32,
    prev_out: f32,
    primed: bool,
}

impl HighPass {
    /// `cutoff_hz` is the −3 dB corner; `rate_hz` the sample rate.
    pub fn new(cutoff_hz: f32, rate_hz: f32) -> Self {
        let a = (-2.0 * std::f32::consts::PI * cutoff_hz / rate_hz).exp();
        HighPass {
            a,
            prev_in: 0.0,
            prev_out: 0.0,
            primed: false,
        }
    }

    #[inline]
    pub fn push(&mut self, x: f32) -> f32 {
        // Seed from the first sample so we don't emit a huge spurious step as the filter
        // charges up from zero against a standing 1 g.
        if !self.primed {
            self.prev_in = x;
            self.primed = true;
            return 0.0;
        }
        let y = self.a * (self.prev_out + x - self.prev_in);
        self.prev_in = x;
        self.prev_out = y;
        y
    }

    pub fn reset(&mut self) {
        self.prev_in = 0.0;
        self.prev_out = 0.0;
        self.primed = false;
    }
}

/// Exponential moving average — our O(1) stand-in for a boxcar window.
#[derive(Debug, Clone)]
pub struct Ema {
    alpha: f32,
    value: f32,
    primed: bool,
}

impl Ema {
    /// `tau_s` is the time constant: the window over which the average effectively spans.
    pub fn new(tau_s: f32, rate_hz: f32) -> Self {
        Ema {
            alpha: 1.0 - (-1.0 / (tau_s * rate_hz)).exp(),
            value: 0.0,
            primed: false,
        }
    }

    #[inline]
    pub fn push(&mut self, x: f32) -> f32 {
        if !self.primed {
            self.value = x;
            self.primed = true;
        } else {
            self.value += self.alpha * (x - self.value);
        }
        self.value
    }

    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn reset(&mut self) {
        self.value = 0.0;
        self.primed = false;
    }
}

/// Rolling maximum over a fixed number of recent samples.
///
/// Used both for peak-hold (so reported intensity reflects the true impact peak rather
/// than whichever sample happened to cross the threshold first) and for the gyroscope
/// corroboration window.
#[derive(Debug, Clone)]
pub struct RollingMax {
    buf: Box<[f32]>,
    idx: usize,
}

impl RollingMax {
    pub fn new(len: usize) -> Self {
        RollingMax {
            buf: vec![f32::NEG_INFINITY; len.max(1)].into_boxed_slice(),
            idx: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        self.buf[self.idx] = x;
        self.idx = (self.idx + 1) % self.buf.len();
    }

    /// Largest value currently in the window.
    #[inline]
    pub fn max(&self) -> f32 {
        self.buf.iter().copied().fold(f32::NEG_INFINITY, f32::max)
    }

    pub fn reset(&mut self) {
        self.buf.fill(f32::NEG_INFINITY);
        self.idx = 0;
    }
}

/// Convert a time constant in seconds to a whole number of samples, at least 1.
#[inline]
pub fn samples_for(seconds: f32, rate_hz: f32) -> usize {
    ((seconds * rate_hz).round() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 805.0;

    #[test]
    fn highpass_rejects_dc() {
        let mut hp = HighPass::new(5.0, RATE);
        // A constant 1 g, as if lying flat forever.
        let mut last = 0.0;
        for _ in 0..4000 {
            last = hp.push(-1.0);
        }
        assert!(last.abs() < 1e-4, "DC leaked through: {last}");
    }

    #[test]
    fn highpass_does_not_spike_on_first_sample() {
        // Priming matters: without it the first sample reads as a 1 g step and every
        // detector fires the instant the daemon starts.
        let mut hp = HighPass::new(5.0, RATE);
        assert_eq!(hp.push(-1.0), 0.0);
        assert!(hp.push(-1.0).abs() < 1e-6);
    }

    #[test]
    fn highpass_passes_a_transient() {
        let mut hp = HighPass::new(5.0, RATE);
        for _ in 0..2000 {
            hp.push(-1.0);
        }
        let spike = hp.push(-3.0);
        assert!(spike.abs() > 1.5, "transient was attenuated: {spike}");
    }

    #[test]
    fn ema_converges_to_a_constant() {
        let mut ema = Ema::new(0.05, RATE);
        for _ in 0..2000 {
            ema.push(4.0);
        }
        assert!((ema.value() - 4.0).abs() < 1e-3);
    }

    #[test]
    fn ema_primes_to_first_value() {
        let mut ema = Ema::new(1.0, RATE);
        assert_eq!(ema.push(7.0), 7.0);
    }

    #[test]
    fn rolling_max_forgets_old_values() {
        let mut m = RollingMax::new(3);
        m.push(1.0);
        m.push(9.0);
        m.push(2.0);
        assert_eq!(m.max(), 9.0);

        // Still within the 3-sample window: [9, 2, 3].
        m.push(3.0);
        assert_eq!(m.max(), 9.0);

        // Now the 9.0 has aged out: [2, 3, 4].
        m.push(4.0);
        assert_eq!(m.max(), 4.0, "the 9.0 should have rolled out of the window");
    }

    #[test]
    fn samples_for_is_never_zero() {
        assert_eq!(samples_for(0.0, RATE), 1);
        assert_eq!(samples_for(1.0, 805.0), 805);
    }
}
