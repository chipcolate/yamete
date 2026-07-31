//! Detector tuning parameters.
//!
//! The reference implementations (MacSlapApp, taigrr/spank) publish threshold constants —
//! 0.005 g for a micro shock, an STA/LTA ratio of 4, and so on. Measured against 20 s of
//! genuinely idle recording on an M4 Max, **every one of those sits below this machine's
//! noise floor**: the envelope alone reaches 0.0159 g at rest and STA/LTA touches 8.3.
//! Adopting them verbatim produces a detector that fires on nothing but noise.
//!
//! So the defaults below are derived from measurement instead, set roughly 2× above the
//! observed idle maximum of each statistic. `spankd analyze` on a fixture annotated
//! `expect=0` prints those maxima and flags any threshold that falls under one, which is
//! how to re-derive these for a different machine or a noisier desk.

use serde::{Deserialize, Serialize};

/// How the gyroscope is allowed to influence detection.
///
/// A slap on the lid imparts angular rate about the hinge; typing, trackpad clicks and
/// desk bumps largely produce linear acceleration without it. No reference project uses
/// the gyro at all — and on the recorded corpus it is the *only* feature that separates a
/// slap from a knock on the desk, so `Require` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GyroMode {
    /// Ignore the gyroscope entirely.
    Off,
    /// Report whether the gyro corroborated, but never suppress on its absence.
    Annotate,
    /// Suppress accelerometer triggers the gyroscope does not corroborate.
    Require,
}

impl Default for GyroMode {
    fn default() -> Self {
        GyroMode::Require
    }
}

/// Thresholds for the five accelerometer detectors.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// STA/LTA ratio above which a timescale is considered triggered.
    pub sta_lta: f32,
    /// CUSUM accumulator level, in units of the running robust scale.
    pub cusum: f32,
    /// Excess kurtosis above which the signal counts as impulsive.
    pub kurtosis: f32,
    /// Peak over robust scale (a modified z-score).
    pub peak_mad: f32,
    /// Absolute high-passed magnitude, in g.
    pub highpass_g: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        // Set from the gap between the idle p99.9 and the scores measured at real
        // detections (`spankd analyze --at-detections`):
        //
        //   detector    idle p99.9   idle max   slap p25   slap median
        //   envelope        0.0128     0.0268     0.0400        0.0500
        //   sta/lta           7.50      13.09      10.00         12.52
        //   peak/mad          7.66      22.24      15.22         18.49
        //   kurtosis         528.8      537.6      229.6         487.4
        //   cusum            130.8      179.8       26.5          43.6
        //
        // Kurtosis and CUSUM do not separate the two populations at all — CUSUM actually
        // reads *lower* during slaps than during idle noise, because it accumulates
        // sustained drift while an impact is a single sharp spike.
        //
        // The instinct on seeing that was to lower every threshold so more detectors
        // would vote. Measured, that was wrong: it let trackpad clicks accumulate votes
        // too, and cost a false positive. Since severity no longer depends on the vote
        // count, the votes only have to answer "impact or noise?" — and strict thresholds
        // answer that better. These sit clear of the idle ceiling and are left there.
        Thresholds {
            sta_lta: 15.0,
            cusum: 200.0,
            kurtosis: 1100.0,
            peak_mad: 25.0,
            highpass_g: 0.035,
        }
    }
}

/// Peak amplitude bands for each severity tier.
///
/// The reference implementations gate tiers on *vote count* — 4+ detectors for a major
/// shock, 3+ for medium. Measured against real slaps that turns out to be the wrong axis:
/// the detectors are a confidence signal ("is this an impact at all?"), while severity is
/// simply how hard the machine was hit. Gating on votes meant a 0.62 g whack that only
/// tripped two detectors was reported as a micro shock, which is plainly wrong.
///
/// So tiers are amplitude only, and confidence lives in [`Config::min_votes`]. Bands are
/// set from the recorded corpus, where slap peaks span 0.04 g to 0.62 g.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tiers {
    /// A deliberate, hard hit.
    pub major_g: f32,
    /// A normal slap.
    pub medium_g: f32,
    /// The softest impact worth reporting.
    pub micro_g: f32,
}

