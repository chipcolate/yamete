//! The long-running daemon: sensor to detector to actions, plus the control socket.
//!
//! The detector runs on a dedicated OS thread rather than a tokio task. It is a tight poll
//! loop over a lock-free ring at ~805 Hz, and it must not be at the mercy of an async
//! scheduler that is also serving sockets. Everything it produces crosses into async land
//! through channels.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use spank_dsp::Detector;
use spank_proto::{DaemonConfig, Event, Slap, Status, Telemetry};
use spank_sensor::{Error, Imu};
use tokio::sync::{broadcast, mpsc, watch};

use crate::actions::Executor;
use crate::pump::{Pump, Source};
use crate::server::{self, Shared};
use crate::store;

/// How often telemetry frames are emitted, when anyone is listening.
const TELEMETRY_INTERVAL: Duration = Duration::from_millis(16);

/// Cap on a single telemetry frame, so a stalled loop cannot produce an enormous one.
const MAX_TELEMETRY_SAMPLES: usize = 64;

/// How often the status snapshot is refreshed.
///
/// Deliberately far slower than the poll loop: publishing it every iteration meant 500
/// allocations and watch-channel wakeups per second, which on its own accounted for most
/// of the daemon's idle CPU. Nothing reads status faster than a UI refresh.
const STATUS_INTERVAL: Duration = Duration::from_millis(500);

/// Event fan-out capacity. Slow clients lag and drop rather than blocking the detector.
const EVENT_CAPACITY: usize = 256;

pub fn run(foreground: bool, exit_with_parent: bool) -> Result<(), Error> {
    let config_path = spank_proto::config_path();
    let (config, warning) = store::load(&config_path);
    if let Some(warning) = warning {
        tracing::warn!("{warning}");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| Error::Iokit(format!("could not start the async runtime: {e}")))?;

    let (config_tx, config_rx) = watch::channel(config.clone());
    let (events_tx, _) = broadcast::channel(EVENT_CAPACITY);
    let (test_tx, test_rx) = mpsc::unbounded_channel();
    let (status_tx, _status_rx) = watch::channel(Status {
        enabled: config.enabled,
        version: env!("CARGO_PKG_VERSION").into(),
        has_gyro: false,
        uptime_s: 0.0,
        slaps: 0,
        rate_hz: 0.0,
        warming_up: true,
        telemetry_subscribers: 0,
    });
    let telemetry_subscribers = Arc::new(AtomicUsize::new(0));

    let shared = Shared {
        config: config_tx,
        events: events_tx.clone(),
        telemetry_subscribers: Arc::clone(&telemetry_subscribers),
        status: status_tx.clone(),
        test_action: test_tx,
        config_path,
    };

    let socket_path = spank_proto::socket_path();
    let listener = runtime
        .block_on(server::bind(&socket_path))
        .map_err(|e| Error::Iokit(format!("could not bind {}: {e}", socket_path.display())))?;
    tracing::info!("listening on {}", socket_path.display());

    {
        let shared = shared.clone();
        runtime.spawn(server::serve(listener, shared));
    }

    // launchd stops a job with SIGTERM. Without this the process dies mid-loop and leaves
    // its socket behind — recoverable, since the next start reclaims a stale socket, but
    // it also means in-flight actions are cut off and the log ends abruptly.
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let shutdown = Arc::clone(&shutdown);
        runtime.spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("could not listen for SIGTERM: {e}");
                    return;
                }
            };
            let mut int = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("could not listen for SIGINT: {e}");
                    return;
                }
            };
            tokio::select! {
                _ = term.recv() => tracing::info!("SIGTERM received, shutting down"),
                _ = int.recv() => tracing::info!("SIGINT received, shutting down"),
            }
            shutdown.store(true, Ordering::Relaxed);
        });
    }

    // Watching stdin is how a parent process owns this daemon without signalling it:
    // read returns 0 the moment the pipe closes, whatever killed the parent.
    if exit_with_parent {
        let shutdown = Arc::clone(&shutdown);
        std::thread::Builder::new()
            .name("spank-parent-watch".into())
            .spawn(move || {
                use std::io::Read;
                let mut buf = [0u8; 64];
                loop {
                    match std::io::stdin().read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => continue,
                    }
                }
                tracing::info!("parent went away, shutting down");
                shutdown.store(true, Ordering::Relaxed);
            })
            .expect("could not spawn the parent watcher");
    }

    // The detector owns this thread for the life of the process.
    let result = detector_loop(
        config,
        config_rx,
        events_tx,
        telemetry_subscribers,
        status_tx,
        test_rx,
        foreground,
        shutdown,
    );

    // launchd restarts us; leaving a live socket behind would make the next start think
    // another daemon is running.
    let _ = std::fs::remove_file(&socket_path);
    result
}

