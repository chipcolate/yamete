//! Daemon configuration: what counts as a slap, and what happens when one lands.
//!
//! The action model is deliberately open-ended. Sounds accept any file the decoder
//! understands rather than a fixed set of bundled clips, webhooks are fully templated, and
//! the exec action hands the whole event to an arbitrary program — which is the escape
//! hatch that makes anything else possible without teaching the daemon about it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use spank_dsp::Tier;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Master switch. False keeps the daemon resident but stops it acting on anything.
    pub enabled: bool,
    /// Detector tuning, including the sensitivity slider.
    pub detector: spank_dsp::Config,
    /// Everything that can fire, in order.
    pub actions: Vec<Action>,

    /// Requested sensor reporting interval, in microseconds.
    ///
    /// 1000 gives the sensor's native ~805 Hz and the best detection quality. Reading two
    /// HID devices at that rate is the daemon's dominant cost — measured at 3.15% of a
    /// core, against 0.35% for all the signal processing combined — so this is the one
    /// knob that meaningfully changes idle power.
    ///
    /// | interval | rate   | recall | false positives | CPU   |
    /// |----------|--------|--------|-----------------|-------|
    /// | 1000 µs  | 805 Hz | 90%    | 1               | 3.2%  |
    /// | 2000 µs  | 402 Hz | 90% *  | 3               | ~1.6% |
    ///
    /// \* at half rate, recall needs sensitivity raised to about 0.6 to match.
    pub report_interval_us: u32,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            enabled: true,
            detector: spank_dsp::Config::default(),
            actions: vec![Action::default_sound()],
            report_interval_us: 1000,
        }
    }
}

impl DaemonConfig {
    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// Reject configurations that would misbehave rather than storing them.
    ///
    /// Config arrives over a socket from a UI, so it is untrusted input in the ordinary
    /// sense: not malicious, but easily wrong in ways that are miserable to debug later.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = BTreeMap::new();
        for action in &self.actions {
            if action.id.trim().is_empty() {
                return Err("every action needs a non-empty id".into());
            }
            if seen.insert(&action.id, ()).is_some() {
                return Err(format!("duplicate action id `{}`", action.id));
            }
            action.validate()?;
        }
        let s = self.detector.sensitivity;
        if !(0.0..=1.0).contains(&s) || s.is_nan() {
            return Err(format!("sensitivity must be between 0 and 1, got {s}"));
        }
        if self.detector.cooldown_s < 0.0 {
            return Err("cooldown cannot be negative".into());
        }
        if !(1000..=8000).contains(&self.report_interval_us) {
            return Err(format!(
                "report_interval_us must be between 1000 and 8000, got {}",
                self.report_interval_us
            ));
        }
        Ok(())
    }
}

/// One thing that happens when a slap is detected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Action {
    /// Stable identifier, used to target this action from the UI.
    pub id: String,
    /// Human-readable label for the UI.
    pub name: String,
    pub enabled: bool,
    /// Which severity tiers fire this action. Empty means all of them.
    pub tiers: Vec<Tier>,
    /// Skip the action below this intensity, 0.0 to 1.0.
    pub min_intensity: f32,

    /// Hold the action back by this many milliseconds after the slap.
    ///
    /// Detection is fast enough that a sound starts while the physical slap is still
    /// audible and gets partly masked by it — the response wants to land *after* the
    /// impact, not on top of it. 200 ms was chosen by ear. It lives on the action rather
    /// than on the sound because the same reasoning applies to a webhook that drives a
    /// light or a script that pops a notification, and each action can differ.
    pub delay_ms: u32,

    pub kind: ActionKind,
}

impl Default for Action {
    fn default() -> Self {
        Action {
            id: "default".into(),
            name: "Action".into(),
            enabled: true,
            tiers: Vec::new(),
            min_intensity: 0.0,
            delay_ms: 0,
            kind: ActionKind::default(),
        }
    }
}

impl Action {
    /// The out-of-the-box action: a macOS system sound, so a fresh install makes a noise
    /// without the user having to find an audio file first.
    pub fn default_sound() -> Self {
        Action {
            id: "sound".into(),
            name: "Play a sound".into(),
            kind: ActionKind::Sound {
                path: PathBuf::from("/System/Library/Sounds/Sosumi.aiff"),
                volume_db: 0.0,
                scale_with_intensity: false,
                intensity_range_pct: default_intensity_range(),
                playback_rate: 1.0,
            },
            delay_ms: 200,
            ..Default::default()
        }
    }

    /// Whether this action should run for a given detection.
    pub fn matches(&self, tier: Tier, intensity: f32) -> bool {
        self.enabled
            && self.is_runnable()
            && (self.tiers.is_empty() || self.tiers.contains(&tier))
            && intensity >= self.min_intensity
    }