impl Default for Tiers {
    fn default() -> Self {
        Tiers {
            major_g: 0.30,
            medium_g: 0.12,
            micro_g: 0.04,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Nominal sample rate. The hardware delivers ~805 Hz.
    pub rate_hz: f32,

    /// Gravity-strip corner frequency, in Hz.
    pub highpass_hz: f32,

    /// The three short-term averaging windows, in seconds.
    pub sta_taus: [f32; 3],
    /// Long-term averaging window, in seconds — the "background" level.
    pub lta_tau: f32,

    pub thresholds: Thresholds,
    pub tiers: Tiers,

    /// Minimum gap between detections, in seconds. Prevents one slap firing repeatedly
    /// as the machine rings.
    pub cooldown_s: f32,

    /// Window over which the reported peak is held, in seconds. The threshold crossing
    /// usually precedes the true peak by a few samples.
    pub peak_hold_s: f32,

    /// Per-sample retention of the CUSUM accumulator. Without a leak below 1.0 the
    /// accumulator grows without bound on ordinary noise.
    pub cusum_leak: f32,

    pub gyro_mode: GyroMode,
    /// Minimum angular rate per unit linear acceleration, in deg/s per g, for an impact
    /// to count as a strike on the machine rather than a knock transmitted through it.
    ///
    /// Measured over the recorded corpus: real slaps run 109-1604, while desk bumps,
    /// trackpad clicks and lid adjustments run 12-254. This single ratio does what neither
    /// peak amplitude nor the five-detector vote could.
    pub gyro_ratio_min: f32,

    /// Absolute peak angular rate, in deg/s, that on its own confirms a strike.
    ///
    /// The ratio test alone rejects genuine hard slaps that also produced a large linear
    /// acceleration. Measured over the corpus every negative peaks below 14.6 deg/s while
    /// hard slaps reach 16-81, so a sufficiently violent rotation is evidence by itself.
    /// Corroboration is the union of the two rules.
    pub gyro_peak_min: f32,
    /// How far back to look for gyro corroboration, in seconds.
    ///
    /// Backward-looking only: the gyro transient is simultaneous with the accelerometer
    /// one, so waiting for a forward window would add latency to the sound for nothing.
    pub gyro_window_s: f32,

    /// How many of the five detectors must agree before an impact counts at all.
    ///
    /// Purely a confidence gate — it has nothing to do with how hard the hit was. Measured
    /// over the corpus, a real slap trips a mean of 2.2 detectors, so requiring more than
    /// two rejects most genuine slaps.
    pub min_votes: u8,

    /// Peak amplitude mapped to intensity 1.0, in g.
    pub full_scale_g: f32,

    /// The user-facing sensitivity slider, 0.0 to 1.0.
    ///
    /// 0.5 leaves the calibrated baselines below exactly as measured. Moving the slider
    /// scales every gate that decides whether an impact registers — both the amplitude
    /// floors *and* the gyroscope corroboration thresholds. Scaling only the amplitudes
    /// (the obvious implementation) would be close to inert, because on this hardware it
    /// is the gyro gate that rejects marginal impacts, not the amplitude floor.
    ///
    /// Measured over the recorded corpus (40 slaps, 150 s of negatives):
    ///
    /// | slider | slaps caught | false positives | character |
    /// |--------|--------------|-----------------|-----------|
    /// | 0.20   | 13/40        | 0               | only a deliberate whack |
    /// | 0.35   | 20/40        | 0               | never fires by accident |
    /// | 0.50   | 36/40        | 1               | default |
    /// | 0.65   | 39/40        | 8               | catches everything, desk bumps too |
    /// | 0.80   | 40/40        | 12              | twitchy |
    /// | 1.00   | 40/40        | 20              | anything that shakes the desk |
    ///
    /// Typing never triggers a detection at any slider position, which is the one case
    /// that would make the app unusable.
    pub sensitivity: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            rate_hz: 805.0,
            highpass_hz: 5.0,
            sta_taus: [0.005, 0.015, 0.040],
            lta_tau: 0.5,
            thresholds: Thresholds::default(),
            tiers: Tiers::default(),
            cooldown_s: 0.75,
            peak_hold_s: 0.006,
            cusum_leak: 0.95,
            gyro_mode: GyroMode::default(),
            gyro_ratio_min: 175.0,
            gyro_peak_min: 15.0,
            gyro_window_s: 0.030,
            min_votes: 2,
            full_scale_g: 1.0,
            sensitivity: 0.5,
        }
    }
}

/// How far the slider is allowed to move the thresholds, as a base-10 exponent.
///
/// 1.2 spans a factor of ~16 end to end: at maximum sensitivity every gate drops to 0.25x
/// its calibrated value, at minimum it rises to 4x.
const SENSITIVITY_DECADES: f32 = 1.2;

