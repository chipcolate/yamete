//! `yamete analyze` — report the signal distribution in a fixture.
//!
//! A threshold is only meaningful relative to what the machine does when untouched: a
//! micro-shock floor of 0.005 g is useless if the idle noise floor is already 0.006 g.
//! This measures the distribution so thresholds can be set from data rather than
//! guessed.

use std::path::PathBuf;

use yamete_dsp::{Config, Detector, Frame};
use yamete_sensor::Error;

use crate::replay;

/// Percentile of a sorted slice, by nearest rank.
fn percentile(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Every per-sample detector score over a fixture.
#[derive(Default)]
struct Traces {
    envelope: Vec<f32>,
    sta_lta: Vec<f32>,
    cusum: Vec<f32>,
    kurtosis: Vec<f32>,
    peak_mad: Vec<f32>,
    gyro: Vec<f32>,
}

/// Replay a fixture purely to collect per-sample detector scores.
fn traces(fixture: &yamete_dsp::Fixture, cfg: Config) -> Traces {
    let mut detector = Detector::new(cfg);
    let mut t = Traces::default();

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
            detector.push_accel(*f);
            // Skip the warmup, where the background estimate is still converging and the
            // scores are not representative of anything.
            if !detector.is_warming_up() {
                let s = detector.scores();
                t.envelope.push(s.envelope);
                t.sta_lta.push(s.sta_lta);
                t.cusum.push(s.cusum);
                t.kurtosis.push(s.kurtosis);
                t.peak_mad.push(s.peak_mad);
            }
        } else {
            detector.push_gyro(*gi.next().unwrap());
            if !detector.is_warming_up() {
                t.gyro.push(detector.scores().gyro);
            }
        }
    }
    t
}

fn report(name: &str, values: &mut [f32], unit: &str) {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!(
        "  {name:<10} p50 {:.5}  p95 {:.5}  p99 {:.5}  p99.9 {:.5}  max {:.5} {unit}",
        percentile(values, 50.0),
        percentile(values, 95.0),
        percentile(values, 99.0),
        percentile(values, 99.9),
        values.last().copied().unwrap_or(0.0),
    );
}

/// Report what each detector reads at the moment a slap actually fires.
///
/// Percentiles over a whole fixture are dominated by the quiet 98% of it, so they say
/// almost nothing about whether a threshold is *reachable*. This samples the scores at
/// each detection instead, which is the only thing that determines the vote count — and
/// therefore the severity tier.
pub fn at_detections(files: &[PathBuf]) -> Result<(), Error> {
    let cfg = Config::default();
    let mut rows: Vec<(String, yamete_dsp::Scores, f32)> = Vec::new();

    for path in files {
        let fixture = replay::load(path)?;
        if fixture.expect.unwrap_or(0) == 0 {
            continue;
        }
        let mut detector = Detector::new(cfg);
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
                    rows.push((fixture.label.clone(), detector.scores(), hit.peak_g));
                }
            } else {
                detector.push_gyro(*gi.next().unwrap());
            }
        }
    }

    if rows.is_empty() {
        return Err(Error::Iokit("no detections in the given fixtures".into()));
    }

    println!("Detector scores at {} detection(s):\n", rows.len());
    println!(
        "  {:<14} {:>9} {:>9} {:>9} {:>9} {:>9}  votes",
        "fixture", "envelope", "sta/lta", "cusum", "kurtosis", "peak/mad"
    );
    for (label, s, _) in &rows {
        println!(
            "  {:<14} {:>9.4} {:>9.1} {:>9.1} {:>9.0} {:>9.1}  {}",
            label, s.envelope, s.sta_lta, s.cusum, s.kurtosis, s.peak_mad, s.votes
        );
    }

    // The threshold that would let a given fraction of real slaps trip each detector.
    let th = cfg.thresholds;
    let quantile = |mut v: Vec<f32>, q: f64| -> f32 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[(((v.len() - 1) as f64) * q).round() as usize]
    };
    println!("\n  Fraction of real slaps that trip each detector at its current threshold:");
    let checks: [(&str, f32, Vec<f32>); 5] = [
        (
            "envelope",
            th.highpass_g,
            rows.iter().map(|r| r.1.envelope).collect(),
        ),
        (
            "sta/lta",
            th.sta_lta,
            rows.iter().map(|r| r.1.sta_lta).collect(),
        ),
        ("cusum", th.cusum, rows.iter().map(|r| r.1.cusum).collect()),
        (
            "kurtosis",
            th.kurtosis,
            rows.iter().map(|r| r.1.kurtosis).collect(),
        ),
        (
            "peak/mad",
            th.peak_mad,
            rows.iter().map(|r| r.1.peak_mad).collect(),
        ),
    ];
    for (name, threshold, values) in checks {
        let hit = values.iter().filter(|v| **v >= threshold).count();
        let pct = hit as f64 / values.len() as f64 * 100.0;
        println!(
            "    {name:<10} threshold {threshold:>8.2}  trips on {pct:5.1}%   \
             (p25 {:.2}, median {:.2}, p75 {:.2}){}",
            quantile(values.clone(), 0.25),
            quantile(values.clone(), 0.50),
            quantile(values.clone(), 0.75),
            if pct < 25.0 {
                "  <- effectively never votes"
            } else {
                ""
            },
        );
    }

    let avg_votes: f64 = rows.iter().map(|r| f64::from(r.1.votes)).sum::<f64>() / rows.len() as f64;
    println!("\n  Mean votes per real slap: {avg_votes:.1} of 5");
    println!(
        "  min_votes is {}, so a slap needs that many detectors to register at all. \
         Severity is amplitude, not votes.",
        cfg.min_votes
    );

    let mut peaks: Vec<f32> = rows.iter().map(|r| r.2).collect();
    peaks.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!(
        "  Slap peaks span {:.3}-{:.3} g (median {:.3}); tiers cut at {:.2} / {:.2} / {:.2} g.",
        peaks.first().copied().unwrap_or(0.0),
        peaks.last().copied().unwrap_or(0.0),
        quantile(peaks.clone(), 0.5),
        cfg.tiers.micro_g,
        cfg.tiers.medium_g,
        cfg.tiers.major_g,
    );
    Ok(())
}

