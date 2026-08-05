//! Talking to a running daemon from the command line.
//!
//! Everything here is also reachable with `nc -U`, but having it built in means the
//! diagnostic path does not depend on remembering the JSON, which matters most when
//! something is wrong under launchd and there is no terminal attached.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use yamete_proto::{Event, Request};
use yamete_sensor::Error;

fn connect() -> Result<UnixStream, Error> {
    let path = yamete_proto::socket_path();
    UnixStream::connect(&path).map_err(|e| {
        Error::Iokit(format!(
            "could not reach the daemon at {}: {e}\nIs it running? Try `yamete run`.",
            path.display()
        ))
    })
}

fn send(stream: &mut UnixStream, request: &Request) -> Result<(), Error> {
    stream
        .write_all(yamete_proto::to_line(request).as_bytes())
        .map_err(|e| Error::Iokit(format!("could not send request: {e}")))
}

/// Print the daemon's current state.
pub fn status() -> Result<(), Error> {
    let mut stream = connect()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| Error::Iokit(e.to_string()))?;
    send(&mut stream, &Request::GetStatus)?;

    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|e| Error::Iokit(format!("no reply from the daemon: {e}")))?;

    match serde_json::from_str::<Event>(&line) {
        Ok(Event::Status(s)) => {
            println!(
                "yamete {}  ({})",
                s.version,
                if s.enabled { "enabled" } else { "DISABLED" }
            );
            println!("  uptime     {:.0}s", s.uptime_s);
            println!(
                "  sensor     {:.0} Hz{}",
                s.rate_hz,
                if s.has_gyro {
                    ", gyro present"
                } else {
                    ", NO GYRO"
                }
            );
            println!("  slaps      {}", s.slaps);
            println!("  telemetry  {} subscriber(s)", s.telemetry_subscribers);
            if s.warming_up {
                println!("  state      warming up (building the background estimate)");
            }
            Ok(())
        }
        Ok(Event::Error { message }) => Err(Error::Iokit(message)),
        Ok(other) => Err(Error::Iokit(format!("unexpected reply: {other:?}"))),
        Err(e) => Err(Error::Iokit(format!("could not parse reply: {e}"))),
    }
}

/// Turn detection on or off in the running daemon.
pub fn set_enabled(value: bool) -> Result<(), Error> {
    let mut stream = connect()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| Error::Iokit(e.to_string()))?;
    send(&mut stream, &Request::SetEnabled { value })?;

    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|e| Error::Iokit(format!("no reply from the daemon: {e}")))?;

    match serde_json::from_str::<Event>(&line) {
        Ok(Event::Config { config }) => {
            println!(
                "detection {}",
                if config.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            Ok(())
        }
        Ok(Event::Error { message }) => Err(Error::Iokit(message)),
        _ => Err(Error::Iokit("unexpected reply".into())),
    }
}

/// Stream slaps from the running daemon until interrupted.
pub fn listen(as_json: bool) -> Result<(), Error> {
    let mut stream = connect()?;
    send(
        &mut stream,
        &Request::Subscribe {
            slaps: true,
            telemetry: false,
        },
    )?;
    if !as_json {
        println!("Listening for slaps. Ctrl-C to stop.");
    }

    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.map_err(|e| Error::Iokit(format!("connection lost: {e}")))?;
        if as_json {
            println!("{line}");
            continue;
        }
        match serde_json::from_str::<Event>(&line) {
            Ok(Event::Slap(s)) => println!(
                "t={:8.3}s  {:6}  peak {:.4} g  intensity {:.2}  votes {}/5  gyro {:.0} deg/s",
                s.t,
                s.tier.as_str(),
                s.peak_g,
                s.intensity,
                s.votes,
                s.gyro_peak,
            ),
            Ok(Event::Error { message }) => eprintln!("error: {message}"),
            _ => {}
        }
    }
    Ok(())
}

/// Fire one action, to check it is configured the way you meant.
pub fn test_action(id: &str, intensity: f32) -> Result<(), Error> {
    let mut stream = connect()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(|e| Error::Iokit(e.to_string()))?;
    send(
        &mut stream,
        &Request::TestAction {
            id: id.to_string(),
            intensity,
        },
    )?;

    // A successful test produces no reply, so anything that arrives is a complaint.
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_ok() && !line.trim().is_empty() {
        if let Ok(Event::Error { message }) = serde_json::from_str::<Event>(&line) {
            return Err(Error::Iokit(message));
        }
    }
    println!("fired `{id}` at intensity {intensity:.2}");
    Ok(())
}