impl Config {
    /// Set the sensitivity slider, clamped to 0.0..=1.0.
    pub fn with_sensitivity(mut self, sensitivity: f32) -> Self {
        self.sensitivity = sensitivity.clamp(0.0, 1.0);
        self
    }

    /// Threshold multiplier implied by the current slider position.
    ///
    /// Logarithmic rather than linear, so each step of the slider feels like a comparable
    /// change — the thresholds span more than an order of magnitude, and a linear map
    /// would make the whole useful range sit in one corner of the travel.
    pub fn sensitivity_scale(&self) -> f32 {
        10f32.powf(-(self.sensitivity.clamp(0.0, 1.0) - 0.5) * 2.0 * SENSITIVITY_DECADES / 2.0)
    }

    /// The config with the sensitivity slider folded into the thresholds.
    ///
    /// [`Detector::new`](crate::Detector::new) applies this, so the stored baselines stay
    /// as calibrated and the slider remains a single reversible parameter rather than a
    /// destructive edit of every threshold.
    pub fn effective(&self) -> Config {
        let k = self.sensitivity_scale();
        let mut c = *self;
        c.thresholds.highpass_g *= k;
        c.tiers.major_g *= k;
        c.tiers.medium_g *= k;
        c.tiers.micro_g *= k;
        // The gyro gate is the dominant discriminator, so the slider must move it too or
        // most of the travel does nothing.
        c.gyro_ratio_min *= k;
        c.gyro_peak_min *= k;
        // Already amplitude-independent, but they still bound what counts as a vote.
        c.thresholds.sta_lta *= k.sqrt();
        c.thresholds.peak_mad *= k.sqrt();
        c.sensitivity = 0.5;
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midpoint_leaves_thresholds_untouched() {
        let c = Config::default();
        assert_eq!(c.sensitivity, 0.5);
        assert!((c.sensitivity_scale() - 1.0).abs() < 1e-5);

        let e = c.effective();
        assert_eq!(e.tiers.micro_g, c.tiers.micro_g);
        assert_eq!(e.gyro_ratio_min, c.gyro_ratio_min);
        assert_eq!(e.gyro_peak_min, c.gyro_peak_min);
    }

    #[test]
    fn higher_sensitivity_lowers_every_gate() {
        let low = Config::default().with_sensitivity(0.2).effective();
        let mid = Config::default().effective();
        let high = Config::default().with_sensitivity(0.9).effective();

        // The gyro gate is the dominant discriminator; if the slider does not move it,
        // most of the travel is inert.
        assert!(high.gyro_ratio_min < mid.gyro_ratio_min);
        assert!(mid.gyro_ratio_min < low.gyro_ratio_min);
        assert!(high.gyro_peak_min < mid.gyro_peak_min);
        assert!(high.tiers.micro_g < mid.tiers.micro_g);
        assert!(mid.tiers.micro_g < low.tiers.micro_g);
        assert!(high.thresholds.highpass_g < low.thresholds.highpass_g);
    }

    #[test]
    fn slider_travel_spans_a_useful_range() {
        let full = Config::default().with_sensitivity(1.0).sensitivity_scale();
        let none = Config::default().with_sensitivity(0.0).sensitivity_scale();
        let span = none / full;
        assert!(
            (10.0..40.0).contains(&span),
            "slider spans {span}x — too narrow to be worth exposing, or so wide the \
             middle of the travel is unusable"
        );
    }

    #[test]
    fn slider_is_clamped() {
        assert_eq!(Config::default().with_sensitivity(-5.0).sensitivity, 0.0);
        assert_eq!(Config::default().with_sensitivity(99.0).sensitivity, 1.0);
        // Out-of-range input must not produce a nonsensical multiplier.
        assert!(Config::default().with_sensitivity(99.0).sensitivity_scale() > 0.0);
    }

    #[test]
    fn applying_effective_twice_is_idempotent() {
        // Detector::new folds the slider in; doing it again must not compound the scaling.
        let once = Config::default().with_sensitivity(0.8).effective();
        let twice = once.effective();
        assert_eq!(once.gyro_ratio_min, twice.gyro_ratio_min);
        assert_eq!(once.tiers.micro_g, twice.tiers.micro_g);
    }

    #[test]
    fn round_trips_through_json() {
        let c = Config::default().with_sensitivity(0.7);
        let text = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Config files written by an older build must still load.
        let back: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(back, Config::default());
        let back: Config = serde_json::from_str(r#"{"sensitivity":0.9}"#).unwrap();
        assert_eq!(back.sensitivity, 0.9);
        assert_eq!(back.gyro_ratio_min, Config::default().gyro_ratio_min);
    }
}
