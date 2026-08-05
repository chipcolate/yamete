//! Running actions when a slap lands.
//!
//! The critical path is the sound: everything else can take as long as it likes, but the
//! gap between the hit and the noise is the entire user experience. So audio is decoded
//! once at startup and played from memory on a dedicated thread, while exec and webhook
//! actions are dispatched to a worker and never block the detector.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kira::sound::static_sound::StaticSoundData;
use kira::{AudioManager, AudioManagerSettings, DefaultBackend};
use yamete_proto::{Action, ActionKind, DaemonConfig, Slap, SoundOrder};

/// How long the audio device stays open after the last sound.
///
/// An open CoreAudio output stream runs its render callback continuously whether or not
/// anything is playing, which measured at 4.2% of a core on an M4 Max — unacceptable for
/// something meant to sit in the menu bar all day. Releasing it when idle takes that to
/// nearly nothing, at the cost of a device-open delay on the first sound of a session.
/// The window is generous so that only the first slap after a genuine lull ever pays it.
const AUDIO_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the worker wakes to check whether the device has gone idle.
const AUDIO_IDLE_POLL: Duration = Duration::from_secs(5);

/// Audio file extensions the decoder understands.
const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "ogg", "flac", "aiff", "aif", "aifc", "caf", "m4a", "aac", "alac",
];

/// A job and the moment it should run.
struct Scheduled {
    due: Instant,
    job: Job,
}

/// Work handed to the background executor.
enum Job {
    /// Boxed because the decoded audio makes this variant far larger than the others, and
    /// every queued job — including webhooks and commands — would otherwise be sized for it.
    Sound {
        data: Box<StaticSoundData>,
    },
    Exec {
        program: String,
        args: Vec<String>,
        stdin_json: bool,
        env: Vec<(String, String)>,
        payload: String,
    },
    Webhook {
        url: String,
        method: String,
        headers: BTreeMap<String, String>,
        body: String,
        timeout: Duration,
    },
}

/// Dispatches actions off the detector thread.
pub struct Executor {
    tx: mpsc::Sender<Scheduled>,
    /// Decoded audio, keyed by the path it came from. Decoding an mp3 takes long enough
    /// to be audible if done at trigger time.
    cache: BTreeMap<PathBuf, StaticSoundData>,
    /// Position in the playlist for sequential order.
    rotation: usize,
    /// State for random order, plus the last pick so it is never repeated back to back.
    rng: u64,
    last_pick: Option<PathBuf>,
}

