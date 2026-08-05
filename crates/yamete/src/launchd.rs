//! Installing the daemon as a user LaunchAgent.
//!
//! A *user agent*, not a system daemon. That is not just the simpler option — it is the
//! correct one. HID access is gated by the Input Monitoring TCC permission, which is a
//! per-user consent granted through a GUI prompt; a root LaunchDaemon has no login session
//! to show that prompt in. It also means no `sudo`, no privileged helper, and no signing
//! requirements for a local install.
//!
//! The Tauri app will register the same job through `SMAppService` so it appears in
//! System Settings → Login Items. This path stays for CLI-only installs and for developing
//! against the daemon before the app exists.

use std::path::{Path, PathBuf};
use std::process::Command;

use yamete_sensor::Error;

/// launchd job label, and the plist filename.
pub const LABEL: &str = "com.chipcolate.yamete.daemon";

pub fn plist_path() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

pub fn log_dir() -> PathBuf {
    home().join("Library/Logs/yamete")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// The `gui/<uid>` domain a user agent lives in.
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn service_target() -> String {
    format!("{}/{LABEL}", domain())
}

/// Build the LaunchAgent plist.
fn plist(program: &Path, log_dir: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>

	<key>ProgramArguments</key>
	<array>
		<string>{program}</string>
		<string>run</string>
		<string>--daemon</string>
	</array>

	<key>RunAtLoad</key>
	<true/>

	<!-- A dict, not <true/>: plain KeepAlive restarts the job even after a clean exit,
	     which produces a respawn loop on machines with no sensor. -->
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>

	<key>ThrottleInterval</key>
	<integer>10</integer>

	<!-- Background would have launchd deprioritise CPU and I/O; the detector is a
	     latency-sensitive poll loop. -->
	<key>ProcessType</key>
	<string>Interactive</string>

	<key>StandardOutPath</key>
	<string>{log_dir}/yamete.log</string>
	<key>StandardErrorPath</key>
	<string>{log_dir}/yamete.err.log</string>
</dict>
</plist>
"#,
        label = LABEL,
        program = xml_escape(&program.display().to_string()),
        log_dir = xml_escape(&log_dir.display().to_string()),
    )
}

/// Escape the five XML predefined entities.
///
/// Paths can contain `&` and `<` — a home directory named "Tom & Jerry" would otherwise
/// produce a plist that launchd rejects with a singularly unhelpful error.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn launchctl(args: &[&str]) -> Result<String, Error> {
    let out = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|e| Error::Iokit(format!("could not run launchctl: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(Error::Iokit(format!(
            "launchctl {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!(" ({})", stdout.trim())
            },
        )))
    }
}

/// Whether launchd currently knows about the job.
pub fn is_loaded() -> bool {
    launchctl(&["print", &service_target()]).is_ok()
}

/// Stop the job and wait for launchd to actually forget it.
///
/// `bootout` returns before the job is fully torn down, and bootstrapping into a domain
/// that still holds the old registration fails with `Bootstrap failed: 5: Input/output
/// error` — an error message that says nothing about the real cause. Polling until the
/// job is gone is the difference between a reliable reinstall and an intermittent one.
fn bootout_and_wait() -> Result<(), Error> {
    if !is_loaded() {
        return Ok(());
    }
    let _ = launchctl(&["bootout", &service_target()]);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !is_loaded() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(Error::Iokit(format!(
        "{LABEL} is still registered five seconds after bootout; \
         try `launchctl bootout {}` by hand",
        service_target()
    )))
}

/// Where `--copy` puts the binary.
fn default_install_dir() -> PathBuf {
    home().join(".local/bin")
}

