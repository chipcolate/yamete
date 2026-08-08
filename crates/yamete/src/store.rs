//! Reading and writing the config file.

use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use yamete_proto::DaemonConfig;

/// Load the config, falling back to defaults.
///
/// Returns any complaint alongside the config rather than failing: a daemon that refuses
/// to start because one field is malformed is worse than one that starts with defaults and
/// says so, especially when it is launched by launchd where nobody sees the exit code.
///
/// A file that cannot be parsed or fails validation is **quarantined** (renamed aside)
/// so the next successful save cannot silently overwrite a hand-edited broken config
/// without leaving a breadcrumb, and so a restart does not keep re-warning on the same
/// unreadable file forever.
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
                Err(problem) => {
                    let msg = format!("{} is invalid: {problem}", path.display());
                    (DaemonConfig::default(), Some(quarantine(path, &msg)))
                }
            }
        }
        Err(e) => {
            let msg = format!("could not parse {}: {e}", path.display());
            (DaemonConfig::default(), Some(quarantine(path, &msg)))
        }
    }
}

/// Move a bad config out of the way. Returns a warning that includes the new path when
/// the rename succeeds, or the original complaint when it does not.
fn quarantine(path: &Path, reason: &str) -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = path.with_file_name(format!(
        "{}.broken-{stamp}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config.json")
    ));
    match std::fs::rename(path, &dest) {
        Ok(()) => format!(
            "{reason} — moved aside to {} so defaults can start cleanly",
            dest.display()
        ),
        Err(e) => format!("{reason} (could not quarantine: {e})"),
    }
}

/// Write the config atomically with owner-only permissions.
///
/// Written to a sibling temp file and renamed, so a crash mid-write cannot leave a
/// truncated config that fails to parse on next boot. Mode is `0600` because the file can
/// hold webhook secrets and exec commands.
pub fn save(path: &Path, config: &DaemonConfig) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        // Best-effort: a world-readable state directory is how secrets leak.
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let tmp = temp_sibling(path);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        use std::io::Write;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    // rename preserves the temp file's mode on APFS/HFS+ for a replace-in-place; still
    // reassert on the final path so an older 0644 config cannot linger after an upgrade.
    std::fs::rename(&tmp, path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
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
        let dir = std::env::temp_dir().join(format!("yamete-store-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let (config, warning) = load(Path::new("/nonexistent/yamete/config.json"));
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
    fn save_is_owner_only() {
        let path = scratch("perms");
        save(&path, &DaemonConfig::default()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config must not be world-readable");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn malformed_json_is_quarantined() {
        let path = scratch("malformed");
        std::fs::write(&path, b"{ this is not json").unwrap();

        let (config, warning) = load(&path);
        assert_eq!(config, DaemonConfig::default());
        let warning = warning.unwrap();
        assert!(warning.contains("could not parse"));
        assert!(warning.contains("moved aside") || warning.contains("quarantine"));
        assert!(!path.exists(), "bad file should not stay at the live path");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_semantically_invalid_config_is_rejected_not_applied() {
        let path = scratch("invalid");
        std::fs::write(&path, br#"{"detector":{"sensitivity":42.0}}"#).unwrap();

        let (config, warning) = load(&path);
        assert_eq!(config.detector.sensitivity, 0.5, "should not adopt 42.0");
        let warning = warning.unwrap();
        assert!(warning.contains("sensitivity"));
        assert!(!path.exists(), "invalid file should be quarantined");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn saving_creates_missing_directories() {
        let dir = std::env::temp_dir().join(format!("yamete-mkdir-{}/a/b", std::process::id()));
        let path = dir.join("config.json");
        save(&path, &DaemonConfig::default()).unwrap();
        assert!(path.exists());
        std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("yamete-mkdir-{}", std::process::id())),
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
