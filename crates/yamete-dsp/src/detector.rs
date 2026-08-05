//! The slap detector: a five-way vote over the accelerometer, corroborated by the gyroscope.
//!
//! No single statistic separates "someone hit this laptop" from "someone typed hard" — each
//! detector here is sensitive to a different aspect of an impact, and requiring several to
//! agree is what keeps the false-positive rate down. The five are ported from the reference
//! implementations; the gyroscope stage is new.

use serde::{Deserialize, Serialize};

use crate::config::{Config, GyroMode};
use crate::filters::{samples_for, Ema, HighPass, RollingMax};

/// Severity of a detected impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Micro,
    Medium,
    Major,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Micro => "micro",
            Tier::Medium => "medium",
            Tier::Major => "major",
        }
    }
}

/// One sample from one sensor, in the sensor's own units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Seconds since the stream started.
    pub t: f64,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Frame {
    #[inline]
    pub fn magnitude(&self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }
}

/// A detected slap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    /// Seconds since the stream started.
    pub t: f64,
    pub tier: Tier,
    /// Peak high-passed acceleration, in g.
    pub peak_g: f32,
    /// Log-scaled 0..1, suitable for driving playback volume.
    pub intensity: f32,
    /// How many of the five detectors agreed.
    pub votes: u8,
    /// Whether the gyroscope saw matching angular rate.
    pub gyro_confirmed: bool,
    /// Peak angular rate accompanying the impact, in deg/s.
    pub gyro_peak: f32,
    /// Angular rate per unit linear acceleration, in deg/s per g.
    ///
    /// This is the discriminator that actually works: striking the lid applies a torque
    /// about the hinge, while a knock transmitted through the desk mostly translates the
    /// whole machine. Peak amplitude alone cannot tell those apart — measured desk bumps
    /// reach 0.56 g where a real slap is 0.05 g — but the rotation-to-translation ratio
    /// can.
    pub gyro_ratio: f32,
    /// Direction of the impact, as the unit high-passed acceleration vector.
    pub axis: [f32; 3],
}

/// Live detector state, for the tuning UI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Scores {
    /// High-passed acceleration magnitude, in g.
    pub envelope: f32,
    /// Best STA/LTA ratio across the three timescales.
    pub sta_lta: f32,
    pub cusum: f32,
    pub kurtosis: f32,
    pub peak_mad: f32,
    /// Gyro STA/LTA over its corroboration window.
    pub gyro: f32,
    /// Peak high-passed angular rate over the corroboration window, in deg/s.
    pub gyro_peak: f32,
    pub votes: u8,
    /// True while the cooldown is suppressing detections.
    pub in_cooldown: bool,
}

/// Running robust scale estimate: an EMA of absolute deviation from an EMA mean.
///
/// A true median absolute deviation would need a sorted window; this tracks the same
/// quantity closely enough at a fraction of the cost, and unlike a standard deviation it
/// is not inflated by the very impacts we're trying to detect.
#[derive(Debug, Clone)]
struct RobustScale {
    mean: Ema,
    dev: Ema,
}

impl RobustScale {
    fn new(tau_s: f32, rate_hz: f32) -> Self {
        RobustScale {
            mean: Ema::new(tau_s, rate_hz),
            dev: Ema::new(tau_s, rate_hz),
        }
    }

    /// Returns (mean, scale) where scale is comparable to a standard deviation.
    #[inline]
    fn push(&mut self, x: f32) -> (f32, f32) {
        let mean = self.mean.push(x);
        // 1.4826 rescales a mean absolute deviation to be σ-comparable for normal data.
        let scale = 1.4826 * self.dev.push((x - mean).abs());
        (mean, scale)
    }

    fn reset(&mut self) {
        self.mean.reset();
        self.dev.reset();
    }
}

/// EMA-based excess kurtosis, measuring how impulsive the signal is right now.
#[derive(Debug, Clone)]
struct Kurtosis {
    m1: Ema,
    m2: Ema,
    m4: Ema,
}

impl Kurtosis {
    fn new(tau_s: f32, rate_hz: f32) -> Self {
        Kurtosis {
            m1: Ema::new(tau_s, rate_hz),
            m2: Ema::new(tau_s, rate_hz),
            m4: Ema::new(tau_s, rate_hz),
        }
    }