#[allow(clippy::too_many_arguments)]
fn detector_loop(
    mut config: DaemonConfig,
    mut config_rx: watch::Receiver<DaemonConfig>,
    events: broadcast::Sender<Event>,
    telemetry_subscribers: Arc<AtomicUsize>,
    status: watch::Sender<Status>,
    mut test_rx: mpsc::UnboundedReceiver<(String, f32)>,
    foreground: bool,
    shutdown: Arc<AtomicBool>,
) -> Result<(), Error> {
    let mut imu = Imu::open_with(spank_sensor::DEFAULT_CAPACITY, config.report_interval_us)?;
    let mut detector = Detector::new(config.detector);
    let mut executor = Executor::new();
    let mut pump = Pump::new();

    for problem in executor.preload(&config) {
        tracing::warn!("{problem}");
    }

    tracing::info!(
        gyro = imu.has_gyro(),
        sensitivity = config.detector.sensitivity,
        actions = config.actions.len(),
        "detector started"
    );
    if foreground {
        println!(
            "spankd running — sensitivity {:.2}, {} action(s), gyro {}. Ctrl-C to stop.",
            config.detector.sensitivity,
            config.actions.len(),
            if imu.has_gyro() { "yes" } else { "NO" },
        );
    }

    let started = Instant::now();
    let mut slaps = 0u64;
    let mut last_telemetry = Instant::now();
    let mut envelope = Vec::with_capacity(MAX_TELEMETRY_SAMPLES);
    let mut gyro_trace = Vec::with_capacity(MAX_TELEMETRY_SAMPLES);
    let mut last_t = 0.0f64;
    let mut last_status = Instant::now() - STATUS_INTERVAL;
    let version: Arc<str> = Arc::from(env!("CARGO_PKG_VERSION"));

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!(slaps, "detector stopped");
            return Ok(());
        }

        if config_rx.has_changed().unwrap_or(false) {
            let previous_interval = config.report_interval_us;
            config = config_rx.borrow_and_update().clone();

            // The report rate is a property of the device, so changing it means closing
            // and reopening. Rare enough that the brief gap doesn't matter.
            if config.report_interval_us != previous_interval {
                tracing::info!(
                    from = previous_interval,
                    to = config.report_interval_us,
                    "reopening the sensor at a new report interval"
                );
                drop(std::mem::replace(
                    &mut imu,
                    Imu::open_with(spank_sensor::DEFAULT_CAPACITY, config.report_interval_us)?,
                ));
                pump = Pump::new();
            }

            // Retuning rebuilds filter state, which restarts the warmup — correct, since
            // the old background estimate was formed under different time constants.
            detector.set_config(config.detector);
            for problem in executor.preload(&config) {
                tracing::warn!("{problem}");
            }
            tracing::info!(
                enabled = config.enabled,
                sensitivity = config.detector.sensitivity,
                "config reloaded"
            );
        }

        while let Ok((id, intensity)) = test_rx.try_recv() {
            if let Some(action) = config.action(&id).cloned() {
                executor.run(&action, &preview_slap(intensity));
            }
        }

        let want_telemetry = telemetry_subscribers.load(Ordering::Relaxed) > 0;
        let enabled = config.enabled;

        pump.drain(&mut imu, |source, frame| {
            last_t = frame.t;
            match source {
                Source::Gyro => {
                    detector.push_gyro(frame);
                    if want_telemetry && gyro_trace.len() < MAX_TELEMETRY_SAMPLES {
                        gyro_trace.push(detector.scores().gyro_peak);
                    }
                }
                Source::Accel => {
                    let detection = detector.push_accel(frame);
                    if want_telemetry && envelope.len() < MAX_TELEMETRY_SAMPLES {
                        envelope.push(detector.scores().envelope);
                    }
                    // Still fed to the detector while disabled, so its background estimate
                    // stays current and re-enabling doesn't need another warmup.
                    if let (Some(detection), true) = (detection, enabled) {
                        let slap = Slap::from(detection);
                        slaps += 1;
                        executor.dispatch(&config, &slap);
                        let _ = events.send(Event::Slap(slap));
                        if foreground {
                            println!(
                                "{:>5}  t={:8.3}s  {:6}  peak {:.4} g  intensity {:.2}  votes {}/5  gyro {:.0} deg/s",
                                slaps,
                                slap.t,
                                slap.tier.as_str(),
                                slap.peak_g,
                                slap.intensity,
                                slap.votes,
                                slap.gyro_peak,
                            );
                        }
                    }
                }
            }
        });

        if want_telemetry && last_telemetry.elapsed() >= TELEMETRY_INTERVAL {
            last_telemetry = Instant::now();
            let dropped = imu
                .accel_stats()
                .dropped
                .load(Ordering::Relaxed);
            let _ = events.send(Event::Telemetry(Telemetry {
                t: last_t,
                envelope: std::mem::take(&mut envelope),
                gyro: std::mem::take(&mut gyro_trace),
                scores: detector.scores(),
                dropped,
            }));
        } else if !want_telemetry && !envelope.is_empty() {
            envelope.clear();
            gyro_trace.clear();
        }

        if last_status.elapsed() >= STATUS_INTERVAL {
            last_status = Instant::now();
            let _ = status.send(Status {
                enabled: config.enabled,
                version: version.to_string(),
                has_gyro: imu.has_gyro(),
                uptime_s: started.elapsed().as_secs_f64(),
                slaps,
                rate_hz: measured_rate(&imu, started),
                warming_up: detector.is_warming_up(),
                telemetry_subscribers: telemetry_subscribers.load(Ordering::Relaxed),
            });
        }

        // 2 ms keeps worst-case detection latency well under the audio buffer while the
        // loop still costs almost nothing — the ring holds ten seconds of headroom.
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn measured_rate(imu: &Imu, started: Instant) -> f32 {
    let elapsed = started.elapsed().as_secs_f32();
    if elapsed < 0.5 {
        return 0.0;
    }
    imu.accel_stats().received.load(Ordering::Relaxed) as f32 / elapsed
}