/// Replace a binary that may currently be running.
///
/// Overwriting a running Mach-O in place rewrites its inode, which invalidates the code
/// signature and makes macOS `SIGKILL` both the running process *and* every subsequent
/// exec of that path — a spectacularly confusing failure, since the binary then dies with
/// no output at all. Unlinking first gives the new file a fresh inode and sidesteps it
/// entirely; the running process keeps the old, now-unnamed one until it exits.
fn replace_binary(from: &Path, to: &Path) -> Result<(), Error> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Iokit(format!("could not create {}: {e}", parent.display())))?;
    }
    if to.exists() {
        std::fs::remove_file(to)
            .map_err(|e| Error::Iokit(format!("could not replace {}: {e}", to.display())))?;
    }
    std::fs::copy(from, to).map_err(|e| {
        Error::Iokit(format!(
            "could not copy {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::Iokit(format!("could not make {} executable: {e}", to.display())))?;
    }
    Ok(())
}

/// Install and start the agent.
pub fn install(program: Option<PathBuf>, copy: bool) -> Result<(), Error> {
    let source = match program {
        Some(p) => p,
        None => std::env::current_exe()
            .map_err(|e| Error::Iokit(format!("could not find my own path: {e}")))?,
    };
    let source = source.canonicalize().unwrap_or(source);
    if !source.exists() {
        return Err(Error::Iokit(format!("{} does not exist", source.display())));
    }

    // Stop the old job before touching anything it might be executing, and wait for it
    // to actually go away.
    bootout_and_wait()?;

    let program = if copy {
        let dest = default_install_dir().join("yamete");
        replace_binary(&source, &dest)?;
        println!("Copied {} -> {}", source.display(), dest.display());
        dest
    } else {
        // launchd gets a minimal environment and an absolute path; a binary inside a build
        // directory works but will vanish on `cargo clean`, so say so rather than leaving
        // a job that mysteriously stops working later.
        if source.components().any(|c| c.as_os_str() == "target") {
            println!(
                "note: installing from a build directory ({}).\n      \
                 `cargo clean` will break the agent, and overwriting this file while the \
                 agent is running corrupts it. Use --copy for a stable install.",
                source.display()
            );
        }
        source
    };

    let logs = log_dir();
    // launchd does not create these, and a missing directory makes the job fail to spawn
    // with no obvious explanation.
    std::fs::create_dir_all(&logs)
        .map_err(|e| Error::Iokit(format!("could not create {}: {e}", logs.display())))?;

    let path = plist_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Iokit(format!("could not create {}: {e}", parent.display())))?;
    }
    std::fs::write(&path, plist(&program, &logs))
        .map_err(|e| Error::Iokit(format!("could not write {}: {e}", path.display())))?;
    {
        use std::os::unix::fs::PermissionsExt;
        // Must not be group- or world-writable or launchd refuses to load it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| Error::Iokit(format!("could not set permissions: {e}")))?;
    }

    launchctl(&["bootstrap", &domain(), &path.to_string_lossy()])?;

    println!("Installed {LABEL}");
    println!("  plist    {}", path.display());
    println!("  program  {}", program.display());
    println!("  logs     {}/yamete.log", logs.display());
    println!("\nIt will start at login and restart if it crashes.");
    println!("Check it with `yamete status`, stop it with `yamete uninstall`.");
    println!("To update: rebuild, then `yamete install --copy` again.");
    Ok(())
}

/// Stop and remove the agent.
pub fn uninstall() -> Result<(), Error> {
    let path = plist_path();
    let mut did_something = false;

    if is_loaded() {
        bootout_and_wait()?;
        did_something = true;
    }
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| Error::Iokit(format!("could not remove {}: {e}", path.display())))?;
        did_something = true;
    }

    if did_something {
        println!("Removed {LABEL}.");
    } else {
        println!("{LABEL} was not installed.");
    }
    Ok(())
}

/// Restart the running agent, picking up a new binary or config.
pub fn restart() -> Result<(), Error> {
    if !is_loaded() {
        return Err(Error::Iokit(format!(
            "{LABEL} is not installed — run `yamete install` first"
        )));
    }
    launchctl(&["kickstart", "-k", &service_target()])?;
    println!("Restarted {LABEL}.");
    Ok(())
}

