//! `yamete sweep` — pick a threshold by scoring it against the whole corpus.
//!
//! Reading detection lists and eyeballing a cutoff is how you end up overfitting to the
//! two examples you happened to look at. This scores every candidate value against every
//! recorded fixture and prints the confusion matrix, so the choice is visible and the
//! cost of moving it in either direction is explicit.

use std::path::PathBuf;

use yamete_dsp::{Config, GyroMode};
use yamete_sensor::Error;

use crate::replay;

/// What a fixture's annotation says it is.
enum Kind {
    /// `expect = 0`: any detection is a false positive.
    Negative,
    /// `expect = n`: exactly n slaps are present.
    Positive(usize),
}

struct Scored {
    label: String,
    kind: Kind,
    fixture: yamete_dsp::Fixture,
}

/// Which knob to sweep.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Knob {
    /// Minimum angular rate per unit acceleration, in deg/s per g.
    GyroRatio,
    /// Micro-tier amplitude floor, in g.
    MicroG,
    /// Minimum detector votes required to fire at all.
    MinVotes,
    /// Absolute angular rate that confirms on its own, in deg/s.
    GyroPeak,
    /// Backward window over which gyro corroboration is looked for, in seconds.
    GyroWindow,
    /// The user-facing sensitivity slider, 0.0 to 1.0.
    Sensitivity,
    /// Excess-kurtosis threshold.
    Kurtosis,
    /// STA/LTA ratio threshold.
    StaLta,
    /// Peak-over-robust-scale threshold.
    PeakMad,
}

impl Knob {
    fn values(self) -> Vec<f32> {
        match self {
            Knob::GyroRatio => (0..=20).map(|i| i as f32 * 25.0).collect(),
            Knob::MicroG => (1..=20).map(|i| i as f32 * 0.005).collect(),
            Knob::MinVotes => (1..=5).map(|i| i as f32).collect(),
            Knob::GyroPeak => (0..=24).map(|i| i as f32 * 2.5).collect(),
            Knob::GyroWindow => (1..=16).map(|i| i as f32 * 0.005).collect(),
            Knob::Sensitivity => (0..=20).map(|i| i as f32 * 0.05).collect(),
            Knob::Kurtosis => (1..=20).map(|i| i as f32 * 100.0).collect(),
            Knob::StaLta => (1..=20).map(|i| i as f32 * 2.5).collect(),
            Knob::PeakMad => (1..=20).map(|i| i as f32 * 2.5).collect(),
        }
    }

    fn apply(self, cfg: &mut Config, v: f32) {
        match self {
            Knob::GyroRatio => cfg.gyro_ratio_min = v,
            Knob::MicroG => cfg.tiers.micro_g = v,
            Knob::MinVotes => cfg.min_votes = v as u8,
            Knob::GyroPeak => cfg.gyro_peak_min = v,
            Knob::GyroWindow => cfg.gyro_window_s = v,
            Knob::Sensitivity => cfg.sensitivity = v,
            Knob::Kurtosis => cfg.thresholds.kurtosis = v,
            Knob::StaLta => cfg.thresholds.sta_lta = v,
            Knob::PeakMad => cfg.thresholds.peak_mad = v,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Knob::GyroRatio => "gyro_ratio_min",
            Knob::MicroG => "micro_g",
            Knob::MinVotes => "min_votes",
            Knob::GyroPeak => "gyro_peak_min",
            Knob::GyroWindow => "gyro_window_s",
            Knob::Sensitivity => "sensitivity",
            Knob::Kurtosis => "kurtosis",
            Knob::StaLta => "sta_lta",
            Knob::PeakMad => "peak_mad",
        }
    }
}

pub fn run(files: &[PathBuf], knob: Knob) -> Result<(), Error> {
    let mut corpus = Vec::new();
    for path in files {
        let fixture = replay::load(path)?;
        let Some(expect) = fixture.expect else {
            continue;
        };
        corpus.push(Scored {
            label: fixture.label.clone(),
            kind: if expect == 0 {
                Kind::Negative
            } else {
                Kind::Positive(expect)
            },
            fixture,
        });
    }

    if corpus.is_empty() {
        return Err(Error::Iokit("no annotated fixtures to sweep against".into()));
    }

    let total_slaps: usize = corpus
        .iter()
        .filter_map(|c| match c.kind {
            Kind::Positive(n) => Some(n),
            Kind::Negative => None,
        })
        .sum();

    println!(
        "Sweeping {} over {} fixture(s), {} annotated slaps.\n",
        knob.label(),
        corpus.len(),
        total_slaps
    );
    println!("  {:>10}   {:>7}   {:>7}   {:>7}   {}", knob.label(), "hits", "missed", "false+", "worst offenders");
    println!("  {:->10}   {:->7}   {:->7}   {:->7}   {:->30}", "", "", "", "", "");

    let mut best: Option<(f32, usize, usize)> = None;

    for value in knob.values() {
        let mut cfg = Config::default();
        // The gyro gate only has an effect when it is allowed to suppress.
        cfg.gyro_mode = GyroMode::Require;
        knob.apply(&mut cfg, value);

        let (mut hits, mut missed, mut false_pos) = (0usize, 0usize, 0usize);
        let mut offenders: Vec<String> = Vec::new();

        for item in &corpus {
            let n = replay::detect(&item.fixture, cfg).len();
            match item.kind {
                Kind::Negative => {
                    false_pos += n;
                    if n > 0 {
                        offenders.push(format!("{}+{n}", item.label));
                    }
                }
                Kind::Positive(expected) => {
                    hits += n.min(expected);
                    missed += expected.saturating_sub(n);
                    // Over-detection inside a positive fixture is still a false positive.
                    false_pos += n.saturating_sub(expected);
                    if n < expected {
                        offenders.push(format!("{}-{}", item.label, expected - n));
                    }
                }
            }
        }

        // Rank by total errors, breaking ties toward fewer false positives: a detector
        // that fires when you didn't hit it is more annoying than one that occasionally
        // misses a gentle tap.
        let errors = missed + false_pos;
        if best.is_none_or(|(_, be, bf)| errors < be || (errors == be && false_pos < bf)) {
            best = Some((value, errors, false_pos));
        }

        println!(
            "  {:>10.3}   {:>7}   {:>7}   {:>7}   {}",
            value,
            hits,
            missed,
            false_pos,
            offenders.join(" ")
        );
    }

    if let Some((value, errors, false_pos)) = best {
        println!(
            "\nBest {} = {value} ({errors} total errors, {false_pos} false positives).",
            knob.label()
        );
    }
    Ok(())
}