pub fn run(files: &[PathBuf]) -> Result<(), Error> {
    let cfg = Config::default();

    for path in files {
        let fixture = replay::load(path)?;
        let mut t = traces(&fixture, cfg);
        if t.envelope.is_empty() {
            println!("\n{}: too short to analyze (all warmup)", fixture.label);
            continue;
        }

        println!(
            "\n{}  ({:.2}s, {:.0} Hz)",
            fixture.label,
            fixture.duration(),
            fixture.rate_hz()
        );
        report("envelope", &mut t.envelope, "g");
        report("sta/lta", &mut t.sta_lta, "");
        report("cusum", &mut t.cusum, "");
        report("kurtosis", &mut t.kurtosis, "");
        report("peak/mad", &mut t.peak_mad, "");
        if !t.gyro.is_empty() {
            report("gyro", &mut t.gyro, "");
        }

        // For a fixture known to contain no slaps, every score's maximum is a hard lower
        // bound on the corresponding threshold: set it any lower and this exact recording
        // would have produced a false positive.
        if fixture.expect == Some(0) {
            let worst = |v: &Vec<f32>| v.last().copied().unwrap_or(0.0);
            println!("\n  Thresholds must exceed these idle maxima to stay silent here:");
            println!("    highpass_g  > {:.4}", worst(&t.envelope));
            println!("    sta_lta     > {:.2}", worst(&t.sta_lta));
            println!("    cusum       > {:.2}", worst(&t.cusum));
            println!("    kurtosis    > {:.2}", worst(&t.kurtosis));
            println!("    peak_mad    > {:.2}", worst(&t.peak_mad));
            if !t.gyro.is_empty() {
                println!("    gyro (sta/lta) > {:.2}", worst(&t.gyro));
            }
            let flagged = [
                ("highpass_g", worst(&t.envelope), cfg.thresholds.highpass_g),
                ("sta_lta", worst(&t.sta_lta), cfg.thresholds.sta_lta),
                ("cusum", worst(&t.cusum), cfg.thresholds.cusum),
                ("kurtosis", worst(&t.kurtosis), cfg.thresholds.kurtosis),
                ("peak_mad", worst(&t.peak_mad), cfg.thresholds.peak_mad),
            ];
            for (name, idle_max, configured) in flagged {
                if configured <= idle_max {
                    println!(
                        "  ! {name} is configured at {configured} but idle noise reaches \
                         {idle_max:.4} — this detector votes on nothing but noise"
                    );
                }
            }
        }
    }
    Ok(())
}