    #[inline]
    fn push(&mut self, x: f32) -> f32 {
        let mean = self.m1.push(x);
        let d = x - mean;
        let d2 = d * d;
        let var = self.m2.push(d2);
        let fourth = self.m4.push(d2 * d2);
        if var <= 1e-12 {
            return 0.0;
        }
        // Excess kurtosis: 0 for a normal distribution, large for rare sharp spikes.
        (fourth / (var * var)) - 3.0
    }

    fn reset(&mut self) {
        self.m1.reset();
        self.m2.reset();
        self.m4.reset();
    }
}

/// One-sided CUSUM, accumulating sustained positive departures from the background.
///
/// The textbook formulation `S = max(0, S + z − k)` is a *rectified* random walk, and on
/// a one-sided, right-skewed signal like a vibration envelope it drifts upward without
/// bound — measured idle noise on an M4 Max drives it past 1000 in six seconds, which
/// makes any fixed threshold meaningless. The leak term below bounds the steady state
/// while leaving the response to a genuine sustained excursion intact.
#[derive(Debug, Clone)]
struct Cusum {
    sum: f32,
    /// Slack, in units of the robust scale — departures smaller than this decay away.
    slack: f32,
    /// Per-sample retention. Caps the accumulator at roughly `slack / (1 − leak)`.
    leak: f32,
}

impl Cusum {
    fn new(leak: f32) -> Self {
        Cusum {
            sum: 0.0,
            slack: 1.0,
            leak: leak.clamp(0.0, 1.0),
        }
    }

    #[inline]
    fn push(&mut self, x: f32, mean: f32, scale: f32) -> f32 {
        if scale <= 1e-9 {
            return self.sum;
        }
        let z = (x - mean) / scale;
        self.sum = (self.sum * self.leak + z - self.slack).max(0.0);
        self.sum
    }

    fn reset(&mut self) {
        self.sum = 0.0;
    }
}

/// The accelerometer processing chain.
struct AccelChannel {
    hp: [HighPass; 3],
    sta: [Ema; 3],
    lta: Ema,
    scale: RobustScale,
    kurtosis: Kurtosis,
    cusum: Cusum,
    peak_hold: RollingMax,
    /// Most recent high-passed vector, kept so a detection can report its direction.
    last_vec: [f32; 3],
}

impl AccelChannel {
    fn new(cfg: &Config) -> Self {
        AccelChannel {
            hp: std::array::from_fn(|_| HighPass::new(cfg.highpass_hz, cfg.rate_hz)),
            sta: std::array::from_fn(|i| Ema::new(cfg.sta_taus[i], cfg.rate_hz)),
            lta: Ema::new(cfg.lta_tau, cfg.rate_hz),
            scale: RobustScale::new(cfg.lta_tau, cfg.rate_hz),
            kurtosis: Kurtosis::new(cfg.lta_tau, cfg.rate_hz),
            cusum: Cusum::new(cfg.cusum_leak),
            peak_hold: RollingMax::new(samples_for(cfg.peak_hold_s, cfg.rate_hz)),
            last_vec: [0.0; 3],
        }
    }
}

/// The gyroscope processing chain — just enough to answer "did it also rotate?".
struct GyroChannel {
    hp: [HighPass; 3],
    sta: Ema,
    lta: Ema,
    /// Rolling max of the STA/LTA ratio over the corroboration window.
    window: RollingMax,
    /// Rolling max of the high-passed angular rate itself, in deg/s.
    amp_window: RollingMax,
}

impl GyroChannel {
    fn new(cfg: &Config) -> Self {
        GyroChannel {
            hp: std::array::from_fn(|_| HighPass::new(cfg.highpass_hz, cfg.rate_hz)),
            sta: Ema::new(cfg.sta_taus[0], cfg.rate_hz),
            lta: Ema::new(cfg.lta_tau, cfg.rate_hz),
            window: RollingMax::new(samples_for(cfg.gyro_window_s, cfg.rate_hz)),
            amp_window: RollingMax::new(samples_for(cfg.gyro_window_s, cfg.rate_hz)),
        }
    }
}

/// Streaming slap detector.
///
/// Feed accelerometer samples to [`Detector::push_accel`], which returns a [`Detection`]
/// on the sample where one fires. Gyroscope samples go to [`Detector::push_gyro`] and only
/// affect corroboration.
pub struct Detector {
    cfg: Config,
    accel: AccelChannel,
    gyro: GyroChannel,
    scores: Scores,
    last_fire_t: Option<f64>,
    /// Suppresses detections until the filters have seen enough data to have a meaningful
    /// background estimate — otherwise startup transients read as a slap.
    warmup_remaining: u32,
}

