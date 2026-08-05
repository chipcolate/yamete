//! Replays the recorded fixture corpus and holds the detector to a measured performance
//! envelope.
//!
//! The unit tests in `detector.rs` use synthetic signals, which prove the code is wired up
//! but say nothing about whether it can tell a slap from someone thumping the desk. This
//! is the test that answers that, and the bounds below are the contract:
//!
//! * **Everyday activity must be silent.** Idle, typing and trackpad use are what the
//!   machine is doing almost all the time; a single false positive there makes the whole
//!   thing unusable, so the budget is zero.
//! * **Deliberate abuse gets a small budget.** `desk-bump` is 30 s of banging the desk as
//!   hard as possible next to the laptop. One leak is tolerable.
//! * **Recall is held at 85 %.** Measured at 90 % (36/40) when these bounds were set, so
//!   there is headroom for hardware variation before this trips.
//!
//! Fixtures are recorded with `cargo run -p yamete -- record-suite`, and thresholds are
//! chosen with `cargo run -p yamete -- sweep`. The suite is skipped rather than failed
//! when `fixtures/` is empty, so a fresh clone still passes `cargo test`.

use std::path::{Path, PathBuf};

use yamete_dsp::{Config, Detector, Fixture, Frame};

/// Negatives where any detection at all is a bug.
const MUST_BE_SILENT: &[&str] = &["idle", "typing", "trackpad"];

/// Total false positives allowed across every negative fixture.
const FALSE_POSITIVE_BUDGET: usize = 1;

/// Fraction of annotated slaps that must be detected.
const MIN_RECALL: f64 = 0.85;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures")
}

fn load_all() -> Vec<(String, Fixture)> {
    let Ok(entries) = std::fs::read_dir(fixtures_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Fixture)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".fixture") || n.ends_with(".fixture.gz"))
        })
        .map(|p| {
            let fx = Fixture::read(&p)
                .unwrap_or_else(|e| panic!("could not load {}: {e}", p.display()));
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.trim_end_matches(".gz").trim_end_matches(".fixture").trim_end_matches('.'))
                .unwrap_or_default()
                .to_string();
            (name, fx)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Replay one fixture, merging both sensors by timestamp exactly as the daemon does.
fn detect(fixture: &Fixture, cfg: Config) -> Vec<yamete_dsp::Detection> {
    let mut detector = Detector::new(cfg);
    let mut hits = Vec::new();

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
                hits.push(hit);
            }
        } else {
            detector.push_gyro(*gi.next().unwrap());
        }
    }
    hits
}

struct Row {
    name: String,
    expected: usize,
    detected: usize,
}

fn score() -> Vec<Row> {
    let cfg = Config::default();
    load_all()
        .into_iter()
        .filter_map(|(name, fx)| {
            let expected = fx.expect?;
            Some(Row {
                name,
                expected,
                detected: detect(&fx, cfg).len(),
            })
        })
        .collect()
}

/// Print the confusion matrix whenever this module runs, so a failure is diagnosable from
/// the test output alone rather than requiring a separate replay.
fn report(rows: &[Row]) -> String {
    let mut out = String::from("\n  fixture           expected  detected\n");
    for r in rows {
        out.push_str(&format!(
            "  {:<18}{:>8}{:>10}{}\n",
            r.name,
            r.expected,
            r.detected,
            if r.detected == r.expected { "" } else { "  <-" },
        ));
    }
    out
}

#[test]
fn everyday_activity_is_silent() {
    let rows = score();
    if rows.is_empty() {
        eprintln!("no fixtures recorded — run `cargo run -p yamete -- record-suite`");
        return;
    }

    let offenders: Vec<&Row> = rows
        .iter()
        .filter(|r| MUST_BE_SILENT.contains(&r.name.as_str()) && r.detected > 0)
        .collect();

    assert!(
        offenders.is_empty(),
        "the detector fires during ordinary use, which makes it unusable:{}{}",
        report(&rows),
        offenders
            .iter()
            .map(|r| format!("\n  {} produced {} detection(s)", r.name, r.detected))
            .collect::<String>(),
    );
}