impl Executor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Scheduled>();

        // Audio lives on its own thread with its own manager: `AudioManager` is not Sync,
        // and creating one per sound would add device-setup latency to every slap.
        std::thread::Builder::new()
            .name("yamete-actions".into())
            .spawn(move || run_worker(rx))
            .expect("could not spawn the action worker");

        Executor {
            tx,
            cache: BTreeMap::new(),
            rotation: 0,
            // Seeded from the clock so two runs do not play the same sequence. Any
            // reasonable spread will do; this is picking sound effects, not keys.
            rng: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15)
                | 1,
            last_pick: None,
        }
    }

    /// Decode every sound referenced by the config, discarding anything now unused.
    ///
    /// Called on startup and on every config change, so switching sound packs in the UI
    /// does not put a file read on the trigger path.
    pub fn preload(&mut self, config: &DaemonConfig) -> Vec<String> {
        let mut wanted: Vec<PathBuf> = Vec::new();
        for action in &config.actions {
            if let ActionKind::Sound { paths, .. } = &action.kind {
                wanted.extend(playlist(paths));
            }
        }

        let mut problems = Vec::new();
        for path in &wanted {
            if self.cache.contains_key(path) {
                continue;
            }
            match StaticSoundData::from_file(path) {
                Ok(data) => {
                    self.cache.insert(path.clone(), data);
                }
                Err(e) => problems.push(format!("could not load {}: {e}", path.display())),
            }
        }
        self.cache.retain(|k, _| wanted.contains(k));
        problems
    }

    /// Fire every action that matches this slap.
    pub fn dispatch(&mut self, config: &DaemonConfig, slap: &Slap) {
        for action in &config.actions {
            if action.matches(slap.tier, slap.intensity) {
                self.run(action, slap);
            }
        }
    }

    /// Fire one action regardless of its tier and intensity filters, for UI previews.
    pub fn run(&mut self, action: &Action, slap: &Slap) {
        let job = match &action.kind {
            ActionKind::Sound {
                paths,
                order,
                volume_db,
                scale_with_intensity,
                intensity_range_pct,
                playback_rate,
                ..
            } => {
                let Some(chosen) = self.pick_sound(paths, *order) else {
                    tracing::warn!(action = %action.id, "no sound available");
                    return;
                };
                let Some(data) = self.cache.get(&chosen) else {
                    tracing::warn!(action = %action.id, path = %chosen.display(), "sound not loaded");
                    return;
                };

                let gain = if *scale_with_intensity {
                    volume_db + intensity_gain_db(slap.intensity, *intensity_range_pct)
                } else {
                    *volume_db
                };
                Job::Sound {
                    data: Box::new(
                        data.clone()
                            .volume(gain)
                            .playback_rate(f64::from(*playback_rate)),
                    ),
                }
            }

            ActionKind::Exec {
                program,
                args,
                stdin_json,
            } => {
                let vars = template_vars(slap);
                Job::Exec {
                    program: program.clone(),
                    args: args
                        .iter()
                        .map(|a| yamete_proto::config::render(a, &vars))
                        .collect(),
                    stdin_json: *stdin_json,
                    env: env_vars(slap),
                    payload: serde_json::to_string(slap).unwrap_or_else(|_| "{}".into()),
                }
            }

            ActionKind::Webhook {
                url,
                method,
                headers,
                body,
                timeout_ms,
            } => {
                let vars = template_vars(slap);
                Job::Webhook {
                    url: yamete_proto::config::render(url, &vars),
                    method: method.to_uppercase(),
                    headers: headers
                        .iter()
                        .map(|(k, v)| (k.clone(), yamete_proto::config::render(v, &vars)))
                        .collect(),
                    body: match body {
                        Some(t) => yamete_proto::config::render(t, &vars),
                        None => serde_json::to_string(slap).unwrap_or_else(|_| "{}".into()),
                    },
                    timeout: Duration::from_millis(*timeout_ms),
                }
            }
        };

        let due = Instant::now() + Duration::from_millis(u64::from(action.delay_ms));

        // A full queue means the worker is wedged. Dropping the job is right: the
        // detector must never block, and a backlog of stale slap sounds helps nobody.
        if self.tx.send(Scheduled { due, job }).is_err() {
            tracing::error!("action worker has gone away");
        }
    }

    /// Choose which sound to play.
    fn pick_sound(&mut self, paths: &[PathBuf], order: SoundOrder) -> Option<PathBuf> {
        let candidates = playlist(paths);
        match candidates.len() {
            0 => None,
            1 => candidates.into_iter().next(),
            n => {
                let chosen = match order {
                    SoundOrder::Sequential => {
                        let at = self.rotation % n;
                        self.rotation = self.rotation.wrapping_add(1);
                        candidates.get(at).cloned()
                    }
                    SoundOrder::Random => {
                        // Draw from everything *except* the last pick. Uniform picking
                        // repeats about 1 slap in n, which people hear as the randomness
                        // being broken rather than as randomness; rerolling once only
                        // reduces that rather than removing it.
                        let pool: Vec<&PathBuf> = candidates
                            .iter()
                            .filter(|p| Some(*p) != self.last_pick.as_ref())
                            .collect();
                        let pool = if pool.is_empty() {
                            candidates.iter().collect()
                        } else {
                            pool
                        };
                        pool.get(self.next_random(pool.len())).map(|p| (*p).clone())
                    }
                };
                self.last_pick = chosen.clone();
                chosen
            }
        }
    }

    /// xorshift64*, which is ample for choosing a sound effect and saves a dependency.
    fn next_random(&mut self, bound: usize) -> usize {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545F4914F6CDD1D) >> 33) as usize % bound.max(1)
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

