//! Recorded sensor traces, for testing the detector against reality.
//!
//! The detector's job is to separate slaps from every other thing that shakes a laptop,
//! and no synthetic signal is a fair test of that. `yamete calibrate` records real traces
//! into this format; the detector tests replay them and assert both that slaps fire and
//! that typing, trackpad clicks and desk bumps do not.
//!
//! The format is line-oriented text so fixtures diff sensibly in review:
//!
//! ```text
//! # yamete-fixture v1
//! # label=slap-lid-left
//! # expect=5
//! # model=Mac16,5
//! s,t,x,y,z
//! a,0.000000,0.0051,0.0061,-0.9974
//! g,0.000000,0.2441,0.0000,-0.0610
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

use crate::detector::Frame;

const MAGIC: &str = "# yamete-fixture v1";

/// The header this format used before the project was renamed. Still accepted on read so
/// a trace recorded with an older build does not become unreadable.
const LEGACY_MAGIC: &str = "# spank-fixture v1";
const HEADER: &str = "s,t,x,y,z";

/// Leading bytes of a gzip member.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// A recorded trace of both sensors.
#[derive(Debug, Clone, Default)]
pub struct Fixture {
    /// Short name describing what was recorded, e.g. `slap-lid-left` or `typing`.
    pub label: String,
    /// How many slaps the recording is supposed to contain. `None` means unannotated.
    pub expect: Option<usize>,
    /// Free-form provenance: machine model, macOS version, measured rate.
    pub meta: BTreeMap<String, String>,
    pub accel: Vec<Frame>,
    pub gyro: Vec<Frame>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid UTF-8")]
    NotUtf8 { path: String },
    #[error("not a spank fixture: missing the `{MAGIC}` header")]
    BadMagic,
    #[error("line {line}: expected 5 comma-separated fields, found {found}")]
    FieldCount { line: usize, found: usize },
    #[error("line {line}: could not parse `{value}` as a number")]
    BadNumber { line: usize, value: String },
    #[error("line {line}: unknown sensor tag `{tag}` (expected `a` or `g`)")]
    BadSensor { line: usize, tag: String },
}

impl Fixture {
    pub fn new(label: impl Into<String>) -> Self {
        Fixture {
            label: label.into(),
            ..Default::default()
        }
    }

    /// Duration of the recording, in seconds.
    pub fn duration(&self) -> f64 {
        let last = |v: &Vec<Frame>| v.last().map_or(0.0, |f| f.t);
        last(&self.accel).max(last(&self.gyro))
    }

    /// Measured accelerometer sample rate over the recording.
    pub fn rate_hz(&self) -> f64 {
        let d = self.duration();
        if d <= 0.0 {
            0.0
        } else {
            self.accel.len() as f64 / d
        }
    }

