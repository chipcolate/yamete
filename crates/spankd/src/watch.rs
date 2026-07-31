//! `spankd watch` — live detection to the terminal.
//!
//! The interactive counterpart to `replay`: useful for confirming the detector responds
//! the way you expect while you're actually hitting the machine.

use std::io::Write;
use std::time::Duration;

use spank_dsp::{Config, Detector};
use spank_sensor::{Error, Imu};

use crate::pump::{Pump, Source};

pub fn run(sensitivity: f32, show_scores: bool) -> Result<(), Error> {
    let mut imu = Imu::open()?;
    let cfg = Config::default().with_sensitivity(sensitivity);
    let mut detector = Detector::new(cfg);
    let mut pump = Pump::new();

    println!(
        "Watching (sensitivity {sensitivity}, gyro {:?}{}). Ctrl-C to stop.",
        cfg.gyro_mode,
        if imu.has_gyro() { "" } else { ", NO GYRO" },
    );

    let mut warmed = false;
    let mut hits = 0u64;
    let mut last_status = std::time::Instant::now();

    loop {
        pump.drain(&mut imu, |source, frame| match source {
            Source::Gyro => detector.push_gyro(frame),
            Source::Accel => {
                if let Some(d) = detector.push_accel(frame) {
                    hits += 1;
                    println!(
                        "\r{:>4}  t={:8.3}s  {:6}  peak {:.4} g  intensity {:.2}  votes {}/5  gyro {}",
                        hits,
                        d.t,
                        d.tier.as_str(),
                        d.peak_g,
                        d.intensity,
                        d.votes,
                        if d.gyro_confirmed { "yes" } else { "no " },
                    );
                }
            }
        });

        if !warmed && !detector.is_warming_up() {
            warmed = true;
            println!("Warmed up — go ahead and slap it.");
        }

        // A live score readout makes threshold tuning tractable; without it you're
        // guessing which of the five detectors is the one holding out.
        if show_scores && warmed && last_status.elapsed() > Duration::from_millis(100) {
            last_status = std::time::Instant::now();
            let s = detector.scores();
            print!(
                "\renv {:.4} g | sta/lta {:5.1} | cusum {:5.1} | kurt {:6.1} | mad {:5.1} | gyro {:5.1} | votes {}{}  ",
                s.envelope,
                s.sta_lta,
                s.cusum,
                s.kurtosis,
                s.peak_mad,
                s.gyro,
                s.votes,
                if s.in_cooldown { " [cooldown]" } else { "" },
            );
            let _ = std::io::stdout().flush();
        }

        std::thread::sleep(Duration::from_millis(2));
    }
}
