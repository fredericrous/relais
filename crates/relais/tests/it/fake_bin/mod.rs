//! Fake executables for the integration tests that do not pay macOS's
//! first-exec check per test.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long warming a shared script may take before the test gives up on
/// it: well past the 15–27 s first-exec check measured under load.
const WARM_DEADLINE: Duration = Duration::from_secs(300);

/// Puts the executable `script` at `dest` as a hard link to one shared copy
/// under the test target directory, already run once.
///
/// macOS checks a new executable on its first exec (`syspolicyd`,
/// XProtect). Measured on 2026-10-14 under load: 15–27 s for each freshly
/// written fake `claude`, against 22 ms for its second exec, and past the
/// 30 s probe relais gives `claude --version`, so a world writing its own
/// fake made the suite fail at random. A hard link shares the checked
/// inode: only the shared copy pays, once, here, outside any probe. The
/// script must exit on `--version` with stdin closed; it runs with
/// `FAKE_CLAUDE_DIR` set to the shared directory. A test must never write
/// through `dest`: it is the shared copy. Where no link can be made, a
/// fresh copy is written, as before.
pub fn install_script(dest: &Path, script: &str) {
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::os::unix::fs::PermissionsExt;
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fake-bins");
    std::fs::create_dir_all(&dir).expect("fake-bins dir");
    let mut hasher = DefaultHasher::new();
    script.hash(&mut hasher);
    let shared = dir.join(format!("{:016x}", hasher.finish()));
    publish_and_warm_once(&shared, &dir, script);
    if std::fs::hard_link(&shared, dest).is_err() {
        std::fs::write(dest, script).expect("write script");
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
    }
}

/// Writes `shared` if no test process has yet, then runs it once per test
/// process, all under one lock, so the tests of a process never race on
/// the file and a test that links it later never pays the first-exec check
/// itself.
fn publish_and_warm_once(shared: &Path, dir: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    static WARMED: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    let mut warmed = WARMED
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if warmed.iter().any(|known| known == shared) {
        return;
    }
    if !shared.exists() {
        // Written aside and renamed, so another test process never runs a
        // half-written copy; the pid keeps processes' aside files apart.
        let aside = dir.join(format!(
            "{}.{}.tmp",
            shared.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        std::fs::write(&aside, script).expect("write shared script");
        std::fs::set_permissions(&aside, std::fs::Permissions::from_mode(0o755))
            .expect("chmod shared script");
        std::fs::rename(&aside, shared).expect("publish shared script");
    }
    let mut child = Command::new(shared)
        .arg("--version")
        .env("FAKE_CLAUDE_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("warm shared script");
    let deadline = Instant::now() + WARM_DEADLINE;
    while child.try_wait().expect("wait for warm-up").is_none() {
        assert!(
            Instant::now() < deadline,
            "warming {} took over {WARM_DEADLINE:?}",
            shared.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    warmed.push(shared.to_path_buf());
}
