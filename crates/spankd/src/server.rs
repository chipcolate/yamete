//! The control socket.
//!
//! Newline-delimited JSON over a unix domain socket. Clients connect, optionally subscribe
//! to slaps and/or telemetry, and can read or replace the configuration. Debuggable with
//! `nc -U ~/Library/Application\ Support/com.chipcolate.spank/spank.sock`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use spank_proto::{to_line, DaemonConfig, Event, Request, Status};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};

/// Shared handles the socket server needs to talk to the detector loop.
#[derive(Clone)]
pub struct Shared {
    /// Latest config. The detector loop watches this for changes.
    pub config: watch::Sender<DaemonConfig>,
    /// Everything the detector emits.
    pub events: broadcast::Sender<Event>,
    /// How many connections currently want telemetry.
    ///
    /// The detector loop checks this before doing any telemetry work at all, so an
    /// unobserved daemon pays nothing for a feature nobody is watching.
    pub telemetry_subscribers: Arc<AtomicUsize>,
    /// Live status, refreshed by the detector loop.
    pub status: watch::Sender<Status>,
    /// Requests to run a single action, for UI previews.
    pub test_action: mpsc::UnboundedSender<(String, f32)>,
    /// Where the config is persisted.
    pub config_path: PathBuf,
}

/// Bind the socket, replacing any stale one.
///
/// A socket file left behind by a crash is not evidence that another daemon is running, so
/// probe it first: if nothing answers, the path is ours to reclaim.
pub async fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("another spankd is already listening on {}", path.display()),
                ))
            }
            Err(_) => {
                tracing::info!("removing stale socket at {}", path.display());
                std::fs::remove_file(path)?;
            }
        }
    }

    let listener = UnixListener::bind(path)?;
    // Owner-only. The socket exposes config writes and arbitrary command execution via
    // exec actions, so it must not be reachable by other local users.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// Reject connections from other users.
///
/// macOS has no `SO_PEERCRED`; `getpeereid` is the equivalent. Belt and braces alongside
/// the 0600 socket mode.
fn peer_is_owner(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    rc == 0 && uid == unsafe { libc::geteuid() }
}

pub async fn serve(listener: UnixListener, shared: Shared) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                continue;
            }
        };
        if !peer_is_owner(&stream) {
            tracing::warn!("rejected a connection from another user");
            continue;
        }
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, shared).await {
                tracing::debug!("connection ended: {e}");
            }
        });
    }
}

/// Per-connection subscription state.
struct Subscription {
    slaps: bool,
    telemetry: bool,
    counter: Arc<AtomicUsize>,
}

impl Subscription {
    fn set(&mut self, slaps: bool, telemetry: bool) {
        if telemetry && !self.telemetry {
            self.counter.fetch_add(1, Ordering::Relaxed);
        } else if !telemetry && self.telemetry {
            self.counter.fetch_sub(1, Ordering::Relaxed);
        }
        self.slaps = slaps;
        self.telemetry = telemetry;
    }

    fn wants(&self, event: &Event) -> bool {
        match event {
            Event::Slap(_) => self.slaps,
            Event::Telemetry(_) => self.telemetry,
            // Config, status and errors are replies, not subscriptions.
            _ => false,
        }
    }
}