impl Detector {
    pub fn new(cfg: Config) -> Self {
        // Fold the sensitivity slider into the thresholds once, here, rather than
        // re-deriving it on every sample.
        let cfg = cfg.effective();
        // Two long-term time constants is enough for the background estimate to settle.
        let warmup = samples_for(cfg.lta_tau * 2.0, cfg.rate_hz) as u32;
        Detector {
            accel: AccelChannel::new(&cfg),
            gyro: GyroChannel::new(&cfg),
            scores: Scores::default(),
            last_fire_t: None,
            warmup_remaining: warmup,
            cfg,
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Replace the tuning parameters, rebuilding filter state.
    ///
    /// Retunes take effect on the next sample, at the cost of another warmup period —
    /// the alternative would be reinterpreting an old background estimate under new
    /// time constants, which produces a burst of spurious detections.
    pub fn set_config(&mut self, cfg: Config) {
        *self = Detector::new(cfg);
    }

    /// Current detector state, for live display.
    pub fn scores(&self) -> Scores {
        self.scores
    }

    /// Whether the detector is still building its background estimate.
    pub fn is_warming_up(&self) -> bool {
        self.warmup_remaining > 0
    }

    /// Feed one gyroscope sample.
    pub fn push_gyro(&mut self, f: Frame) {
        let g = &mut self.gyro;
        let hp = [g.hp[0].push(f.x), g.hp[1].push(f.y), g.hp[2].push(f.z)];
        let mag = (hp[0] * hp[0] + hp[1] * hp[1] + hp[2] * hp[2]).sqrt();
        let sta = g.sta.push(mag);
        let lta = g.lta.push(mag);
        let ratio = if lta > 1e-9 { sta / lta } else { 0.0 };
        g.window.push(ratio);
        g.amp_window.push(mag);
        self.scores.gyro = g.window.max();
        self.scores.gyro_peak = g.amp_window.max();
    }

    /// Feed one accelerometer sample, returning a detection if one fires here.
    pub fn push_accel(&mut self, f: Frame) -> Option<Detection> {
        let a = &mut self.accel;

        // Strip gravity, then work on the magnitude of what's left.
        let hp = [a.hp[0].push(f.x), a.hp[1].push(f.y), a.hp[2].push(f.z)];
        a.last_vec = hp;
        let mag = (hp[0] * hp[0] + hp[1] * hp[1] + hp[2] * hp[2]).sqrt();
        a.peak_hold.push(mag);

        let (mean, scale) = a.scale.push(mag);
        let lta = a.lta.push(mag);

        let sta_lta = a
            .sta
            .iter_mut()
            .map(|e| {
                let sta = e.push(mag);
                if lta > 1e-9 {
                    sta / lta
                } else {
                    0.0
                }
            })
            .fold(0.0f32, f32::max);

        let cusum = a.cusum.push(mag, mean, scale);
        let kurtosis = a.kurtosis.push(mag);
        let peak_mad = if scale > 1e-9 {
            (mag - mean) / scale
        } else {
            0.0
        };

        let th = &self.cfg.thresholds;
        let votes = u8::from(sta_lta >= th.sta_lta)
            + u8::from(cusum >= th.cusum)
            + u8::from(kurtosis >= th.kurtosis)
            + u8::from(peak_mad >= th.peak_mad)
            + u8::from(mag >= th.highpass_g);

        self.scores = Scores {
            envelope: mag,
            sta_lta,
            cusum,
            kurtosis,
            peak_mad,
            gyro: self.scores.gyro,
            gyro_peak: self.scores.gyro_peak,
            votes,
            in_cooldown: false,
        };

        if self.warmup_remaining > 0 {
            self.warmup_remaining -= 1;
            return None;
        }

        // Cooldown: one slap rings the chassis for a while, and we want one sound per hit.
        if let Some(last) = self.last_fire_t {
            if f.t - last < f64::from(self.cfg.cooldown_s) {
                self.scores.in_cooldown = true;
                return None;
            }
        }

        // Confidence and severity are separate questions. The vote count answers "is this
        // an impact rather than noise?"; the amplitude answers "how hard?".
        if votes < self.cfg.min_votes {
            return None;
        }

        let peak_g = a.peak_hold.max().max(mag);
        let tiers = &self.cfg.tiers;
        let tier = if peak_g >= tiers.major_g {
            Tier::Major
        } else if peak_g >= tiers.medium_g {
            Tier::Medium
        } else if peak_g >= tiers.micro_g {
            Tier::Micro
        } else {
            return None;
        };

        let gyro_peak = self.gyro.amp_window.max().max(0.0);
        let gyro_ratio = if peak_g > 1e-6 {
            gyro_peak / peak_g
        } else {
            0.0
        };
        // Either a large rotation relative to translation, or a large rotation outright.
        let gyro_confirmed =
            gyro_ratio >= self.cfg.gyro_ratio_min || gyro_peak >= self.cfg.gyro_peak_min;
        if self.cfg.gyro_mode == GyroMode::Require && !gyro_confirmed {
            return None;
        }

        self.last_fire_t = Some(f.t);
        // Reset CUSUM so the next detection starts from a clean accumulator rather than
        // riding the tail of this one.
        self.accel.cusum.reset();

        let norm = if mag > 1e-9 { 1.0 / mag } else { 0.0 };
        Some(Detection {
            t: f.t,
            tier,
            peak_g,
            intensity: intensity_for(peak_g, self.cfg.full_scale_g),
            votes,
            gyro_confirmed: match self.cfg.gyro_mode {
                GyroMode::Off => false,
                _ => gyro_confirmed,
            },
            gyro_peak,
            gyro_ratio,
            axis: [hp[0] * norm, hp[1] * norm, hp[2] * norm],
        })
    }

    /// Clear all filter state and restart the warmup.
    pub fn reset(&mut self) {
        for h in &mut self.accel.hp {
            h.reset();
        }
        for e in &mut self.accel.sta {
            e.reset();
        }
        self.accel.lta.reset();
        self.accel.scale.reset();
        self.accel.kurtosis.reset();
        self.accel.cusum.reset();
        self.accel.peak_hold.reset();
        for h in &mut self.gyro.hp {
            h.reset();
        }
        self.gyro.sta.reset();
        self.gyro.lta.reset();
        self.gyro.window.reset();
        self.gyro.amp_window.reset();
        self.scores = Scores::default();
        self.last_fire_t = None;
        self.warmup_remaining = samples_for(self.cfg.lta_tau * 2.0, self.cfg.rate_hz) as u32;
    }
}

/// Map peak amplitude to a 0..1 intensity on a log scale.
///
/// Linear amplitude maps poorly to perceived loudness, so this is logarithmic:
/// `log(1 + 99t) / log(100)`.
#[inline]
pub fn intensity_for(peak_g: f32, full_scale_g: f32) -> f32 {
    let t = (peak_g / full_scale_g.max(1e-6)).clamp(0.0, 1.0);
    ((1.0 + t * 99.0).ln() / 100f32.ln()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 805.0;

    /// Generate `n` seconds of a laptop sitting still: 1 g down plus a little sensor noise.
    fn quiet(secs: f64, start_t: f64) -> Vec<Frame> {
        let n = (secs * f64::from(RATE)) as usize;
        (0..n)
            .map(|i| {
                // Deterministic pseudo-noise, so tests don't need an RNG dependency.
                let p = (i as f32 * 12.9898).sin() * 43758.547;
                let noise = (p - p.floor() - 0.5) * 0.002;
                Frame {
                    t: start_t + i as f64 / f64::from(RATE),
                    x: noise,
                    y: noise * 0.7,
                    z: -1.0 + noise,
                }
            })
            .collect()
    }

    /// A decaying oscillation on top of gravity, which is what an impact actually looks
    /// like: the chassis rings and the ringing dies away.
    fn impact(at: f64, amplitude: f32, secs: f64) -> Vec<Frame> {
        let n = (secs * f64::from(RATE)) as usize;
        (0..n)
            .map(|i| {
                let dt = i as f32 / RATE;
                let env = amplitude * (-dt * 120.0).exp();
                let osc = (dt * 2.0 * std::f32::consts::PI * 180.0).sin();
                let v = env * osc;
                Frame {
                    t: at + f64::from(dt),
                    x: v * 0.3,
                    y: v * 0.2,
                    z: -1.0 + v,
                }
            })
            .collect()
    }

    /// Config for exercising the accelerometer ensemble on its own.
    ///
    /// The shipped default is `GyroMode::Require`, which correctly suppresses everything
    /// when no gyroscope samples are fed. These tests are about the five accelerometer
    /// detectors, so they opt out of corroboration explicitly rather than silently
    /// depending on the default.
    fn accel_only() -> Config {
        Config {
            gyro_mode: GyroMode::Annotate,
            ..Config::default()
        }
    }

    fn run(frames: &[Frame], cfg: Config) -> Vec<Detection> {
        let mut d = Detector::new(cfg);
        frames.iter().filter_map(|f| d.push_accel(*f)).collect()
    }

    #[test]
    fn quiet_machine_never_fires() {
        let hits = run(&quiet(6.0, 0.0), accel_only());
        assert!(hits.is_empty(), "false positives while idle: {hits:?}");
    }

    #[test]
    fn warmup_suppresses_the_startup_transient() {
        // The very first samples arrive with every filter unprimed; without a warmup
        // guard that alone reads as a large impact.
        let mut frames = vec![Frame {
            t: 0.0,
            x: 0.0,
            y: 0.0,
            z: -1.0,
        }];
        frames.extend(quiet(0.2, 0.001));
        assert!(run(&frames, accel_only()).is_empty());
    }

    #[test]
    fn detects_a_solid_impact() {
        let mut frames = quiet(2.0, 0.0);
        frames.extend(impact(2.0, 0.6, 0.5));
        let hits = run(&frames, accel_only());
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one detection, got {hits:?}"
        );
        assert_eq!(hits[0].tier, Tier::Major);
        assert!(hits[0].votes >= 4, "votes = {}", hits[0].votes);
        assert!(hits[0].intensity > 0.5, "intensity = {}", hits[0].intensity);
    }

    #[test]
    fn cooldown_collapses_ringing_into_one_detection() {
        // Three impacts 100 ms apart, well inside the 750 ms cooldown.
        let mut frames = quiet(2.0, 0.0);
        for k in 0..3 {
            frames.extend(impact(2.0 + f64::from(k) * 0.1, 0.6, 0.1));
        }
        frames.extend(quiet(1.0, 2.4));
        assert_eq!(run(&frames, accel_only()).len(), 1);
    }

    #[test]
    fn separate_slaps_past_the_cooldown_both_fire() {
        let mut frames = quiet(2.0, 0.0);
        frames.extend(impact(2.0, 0.6, 0.9));
        frames.extend(impact(2.9, 0.6, 0.9));
        assert_eq!(run(&frames, accel_only()).len(), 2);
    }

    #[test]
    fn tiers_track_impact_strength() {
        let tier_of = |amp: f32| {
            let mut frames = quiet(2.0, 0.0);
            frames.extend(impact(2.0, amp, 0.5));
            run(&frames, accel_only()).first().map(|d| d.tier)
        };
        assert_eq!(tier_of(0.8), Some(Tier::Major));
        // A tap far below every tier's amplitude floor must not register at all.
        assert_eq!(tier_of(0.0008), None);
    }

    #[test]
    fn sensitivity_factor_changes_what_registers() {
        let mut frames = quiet(2.0, 0.0);
        frames.extend(impact(2.0, 0.02, 0.3));

        // Too small to clear the default micro floor of 0.04 g at the midpoint.
        assert!(run(&frames, accel_only()).is_empty());
        // Pushing the slider up lowers every gate until it registers.
        assert!(!run(&frames, accel_only().with_sensitivity(1.0)).is_empty());
        // ...and pulling it down keeps it out.
        assert!(run(&frames, accel_only().with_sensitivity(0.0)).is_empty());
    }

    #[test]
    fn gyro_require_suppresses_uncorroborated_impacts() {
        let mut frames = quiet(2.0, 0.0);
        frames.extend(impact(2.0, 0.6, 0.5));

        let cfg = Config {
            gyro_mode: GyroMode::Require,
            ..Config::default()
        };
        // No gyro samples were ever pushed, so nothing can corroborate.
        assert!(run(&frames, cfg).is_empty());

        // The same signal in annotate mode fires, but honestly reports no corroboration.
        let hits = run(&frames, accel_only());
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].gyro_confirmed);
    }

