//! `yamete replay` — run the detector over a recorded fixture.
//!
//! This is the tuning loop: record once, then replay against changed thresholds as often
//! as you like without having to hit the laptop again.

use std::path::Path;

use yamete_dsp::{Config, Detector, Fixture, Frame};
use crate::error::Error;

/// Result of replaying one fixture.
pub struct Outcome {
    pub label: String,
    pub expected: Option<usize>,
    pub detected: usize,
}

impl Outcome {
    /// Whether the detection count matched the annotation. Unannotated fixtures pass.
    pub fn passed(&self) -> bool {
        self.expected.map_or(true, |e| e == self.detected)
    }
}

/// Replay a parsed fixture, returning every detection in order.
///
/// Frames are merged by timestamp so the gyroscope corroboration window sees the same
/// interleaving it would live.
pub fn detect(fixture: &Fixture, cfg: Config) -> Vec<yamete_dsp::Detection> {
    let mut detector = Detector::new(cfg);
    let mut hits = Vec::new();

    let mut ai = fixture.accel.iter().peekable();
    let mut gi = fixture.gyro.iter().peekable();
    loop {
        let take_accel = match (ai.peek(), gi.peek()) {
            (Some(a), Some(g)) => a.t <= g.t,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_accel {
            let f: &Frame = ai.next().unwrap();
            if let Some(hit) = detector.push_accel(*f) {
                hits.push(hit);
            }
        } else {
            detector.push_gyro(*gi.next().unwrap());
        }
    }
    hits
}

pub fn load(path: &Path) -> Result<Fixture, Error> {
    Fixture::read(path).map_err(|e| Error::other(e.to_string()))
}

/// Keep every `n`th sample, as if the sensor had been configured to report more slowly.
///
/// Used to answer "could we halve the report rate and still detect slaps?" without having
/// to re-record the whole corpus at each candidate rate.
pub fn decimate(fixture: &Fixture, n: usize) -> Fixture {
    if n <= 1 {
        return fixture.clone();
    }
    let take_nth = |v: &Vec<yamete_dsp::Frame>| v.iter().step_by(n).copied().collect();
    Fixture {
        label: fixture.label.clone(),
        expect: fixture.expect,
        meta: fixture.meta.clone(),
        accel: take_nth(&fixture.accel),
        gyro: take_nth(&fixture.gyro),
    }
}

pub fn run(
    paths: &[std::path::PathBuf],
    sensitivity: f32,
    verbose: bool,
    decimation: usize,
) -> Result<(), Error> {
    let mut cfg = Config::default().with_sensitivity(sensitivity);
    if decimation > 1 {
        cfg.rate_hz /= decimation as f32;
        println!(
            "Simulating a {:.0} Hz sensor (decimating {decimation}:1).",
            cfg.rate_hz
        );
    }
    let mut outcomes = Vec::new();

    for path in paths {
        let fixture = decimate(&load(path)?, decimation);
        let hits = detect(&fixture, cfg);

        println!(
            "\n{}  ({:.2}s, {:.0} Hz, {} accel / {} gyro)",
            fixture.label,
            fixture.duration(),
            fixture.rate_hz(),
            fixture.accel.len(),
            fixture.gyro.len(),
        );
        match fixture.expect {
            Some(n) => println!("  expected {n}, detected {}", hits.len()),
            None => println!("  detected {}", hits.len()),
        }
        if verbose {
            for h in &hits {
                println!(
                    "    t={:7.3}s  {:6}  peak {:7.4} g  gyro {:7.2} deg/s  ratio {:8.1}  votes {}",
                    h.t,
                    h.tier.as_str(),
                    h.peak_g,
                    h.gyro_peak,
                    h.gyro_ratio,
                    h.votes,
                );
            }
        }

        outcomes.push(Outcome {
            label: fixture.label.clone(),
            expected: fixture.expect,
            detected: hits.len(),
        });
    }

    // Report rather than fail: the tuned detector legitimately misses some gentle slaps,
    // and the pass/fail judgement lives in `yamete-dsp`'s fixture tests against a measured
    // envelope. `sweep` is the tool for comparing candidate thresholds.
    let expected: usize = outcomes.iter().filter_map(|o| o.expected).sum();
    let hits: usize = outcomes
        .iter()
        .filter_map(|o| o.expected.map(|e| o.detected.min(e)))
        .sum();
    let false_pos: usize = outcomes
        .iter()
        .filter_map(|o| o.expected.map(|e| o.detected.saturating_sub(e)))
        .sum();

    println!();
    if expected > 0 {
        println!(
            "{hits}/{expected} slaps detected ({:.0}% recall), {false_pos} false positive(s).",
            hits as f64 / expected as f64 * 100.0,
        );
    } else {
        println!(
            "{false_pos} false positive(s) across {} fixture(s).",
            outcomes.len()
        );
    }
    for o in outcomes.iter().filter(|o| !o.passed()) {
        println!(
            "  {}: expected {}, detected {}",
            o.label,
            o.expected.unwrap_or(0),
            o.detected
        );
    }
    Ok(())
}