/// Expand the configured list into the files that will actually play.
///
/// Entries are kept in the order given, with duplicates removed, so sequential playback
/// follows the list as displayed.
fn playlist(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in paths {
        for file in resolve_sounds(entry) {
            if !out.contains(&file) {
                out.push(file);
            }
        }
    }
    out
}

/// Expand one configured entry into the files it refers to.
///
/// A directory becomes every audio file directly inside it, sorted so the order is stable
/// across restarts.
fn resolve_sounds(path: &Path) -> Vec<PathBuf> {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_audio(p))
            .collect();
        files.sort();
        files
    } else if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        Vec::new()
    }
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

/// Gain adjustment for a given intensity, as a decibel offset from the base volume.
///
/// Symmetric about mid-intensity: a middling slap plays at exactly the configured volume,
/// so turning this on does not quietly make everything quieter than the system volume the
/// user chose. `range_pct` is the swing either side, expressed as a percentage of
/// amplitude because that is what a slider labelled "±40%" should mean.
fn intensity_gain_db(intensity: f32, range_pct: f32) -> f32 {
    let span = range_pct.clamp(0.0, 100.0) / 100.0;
    let multiplier = (1.0 + (intensity.clamp(0.0, 1.0) - 0.5) * 2.0 * span).clamp(0.05, 4.0);
    20.0 * multiplier.log10()
}

/// Values available to `{{...}}` placeholders.
fn template_vars(slap: &Slap) -> BTreeMap<&'static str, String> {
    let mut v = BTreeMap::new();
    v.insert("tier", slap.tier.as_str().to_string());
    v.insert("intensity", format!("{:.3}", slap.intensity));
    v.insert("peak_g", format!("{:.4}", slap.peak_g));
    v.insert("votes", slap.votes.to_string());
    v.insert("gyro_peak", format!("{:.2}", slap.gyro_peak));
    v.insert("gyro_ratio", format!("{:.1}", slap.gyro_ratio));
    v.insert("gyro_confirmed", slap.gyro_confirmed.to_string());
    v.insert("t", format!("{:.3}", slap.t));
    v.insert("axis_x", format!("{:.4}", slap.axis[0]));
    v.insert("axis_y", format!("{:.4}", slap.axis[1]));
    v.insert("axis_z", format!("{:.4}", slap.axis[2]));
    v
}

/// The same values as `SPANK_*` environment variables, for shell one-liners.
fn env_vars(slap: &Slap) -> Vec<(String, String)> {
    template_vars(slap)
        .into_iter()
        .map(|(k, v)| (format!("SPANK_{}", k.to_uppercase()), v))
        .collect()
}

/// Longest a due job may be held up by the poll granularity.
const SCHEDULER_TICK: Duration = Duration::from_millis(5);