impl Drop for Subscription {
    /// Decrement on any disconnect, including an abrupt one — otherwise a client that
    /// crashes mid-subscription leaves the daemon generating telemetry forever.
    fn drop(&mut self) {
        if self.telemetry {
            self.counter.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

async fn handle(stream: UnixStream, shared: Shared) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let mut events = shared.events.subscribe();

    let mut sub = Subscription {
        slaps: false,
        telemetry: false,
        counter: Arc::clone(&shared.telemetry_subscribers),
    };

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let reply = match serde_json::from_str::<Request>(&line) {
                    Ok(request) => handle_request(request, &shared, &mut sub),
                    Err(e) => Some(Event::Error {
                        message: format!("could not parse request: {e}"),
                    }),
                };
                if let Some(event) = reply {
                    write_half.write_all(to_line(&event).as_bytes()).await?;
                }
            }

            event = events.recv() => {
                match event {
                    Ok(event) if sub.wants(&event) => {
                        write_half.write_all(to_line(&event).as_bytes()).await?;
                    }
                    Ok(_) => {}
                    // A slow client falling behind drops frames rather than stalling the
                    // detector — telemetry is a live view, not a log.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!("client lagged, dropped {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

fn handle_request(request: Request, shared: &Shared, sub: &mut Subscription) -> Option<Event> {
    match request {
        Request::Ping => Some(Event::Pong),

        Request::GetConfig => Some(Event::Config {
            config: Box::new(shared.config.borrow().clone()),
        }),

        Request::GetStatus => Some(Event::Status(shared.status.borrow().clone())),

        Request::SetConfig { mut config } => {
            config.normalize();
            if let Err(problem) = config.validate() {
                return Some(Event::Error {
                    message: format!("rejected: {problem}"),
                });
            }
            if let Err(e) = crate::store::save(&shared.config_path, &config) {
                // Applying it anyway would silently revert on restart, so treat a failed
                // write as a failed change.
                return Some(Event::Error {
                    message: format!("could not save config: {e}"),
                });
            }
            let applied = *config;
            let _ = shared.config.send(applied.clone());
            Some(Event::Config {
                config: Box::new(applied),
            })
        }

        Request::SetEnabled { value } => {
            let mut config = shared.config.borrow().clone();
            config.enabled = value;
            if let Err(e) = crate::store::save(&shared.config_path, &config) {
                tracing::warn!("could not persist enabled={value}: {e}");
            }
            let _ = shared.config.send(config.clone());
            Some(Event::Config {
                config: Box::new(config),
            })
        }

        Request::Subscribe { slaps, telemetry } => {
            sub.set(slaps, telemetry);
            None
        }

        Request::TestAction { id, intensity } => {
            if shared.config.borrow().action(&id).is_none() {
                return Some(Event::Error {
                    message: format!("no action with id `{id}`"),
                });
            }
            let _ = shared.test_action.send((id, intensity.clamp(0.0, 1.0)));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscription() -> (Subscription, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        (
            Subscription {
                slaps: false,
                telemetry: false,
                counter: Arc::clone(&counter),
            },
            counter,
        )
    }

    fn slap_event() -> Event {
        Event::Slap(spank_proto::Slap {
            t: 0.0,
            tier: spank_dsp::Tier::Major,
            peak_g: 0.1,
            intensity: 0.5,
            votes: 4,
            gyro_confirmed: true,
            gyro_peak: 30.0,
            gyro_ratio: 300.0,
            axis: [0.0, 0.0, 1.0],
        })
    }

    fn telemetry_event() -> Event {
        Event::Telemetry(spank_proto::Telemetry {
            t: 0.0,
            envelope: vec![],
            gyro: vec![],
            scores: Default::default(),
            dropped: 0,
        })
    }

    #[test]
    fn subscriptions_filter_by_kind() {
        let (mut sub, _) = subscription();
        assert!(!sub.wants(&slap_event()));

        sub.set(true, false);
        assert!(sub.wants(&slap_event()));
        assert!(!sub.wants(&telemetry_event()));

        sub.set(true, true);
        assert!(sub.wants(&telemetry_event()));
    }

    #[test]
    fn replies_are_never_delivered_as_subscription_events() {
        // Otherwise a client that subscribes to slaps would also receive every other
        // client's config replies.
        let (mut sub, _) = subscription();
        sub.set(true, true);
        assert!(!sub.wants(&Event::Pong));
        assert!(!sub.wants(&Event::Error { message: "x".into() }));
        assert!(!sub.wants(&Event::Config {
            config: Box::new(DaemonConfig::default())
        }));
    }

    #[test]
    fn telemetry_subscriber_count_tracks_state_changes() {
        let (mut sub, counter) = subscription();
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        sub.set(false, true);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // Re-subscribing must not double-count.
        sub.set(true, true);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        sub.set(true, false);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn dropping_a_subscribed_connection_releases_its_count() {
        let (mut sub, counter) = subscription();
        sub.set(false, true);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub);
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "a client that disconnects abruptly would leave telemetry running forever"
        );
    }

    #[test]
    fn dropping_an_unsubscribed_connection_does_not_underflow() {
        let (sub, counter) = subscription();
        drop(sub);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    // --- end-to-end over a real socket ---
    //
    // This protocol is the contract every client depends on, so it is worth exercising
    // over an actual UnixListener rather than only unit-testing the pieces.

    use tokio::io::AsyncBufReadExt;

    struct Harness {
        path: PathBuf,
        shared: Shared,
        _test_rx: mpsc::UnboundedReceiver<(String, f32)>,
        status_rx: watch::Receiver<Status>,
        config_rx: watch::Receiver<DaemonConfig>,
    }

    fn harness(name: &str) -> Harness {
        let dir = std::env::temp_dir().join(format!("spank-sock-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (config, config_rx) = watch::channel(DaemonConfig::default());
        let (events, _) = broadcast::channel(64);
        let (test_action, _test_rx) = mpsc::unbounded_channel();
        let (status, status_rx) = watch::channel(Status {
            enabled: true,
            version: "test".into(),
            has_gyro: true,
            uptime_s: 1.0,
            slaps: 3,
            rate_hz: 805.0,
            warming_up: false,
            telemetry_subscribers: 0,
        });

        Harness {
            path: dir.join("s.sock"),
            shared: Shared {
                config,
                events,
                telemetry_subscribers: Arc::new(AtomicUsize::new(0)),
                status,
                test_action,
                config_path: dir.join("config.json"),
            },
            _test_rx,
            status_rx,
            config_rx,
        }
    }

    async fn connect(h: &Harness) -> (tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>, tokio::net::unix::OwnedWriteHalf) {
        let stream = UnixStream::connect(&h.path).await.unwrap();
        let (r, w) = stream.into_split();
        (BufReader::new(r).lines(), w)
    }

    async fn ask(h: &Harness, request: &str) -> Event {
        let (mut lines, mut w) = connect(h).await;
        w.write_all(format!("{request}\n").as_bytes()).await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
            .await
            .expect("timed out waiting for a reply")
            .unwrap()
            .expect("connection closed without replying");
        serde_json::from_str(&line).unwrap()
    }

    use std::time::Duration;

    #[tokio::test]
    async fn round_trips_over_a_real_socket() {
        let h = harness("roundtrip");
        let listener = bind(&h.path).await.unwrap();
        tokio::spawn(serve(listener, h.shared.clone()));

        assert!(matches!(ask(&h, r#"{"cmd":"ping"}"#).await, Event::Pong));
        assert!(matches!(ask(&h, r#"{"cmd":"get_config"}"#).await, Event::Config { .. }));

        match ask(&h, r#"{"cmd":"get_status"}"#).await {
            Event::Status(s) => {
                assert_eq!(s.slaps, 3);
                assert!(s.has_gyro);
            }
            other => panic!("expected status, got {other:?}"),
        }

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn the_socket_is_owner_only() {
        let h = harness("perms");
        let _listener = bind(&h.path).await.unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&h.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the socket accepts config writes and exec actions; it must not be \
             reachable by other local users"
        );

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_reclaimed() {
        let h = harness("stale");
        // Simulate a crash: a socket file with nothing listening behind it.
        std::fs::write(&h.path, b"").unwrap();
        assert!(
            bind(&h.path).await.is_ok(),
            "a leftover socket file must not stop the daemon starting"
        );
        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn a_second_daemon_refuses_to_start() {
        let h = harness("inuse");
        let listener = bind(&h.path).await.unwrap();
        tokio::spawn(serve(listener, h.shared.clone()));

        let err = bind(&h.path).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn set_config_persists_and_notifies_the_detector() {
        let h = harness("setconfig");
        let listener = bind(&h.path).await.unwrap();
        tokio::spawn(serve(listener, h.shared.clone()));
        let mut config_rx = h.config_rx.clone();

        let mut cfg = DaemonConfig::default();
        cfg.detector.sensitivity = 0.8;
        let request = format!(
            r#"{{"cmd":"set_config","config":{}}}"#,
            serde_json::to_string(&cfg).unwrap()
        );

        match ask(&h, &request).await {
            Event::Config { config } => assert_eq!(config.detector.sensitivity, 0.8),
            other => panic!("expected config, got {other:?}"),
        }

        // The detector loop learns about it through the watch channel...
        assert!(config_rx.has_changed().unwrap());
        assert_eq!(config_rx.borrow_and_update().detector.sensitivity, 0.8);
        // ...and it survives a restart.
        let (reloaded, warning) = crate::store::load(&h.shared.config_path);
        assert!(warning.is_none());
        assert_eq!(reloaded.detector.sensitivity, 0.8);

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn an_invalid_config_is_rejected_and_nothing_changes() {
        let h = harness("reject");
        let listener = bind(&h.path).await.unwrap();
        tokio::spawn(serve(listener, h.shared.clone()));

        match ask(&h, r#"{"cmd":"set_config","config":{"detector":{"sensitivity":9.0}}}"#).await {
            Event::Error { message } => assert!(message.contains("sensitivity"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
        assert_eq!(h.shared.config.borrow().detector.sensitivity, 0.5);
        assert!(
            !h.shared.config_path.exists(),
            "a rejected config must not be written to disk"
        );

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn subscribers_receive_only_what_they_asked_for() {
        let h = harness("subscribe");
        let listener = bind(&h.path).await.unwrap();
        tokio::spawn(serve(listener, h.shared.clone()));

        let (mut lines, mut w) = connect(&h).await;
        w.write_all(b"{\"cmd\":\"subscribe\",\"slaps\":true}\n").await.unwrap();

        // Give the connection task a moment to register the subscription.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(h.shared.telemetry_subscribers.load(Ordering::Relaxed), 0);

        // Telemetry must not arrive; the slap that follows it must.
        let _ = h.shared.events.send(Event::Telemetry(spank_proto::Telemetry {
            t: 0.0,
            envelope: vec![],
            gyro: vec![],
            scores: Default::default(),
            dropped: 0,
        }));
        let _ = h.shared.events.send(slap_event());

        let line = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        assert!(line.contains(r#""event":"slap""#), "got {line}");

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn telemetry_subscribers_are_counted_and_released() {
        let h = harness("count");
        let listener = bind(&h.path).await.unwrap();
        tokio::spawn(serve(listener, h.shared.clone()));

        {
            let (_lines, mut w) = connect(&h).await;
            w.write_all(b"{\"cmd\":\"subscribe\",\"telemetry\":true}\n").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(h.shared.telemetry_subscribers.load(Ordering::Relaxed), 1);
        }

        // Dropping the client releases the count, so the detector stops doing the work.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(h.shared.telemetry_subscribers.load(Ordering::Relaxed), 0);

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn a_garbage_line_does_not_kill_the_connection() {
        let h = harness("garbage");
        let listener = bind(&h.path).await.unwrap();
        tokio::spawn(serve(listener, h.shared.clone()));

        let (mut lines, mut w) = connect(&h).await;
        w.write_all(b"this is not json\n\n{\"cmd\":\"ping\"}\n").await.unwrap();

        let first = lines.next_line().await.unwrap().unwrap();
        assert!(first.contains("error"), "got {first}");
        // Blank lines are skipped and the connection stays usable.
        let second = lines.next_line().await.unwrap().unwrap();
        assert_eq!(second.trim(), r#"{"event":"pong"}"#);

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn status_reflects_the_detector_loop() {
        let h = harness("status");
        let listener = bind(&h.path).await.unwrap();
        tokio::spawn(serve(listener, h.shared.clone()));
        let mut status_rx = h.status_rx.clone();

        h.shared
            .status
            .send(Status {
                enabled: false,
                version: "test".into(),
                has_gyro: false,
                uptime_s: 99.0,
                slaps: 42,
                rate_hz: 804.0,
                warming_up: true,
                telemetry_subscribers: 0,
            })
            .unwrap();
        assert!(status_rx.has_changed().unwrap());

        match ask(&h, r#"{"cmd":"get_status"}"#).await {
            Event::Status(s) => {
                assert_eq!(s.slaps, 42);
                assert!(s.warming_up);
                assert!(!s.enabled);
            }
            other => panic!("expected status, got {other:?}"),
        }

        std::fs::remove_dir_all(h.path.parent().unwrap()).ok();
    }
}