    /// Load a fixture from disk, decompressing if it is gzipped.
    ///
    /// Recorded traces are ~1.7 MB each as text and compress about 4.5x, which is the
    /// difference between a corpus worth committing and one that isn't. Both forms are
    /// accepted so an uncompressed fixture dropped in by hand still works.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, ParseError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| ParseError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_bytes(&bytes).map_err(|e| match e {
            ParseError::NotUtf8 { .. } => ParseError::NotUtf8 {
                path: path.display().to_string(),
            },
            other => other,
        })
    }

    /// Parse a fixture from raw bytes, gzipped or not.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
        let text = if bytes.starts_with(&GZIP_MAGIC) {
            let mut out = String::new();
            flate2::read::GzDecoder::new(bytes)
                .read_to_string(&mut out)
                .map_err(|source| ParseError::Io {
                    path: "<gzip stream>".into(),
                    source,
                })?;
            out
        } else {
            String::from_utf8(bytes.to_vec()).map_err(|_| ParseError::NotUtf8 {
                path: "<input>".into(),
            })?
        };
        Self::from_text(&text)
    }

    /// Write a fixture, gzipping when the path ends in `.gz`.
    pub fn write(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        use std::io::Write as _;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = self.to_text();

        if path.extension().is_some_and(|e| e == "gz") {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            encoder.write_all(text.as_bytes())?;
            std::fs::write(path, encoder.finish()?)
        } else {
            std::fs::write(path, text)
        }
    }

    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity((self.accel.len() + self.gyro.len()) * 48);
        let _ = writeln!(out, "{MAGIC}");
        let _ = writeln!(out, "# label={}", self.label);
        if let Some(n) = self.expect {
            let _ = writeln!(out, "# expect={n}");
        }
        for (k, v) in &self.meta {
            let _ = writeln!(out, "# {k}={v}");
        }
        let _ = writeln!(out, "{HEADER}");

        // Interleave by timestamp so the file reads in chronological order, which makes
        // eyeballing a trace around an event practical.
        let mut a = self.accel.iter().peekable();
        let mut g = self.gyro.iter().peekable();
        loop {
            let take_accel = match (a.peek(), g.peek()) {
                (Some(x), Some(y)) => x.t <= y.t,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            let (tag, f) = if take_accel {
                ("a", a.next().unwrap())
            } else {
                ("g", g.next().unwrap())
            };
            let _ = writeln!(
                out,
                "{tag},{:.6},{:.5},{:.5},{:.5}",
                f.t, f.x, f.y, f.z
            );
        }
        out
    }

    pub fn from_text(text: &str) -> Result<Self, ParseError> {
        let mut lines = text.lines().enumerate();
        let (_, first) = lines.next().ok_or(ParseError::BadMagic)?;
        let header = first.trim();
        if header != MAGIC && header != LEGACY_MAGIC {
            return Err(ParseError::BadMagic);
        }

        let mut fx = Fixture::default();
        for (idx, raw) in lines {
            let line = raw.trim();
            let line_no = idx + 1;

            if line.is_empty() || line == HEADER {
                continue;
            }
            if let Some(comment) = line.strip_prefix('#') {
                if let Some((k, v)) = comment.trim().split_once('=') {
                    match k {
                        "label" => fx.label = v.to_string(),
                        "expect" => fx.expect = v.parse().ok(),
                        _ => {
                            fx.meta.insert(k.to_string(), v.to_string());
                        }
                    }
                }
                continue;
            }

            let mut parts = line.split(',');
            let mut next = |line: usize| -> Result<&str, ParseError> {
                parts.next().ok_or(ParseError::FieldCount { line, found: 0 })
            };
            let tag = next(line_no)?.to_string();
            let mut num = |line: usize| -> Result<f64, ParseError> {
                let raw = parts.next().ok_or(ParseError::FieldCount { line, found: 0 })?;
                raw.parse::<f64>().map_err(|_| ParseError::BadNumber {
                    line,
                    value: raw.to_string(),
                })
            };
            let frame = Frame {
                t: num(line_no)?,
                x: num(line_no)? as f32,
                y: num(line_no)? as f32,
                z: num(line_no)? as f32,
            };
            if parts.next().is_some() {
                return Err(ParseError::FieldCount {
                    line: line_no,
                    found: 6,
                });
            }

            match tag.as_str() {
                "a" => fx.accel.push(frame),
                "g" => fx.gyro.push(frame),
                _ => return Err(ParseError::BadSensor { line: line_no, tag }),
            }
        }
        Ok(fx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_fixture() -> Fixture {
        let mut fx = Fixture::new("slap-lid-left");
        fx.expect = Some(3);
        fx.meta.insert("model".into(), "Mac16,5".into());
        fx.accel = vec![
            Frame { t: 0.0, x: 0.0051, y: 0.0061, z: -0.9974 },
            Frame { t: 0.00124, x: 0.0053, y: 0.0060, z: -0.9970 },
        ];
        fx.gyro = vec![Frame { t: 0.0006, x: 0.2441, y: 0.0, z: -0.061 }];
        fx
    }

    #[test]
    fn round_trips() {
        let fx = sample_fixture();
        let parsed = Fixture::from_text(&fx.to_text()).unwrap();

        assert_eq!(parsed.label, "slap-lid-left");
        assert_eq!(parsed.expect, Some(3));
        assert_eq!(parsed.meta.get("model").map(String::as_str), Some("Mac16,5"));
        assert_eq!(parsed.accel.len(), 2);
        assert_eq!(parsed.gyro.len(), 1);
        assert!((parsed.accel[0].z - (-0.9974)).abs() < 1e-5);
        assert!((parsed.gyro[0].x - 0.2441).abs() < 1e-5);
    }

    #[test]
    fn writes_samples_in_chronological_order() {
        let text = sample_fixture().to_text();
        let tags: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with('#') && *l != HEADER && !l.is_empty())
            .map(|l| l.split(',').next().unwrap())
            .collect();
        // accel@0.0, gyro@0.0006, accel@0.00124
        assert_eq!(tags, ["a", "g", "a"]);
    }

    #[test]
    fn computes_rate_from_the_recording() {
        let mut fx = Fixture::new("x");
        fx.accel = (0..805)
            .map(|i| Frame { t: f64::from(i) / 805.0, x: 0.0, y: 0.0, z: -1.0 })
            .collect();
        assert!((fx.rate_hz() - 805.0).abs() < 2.0, "rate = {}", fx.rate_hz());
    }

    #[test]
    fn round_trips_through_gzip() {
        let fx = sample_fixture();
        let dir = std::env::temp_dir().join(format!("yamete-gz-{}", std::process::id()));
        let path = dir.join("x.fixture.gz");
        fx.write(&path).unwrap();

        // Actually compressed, not just named .gz.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], &GZIP_MAGIC);

        let back = Fixture::read(&path).unwrap();
        assert_eq!(back.label, fx.label);
        assert_eq!(back.accel.len(), fx.accel.len());
        assert_eq!(back.expect, fx.expect);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_plain_and_gzipped_alike() {
        let fx = sample_fixture();
        let dir = std::env::temp_dir().join(format!("yamete-both-{}", std::process::id()));
        fx.write(dir.join("a.fixture")).unwrap();
        fx.write(dir.join("b.fixture.gz")).unwrap();

        let plain = Fixture::read(dir.join("a.fixture")).unwrap();
        let gzipped = Fixture::read(dir.join("b.fixture.gz")).unwrap();
        assert_eq!(plain.accel, gzipped.accel);
        assert_eq!(plain.gyro, gzipped.gyro);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reading_a_missing_file_reports_the_path() {
        let err = Fixture::read("/nonexistent/x.fixture.gz").unwrap_err();
        assert!(err.to_string().contains("x.fixture.gz"), "{err}");
    }

    #[test]
    fn a_fixture_from_before_the_rename_still_loads() {
        let text = format!("{LEGACY_MAGIC}\n# label=old\n{HEADER}\na,0.0,0,0,-1\n");
        let fx = Fixture::from_text(&text).unwrap();
        assert_eq!(fx.label, "old");
        assert_eq!(fx.accel.len(), 1);
    }

    #[test]
    fn rejects_a_foreign_file() {
        assert!(matches!(
            Fixture::from_text("x,y,z\n1,2,3"),
            Err(ParseError::BadMagic)
        ));
        assert!(matches!(Fixture::from_text(""), Err(ParseError::BadMagic)));
    }

    #[test]
    fn rejects_malformed_rows() {
        let bad_tag = format!("{MAGIC}\n{HEADER}\nq,0.0,1,2,3\n");
        assert!(matches!(
            Fixture::from_text(&bad_tag),
            Err(ParseError::BadSensor { .. })
        ));

        let short = format!("{MAGIC}\n{HEADER}\na,0.0,1\n");
        assert!(Fixture::from_text(&short).is_err());

        let nan = format!("{MAGIC}\n{HEADER}\na,0.0,one,2,3\n");
        assert!(matches!(
            Fixture::from_text(&nan),
            Err(ParseError::BadNumber { .. })
        ));

        let long = format!("{MAGIC}\n{HEADER}\na,0.0,1,2,3,4\n");
        assert!(matches!(
            Fixture::from_text(&long),
            Err(ParseError::FieldCount { .. })
        ));
    }

    #[test]
    fn tolerates_an_unannotated_fixture() {
        let text = format!("{MAGIC}\n# label=mystery\n{HEADER}\na,0.0,0,0,-1\n");
        let fx = Fixture::from_text(&text).unwrap();
        assert_eq!(fx.expect, None);
        assert_eq!(fx.accel.len(), 1);
    }
}
