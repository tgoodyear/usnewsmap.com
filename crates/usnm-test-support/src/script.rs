use std::io;
use std::path::Path;

/// Writes `contents` to `path` as an executable script (mode 0755) that can be run at once.
///
/// Running a file while any process still has it open for writing fails with `ETXTBSY` ("Text file
/// busy"). A process that writes the file itself holds it open for writing for a moment, and a
/// child that another thread forks in that moment inherits the handle until it execs. So a test
/// that writes a script and runs it straight away fails now and then when other tests in the same
/// binary spawn processes (about 2.6% of writes in a stress test with four threads forking).
///
/// Here a short-lived `install` child writes `path`, so this process never holds it open for
/// writing: nothing it forks can inherit the handle, and the child has exited, and its handle is
/// closed, before this returns. The same stress test saw no failure in 4,800 writes.
pub fn write_executable(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    write_executable_with_mode(path, contents, 0o755)
}

/// [`write_executable`] with another file mode. Where there is no file mode (not Unix) this is
/// just a write.
pub fn write_executable_with_mode(
    path: impl AsRef<Path>,
    contents: impl AsRef<[u8]>,
    mode: u32,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::write(path.as_ref(), contents.as_ref(), mode)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::write(path, contents)
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::process::Command;

    pub(super) fn write(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
        // Staged in a private directory of its own (random name, mode 0700), not at a name
        // another user could guess in the shared temporary directory and point at a file of
        // ours. It is also outside the destination's directory, so tests that list that never
        // see it. The staged file is never run, so holding it open for writing here is harmless.
        let staging = tempfile::tempdir()?;
        let staged = staging.path().join("script");
        std::fs::write(&staged, contents)?;
        let status = Command::new("install")
            .arg("-m")
            .arg(format!("{mode:o}"))
            .arg("--")
            .arg(&staged)
            .arg(path)
            .status();
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(io::Error::other(format!(
                "install could not write {}: {status}",
                path.display()
            ))),
            Err(e) => Err(e),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const BUSY: i32 = 26;

    #[test]
    fn writes_the_contents_with_the_mode_and_replaces_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        write_executable(&path, "#!/bin/sh\nprintf one\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\nprintf one\n"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(Command::new(&path).output().unwrap().stdout, b"one");
        write_executable_with_mode(&path, "#!/bin/sh\nprintf two\n", 0o700).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(Command::new(&path).output().unwrap().stdout, b"two");
        // Only the script is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_executable(dir.path().join("missing/tool"), "#!/bin/sh\n").is_err());
    }

    /// Writes scripts and runs each at once while other threads keep forking, as the other tests
    /// in a binary do. Writing with `std::fs::write` and a chmod fails this about 2.6% of the
    /// time (the chance of passing is then below one in a million).
    #[test]
    fn a_script_can_be_run_at_once_while_other_threads_fork() {
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let forkers: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = Command::new("sh").args(["-c", "true"]).status();
                    }
                })
            })
            .collect();
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let mut busy = 0;
                    for i in 0..150 {
                        let script = dir.path().join(format!("script-{w}-{i}"));
                        write_executable(&script, "#!/bin/sh\nprintf ok\n").unwrap();
                        match Command::new(&script).output() {
                            Ok(out) => assert_eq!(out.stdout, b"ok"),
                            Err(e) if e.raw_os_error() == Some(BUSY) => busy += 1,
                            Err(e) => panic!("{e}"),
                        }
                    }
                    busy
                })
            })
            .collect();
        let busy: i32 = writers.into_iter().map(|w| w.join().unwrap()).sum();
        stop.store(true, Ordering::Relaxed);
        for f in forkers {
            f.join().unwrap();
        }
        assert_eq!(busy, 0, "{busy} of 600 scripts were still busy when run");
    }
}
