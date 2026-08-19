//! `yamete record-suite` — walk through recording the whole fixture corpus.
//!
//! The detector is only as good as the negatives it was tested against. Positives are
//! easy to come by; what actually determines whether this is usable is whether typing,
//! trackpad clicks and setting a mug down next to the laptop stay silent. This walks
//! through recording all of it in one sitting, with consistent labels and annotations.

use std::io::Write;
use std::path::Path;

use crate::error::Error;

use crate::calibrate::{self, Cue, Recording};

/// One recording in the corpus.
struct Take {
    label: &'static str,
    secs: f64,
    /// How many slaps this take should contain — 0 for every negative.
    expect: usize,
    prompt: &'static str,
}

/// The corpus. Negatives first: it is more useful to discover the detector fires on
/// typing before spending effort on how well it catches slaps.
const TAKES: &[Take] = &[
    Take {
        label: "idle",
        secs: 30.0,
        expect: 0,
        prompt: "Don't touch the laptop at all.",
    },
    Take {
        label: "typing",
        secs: 30.0,
        expect: 0,
        prompt: "Type normally on the built-in keyboard, including some hard keypresses.",
    },
    Take {
        label: "trackpad",
        secs: 30.0,
        expect: 0,
        prompt: "Click and force-click the trackpad, scroll, drag.",
    },
    Take {
        label: "desk-bump",
        secs: 30.0,
        expect: 0,
        prompt: "Bump the DESK (not the laptop): set a mug down, knock the surface nearby.",
    },
    Take {
        label: "lid-and-ports",
        secs: 30.0,
        expect: 0,
        prompt: "Adjust the screen angle, plug/unplug a cable, nudge the laptop's position.",
    },
    Take {
        label: "spank-gentle",
        secs: 30.0,
        expect: 10,
        prompt: "Spank the lid GENTLY 10 times, about one every 3 seconds.",
    },
    Take {
        label: "spank-medium",
        secs: 30.0,
        expect: 10,
        prompt: "Spank the lid at NORMAL strength 10 times, about one every 3 seconds.",
    },
    Take {
        label: "spank-hard",
        secs: 30.0,
        expect: 10,
        prompt: "Spank the lid HARD 10 times, about one every 3 seconds.",
    },
    Take {
        label: "spank-side",
        secs: 30.0,
        expect: 10,
        prompt: "Spank the SIDE / palm rest 10 times, about one every 3 seconds.",
    },
];

fn wait_for_enter(msg: &str) -> bool {
    print!("{msg}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    !line.trim().eq_ignore_ascii_case("s")
}

pub fn run(dir: &Path, only: Option<&str>) -> Result<(), Error> {
    let takes: Vec<&Take> = TAKES
        .iter()
        .filter(|t| only.map_or(true, |o| t.label.contains(o)))
        .collect();

    if takes.is_empty() {
        return Err(Error::other(format!(
            "no take matches `{}`. Available: {}",
            only.unwrap_or(""),
            TAKES.iter().map(|t| t.label).collect::<Vec<_>>().join(", "),
        )));
    }

    println!(
        "Recording {} take(s) into {}.\n\
         Takes 1-5 are the quiet ones (no slapping). Slapping starts at take 6,\n\
         and the recorder beats out the rhythm for you.\n",
        takes.len(),
        dir.display(),
    );

    let mut recorded = 0;
    for (i, take) in takes.iter().enumerate() {
        println!("─────────────────────────────────────────────");
        println!(
            "[{}/{}] {}  ({:.0}s)",
            i + 1,
            takes.len(),
            take.label,
            take.secs
        );
        println!("  {}", take.prompt);
        if take.expect > 0 {
            println!(
                "  {} slaps — the recorder will tell you exactly when to hit it.",
                take.expect
            );
        } else {
            println!("  No slaps in this take.");
        }
        if !wait_for_enter("  Press Enter when ready (or 's' to skip): ") {
            println!("  skipped\n");
            continue;
        }

        let out = dir.join(format!("{}.fixture.gz", take.label));
        calibrate::run(
            &Recording {
                label: take.label,
                prompt: take.prompt,
                secs: take.secs,
                expect: Some(take.expect),
                cue: (take.expect > 0).then_some(Cue { count: take.expect }),
                countdown: 3,
            },
            &out,
        )?;
        recorded += 1;
        println!();
    }

    println!("─────────────────────────────────────────────");
    println!("Recorded {recorded} take(s).");
    println!(
        "If you slapped a different number of times than asked, just edit the\n\
         `# expect=` line at the top of that fixture — the count is the assertion."
    );
    println!("\nNow check them with:");
    println!(
        "  cargo run -p yamete -- replay {}/*.fixture.gz -v",
        dir.display()
    );
    println!(
        "  cargo run -p yamete -- analyze {}/idle.fixture.gz",
        dir.display()
    );
    Ok(())
}