    /// Whether this action has enough configuration to do anything.
    ///
    /// Distinct from validity: an action the user has switched on but not filled in yet
    /// is incomplete, not wrong, and rejecting the whole config for it would make the UI
    /// unusable — you cannot type a URL into a field that refuses to save.
    pub fn is_runnable(&self) -> bool {
        match &self.kind {
            ActionKind::Sound { path, .. } => !path.as_os_str().is_empty(),
            ActionKind::Exec { program, .. } => !program.trim().is_empty(),
            ActionKind::Webhook { url, .. } => !url.trim().is_empty(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.min_intensity) {
            return Err(format!(
                "action `{}`: min_intensity must be between 0 and 1",
                self.id
            ));
        }
        if self.delay_ms > 5_000 {
            return Err(format!(
                "action `{}`: delay_ms of {} is longer than any response to a slap should wait",
                self.id, self.delay_ms
            ));
        }
        match &self.kind {
            ActionKind::Sound {
                playback_rate,
                intensity_range_pct,
                ..
            } => {
                if !(0.0..=100.0).contains(intensity_range_pct) {
                    return Err(format!(
                        "action `{}`: intensity_range_pct must be between 0 and 100",
                        self.id
                    ));
                }
                if *playback_rate <= 0.0 {
                    return Err(format!(
                        "action `{}`: playback_rate must be positive",
                        self.id
                    ));
                }
            }
            ActionKind::Exec { .. } => {}
            ActionKind::Webhook { url, .. } => {
                // An empty URL is simply not filled in yet. A non-empty one that is not
                // HTTP is a mistake worth naming.
                let url = url.trim();
                if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://")
                {
                    return Err(format!(
                        "action `{}`: webhook url must start with http:// or https://",
                        self.id
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionKind {
    /// Play an audio file.
    Sound {
        /// A file, or a directory to pick a random file from — the equivalent of the
        /// reference apps' sound "modes", without hardcoding any of them.
        path: PathBuf,
        /// Trim in decibels, relative to the file as recorded.
        ///
        /// 0 means "play it as it is" and lets the system volume decide how loud that is,
        /// which is what people expect a sound to do.
        volume_db: f32,

        /// Vary the volume with how hard the slap was.
        ///
        /// Off by default. Scaling volume by force sounds like a good idea and mostly
        /// isn't: it makes the app quieter than the system volume you chose, for reasons
        /// that aren't visible. Severity is better expressed by binding *different sounds*
        /// per tier, which the `tiers` filter already does.
        #[serde(default)]
        scale_with_intensity: bool,

        /// How far the volume swings either side of the base, as a percentage.
        ///
        /// Symmetric on purpose: a mid-strength slap plays at exactly the configured
        /// volume, a gentle one that much quieter and a hard one that much louder. Only
        /// used when `scale_with_intensity` is on.
        #[serde(default = "default_intensity_range")]
        intensity_range_pct: f32,
        /// 1.0 is normal speed; higher is faster and higher-pitched.
        playback_rate: f32,
    },

    /// Run a program.
    ///
    /// The slap is delivered twice over: as `SPANK_*` environment variables for shell
    /// one-liners, and as a JSON object on stdin for anything that wants the full event.
    /// That second path is what makes an external script — a Bun/TS program, say — a
    /// first-class action without the daemon needing to know anything about it.
    Exec {
        program: String,
        #[serde(default)]
        args: Vec<String>,
        /// Send the slap JSON on stdin.
        #[serde(default = "yes")]
        stdin_json: bool,
    },

    /// Send an HTTP request.
    ///
    /// `url`, `headers` and `body` are all templated, so this can talk to Home Assistant,
    /// a Shortcut, an IFTTT hook or anything else without special-casing.
    Webhook {
        url: String,
        #[serde(default = "post")]
        method: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        /// Defaults to the slap event as JSON when omitted.
        #[serde(default)]
        body: Option<String>,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
}

impl Default for ActionKind {
    fn default() -> Self {
        ActionKind::Sound {
            path: PathBuf::from("/System/Library/Sounds/Sosumi.aiff"),
            volume_db: 0.0,
            scale_with_intensity: false,
            intensity_range_pct: default_intensity_range(),
            playback_rate: 1.0,
        }
    }
}

fn yes() -> bool {
    true
}

fn post() -> String {
    "POST".into()
}

fn default_timeout() -> u64 {
    5_000
}

fn default_intensity_range() -> f32 {
    40.0
}

/// Substitute `{{name}}` placeholders.
///
/// Unknown placeholders are left untouched rather than blanked: silently emptying a
/// misspelled field produces a malformed request that looks like a server problem, while
/// a literal `{{intenstiy}}` in the output points straight at the typo.
pub fn render(template: &str, vars: &BTreeMap<&str, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                match vars.get(key) {
                    Some(value) => out.push_str(value),
                    None => {
                        out.push_str("{{");
                        out.push_str(&after[..end]);
                        out.push_str("}}");
                    }
                }
                rest = &after[end + 2..];
            }
            None => {
                // Unterminated placeholder: emit the rest verbatim.
                out.push_str("{{");
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid_and_makes_a_noise() {
        let c = DaemonConfig::default();
        assert!(c.validate().is_ok());
        assert_eq!(c.actions.len(), 1);
        assert!(c.enabled);
    }

    #[test]
    fn config_round_trips_through_json() {
        let c = DaemonConfig::default();
        let text = serde_json::to_string_pretty(&c).unwrap();
        let back: DaemonConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn an_empty_object_loads_as_defaults() {
        // Config written by an older build must keep working.
        let back: DaemonConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(back, DaemonConfig::default());
    }

    #[test]
    fn tier_filter_empty_means_all() {
        let a = Action::default_sound();
        assert!(a.matches(Tier::Micro, 0.1));
        assert!(a.matches(Tier::Major, 1.0));
    }

    #[test]
    fn tier_filter_restricts_when_set() {
        let a = Action {
            tiers: vec![Tier::Major],
            ..Action::default_sound()
        };
        assert!(a.matches(Tier::Major, 0.5));
        assert!(!a.matches(Tier::Micro, 0.5));
    }

    #[test]
    fn min_intensity_gates_quiet_slaps() {
        let a = Action {
            min_intensity: 0.6,
            ..Action::default_sound()
        };
        assert!(!a.matches(Tier::Major, 0.59));
        assert!(a.matches(Tier::Major, 0.60));
    }

    #[test]
    fn disabled_actions_never_match() {
        let a = Action {
            enabled: false,
            ..Action::default_sound()
        };
        assert!(!a.matches(Tier::Major, 1.0));
    }

    #[test]
    fn validation_rejects_duplicate_ids() {
        let c = DaemonConfig {
            actions: vec![Action::default_sound(), Action::default_sound()],
            ..Default::default()
        };
        assert!(c.validate().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn an_unconfigured_action_is_valid_but_does_not_fire() {
        // Switching an action on before filling it in must not make the whole config
        // unsaveable, or the UI cannot let you type a URL at all.
        let mut config = DaemonConfig::default();
        config.actions.push(Action {
            id: "webhook".into(),
            enabled: true,
            kind: ActionKind::Webhook {
                url: String::new(),
                method: "POST".into(),
                headers: BTreeMap::new(),
                body: None,
                timeout_ms: 5000,
            },
            ..Default::default()
        });
        assert!(config.validate().is_ok(), "{:?}", config.validate());

        let webhook = config.action("webhook").unwrap();
        assert!(!webhook.is_runnable());
        assert!(
            !webhook.matches(Tier::Major, 1.0),
            "an action with nowhere to send anything must not fire"
        );

        // Once filled in, it fires.
        let mut filled = webhook.clone();
        filled.kind = ActionKind::Webhook {
            url: "https://example.com/hook".into(),
            method: "POST".into(),
            headers: BTreeMap::new(),
            body: None,
            timeout_ms: 5000,
        };
        assert!(filled.is_runnable());
        assert!(filled.matches(Tier::Major, 1.0));
    }

    #[test]
    fn a_malformed_url_is_still_rejected() {
        // Incomplete is fine; wrong is not.
        let config = DaemonConfig {
            actions: vec![Action {
                id: "hook".into(),
                kind: ActionKind::Webhook {
                    url: "ftp://example.com".into(),
                    method: "POST".into(),
                    headers: BTreeMap::new(),
                    body: None,
                    timeout_ms: 5000,
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(config.validate().unwrap_err().contains("http"));
    }

    #[test]
    fn validation_rejects_nonsense_values() {
        let mut bad_sensitivity = DaemonConfig::default();
        bad_sensitivity.detector.sensitivity = 5.0;
        assert!(bad_sensitivity.validate().unwrap_err().contains("sensitivity"));

        let bad_rate = DaemonConfig {
            actions: vec![Action {
                kind: ActionKind::Sound {
                    path: "/x.wav".into(),
                    volume_db: 0.0,
                    scale_with_intensity: false,
                    intensity_range_pct: 40.0,
                    playback_rate: 0.0,
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(bad_rate.validate().unwrap_err().contains("playback_rate"));
    }

    fn vars() -> BTreeMap<&'static str, String> {
        let mut v = BTreeMap::new();
        v.insert("intensity", "0.83".to_string());
        v.insert("tier", "major".to_string());
        v
    }

    #[test]
    fn templates_substitute_known_keys() {
        assert_eq!(render("{{tier}}", &vars()), "major");
        assert_eq!(
            render("a {{tier}} slap at {{intensity}}!", &vars()),
            "a major slap at 0.83!"
        );
        // Whitespace inside the braces is tolerated.
        assert_eq!(render("{{ tier }}", &vars()), "major");
    }

    #[test]
    fn templates_leave_unknown_keys_visible() {
        // A typo must be obvious in the output rather than becoming an empty string.
        assert_eq!(render("{{intenstiy}}", &vars()), "{{intenstiy}}");
    }

    #[test]
    fn templates_survive_malformed_input() {
        assert_eq!(render("", &vars()), "");
        assert_eq!(render("no placeholders", &vars()), "no placeholders");
        assert_eq!(render("{{unterminated", &vars()), "{{unterminated");
        assert_eq!(render("}}{{tier}}", &vars()), "}}major");
        assert_eq!(render("{{}}", &vars()), "{{}}");
    }

    #[test]
    fn templates_handle_adjacent_placeholders() {
        assert_eq!(render("{{tier}}{{tier}}", &vars()), "majormajor");
    }
}
