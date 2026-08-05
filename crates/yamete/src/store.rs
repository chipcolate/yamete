//! Reading and writing the config file.

use std::io;
use std::path::{Path, PathBuf};

use yamete_proto::DaemonConfig;

/// Load the config, falling back to defaults.
///
/// Returns any complaint alongside the config rather than failing: a daemon that refuses
/// to start because one field is malformed is worse than one that starts with defaults and
/// says so, especially when it is launched by launchd where nobody sees the exit code.
pub fn load(path: &Path) -> (DaemonConfig, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (DaemonConfig::default(), None),
        Err(e) => {
            return (
                DaemonConfig::default(),
                Some(format!("could not read {}: {e}", path.display())),
            )
        }
    };

    match serde_json::from_str::<DaemonConfig>(&text) {
        Ok(mut config) => {
            // Fold any older layout forward before anything else looks at it.
            config.normalize();
            match config.validate() {
                Ok(()) => (config, None),
                Err(problem) => (
                    DaemonConfig::default(),
                    Some(format!("{} is invalid: {problem}", path.display())),
                ),
            }
        }
        Err(e) => (
            DaemonConfig::default(),
            Some(format!("could not parse {}: {e}", path.display())),
        ),
    }
}

/// Write the config atomically.
///
/// Written to a sibling temp file and renamed, so a crash mid-write cannot leave a
/// truncated config that fails to parse on next boot.
pub fn save(path: &Path, config: &DaemonConfig) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let tmp = temp_sibling(path);
    std::fs::write(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spank-store-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let (config, warning) = load(Path::new("/nonexistent/spank/config.json"));
        assert_eq!(config, DaemonConfig::default());
        assert!(warning.is_none(), "first run should not warn");
    }

    #[test]
    fn round_trips() {
        let path = scratch("roundtrip");
        let mut config = DaemonConfig::default();
        config.detector.sensitivity = 0.75;
        config.enabled = false;

        save(&path, &config).unwrap();
        let (back, warning) = load(&path);
        assert!(warning.is_none());
        assert_eq!(back, config);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn malformed_json_degrades_to_defaults_with_a_warning() {
        let path = scratch("malformed");
        std::fs::write(&path, b"{ this is not json").unwrap();

        let (config, warning) = load(&path);
        assert_eq!(config, DaemonConfig::default());
        assert!(warning.unwrap().contains("could not parse"));

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_semantically_invalid_config_is_rejected_not_applied() {
        let path = scratch("invalid");
        std::fs::write(&path, br#"{"detector":{"sensitivity":42.0}}"#).unwrap();

        let (config, warning) = load(&path);
        assert_eq!(config.detector.sensitivity, 0.5, "should not adopt 42.0");
        assert!(warning.unwrap().contains("sensitivity"));

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn saving_creates_missing_directories() {
        let dir = std::env::temp_dir().join(format!("spank-mkdir-{}/a/b", std::process::id()));
        let path = dir.join("config.json");
        save(&path, &DaemonConfig::default()).unwrap();
        assert!(path.exists());
        std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("spank-mkdir-{}", std::process::id())),
        )
        .ok();
    }

    #[test]
    fn saving_leaves_no_temp_file_behind() {
        let path = scratch("atomic");
        save(&path, &DaemonConfig::default()).unwrap();

        let strays: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "left temp files: {strays:?}");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
