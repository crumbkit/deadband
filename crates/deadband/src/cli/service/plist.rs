//! The launch agent's plist, as a pure function of the binary, the home
//! directory and the per-user temporary directory.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use super::{LABEL, LOG};

/// The directories after the binary's own on the daemon's `PATH`, with
/// `~` for the home directory.
const PATH_AFTER_BINARY: [&str; 8] = [
    "~/.local/bin",
    "~/.cargo/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// Returns the path of the running binary as it was invoked: `exe` made
/// absolute against `cwd` and never resolved through a symbolic link, so a
/// package manager's link keeps working after an upgrade.
#[must_use]
pub fn binary_path(exe: &Path, cwd: Option<&Path>) -> PathBuf {
    match cwd {
        Some(cwd) if exe.is_relative() => cwd.join(exe),
        _ => exe.to_path_buf(),
    }
}

/// Returns the plist that runs `binary daemon`, with `--max-jobs` when
/// `max_jobs` is given, at login and again whenever it stops.
#[must_use]
pub fn render(
    binary: &Path,
    max_jobs: Option<NonZeroUsize>,
    home: &Path,
    temp_dir: &Path,
) -> String {
    let mut arguments = vec![binary.to_string_lossy().into_owned(), "daemon".to_owned()];
    if let Some(max_jobs) = max_jobs {
        arguments.extend(["--max-jobs".to_owned(), max_jobs.to_string()]);
    }
    let arguments: String = arguments
        .iter()
        .map(|word| format!("\t\t<string>{}</string>\n", escape(word)))
        .collect();
    let path = search_path(binary, home);
    let temp_dir = temp_dir.to_string_lossy();
    let log = home.join(LOG);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
{arguments}	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>{path}</string>
		<key>TMPDIR</key>
		<string>{temp_dir}</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        path = escape(&path),
        temp_dir = escape(&temp_dir),
        log = escape(&log.to_string_lossy()),
    )
}

/// Reports whether `text` reads as an XML property list. Install refuses
/// to replace a file that does not, such as a binary plist or a file that
/// is not a plist at all, so it never overwrites what it cannot compare.
#[must_use]
pub fn is_plist(text: &str) -> bool {
    text.contains("<plist") && text.trim_end().ends_with("</plist>")
}

/// Returns the daemon's `PATH`: the binary's directory, then
/// [`PATH_AFTER_BINARY`] with `~` replaced by `home`, each directory once.
fn search_path(binary: &Path, home: &Path) -> String {
    let home = home.to_string_lossy();
    let own = binary
        .parent()
        .map(|dir| dir.to_string_lossy().into_owned());
    let mut dirs: Vec<String> = Vec::new();
    let after = PATH_AFTER_BINARY.iter().map(|dir| {
        dir.strip_prefix('~')
            .map_or_else(|| (*dir).to_owned(), |rest| format!("{home}{rest}"))
    });
    for dir in own.into_iter().chain(after) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs.join(":")
}

/// Returns `text` with the characters that XML reserves in text replaced
/// by their entities.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