    #[test]
    fn gyro_corroboration_is_reported_when_present() {
        let accel = {
            let mut f = quiet(2.0, 0.0);
            f.extend(impact(2.0, 0.6, 0.5));
            f
        };
        // Rotation about the hinge, simultaneous with the linear impact.
        let gyro = {
            let mut f: Vec<Frame> = quiet(2.0, 0.0)
                .into_iter()
                .map(|f| Frame { z: 0.0, ..f })
                .collect();
            f.extend(
                impact(2.0, 40.0, 0.5)
                    .into_iter()
                    .map(|f| Frame { z: f.z + 1.0, ..f }),
            );
            f
        };

        let cfg = Config {
            gyro_mode: GyroMode::Require,
            ..Config::default()
        };
        let mut d = Detector::new(cfg);
        let mut hits = Vec::new();
        for (a, g) in accel.iter().zip(gyro.iter()) {
            d.push_gyro(*g);
            if let Some(hit) = d.push_accel(*a) {
                hits.push(hit);
            }
        }
        assert_eq!(hits.len(), 1, "gyro should have corroborated this impact");
        assert!(hits[0].gyro_confirmed);
    }

    /// Severity must track how hard the machine was hit, not how many statistics agreed.
    ///
    /// The peaks below are real, taken from a live session: the same slap that reported
    /// 0.62 g was classified `micro` under the original vote-gated tiering, which is the
    /// bug this replaced.
    #[test]
    fn tiers_band_by_amplitude_across_a_real_session() {
        let cfg = Config::default();
        let tier_of = |peak: f32| {
            if peak >= cfg.tiers.major_g {
                Some(Tier::Major)
            } else if peak >= cfg.tiers.medium_g {
                Some(Tier::Medium)
            } else if peak >= cfg.tiers.micro_g {
                Some(Tier::Micro)
            } else {
                None
            }
        };

        let session = [
            (0.0632, Some(Tier::Micro)),
            (0.0544, Some(Tier::Micro)),
            (0.2806, Some(Tier::Medium)),
            (0.3513, Some(Tier::Major)),
            (0.4122, Some(Tier::Major)),
            (0.6187, Some(Tier::Major)),
            (0.0100, None), // below the floor entirely
        ];
        for (peak, expected) in session {
            assert_eq!(tier_of(peak), expected, "peak {peak} g classified wrongly");
        }

        // And the bands must actually be ordered, or the if-chain silently collapses.
        assert!(cfg.tiers.micro_g < cfg.tiers.medium_g);
        assert!(cfg.tiers.medium_g < cfg.tiers.major_g);
    }