#[test]
fn false_positives_stay_within_budget() {
    let rows = score();
    if rows.is_empty() {
        return;
    }

    let false_positives: usize = rows
        .iter()
        .map(|r| r.detected.saturating_sub(r.expected))
        .sum();

    assert!(
        false_positives <= FALSE_POSITIVE_BUDGET,
        "{false_positives} false positive(s), budget is {FALSE_POSITIVE_BUDGET}:{}",
        report(&rows),
    );
}

#[test]
fn recall_meets_target() {
    let rows = score();
    if rows.is_empty() {
        return;
    }

    let expected: usize = rows.iter().map(|r| r.expected).sum();
    let hits: usize = rows.iter().map(|r| r.detected.min(r.expected)).sum();
    if expected == 0 {
        return;
    }

    let recall = hits as f64 / expected as f64;
    assert!(
        recall >= MIN_RECALL,
        "recall {:.0}% ({hits}/{expected}) is below the {:.0}% target:{}",
        recall * 100.0,
        MIN_RECALL * 100.0,
        report(&rows),
    );
}

/// Whatever is detected has to be usable downstream: intensity in range for volume
/// scaling, and a normalised direction vector.
#[test]
fn detections_are_well_formed() {
    let cfg = Config::default();
    for (name, fx) in load_all() {
        for hit in detect(&fx, cfg) {
            assert!(
                hit.intensity > 0.0 && hit.intensity <= 1.0,
                "{name}: intensity {} out of range",
                hit.intensity,
            );
            let len = (hit.axis[0].powi(2) + hit.axis[1].powi(2) + hit.axis[2].powi(2)).sqrt();
            assert!(
                (len - 1.0).abs() < 1e-2,
                "{name}: axis not normalised ({len})",
            );
            assert!(
                hit.gyro_ratio >= 0.0 && hit.gyro_peak >= 0.0,
                "{name}: negative gyro statistics",
            );
        }
    }
}

/// The slider must behave like a slider.
///
/// Every gate it touches moves in the same direction, so detections can only increase as
/// it rises. A violation means some threshold is being scaled the wrong way — easy to
/// introduce when adding a new detector, and confusing rather than merely suboptimal for
/// anyone using the UI.
#[test]
fn sensitivity_is_monotonic_over_real_data() {
    let corpus = load_all();
    if corpus.is_empty() {
        return;
    }

    let mut prev: Option<(f32, usize)> = None;
    let mut curve = String::new();

    for step in 0..=10 {
        let s = step as f32 / 10.0;
        let cfg = Config::default().with_sensitivity(s);
        let total: usize = corpus.iter().map(|(_, fx)| detect(fx, cfg).len()).sum();
        curve.push_str(&format!("\n  {s:.1} -> {total}"));

        if let Some((prev_s, prev_total)) = prev {
            assert!(
                total >= prev_total,
                "raising sensitivity from {prev_s:.1} to {s:.1} *reduced* detections \
                 ({prev_total} -> {total}):{curve}",
            );
        }
        prev = Some((s, total));
    }

    let (_, lowest) = (0, corpus.iter().map(|(_, fx)| {
        detect(fx, Config::default().with_sensitivity(0.0)).len()
    }).sum::<usize>());
    let highest: usize = corpus
        .iter()
        .map(|(_, fx)| detect(fx, Config::default().with_sensitivity(1.0)).len())
        .sum();
    assert!(
        highest > lowest * 2,
        "the slider barely does anything ({lowest} -> {highest} detections end to end):{curve}",
    );
}

/// Typing is the case that decides whether this is shippable, so it is held silent across
/// the entire slider range rather than only at the default.
#[test]
fn typing_never_fires_at_any_sensitivity() {
    let Some((_, typing)) = load_all().into_iter().find(|(n, _)| n == "typing") else {
        return;
    };

    for step in 0..=10 {
        let s = step as f32 / 10.0;
        let hits = detect(&typing, Config::default().with_sensitivity(s));
        assert!(
            hits.is_empty(),
            "typing produced {} detection(s) at sensitivity {s:.1}: {:?}",
            hits.len(),
            hits.iter().map(|h| (h.t, h.peak_g, h.gyro_ratio)).collect::<Vec<_>>(),
        );
    }
}
