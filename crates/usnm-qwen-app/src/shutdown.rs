//! Stopping `llama-server` without waiting on it forever.
//!
//! The server runs as the leader of its own process group. It is asked to stop with SIGTERM and
//! given [`SHUTDOWN_GRACE`] to exit; a server still there after that is killed with SIGKILL. The
//! process is always reaped, and which way it ended is logged to `stderr.txt`.
//!
//! A server can hang in its own teardown after SIGTERM, still holding its GPU memory and its
//! open connections ("cleaning up before exit..." and then nothing). A request waiting on such a
//! server never sees its reply or a closed connection, so the task stays "executing" with an idle
//! GPU until its deadline. The bounded stop applies both where the app ends the server itself
//! ([`stop`], from `Server`'s drop) and where the BOINC client asks the task to exit
//! ([`terminate_group`], from the control channel), so either way the process is gone within the
//! grace and any request on it fails.
use std::{
    collections::BTreeSet,
    process::{Child, ExitStatus},
    sync::Mutex,
    time::{Duration, Instant},
};

/// How long a server has to exit after SIGTERM before it is killed.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);
/// How long to wait for the kernel to finish off a killed process before giving up on reaping it.
const KILL_REAP_LIMIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);

/// Process groups of servers started and not yet reaped. A pid cannot be reused until its parent
/// reaps it, and every reap of a server goes through [`try_reap`] under this lock, so a group
/// listed here is still the server's own and is safe to signal.
static LIVE: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// How a server ended when it was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stopped {
    /// It had already exited (a crash, or an earlier signal) and was only reaped.
    AlreadyExited,
    /// It exited within the grace after SIGTERM.
    #[cfg_attr(not(unix), allow(dead_code))]
    Terminated,
    /// It was still running after the grace and was killed with SIGKILL.
    Killed,
    /// It was killed but had still not exited after [`KILL_REAP_LIMIT`]; it is left unreaped
    /// rather than waited on forever.
    Unreaped,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AlreadyExited => "had already exited",
            Self::Terminated => "exited after SIGTERM",
            Self::Killed => "ignored SIGTERM and was killed with SIGKILL",
            Self::Unreaped => "was killed with SIGKILL but has not exited; left unreaped",
        })
    }
}

/// Records a just-started server's process group as live.
pub(crate) fn register(child: &Child) {
    if let Ok(mut live) = LIVE.lock() {
        live.insert(child.id());
    }
}

/// `try_wait` under the registry lock: a reaped server's group is no longer listed.
pub(crate) fn try_reap(child: &mut Child) -> std::io::Result<Option<ExitStatus>> {
    let mut live = LIVE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let r = child.try_wait();
    if !matches!(r, Ok(None)) {
        live.remove(&child.id());
    }
    r
}

#[cfg(unix)]
fn signal_group(pgid: u32, sig: rustix::process::Signal) {
    if let Some(pid) = rustix::process::Pid::from_raw(pgid as i32) {
        let _ = rustix::process::kill_process_group(pid, sig);
    }
}

/// Stops `child`, the leader of its own process group, with at most `grace` for SIGTERM, and
/// reaps it. Returns how it ended and how long the stop took.
pub(crate) fn stop(child: &mut Child, grace: Duration) -> (Stopped, Duration) {
    let started = Instant::now();
    if !matches!(try_reap(child), Ok(None)) {
        return (Stopped::AlreadyExited, started.elapsed());
    }
    #[cfg(unix)]
    {
        use rustix::process::Signal;
        signal_group(child.id(), Signal::TERM);
        // A suspended task's server is stopped; SIGTERM only acts once it runs again.
        signal_group(child.id(), Signal::CONT);
    }
    #[cfg(not(unix))]
    let _ = grace;
    #[cfg(unix)]
    while started.elapsed() < grace {
        if !matches!(try_reap(child), Ok(None)) {
            return (Stopped::Terminated, started.elapsed());
        }
        std::thread::sleep(POLL);
    }
    #[cfg(unix)]
    signal_group(child.id(), rustix::process::Signal::KILL);
    let _ = child.kill();
    let killed = Instant::now();
    while killed.elapsed() < KILL_REAP_LIMIT {
        if !matches!(try_reap(child), Ok(None)) {
            return (Stopped::Killed, started.elapsed());
        }
        std::thread::sleep(POLL);
    }
    (Stopped::Unreaped, started.elapsed())
}

/// [`stop`], logged to `stderr.txt`.
pub(crate) fn stop_and_log(child: &mut Child, grace: Duration) -> Stopped {
    let pid = child.id();
    let (how, took) = stop(child, grace);
    eprintln!(
        "llama-server (pid {pid}) {how} ({:.1}s)",
        took.as_secs_f64()
    );
    how
}

