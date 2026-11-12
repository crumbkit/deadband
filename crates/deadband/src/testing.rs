//! Helpers that the unit tests of several modules share.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::daemon::lock_path;

/// A socket path in the temporary directory that no other test of this
/// process uses. Dropping it removes the socket and its lock file.
#[derive(Debug)]
pub(crate) struct TestSocket(PathBuf);

impl Deref for TestSocket {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestSocket {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(lock_path(&self.0));
    }
}

/// Returns a new [`TestSocket`].
pub(crate) fn socket_path() -> TestSocket {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    TestSocket(std::env::temp_dir().join(format!("db-{}-{n}.sock", std::process::id())))
}

/// A launchd that records each call in `calls`, answers `is_loaded` from
/// `loaded`, and fails the call whose first word is `fail`.
#[derive(Debug, Default)]
pub(crate) struct FakeLaunchd {
    /// Whether launchd has loaded the agent.
    pub(crate) loaded: std::sync::Mutex<bool>,
    /// Each call, as the words of its launchctl command line.
    pub(crate) calls: std::sync::Mutex<Vec<String>>,
    /// The launchctl word, such as `bootstrap`, whose call fails.
    pub(crate) fail: Option<&'static str>,
    /// How many `print` calls after a bootout still find the agent loaded,
    /// as launchd does while the booted-out daemon exits. A bootstrap in
    /// that time fails, as launchctl's does.
    pub(crate) lingering: std::sync::Mutex<u32>,
    /// How many `print` calls a bootout leaves lingering.
    pub(crate) linger: u32,
    /// The directory that getconf prints, or the default when `None`.
    pub(crate) temp: Option<PathBuf>,
}

impl FakeLaunchd {
    /// Returns a launchd that has loaded the agent when `loaded` is true.
    pub(crate) fn new(loaded: bool) -> Self {
        Self {
            loaded: std::sync::Mutex::new(loaded),
            ..Self::default()
        }
    }

    /// Returns a launchd as [`FakeLaunchd::new`] does whose `fail` call
    /// fails.
    pub(crate) fn failing(loaded: bool, fail: &'static str) -> Self {
        Self {
            fail: Some(fail),
            ..Self::new(loaded)
        }
    }

    /// Returns the calls that changed something: every call but `print`
    /// and `pause`.
    pub(crate) fn changes(&self) -> Vec<String> {
        let calls = self.calls.lock().expect("the calls lock");
        calls
            .iter()
            .filter(|call| !call.starts_with("print") && call.as_str() != "pause")
            .cloned()
            .collect()
    }

    fn call(&self, line: String) -> Result<(), crate::job::launchd::Failed> {
        let word = line.split(' ').next().unwrap_or_default().to_owned();
        self.calls
            .lock()
            .expect("the calls lock")
            .push(line.clone());
        if self.fail == Some(word.as_str()) {
            return Err(crate::job::launchd::Failed {
                command: format!("launchctl {line}"),
                detail: "it exited with 5: Input/output error".to_owned(),
            });
        }
        Ok(())
    }
}

impl crate::job::launchd::Launchd for FakeLaunchd {
    fn temp_dir(&self) -> Result<PathBuf, crate::job::launchd::Failed> {
        Ok(self
            .temp
            .clone()
            .unwrap_or_else(|| PathBuf::from("/var/folders/xy/abc123/T/")))
    }

    fn is_loaded(&self, target: &str) -> Result<bool, crate::job::launchd::Failed> {
        self.call(format!("print {target}"))?;
        let mut lingering = self.lingering.lock().expect("the lingering lock");
        if *lingering > 0 {
            *lingering -= 1;
            return Ok(true);
        }
        Ok(*self.loaded.lock().expect("the loaded lock"))
    }

    fn bootstrap(&self, domain: &str, plist: &Path) -> Result<(), crate::job::launchd::Failed> {
        let line = format!("bootstrap {domain} {}", plist.display());
        if *self.lingering.lock().expect("the lingering lock") > 0 {
            self.calls
                .lock()
                .expect("the calls lock")
                .push(line.clone());
            return Err(crate::job::launchd::Failed {
                command: format!("launchctl {line}"),
                detail: "it exited with 5: Input/output error".to_owned(),
            });
        }
        self.call(line)?;
        *self.loaded.lock().expect("the loaded lock") = true;
        Ok(())
    }

    fn bootout(&self, target: &str) -> Result<(), crate::job::launchd::Failed> {
        self.call(format!("bootout {target}"))?;
        *self.loaded.lock().expect("the loaded lock") = false;
        *self.lingering.lock().expect("the lingering lock") = self.linger;
        Ok(())
    }

    fn pause(&self) {
        self.calls
            .lock()
            .expect("the calls lock")
            .push("pause".to_owned());
    }
}

/// A home directory in the temporary directory that no other test of this
/// process uses. Dropping it removes the directory.
#[derive(Debug)]
pub(crate) struct TestHome(PathBuf);

impl TestHome {
    /// Returns a new, empty home.
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("db-home-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test home is writable");
        Self(path)
    }
}

impl Deref for TestHome {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
