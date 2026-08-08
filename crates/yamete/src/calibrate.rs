//! `yamete calibrate` — record a raw sensor trace to a fixture file.
//!
//! Detector tuning against synthetic signals only proves the code runs. What actually
//! matters is whether it can tell a slap from typing on *this* machine, and that needs
//! recordings of both.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use yamete_dsp::Fixture;
use yamete_sensor::Imu;

use crate::error::Error;
use crate::pump::{Pump, Source};

/// A metronome for takes that need a known number of slaps.
///
/// Counting your own slaps while watching a terminal is exactly the kind of thing people
/// get wrong, and the count *is* the test assertion — so the recorder beats out the rhythm
/// and the person just follows it.
#[derive(Debug, Clone, Copy)]
pub struct Cue {
    pub count: usize,
}

impl Cue {
    /// Beat times, inset from both ends so no slap lands in the ramp-up or gets clipped.
    fn beats(&self, secs: f64) -> Vec<f64> {
        if self.count == 0 {
            return Vec::new();
        }
        let lead_in = 2.0_f64.min(secs * 0.1);
        let tail = 2.0_f64.min(secs * 0.1);
        let span = (secs - lead_in - tail).max(0.0);
        if self.count == 1 {
            return vec![lead_in + span / 2.0];
        }
        let step = span / (self.count - 1) as f64;
        (0..self.count).map(|i| lead_in + step * i as f64).collect()
    }
}

/// How the terminal status line should read while recording.
pub struct Recording<'a> {
    pub label: &'a str,
    /// Restated on the status line, because the setup text has scrolled away by now.
    pub prompt: &'a str,
    pub secs: f64,
    pub expect: Option<usize>,
    pub cue: Option<Cue>,
    pub countdown: u64,
}

pub fn run(rec: &Recording<'_>, out: &Path) -> Result<(), Error> {
    let mut imu = Imu::open()?;

    let mut fixture = Fixture::new(rec.label);
    fixture.expect = rec.expect;
    fixture.meta = machine_metadata();

    let beats = rec.cue.map(|c| c.beats(rec.secs)).unwrap_or_default();
    if !beats.is_empty() {
        println!("  Follow the SLAP NOW prompts — don't count, just react.");
    }

    for n in (1..=rec.countdown).rev() {
        print!("\r  starting in {n}...   ");
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("\r  RECORDING — {}                    ", rec.prompt);

    // Discard anything buffered during the countdown so the trace starts clean.
    let mut warm = Pump::new();
    warm.drain(&mut imu, |_, _| {});
    let mut pump = Pump::new();

    let start = Instant::now();
    let duration = Duration::from_secs_f64(rec.secs);
    let mut next_beat = 0usize;
    let mut flash_until = None::<Instant>;
    let mut last_paint = Instant::now() - Duration::from_secs(1);

    while start.elapsed() < duration {
        pump.drain(&mut imu, |source, frame| match source {
            Source::Accel => fixture.accel.push(frame),
            Source::Gyro => fixture.gyro.push(frame),
        });

        let elapsed = start.elapsed().as_secs_f64();

        if next_beat < beats.len() && elapsed >= beats[next_beat] {
            next_beat += 1;
            flash_until = Some(Instant::now() + Duration::from_millis(600));
            // The bell matters here: you're looking at the lid you're about to hit,
            // not at the terminal.
            print!("\x07");
        }

        if last_paint.elapsed() > Duration::from_millis(80) {
            last_paint = Instant::now();
            let left = (rec.secs - elapsed).max(0.0);
            if beats.is_empty() {
                print!("\r  [{left:5.1}s left]  {}          ", rec.prompt);
            } else if flash_until.is_some_and(|t| t > Instant::now()) {
                print!(
                    "\r  [{left:5.1}s left]  ►►► SLAP NOW ◄◄◄   ({} of {})      ",
                    next_beat,
                    beats.len()
                );
            } else {
                let next_in = beats
                    .get(next_beat)
                    .map(|b| b - elapsed)
                    .unwrap_or(f64::NAN);
                if next_in.is_nan() {
                    print!(
                        "\r  [{left:5.1}s left]  done — {} of {} slaps, hold still   ",
                        next_beat,
                        beats.len()
                    );
                } else {
                    print!(
                        "\r  [{left:5.1}s left]  next slap in {next_in:3.1}s   ({} of {} done)   ",
                        next_beat,
                        beats.len()
                    );
                }
            }
            let _ = std::io::stdout().flush();
        }

        std::thread::sleep(Duration::from_millis(2));
    }
    pump.drain(&mut imu, |source, frame| match source {
        Source::Accel => fixture.accel.push(frame),
        Source::Gyro => fixture.gyro.push(frame),
    });
    println!("\r  done.                                                              ");

    fixture
        .meta
        .insert("rate_hz".into(), format!("{:.1}", fixture.rate_hz()));

    fixture
        .write(out)
        .map_err(|e| Error::other(format!("could not write {}: {e}", out.display())))?;

    println!(
        "  wrote {} — {} accel + {} gyro over {:.1}s ({:.0} Hz){}",
        out.display(),
        fixture.accel.len(),
        fixture.gyro.len(),
        fixture.duration(),
        fixture.rate_hz(),
        match rec.expect {
            Some(n) => format!(", annotated expect={n}"),
            None => String::new(),
        },
    );
    Ok(())
}

/// Provenance for the fixture header, so a trace recorded on one machine is identifiable
/// when it later fails on another.
fn machine_metadata() -> BTreeMap<String, String> {
    let mut meta = BTreeMap::new();
    if let Some(model) = sysctl_string("hw.model") {
        meta.insert("model".into(), model);
    }
    if let Some(cpu) = sysctl_string("machdep.cpu.brand_string") {
        meta.insert("cpu".into(), cpu);
    }
    meta
}

fn sysctl_string(name: &str) -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_are_inset_and_evenly_spaced() {
        let beats = Cue { count: 10 }.beats(30.0);
        assert_eq!(beats.len(), 10);
        assert!(beats[0] >= 2.0, "first beat too early: {}", beats[0]);
        assert!(*beats.last().unwrap() <= 28.0, "last beat too late");

        let step = beats[1] - beats[0];
        // Must exceed the 0.75 s detector cooldown, or two slaps merge into one
        // detection and the annotated count can never be met.
        assert!(step > 2.0, "beat spacing {step} is inside the cooldown");
        for w in beats.windows(2) {
            assert!((w[1] - w[0] - step).abs() < 1e-9, "spacing is uneven");
        }
    }

    #[test]
    fn handles_degenerate_counts() {
        assert!(Cue { count: 0 }.beats(30.0).is_empty());
        assert_eq!(Cue { count: 1 }.beats(30.0).len(), 1);
    }
}