    #[test]
    fn min_votes_gates_firing_without_touching_severity() {
        let mut frames = quiet(2.0, 0.0);
        frames.extend(impact(2.0, 0.6, 0.5));

        // A hard impact is major regardless of how many detectors happened to agree...
        let hits = run(&frames, accel_only());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tier, Tier::Major);

        // ...but demanding all five detectors suppresses it entirely.
        let strict = Config {
            min_votes: 5,
            ..accel_only()
        };
        assert!(run(&frames, strict).is_empty());
    }

    #[test]
    fn intensity_is_monotonic_and_bounded() {
        assert_eq!(intensity_for(0.0, 1.0), 0.0);
        assert!((intensity_for(1.0, 1.0) - 1.0).abs() < 1e-5);
        assert!(intensity_for(0.1, 1.0) < intensity_for(0.5, 1.0));
        // Clamps rather than exceeding 1 for over-scale impacts.
        assert!((intensity_for(50.0, 1.0) - 1.0).abs() < 1e-5);
        // Log scaling: a tenth of full scale is much louder than a tenth of the range.
        assert!(intensity_for(0.1, 1.0) > 0.4);
    }

    #[test]
    fn axis_reports_impact_direction_as_a_unit_vector() {
        let mut frames = quiet(2.0, 0.0);
        frames.extend(impact(2.0, 0.6, 0.5));
        let hits = run(&frames, accel_only());
        let a = hits[0].axis;
        let len = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-3, "axis not normalised: {len}");
    }

    #[test]
    fn retuning_restarts_cleanly_without_spurious_hits() {
        let mut d = Detector::new(accel_only());
        for f in quiet(2.0, 0.0) {
            d.push_accel(f);
        }
        d.set_config(accel_only().with_sensitivity(0.5));
        let hits: Vec<_> = quiet(2.0, 2.0)
            .into_iter()
            .filter_map(|f| d.push_accel(f))
            .collect();
        assert!(hits.is_empty(), "retune produced false positives: {hits:?}");
    }
}
