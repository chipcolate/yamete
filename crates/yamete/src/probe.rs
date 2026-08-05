//! `yamete probe` — verify the sensor works on this machine and report stream health.
//!
//! This exists to make failure legible. The sensor is undocumented, model-dependent and
//! gated by a TCC permission, so "it didn't work" has several very different causes that
//! deserve different messages.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use yamete_sensor::{Access, Error, Imu, Sample, SensorKind, Stats};

/// Running min/max/mean over one stream, so we can sanity-check the decode.
#[derive(Default)]
struct Summary {
    count: u64,
    mag_sum: f64,
    mag_min: f32,
    mag_max: f32,
    last: Option<Sample>,
}

impl Summary {
    fn new() -> Self {
        Summary {
            mag_min: f32::INFINITY,
            mag_max: f32::NEG_INFINITY,
            ..Default::default()
        }
    }

    fn push(&mut self, s: Sample) {
        let m = s.magnitude();
        self.count += 1;
        self.mag_sum += f64::from(m);
        self.mag_min = self.mag_min.min(m);
        self.mag_max = self.mag_max.max(m);
        self.last = Some(s);
    }

    fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.mag_sum / self.count as f64
        }
    }
}

fn report_stream(kind: SensorKind, summary: &Summary, stats: &Stats, elapsed: Duration) {
    let hz = summary.count as f64 / elapsed.as_secs_f64();
    println!(
        "\n  {} (usage {})",
        kind.as_str(),
        match kind {
            SensorKind::Accel => 3,
            SensorKind::Gyro => 9,
        }
    );
    println!(
        "    samples      {} in {:.2}s → {:.1} Hz",
        summary.count,
        elapsed.as_secs_f64(),
        hz
    );
    println!(
        "    magnitude    mean {:.4} {u}, min {:.4}, max {:.4}",
        summary.mean(),
        summary.mag_min,
        summary.mag_max,
        u = kind.unit(),
    );
    if let Some(s) = summary.last {
        println!(
            "    last sample  x {:+.4}  y {:+.4}  z {:+.4}  ({} °C die)",
            s.x, s.y, s.z, s.temp_c as i32,
        );
    }
    println!(
        "    stream       {} received, {} dropped, {} ring overruns, {} malformed",
        stats.received.load(Ordering::Relaxed),
        stats.dropped.load(Ordering::Relaxed),
        stats.overruns.load(Ordering::Relaxed),
        stats.malformed.load(Ordering::Relaxed),
    );
}

pub fn run(secs: f64) -> Result<(), Error> {
    println!("Input Monitoring: {:?}", yamete_sensor::check_access());
    if yamete_sensor::check_access() == Access::Denied {
        println!("  (denied — requesting; approve the prompt and re-run)");
        yamete_sensor::request_access();
    }

    let mut imu = Imu::open()?;
    println!(
        "Opened accelerometer{}.",
        if imu.has_gyro() {
            " and gyroscope"
        } else {
            " (no gyroscope)"
        }
    );
    println!("Sampling for {secs:.1}s — hold the machine still...");

    let mut accel = Summary::new();
    let mut gyro = Summary::new();

    let start = Instant::now();
    let duration = Duration::from_secs_f64(secs);
    while start.elapsed() < duration {
        while let Ok(s) = imu.accel.pop() {
            accel.push(s);
        }
        if let Some(g) = imu.gyro.as_mut() {
            while let Ok(s) = g.pop() {
                gyro.push(s);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let elapsed = start.elapsed();

    if accel.count == 0 {
        return Err(Error::Iokit(
            "the device opened but delivered no reports. This is the signature of the \
             SPU not being woken — check that the AppleSPUHIDDriver properties were set \
             before open, not on the IOHIDDevice"
                .into(),
        ));
    }

    report_stream(SensorKind::Accel, &accel, imu.accel_stats(), elapsed);
    if let Some(stats) = imu.gyro_stats() {
        report_stream(SensorKind::Gyro, &gyro, stats, elapsed);
    }

    // At rest the accelerometer measures gravity alone, so anything far from 1 g means
    // the byte offsets or the fixed-point divisor are wrong.
    let mean = accel.mean();
    println!();
    if (mean - 1.0).abs() < 0.05 {
        println!("Decode check: PASS — mean magnitude {mean:.4} g ≈ 1 g at rest.");
        Ok(())
    } else {
        Err(Error::Iokit(format!(
            "decode check FAILED — mean magnitude {mean:.4} g, expected ~1.0 g at rest. \
             Either the machine was moving, or the report layout differs on this model"
        )))
    }
}