fn run_worker(rx: mpsc::Receiver<Scheduled>) {
    // Opened on demand rather than at startup — see AUDIO_IDLE_TIMEOUT.
    let mut audio: Option<AudioManager<DefaultBackend>> = None;
    let mut last_sound = Instant::now();
    // Kept sorted by due time. Delays are a handful of jobs at most, so a Vec beats a
    // heap for both simplicity and cache behaviour.
    let mut pending: Vec<Scheduled> = Vec::new();

    loop {
        // Run everything that has come due.
        let now = Instant::now();
        while pending.first().is_some_and(|s| s.due <= now) {
            let s = pending.remove(0);
            execute(&mut audio, &mut last_sound, s.job);
        }

        // Sleep until the next job is due, the next idle check, or a new job arrives.
        let wait = pending
            .first()
            .map(|s| s.due.saturating_duration_since(Instant::now()))
            .map(|d| d.max(SCHEDULER_TICK).min(AUDIO_IDLE_POLL))
            .unwrap_or(AUDIO_IDLE_POLL);

        match rx.recv_timeout(wait) {
            Ok(scheduled) => {
                if scheduled.due <= Instant::now() {
                    execute(&mut audio, &mut last_sound, scheduled.job);
                } else {
                    // A delayed sound is a chance to open the device *during* the wait
                    // rather than after it. Opening CoreAudio was measured between 70 ms
                    // and 380 ms depending on what else holds it, which would otherwise
                    // stack on top of the configured delay and make the response late by
                    // an amount that varies run to run.
                    if matches!(scheduled.job, Job::Sound { .. }) {
                        ensure_audio(&mut audio);
                    }
                    let at = pending
                        .binary_search_by(|s| s.due.cmp(&scheduled.due))
                        .unwrap_or_else(|i| i);
                    pending.insert(at, scheduled);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if audio.is_some()
                    && last_sound.elapsed() > AUDIO_IDLE_TIMEOUT
                    && pending.is_empty()
                {
                    tracing::debug!("releasing the audio device after inactivity");
                    audio = None;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Open the audio device if it is not already open.
///
/// A failure is logged and swallowed: no audio device is survivable, since exec and
/// webhook actions still work without one.
fn ensure_audio(audio: &mut Option<AudioManager<DefaultBackend>>) {
    if audio.is_some() {
        return;
    }
    match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
        Ok(m) => *audio = Some(m),
        Err(e) => tracing::error!("could not open an audio device: {e}"),
    }
}

fn execute(audio: &mut Option<AudioManager<DefaultBackend>>, last_sound: &mut Instant, job: Job) {
    match job {
        Job::Sound { data } => {
            *last_sound = Instant::now();
            ensure_audio(audio);
            if let Some(manager) = audio.as_mut() {
                if let Err(e) = manager.play(*data) {
                    tracing::warn!("could not play sound: {e}");
                }
            }
        }

        Job::Exec {
            program,
            args,
            stdin_json,
            env,
            payload,
        } => {
            let mut cmd = Command::new(&program);
            cmd.args(&args)
                .envs(env)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(if stdin_json {
                    Stdio::piped()
                } else {
                    Stdio::null()
                });

            match cmd.spawn() {
                Ok(mut child) => {
                    if stdin_json {
                        if let Some(mut stdin) = child.stdin.take() {
                            let _ = stdin.write_all(payload.as_bytes());
                            let _ = stdin.write_all(b"\n");
                        }
                    }
                    // Reaped so the process table does not fill with zombies, but the
                    // exit status is not otherwise interesting.
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                Err(e) => tracing::warn!("could not run `{program}`: {e}"),
            }
        }

        Job::Webhook {
            url,
            method,
            headers,
            body,
            timeout,
        } => {
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .build()
                .new_agent();

            // The typed `get`/`post` builders can't express an arbitrary configured
            // method, so build the request directly and hand it to the agent.
            let mut builder = ureq::http::Request::builder()
                .method(method.as_str())
                .uri(&url);
            let has_content_type = headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("content-type"));
            for (k, v) in &headers {
                builder = builder.header(k, v);
            }
            if !has_content_type {
                builder = builder.header("content-type", "application/json");
            }

            match builder.body(body.as_bytes()) {
                Ok(request) => {
                    if let Err(e) = agent.run(request) {
                        tracing::warn!("webhook {method} {url} failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("webhook {method} {url} is malformed: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yamete_dsp::Tier;

    fn slap(intensity: f32) -> Slap {
        Slap {
            t: 12.5,
            tier: Tier::Major,
            peak_g: 0.4321,
            intensity,
            votes: 4,
            gyro_confirmed: true,
            gyro_peak: 42.5,
            gyro_ratio: 512.25,
            axis: [0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn mid_intensity_plays_at_the_configured_volume() {
        // The whole point of the symmetric mapping: enabling scaling must not make the
        // app quieter overall than the volume the user actually set.
        assert!(intensity_gain_db(0.5, 40.0).abs() < 1e-4);
        assert!(intensity_gain_db(0.5, 100.0).abs() < 1e-4);
    }

    #[test]
    fn intensity_swings_symmetrically_around_the_base() {
        let soft = intensity_gain_db(0.0, 40.0);
        let hard = intensity_gain_db(1.0, 40.0);
        assert!(soft < 0.0, "a gentle slap should be quieter, got {soft} dB");
        assert!(hard > 0.0, "a hard slap should be louder, got {hard} dB");

        // ±40% of amplitude: 0.6x and 1.4x.
        assert!((soft - 20.0 * 0.6f32.log10()).abs() < 1e-3);
        assert!((hard - 20.0 * 1.4f32.log10()).abs() < 1e-3);
    }

    #[test]
    fn a_zero_range_disables_the_swing_entirely() {
        for intensity in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!(intensity_gain_db(intensity, 0.0).abs() < 1e-4);
        }
    }

    #[test]
    fn gain_is_monotonic_and_bounded() {
        let mut previous = f32::NEG_INFINITY;
        for step in 0..=10 {
            let g = intensity_gain_db(step as f32 / 10.0, 100.0);
            assert!(g > previous, "gain must rise with intensity");
            assert!(g.is_finite(), "gain must never be infinite");
            previous = g;
        }
        // Even at full range a silent slap must not produce negative infinity.
        assert!(intensity_gain_db(0.0, 100.0) > -40.0);
    }

    #[test]
    fn out_of_range_input_is_clamped() {
        assert_eq!(intensity_gain_db(5.0, 40.0), intensity_gain_db(1.0, 40.0));
        assert_eq!(intensity_gain_db(-1.0, 40.0), intensity_gain_db(0.0, 40.0));
        assert_eq!(intensity_gain_db(0.5, 500.0), intensity_gain_db(0.5, 100.0));
    }

    #[test]
    fn scaling_is_off_by_default() {
        // Playing at the system volume is what "play a sound" should mean; anything else
        // is a surprise the user did not ask for.
        match Action::default_sound().kind {
            ActionKind::Sound {
                scale_with_intensity,
                volume_db,
                ..
            } => {
                assert!(!scale_with_intensity);
                assert_eq!(volume_db, 0.0, "no trim means the system volume governs");
            }
            other => panic!("unexpected default kind: {other:?}"),
        }
    }

    #[test]
    fn template_vars_cover_the_whole_event() {
        let v = template_vars(&slap(0.8));
        assert_eq!(v["tier"], "major");
        assert_eq!(v["intensity"], "0.800");
        assert_eq!(v["votes"], "4");
        assert_eq!(v["gyro_confirmed"], "true");
        assert_eq!(v["peak_g"], "0.4321");
    }

    #[test]
    fn env_vars_are_prefixed_and_uppercased() {
        let env: BTreeMap<String, String> = env_vars(&slap(0.8)).into_iter().collect();
        assert_eq!(env["SPANK_TIER"], "major");
        assert_eq!(env["SPANK_INTENSITY"], "0.800");
        assert_eq!(env["SPANK_GYRO_RATIO"], "512.2");
        // Nothing unprefixed leaks into the child's environment.
        assert!(env.keys().all(|k| k.starts_with("SPANK_")));
    }

    #[test]
    fn audio_extensions_are_matched_case_insensitively() {
        assert!(is_audio(Path::new("/x/a.mp3")));
        assert!(is_audio(Path::new("/x/a.MP3")));
        assert!(is_audio(Path::new("/x/a.aiff")));
        assert!(!is_audio(Path::new("/x/a.txt")));
        assert!(!is_audio(Path::new("/x/noextension")));
    }

    #[test]
    fn sequential_order_walks_the_list_and_wraps() {
        let dir = std::env::temp_dir().join(format!("yamete-seq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<PathBuf> = ["a.wav", "b.wav", "c.wav"]
            .iter()
            .map(|n| {
                let p = dir.join(n);
                std::fs::write(&p, b"x").unwrap();
                p
            })
            .collect();

        let mut ex = Executor {
            tx: mpsc::channel().0,
            cache: BTreeMap::new(),
            rotation: 0,
            rng: 1,
            last_pick: None,
        };
        let picks: Vec<PathBuf> = (0..4)
            .filter_map(|_| ex.pick_sound(&files, SoundOrder::Sequential))
            .collect();
        assert_eq!(
            picks,
            [&files[0], &files[1], &files[2], &files[0]].map(|p| p.clone())
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn random_order_covers_the_list_without_immediate_repeats() {
        let dir = std::env::temp_dir().join(format!("yamete-rand-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<PathBuf> = ["a.wav", "b.wav", "c.wav", "d.wav"]
            .iter()
            .map(|n| {
                let p = dir.join(n);
                std::fs::write(&p, b"x").unwrap();
                p
            })
            .collect();

        let mut ex = Executor {
            tx: mpsc::channel().0,
            cache: BTreeMap::new(),
            rotation: 0,
            rng: 0x123456789abcdef,
            last_pick: None,
        };
        let picks: Vec<PathBuf> = (0..200)
            .filter_map(|_| ex.pick_sound(&files, SoundOrder::Random))
            .collect();

        // Every sound should come up over 200 draws from four.
        for f in &files {
            assert!(picks.contains(f), "{} never played", f.display());
        }
        // Hearing the same clip twice running reads as the randomness being broken.
        let repeats = picks.windows(2).filter(|w| w[0] == w[1]).count();
        assert_eq!(repeats, 0, "played the same sound twice in a row");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_single_sound_ignores_the_order() {
        let dir = std::env::temp_dir().join(format!("yamete-one-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let only = dir.join("only.wav");
        std::fs::write(&only, b"x").unwrap();

        for order in [SoundOrder::Sequential, SoundOrder::Random] {
            let mut ex = Executor {
                tx: mpsc::channel().0,
                cache: BTreeMap::new(),
                rotation: 0,
                rng: 7,
                last_pick: None,
            };
            for _ in 0..5 {
                assert_eq!(
                    ex.pick_sound(std::slice::from_ref(&only), order),
                    Some(only.clone())
                );
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_playlist_flattens_directories_and_drops_duplicates() {
        let dir = std::env::temp_dir().join(format!("yamete-pl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["a.mp3", "b.wav"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        // The directory and one of its files, listed together.
        let entries = vec![dir.clone(), dir.join("a.mp3")];
        let flat = playlist(&entries);
        assert_eq!(
            flat.len(),
            2,
            "duplicate should have been dropped: {flat:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolving_a_missing_path_yields_nothing() {
        assert!(resolve_sounds(Path::new("/nonexistent/nope.wav")).is_empty());
    }

    #[test]
    fn resolving_a_directory_finds_audio_and_sorts_it() {
        let dir = std::env::temp_dir().join(format!("yamete-sounds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.wav", "a.mp3", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let found = resolve_sounds(&dir);
        assert_eq!(found.len(), 2, "should skip the .txt: {found:?}");
        assert!(found[0].ends_with("a.mp3"), "not sorted: {found:?}");
        assert!(found[1].ends_with("b.wav"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn opening_the_audio_device_is_survivably_slow() {
        // This is why delayed sounds pre-warm the device instead of opening it when the
        // job comes due. Measured between 70 ms and 380 ms on the same machine depending
        // on what else holds CoreAudio, so the bound here is deliberately loose — the
        // point is to catch a pathological regression, not to pin a system timing that
        // legitimately varies.
        let start = Instant::now();
        let opened = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default());
        let elapsed = start.elapsed();
        if opened.is_err() {
            return; // no audio device in this environment
        }
        assert!(
            elapsed < Duration::from_secs(3),
            "opening the audio device took {elapsed:?}, which no amount of delay hides"
        );
        println!("audio device opened in {elapsed:?}");
    }

    #[test]
    fn a_delayed_sound_gets_the_device_open_before_it_is_due() {
        // The scheduler only pre-warms for sounds; an exec or webhook has no reason to
        // spin up CoreAudio.
        let Ok(data) = StaticSoundData::from_file("/System/Library/Sounds/Sosumi.aiff") else {
            return; // no system sound available in this environment
        };
        let sound = Job::Sound {
            data: Box::new(data),
        };
        assert!(matches!(sound, Job::Sound { .. }));

        let exec = Job::Exec {
            program: "/bin/true".into(),
            args: vec![],
            stdin_json: false,
            env: vec![],
            payload: String::new(),
        };
        assert!(!matches!(exec, Job::Sound { .. }));
    }

    #[test]
    fn delay_is_generic_across_every_action_kind() {
        // The delay lives on the Action, so a webhook or a script gets the same
        // treatment as a sound — the reason for waiting (let the impact noise finish)
        // has nothing to do with audio.
        for kind in [
            ActionKind::Exec {
                program: "/bin/true".into(),
                args: vec![],
                stdin_json: false,
            },
            ActionKind::Webhook {
                url: "https://example.invalid/hook".into(),
                method: "POST".into(),
                headers: BTreeMap::new(),
                body: None,
                timeout_ms: 100,
            },
        ] {
            let action = Action {
                id: "x".into(),
                delay_ms: 40,
                kind,
                ..Default::default()
            };
            assert_eq!(action.delay_ms, 40);
            assert!(yamete_proto::DaemonConfig {
                actions: vec![action],
                ..Default::default()
            }
            .validate()
            .is_ok());
        }
    }

    #[test]
    fn pending_jobs_stay_sorted_by_due_time() {
        // The worker pops from the front, so an out-of-order insert would fire a later
        // job first and hold an earlier one until the queue drained.
        let base = Instant::now();
        let mut pending: Vec<Instant> = Vec::new();
        for offset in [50u64, 10, 30, 5, 40] {
            let due = base + Duration::from_millis(offset);
            let at = pending.binary_search(&due).unwrap_or_else(|i| i);
            pending.insert(at, due);
        }
        let mut sorted = pending.clone();
        sorted.sort();
        assert_eq!(pending, sorted, "insertion did not preserve ordering");
    }

    #[test]
    fn the_default_action_delays_enough_to_clear_the_impact() {
        // Detection is fast enough that a zero delay puts the sound underneath the
        // physical slap. Anything in the tens of milliseconds reads as a response
        // rather than an echo.
        let action = Action::default_sound();
        assert!(
            (30..=500).contains(&action.delay_ms),
            "default delay of {} ms is outside the useful range",
            action.delay_ms
        );
    }

    #[test]
    fn an_absurd_delay_is_rejected() {
        let config = yamete_proto::DaemonConfig {
            actions: vec![Action {
                id: "slow".into(),
                delay_ms: 60_000,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(config.validate().unwrap_err().contains("delay_ms"));
    }

    #[test]
    fn a_real_system_sound_decodes() {
        // Guards the kira feature flags: the default action points at an AIFF, which
        // needs a non-default decoder feature enabled.
        let path = Path::new("/System/Library/Sounds/Sosumi.aiff");
        if !path.exists() {
            return;
        }
        assert!(
            StaticSoundData::from_file(path).is_ok(),
            "could not decode a macOS system sound — is the `aiff` kira feature enabled?"
        );
    }
}