/// A synthetic slap for previewing an action from the UI.
fn preview_slap(intensity: f32) -> Slap {
    Slap {
        t: 0.0,
        tier: spank_dsp::Tier::Major,
        peak_g: 0.25,
        intensity: intensity.clamp(0.0, 1.0),
        votes: 4,
        gyro_confirmed: true,
        gyro_peak: 40.0,
        gyro_ratio: 400.0,
        axis: [0.0, 0.0, -1.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_slap_is_clamped_and_plausible() {
        assert_eq!(preview_slap(5.0).intensity, 1.0);
        assert_eq!(preview_slap(-1.0).intensity, 0.0);
        let s = preview_slap(0.5);
        assert_eq!(s.intensity, 0.5);
        // Must satisfy every action filter a user could reasonably set, or "Test" in the
        // UI would silently do nothing for tier-restricted actions.
        assert_eq!(s.tier, spank_dsp::Tier::Major);
    }

    #[test]
    fn telemetry_frames_are_bounded() {
        // At 805 Hz a 16 ms frame is ~13 samples; the cap only matters if the loop stalls,
        // and exists so a stall cannot turn into an unbounded allocation.
        assert!(MAX_TELEMETRY_SAMPLES >= 32);
        let expected = 805.0 * TELEMETRY_INTERVAL.as_secs_f64();
        assert!(
            expected < MAX_TELEMETRY_SAMPLES as f64,
            "normal operation would hit the cap and truncate the scope"
        );
    }
}