/// The BOINC client asked the task to exit (quit, abort, or a lost heartbeat): SIGTERM to the
/// server's group now, and SIGKILL after `grace` if it is still there and not yet reaped. Returns
/// at once; the escalation runs on its own thread, so a request blocked on a server hung in its
/// teardown ends once the server is killed.
#[cfg(unix)]
pub(crate) fn terminate_group(pgid: u32, grace: Duration) {
    use rustix::process::Signal;
    signal_group(pgid, Signal::TERM);
    signal_group(pgid, Signal::CONT);
    let _ = std::thread::Builder::new()
        .name("llama-server-kill".into())
        .spawn(move || {
            std::thread::sleep(grace);
            let live = LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if live.contains(&pgid) {
                signal_group(pgid, Signal::KILL);
                eprintln!(
                    "llama-server (pid {pgid}) still running {}s after SIGTERM; killed with SIGKILL",
                    grace.as_secs()
                );
            }
        });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    use std::process::{Command, Stdio};

    /// A stand-in server in its own process group. `trap '' TERM` ignores SIGTERM in the shell
    /// and in the `sleep` it runs, as a server hung in its teardown does.
    fn spawn(ignore_term: bool) -> Child {
        let script = if ignore_term {
            "trap '' TERM; while :; do sleep 0.05; done"
        } else {
            "exec sleep 600"
        };
        let child = Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        register(&child);
        // Let the shell set its trap before it is signalled.
        std::thread::sleep(Duration::from_millis(200));
        child
    }

    fn live(child: &Child) -> bool {
        LIVE.lock().unwrap().contains(&child.id())
    }

    #[test]
    fn a_server_that_exits_on_sigterm_is_terminated_and_reaped() {
        let mut c = spawn(false);
        let (how, took) = stop(&mut c, Duration::from_secs(5));
        assert_eq!(how, Stopped::Terminated);
        assert!(took < Duration::from_secs(5), "{took:?}");
        assert!(!live(&c));
    }

    #[test]
    fn a_server_that_ignores_sigterm_is_killed_after_the_grace_and_reaped() {
        let mut c = spawn(true);
        let grace = Duration::from_millis(400);
        let (how, took) = stop(&mut c, grace);
        assert_eq!(how, Stopped::Killed);
        assert!(took >= grace, "{took:?}");
        assert!(took < grace + Duration::from_secs(5), "{took:?}");
        assert!(!live(&c));
        assert!(matches!(c.try_wait(), Ok(Some(_))));
    }

    #[test]
    fn a_server_that_already_exited_is_only_reaped() {
        let mut c = spawn(false);
        crash(c.id());
        std::thread::sleep(Duration::from_millis(200));
        let (how, _) = stop(&mut c, Duration::from_secs(5));
        assert_eq!(how, Stopped::AlreadyExited);
        assert!(!live(&c));
    }

    /// SIGKILL to one process, standing in for a crash.
    fn crash(pid: u32) {
        let pid = rustix::process::Pid::from_raw(pid as i32).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::KILL).unwrap();
    }

    #[test]
    fn a_client_exit_request_kills_a_server_that_ignores_sigterm() {
        let mut c = spawn(true);
        let grace = Duration::from_millis(400);
        let asked = Instant::now();
        terminate_group(c.id(), grace);
        let status = loop {
            if let Some(s) = try_reap(&mut c).unwrap() {
                break s;
            }
            assert!(
                asked.elapsed() < grace + Duration::from_secs(5),
                "the server outlived its grace"
            );
            std::thread::sleep(POLL);
        };
        assert!(asked.elapsed() >= grace);
        assert_eq!(status.signal(), Some(9), "{status:?}");
        assert!(!live(&c));
    }

    #[test]
    fn a_client_exit_request_leaves_a_reaped_group_alone() {
        let mut c = spawn(false);
        terminate_group(c.id(), Duration::from_millis(300));
        let (how, _) = stop(&mut c, Duration::from_secs(5));
        assert!(matches!(how, Stopped::Terminated | Stopped::AlreadyExited));
        assert!(!live(&c));
        // The escalation finds the group gone and sends nothing.
        std::thread::sleep(Duration::from_millis(500));
    }
    /// The hang as seen on a host: a server that ignores SIGTERM holds a request open, the way
    /// one stuck in its teardown does. A client exit request must end that request within the
    /// grace, not at the task's deadline.
    const HANGING_STUB: &str = r#"
import http.server, signal, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def do_POST(self):
        while True:
            time.sleep(1)
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
"#;

    #[test]
    fn a_request_held_open_by_a_server_that_ignores_sigterm_ends_within_the_grace() {
        use std::io::BufRead as _;
        let Ok(mut c) = Command::new("python3")
            .args(["-c", HANGING_STUB])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
        else {
            eprintln!("python3 not available; skipping");
            return;
        };
        register(&c);
        let mut line = String::new();
        std::io::BufReader::new(c.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port: u16 = line.trim().parse().unwrap();
        let request = std::thread::spawn(move || {
            let t = Instant::now();
            let r = crate::server::http_request(
                port,
                "POST",
                "/v1/chat/completions",
                Some(b"{}"),
                Duration::from_secs(3600),
            );
            (r.is_err(), t.elapsed())
        });
        std::thread::sleep(Duration::from_millis(300));
        let grace = Duration::from_millis(500);
        terminate_group(c.id(), grace);
        let (failed, took) = request.join().unwrap();
        assert!(failed, "the request should fail once the server is killed");
        assert!(took < grace + Duration::from_secs(5), "{took:?}");
        let (how, _) = stop(&mut c, Duration::from_secs(1));
        assert_eq!(how, Stopped::AlreadyExited);
    }
}
