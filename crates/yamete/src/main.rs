use std::path::PathBuf;
use std::process::ExitCode;

mod actions;
mod analyze;
mod calibrate;
mod client;
mod daemon;
mod error;
mod launchd;
mod probe;
mod pump;
mod replay;
mod server;
mod store;
mod suite;
mod sweep;
mod watch;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "yamete",
    version,
    about = "Slap detection for Apple Silicon MacBooks"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the detector and serve the control socket.
    Run {
        /// Run under launchd: log as JSON, no stdout chatter.
        #[arg(long)]
        daemon: bool,
        /// Exit when stdin closes.
        ///
        /// Lets a parent process own this daemon's lifetime without having to signal it:
        /// the pipe closes when the parent dies for any reason, including being force
        /// quit, so there is no way to orphan the daemon.
        #[arg(long)]
        exit_with_parent: bool,
    },

    /// Install as a user LaunchAgent so it starts at login.
    Install {
        /// Binary to run. Defaults to the one you invoked.
        #[arg(long)]
        program: Option<PathBuf>,
        /// Copy the binary to ~/.local/bin first, so rebuilding cannot break the agent.
        #[arg(long)]
        copy: bool,
    },

    /// Stop and remove the LaunchAgent.
    Uninstall,

    /// Restart the LaunchAgent, picking up a new binary.
    Restart,

    /// Report what launchd knows about the agent, plus recent log lines.
    Service,

    /// Show the running daemon's state.
    Status,

    /// Turn detection on or off in the running daemon.
    Toggle {
        /// `on` or `off`.
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },

    /// Stream slaps from the running daemon.
    Listen {
        /// Emit raw NDJSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },

    /// Fire one configured action, to preview it.
    Test {
        /// The action id, as shown in the config.
        id: String,
        #[arg(long, default_value_t = 0.8)]
        intensity: f32,
    },

    /// Check sensor availability and stream health, then exit.
    Probe {
        /// How long to sample for, in seconds.
        #[arg(long, default_value_t = 3.0)]
        secs: f64,
    },

    /// Print detections live while you hit the machine.
    Watch {
        /// Sensitivity slider, 0.0 (only hard deliberate slaps) to 1.0 (anything).
        #[arg(long, default_value_t = 0.5)]
        sensitivity: f32,
        /// Continuously display the five detector scores.
        #[arg(long)]
        scores: bool,
    },

    /// Record a raw sensor trace to a fixture file for offline tuning.
    Calibrate {
        /// Short name for what is being recorded, e.g. `slap-lid-left` or `typing`.
        #[arg(long)]
        label: String,
        /// Recording length, in seconds.
        #[arg(long, default_value_t = 10.0)]
        secs: f64,
        /// How many slaps the recording should contain, for use as a test assertion.
        #[arg(long)]
        expect: Option<usize>,
        /// Seconds of countdown before recording starts.
        #[arg(long, default_value_t = 3)]
        countdown: u64,
        /// Where to write the fixture.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },

    /// Walk through recording the whole fixture corpus interactively.
    RecordSuite {
        /// Directory to write fixtures into.
        #[arg(long, default_value = "fixtures")]
        dir: PathBuf,
        /// Only record takes whose label contains this substring.
        #[arg(long)]
        only: Option<String>,
    },

    /// Report the signal distribution in recorded fixtures, to set thresholds from data.
    Analyze {
        /// Fixture files to analyze.
        files: Vec<PathBuf>,
        /// Report detector scores at each detection rather than over the whole trace.
        #[arg(long)]
        at_detections: bool,
    },

    /// Score a threshold against the whole corpus and print the confusion matrix.
    Sweep {
        /// Fixture files to score against.
        files: Vec<PathBuf>,
        /// Which threshold to sweep.
        #[arg(long, value_enum, default_value_t = sweep::Knob::GyroRatio)]
        knob: sweep::Knob,
    },

    /// Replay recorded fixtures through the detector.
    Replay {
        /// Fixture files to replay.
        files: Vec<PathBuf>,
        /// Sensitivity slider, 0.0 (only hard deliberate slaps) to 1.0 (anything).
        #[arg(long, default_value_t = 0.5)]
        sensitivity: f32,
        /// List every detection, not just the counts.
        #[arg(short, long)]
        verbose: bool,
        /// Keep every Nth sample, simulating a slower sensor report rate.
        #[arg(long, default_value_t = 1)]
        decimate: usize,
    },
}

/// Under launchd there is no terminal, so logs go to the unified log via stderr, which
/// launchd captures. In the foreground they are for a human reading along.
fn init_logging(as_daemon: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("YAMETE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    if as_daemon {
        builder.with_ansi(false).init();
    } else {
        builder.with_target(false).without_time().init();
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run {
            daemon: as_daemon,
            exit_with_parent,
        } => {
            init_logging(as_daemon);
            daemon::run(!as_daemon, exit_with_parent)
        }
        Command::Install { program, copy } => launchd::install(program, copy),
        Command::Uninstall => launchd::uninstall(),
        Command::Restart => launchd::restart(),
        Command::Service => launchd::show(),
        Command::Status => client::status(),
        Command::Toggle { state } => client::set_enabled(state == "on"),
        Command::Listen { json } => client::listen(json),
        Command::Test { id, intensity } => client::test_action(&id, intensity),
        Command::Probe { secs } => probe::run(secs),
        Command::Watch {
            sensitivity,
            scores,
        } => watch::run(sensitivity, scores),
        Command::Calibrate {
            label,
            secs,
            expect,
            countdown,
            out,
        } => {
            let out = out.unwrap_or_else(|| PathBuf::from(format!("fixtures/{label}.fixture.gz")));
            calibrate::run(
                &calibrate::Recording {
                    label: &label,
                    prompt: &label,
                    secs,
                    expect,
                    cue: expect
                        .filter(|n| *n > 0)
                        .map(|n| calibrate::Cue { count: n }),
                    countdown,
                },
                &out,
            )
        }
        Command::RecordSuite { dir, only } => suite::run(&dir, only.as_deref()),
        Command::Analyze {
            files,
            at_detections,
        } => {
            if files.is_empty() {
                eprintln!("error: no fixture files given");
                return ExitCode::FAILURE;
            }
            if at_detections {
                analyze::at_detections(&files)
            } else {
                analyze::run(&files)
            }
        }
        Command::Sweep { files, knob } => {
            if files.is_empty() {
                eprintln!("error: no fixture files given");
                return ExitCode::FAILURE;
            }
            sweep::run(&files, knob)
        }
        Command::Replay {
            files,
            sensitivity,
            verbose,
            decimate,
        } => {
            if files.is_empty() {
                eprintln!("error: no fixture files given");
                return ExitCode::FAILURE;
            }
            replay::run(&files, sensitivity, verbose, decimate.max(1))
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        // A Mac without the sensor will never grow one. Exiting non-zero would have
        // launchd restart us every ThrottleInterval forever, so report this as a clean
        // stop and let the job stay down.
        Err(err) if err.is_no_sensor() => {
            eprintln!("error: {err}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