/// Report what launchd knows, for when the daemon is not behaving.
pub fn show() -> Result<(), Error> {
    let path = plist_path();
    println!("plist    {} ({})", path.display(), if path.exists() { "present" } else { "missing" });
    println!("loaded   {}", if is_loaded() { "yes" } else { "no" });

    match launchctl(&["print", &service_target()]) {
        Ok(output) => {
            // The full dump is enormous; these are the fields that actually explain a
            // job that isn't running.
            for key in ["state = ", "pid = ", "last exit code = ", "path = "] {
                if let Some(line) = output.lines().find(|l| l.trim().starts_with(key)) {
                    println!("{}", line.trim());
                }
            }
        }
        Err(_) => println!("\nlaunchd has no record of this job."),
    }

    let logs = log_dir();
    for name in ["yamete.err.log", "yamete.log"] {
        let file = logs.join(name);
        if let Ok(text) = std::fs::read_to_string(&file) {
            let tail: Vec<&str> = text.lines().rev().take(5).collect();
            if !tail.is_empty() {
                println!("\n{} (last {} lines):", file.display(), tail.len());
                for line in tail.into_iter().rev() {
                    println!("  {line}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_well_formed_and_complete() {
        let text = plist(Path::new("/usr/local/bin/yamete"), Path::new("/tmp/logs"));

        assert!(text.starts_with("<?xml"));
        assert!(text.contains("<key>Label</key>"));
        // Against the constant, not a copy of it, so a rename cannot leave this passing
        // while the plist says something else.
        assert!(text.contains(&format!("<string>{LABEL}</string>")));
        assert!(text.contains("<string>/usr/local/bin/yamete</string>"));
        assert!(text.contains("<string>--daemon</string>"));
        assert!(text.contains("/tmp/logs/yamete.log"));
        // Balanced tags — an unbalanced plist loads as garbage or not at all.
        assert_eq!(text.matches("<dict>").count(), text.matches("</dict>").count());
        assert_eq!(text.matches("<array>").count(), text.matches("</array>").count());
    }

    #[test]
    fn keepalive_only_restarts_on_failure() {
        let text = plist(Path::new("/x"), Path::new("/y"));
        // A bare <true/> KeepAlive respawns after a clean exit too, which turns "this Mac
        // has no sensor" into an infinite restart loop.
        assert!(text.contains("<key>KeepAlive</key>"));
        assert!(text.contains("<key>SuccessfulExit</key>"));
        assert!(
            !text.contains("<key>KeepAlive</key>\n\t<true/>"),
            "KeepAlive must be a dict, not a bare true"
        );
    }

    #[test]
    fn paths_with_xml_metacharacters_are_escaped() {
        let text = plist(
            Path::new("/Users/tom & jerry/bin/yamete"),
            Path::new("/Users/tom & jerry/logs"),
        );
        assert!(text.contains("tom &amp; jerry"), "unescaped ampersand would break the plist");
        assert!(!text.contains("tom & jerry"));
    }

    #[test]
    fn xml_escape_covers_the_predefined_entities() {
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("<x>"), "&lt;x&gt;");
        assert_eq!(xml_escape("\"q\""), "&quot;q&quot;");
        assert_eq!(xml_escape("it's"), "it&apos;s");
        assert_eq!(xml_escape("plain/path-1_2"), "plain/path-1_2");
    }

    #[test]
    fn the_generated_plist_parses_as_a_real_plist() {
        // The authority on whether this is valid is macOS itself, not our string builder.
        let text = plist(Path::new("/usr/local/bin/yamete"), Path::new("/tmp/logs"));
        let tmp = std::env::temp_dir().join(format!("spank-plist-{}.plist", std::process::id()));
        std::fs::write(&tmp, &text).unwrap();

        let out = Command::new("/usr/bin/plutil")
            .args(["-lint", &tmp.to_string_lossy()])
            .output()
            .expect("plutil should exist on macOS");
        assert!(
            out.status.success(),
            "plutil rejected the plist: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );

        // And the values survive a round trip through the real parser.
        let label = Command::new("/usr/bin/plutil")
            .args(["-extract", "Label", "raw", "-o", "-", &tmp.to_string_lossy()])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&label.stdout).trim(), LABEL);

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn replacing_a_binary_unlinks_rather_than_overwriting() {
        let dir = std::env::temp_dir().join(format!("spank-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("new");
        let dest = dir.join("installed");
        std::fs::write(&src, b"new contents").unwrap();
        std::fs::write(&dest, b"old contents").unwrap();

        use std::os::unix::fs::MetadataExt;
        let old_inode = std::fs::metadata(&dest).unwrap().ino();
        replace_binary(&src, &dest).unwrap();
        let new_inode = std::fs::metadata(&dest).unwrap().ino();

        assert_ne!(
            old_inode, new_inode,
            "reusing the inode invalidates the code signature of a running binary and \
             macOS then SIGKILLs every exec of that path"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"new contents");

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the installed binary must be executable");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replacing_into_a_missing_directory_creates_it() {
        let dir = std::env::temp_dir().join(format!("spank-mk-{}/a/b", std::process::id()));
        let src = std::env::temp_dir().join(format!("spank-src-{}", std::process::id()));
        std::fs::write(&src, b"x").unwrap();
        replace_binary(&src, &dir.join("yamete")).unwrap();
        assert!(dir.join("yamete").exists());
        std::fs::remove_file(&src).ok();
        std::fs::remove_dir_all(std::env::temp_dir().join(format!("spank-mk-{}", std::process::id()))).ok();
    }

    #[test]
    fn service_target_is_the_gui_domain() {
        // A user agent lives in gui/<uid>; using `system` would need root and could not
        // reach the login session the Input Monitoring grant belongs to.
        assert!(domain().starts_with("gui/"));
        assert!(service_target().ends_with(LABEL));
    }
}
