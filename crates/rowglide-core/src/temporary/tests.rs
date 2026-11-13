//! Filesystem and real-process regressions for temporary cache ownership.

use super::{Directory, sweep};
use std::{
    error::Error,
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Wait for asynchronous cleanup without allowing a stalled cleaner to hang the test.
fn removed(path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while path.exists() {
        if Instant::now() > deadline {
            return Err(format!("Cache directory was not removed: {}", path.display()).into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// Recovery preserves active data, removes interrupted creation and ignores unrelated names.
#[test]
fn preserves_live_entries_and_cleans_interrupted_creation() -> Result<(), Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    let active = Directory::create_in(root.path())?;
    let active_path = active.path().to_path_buf();
    fs::write(active.path().join("result.arrow"), "retained rows")?;
    let interrupted = root.path().join("entry-0000000000000000");
    fs::create_dir(&interrupted)?;
    fs::write(interrupted.join("partial"), "interrupted creation")?;
    let unrelated = root.path().join("user-data");
    fs::create_dir(&unrelated)?;
    sweep(root.path())?;
    assert!(active.path().join("result.arrow").exists());
    assert!(!interrupted.exists());
    assert!(unrelated.exists());
    drop(active);
    removed(&active_path)?;
    Ok(())
}

/// Concurrent directory creation and recovery serialize before a lease becomes visible.
#[test]
fn concurrent_creation_and_recovery_preserve_owners() -> Result<(), Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    let mut workers = Vec::new();
    for _ in 0..4 {
        let path = root.path().to_path_buf();
        workers.push(thread::spawn(move || -> std::io::Result<Directory> {
            let directory = Directory::create_in(&path)?;
            sweep(&path)?;
            assert!(directory.path().exists());
            Ok(directory)
        }));
    }
    for worker in workers {
        let directory = worker.join().map_err(|_| "Cache worker panicked")??;
        let path = directory.path().to_path_buf();
        drop(directory);
        removed(&path)?;
    }
    Ok(())
}

/// Ensure the child is reaped even when an assertion or an I/O operation fails.
struct ChildGuard {
    /// Test process holding a real OS file lease until killed or timed out.
    child: Child,
}

impl Drop for ChildGuard {
    /// Stop and reap the fixture without allowing cleanup errors to panic the test runner.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Child-process fixture; ordinary test runs do nothing without its private root variable.
#[test]
fn lease_process() -> Result<(), Box<dyn Error>> {
    let Some(root) = std::env::var_os("ROWGLIDE_TEST_LEASE_ROOT") else {
        return Ok(());
    };
    let root = Path::new(&root);
    let directory = Directory::create_in(root)?;
    fs::write(directory.path().join("result.arrow"), "partial result")?;
    fs::create_dir(directory.path().join("spill"))?;
    fs::write(directory.path().join("spill/data"), "engine scratch")?;
    fs::write(
        root.join("ready.tmp"),
        directory.path().to_string_lossy().as_bytes(),
    )?;
    fs::rename(root.join("ready.tmp"), root.join("ready"))?;
    // Bound fixture lifetime even if the parent crashes before reaping it.
    thread::sleep(Duration::from_secs(20));
    drop(directory);
    Ok(())
}

/// A separate live process is protected, but forcefully terminating it releases its lease.
#[test]
fn reclaims_crashed_process_without_touching_live_process() -> Result<(), Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    let mut child = ChildGuard {
        child: Command::new(std::env::current_exe()?)
            .args(["--exact", "temporary::tests::lease_process", "--nocapture"])
            .env("ROWGLIDE_TEST_LEASE_ROOT", root.path())
            .stdout(Stdio::null())
            .spawn()?,
    };
    let ready = root.path().join("ready");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        if child.child.try_wait()?.is_some() || Instant::now() > deadline {
            return Err("Child did not acquire a cache lease".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let path = std::path::PathBuf::from(fs::read_to_string(ready)?);
    sweep(root.path())?;
    assert!(path.join("result.arrow").exists());
    assert!(path.join("spill/data").exists());
    child.child.kill()?;
    child.child.wait()?;
    sweep(root.path())?;
    assert!(!path.exists());
    Ok(())
}

/// Recovery never traverses a symlink entry or treats a symlink lease as deletion permission.
#[cfg(unix)]
#[test]
fn ignores_symbolic_links() -> Result<(), Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    let external = tempfile::tempdir()?;
    let sentinel = external.path().join("keep");
    fs::write(&sentinel, "user data")?;
    let linked_entry = root.path().join("entry-1111111111111111");
    std::os::unix::fs::symlink(external.path(), &linked_entry)?;
    let linked_lease = root.path().join("entry-2222222222222222");
    fs::create_dir(&linked_lease)?;
    std::os::unix::fs::symlink(&sentinel, linked_lease.join("lease"))?;
    sweep(root.path())?;
    assert!(sentinel.exists());
    assert!(linked_entry.symlink_metadata()?.file_type().is_symlink());
    assert!(linked_lease.exists());
    Ok(())
}
