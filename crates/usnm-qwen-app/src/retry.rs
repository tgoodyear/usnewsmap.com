//! Retrying one crop when the connection to `llama-server` breaks.
//!
//! On the Intel Vulkan hosts `llama-server` now and then drops a request partway through. The
//! crop is retried on a freshly started server; if that keeps failing, the task asks the BOINC
//! client to restart it later, which resumes from the per-crop checkpoint. A task that has
//! already done that [`MAX_TEMPORARY_EXITS`] times fails as before, so a broken host cannot
//! loop forever.
use crate::{
    boinc::TEMPORARY_EXIT_FILE,
    server::{classify, transport, FailureClass},
};
use std::path::Path;
use usnm_ocr_quality::slot::write_atomic;

/// Restarts of `llama-server`, each followed by another try, before a crop gives up its slot run.
pub(crate) const MAX_CROP_RETRIES: u32 = 2;
/// Times one task may ask the client to restart it for a failing crop.
pub(crate) const MAX_TEMPORARY_EXITS: u32 = 3;
/// How long the client waits before it restarts the task.
pub(crate) const TEMPORARY_EXIT_DELAY_SECS: u32 = 60;

/// How one crop's attempts ended.
#[derive(Debug)]
pub(crate) enum CropEnd<T> {
    Done(T),
    /// A failure no retry would fix, or one that came after the time ran out.
    Failed(anyhow::Error),
    /// `llama-server` kept failing: restart the task and resume from the checkpoint.
    GiveUp(anyhow::Error),
}

/// Whether the error warrants a fresh `llama-server`: the connection broke, or the process is
/// gone. A real HTTP status, a deadline and a reply that arrived but is wrong never do.
pub(crate) fn retriable(err: &anyhow::Error, server_exited: bool) -> bool {
    match classify(err) {
        FailureClass::Transport => true,
        FailureClass::Status | FailureClass::Deadline => false,
        FailureClass::Other => server_exited,
    }
}

/// Runs `attempt`, restarting the server and trying again after a retriable failure, at most
/// [`MAX_CROP_RETRIES`] times. `is_blank` says a reading carries no text and no tokens. `stop` says the deadline passed or BOINC asked the task to exit,
/// in which case nothing is retried. `note` hears about each restart.
pub(crate) fn run_crop<S, T>(
    state: &mut S,
    mut attempt: impl FnMut(&mut S) -> anyhow::Result<T>,
    is_blank: impl Fn(&T) -> bool,
    mut exited: impl FnMut(&mut S) -> bool,
    mut restart: impl FnMut(&mut S) -> anyhow::Result<()>,
    mut stop: impl FnMut() -> bool,
    mut note: impl FnMut(&str),
) -> CropEnd<T> {
    let mut retries = 0;
    let mut result = attempt(state);
    loop {
        let err = match result {
            // A blank, 0-token reply is real when the server was healthy throughout. From a
            // server that died, or one just restarted after dying, it is the damage of the
            // crash (a reset GPU answers nothing), so it is a failed attempt, never a reading.
            Ok(v) if is_blank(&v) && (retries > 0 || exited(state)) => {
                transport("blank 0-token reply from a llama-server that had died or restarted")
            }
            Ok(v) => return CropEnd::Done(v),
            Err(e) => e,
        };
        if stop() || !retriable(&err, exited(state)) {
            return CropEnd::Failed(err);
        }
        if retries >= MAX_CROP_RETRIES {
            return CropEnd::GiveUp(err);
        }
        retries += 1;
        note(&format!(
            "restart {retries}/{MAX_CROP_RETRIES} after: {err:#}"
        ));
        // A restart that fails reports the failure again, so it spends a retry like any other.
        // The time and BOINC's wishes are checked again once it is up: loading can take minutes.
        result = match restart(state) {
            Ok(()) if stop() => return CropEnd::Failed(err),
            Ok(()) => attempt(state),
            Err(e) => Err(e),
        };
    }
}

/// Writes the file that asks the BOINC client to restart the task after a delay, as
/// `boinc_temporary_exit()` does; `attempt` is this task's count of such exits, this one included.
pub(crate) fn write_temporary_exit(work: &Path, attempt: u32, reason: &str) -> anyhow::Result<()> {
    let first_line: String = reason
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(160)
        .collect();
    write_atomic(
        &work.join(TEMPORARY_EXIT_FILE),
        format!(
            "{TEMPORARY_EXIT_DELAY_SECS}\nllama-server connection failed ({attempt}/{MAX_TEMPORARY_EXITS}): {first_line}\n"
        )
        .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{Deadline, HttpStatus};
    use std::io::{Error, ErrorKind};

    fn status() -> anyhow::Error {
        anyhow::Error::new(HttpStatus("llama-server status 500: boom".into()))
    }

    #[test]
    fn only_a_broken_connection_or_a_dead_server_is_retried() {
        // The connection broke.
        for e in [
            transport("empty or garbled HTTP status line"),
            transport("malformed HTTP response"),
            transport("completion stream ended early"),
            anyhow::Error::new(Error::from(ErrorKind::ConnectionReset)),
            anyhow::Error::new(Error::from(ErrorKind::UnexpectedEof)),
            anyhow::Error::new(Error::from(ErrorKind::BrokenPipe)),
            anyhow::Error::new(Error::from(ErrorKind::ConnectionRefused)),
            anyhow::Error::new(Error::from(ErrorKind::InvalidData)),
            transport("cut short").context("c01"),
        ] {
            assert!(retriable(&e, false), "{e:#}");
        }
        // A live server's refusal, and a deadline, never are.
        for e in [
            status(),
            status().context("c01"),
            anyhow::Error::new(Deadline),
            anyhow::Error::new(Error::from(ErrorKind::TimedOut)),
            anyhow::Error::new(Error::from(ErrorKind::WouldBlock)),
            // A timeout anywhere in the chain wins over a transport-looking cause.
            anyhow::Error::new(Error::from(ErrorKind::TimedOut)).context(transport("reset")),
        ] {
            assert!(!retriable(&e, false), "{e:#}");
            assert!(!retriable(&e, true), "{e:#}");
        }
        // A reply that arrived but is wrong is a validation failure, unless the process is gone.
        let bad = anyhow::anyhow!("chat response has no choices[0].message.content");
        assert!(!retriable(&bad, false));
        assert!(retriable(&bad, true));
    }

    #[derive(Default)]
    struct Mock {
        attempts: u32,
        restarts: u32,
        exited: bool,
    }

    fn run(
        failures: Vec<fn() -> anyhow::Error>,
        restart_fails: bool,
        stop: bool,
    ) -> (CropEnd<&'static str>, Mock, Vec<String>) {
        let mut m = Mock::default();
        let mut notes = Vec::new();
        let end = run_crop(
            &mut m,
            |m| {
                m.attempts += 1;
                match failures.get(m.attempts as usize - 1) {
                    Some(f) => Err(f()),
                    None => Ok("reading"),
                }
            },
            |_| false,
            |m| m.exited,
            |m| {
                m.restarts += 1;
                if restart_fails {
                    Err(transport("llama-server did not restart"))
                } else {
                    Ok(())
                }
            },
            || stop,
            |n| notes.push(n.to_owned()),
        );
        (end, m, notes)
    }

    fn broken() -> anyhow::Error {
        transport("malformed HTTP response")
    }

    #[test]
    fn a_crop_succeeds_on_the_first_try_without_a_restart() {
        let (end, m, notes) = run(vec![], false, false);
        assert!(matches!(end, CropEnd::Done("reading")));
        assert_eq!((m.attempts, m.restarts), (1, 0));
        assert!(notes.is_empty());
    }

    #[test]
    fn a_crop_retries_on_a_restarted_server_and_keeps_its_reading() {
        let (end, m, notes) = run(vec![broken], false, false);
        assert!(matches!(end, CropEnd::Done("reading")));
        assert_eq!((m.attempts, m.restarts), (2, 1));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("restart 1/2"), "{notes:?}");

        let (end, m, _) = run(vec![broken, broken], false, false);
        assert!(matches!(end, CropEnd::Done("reading")));
        assert_eq!((m.attempts, m.restarts), (3, 2));
    }

    #[test]
    fn a_crop_gives_up_after_two_restarts() {
        let (end, m, notes) = run(vec![broken, broken, broken, broken], false, false);
        let CropEnd::GiveUp(e) = end else {
            panic!("expected GiveUp, got {end:?}")
        };
        assert!(format!("{e:#}").contains("malformed HTTP response"));
        assert_eq!((m.attempts, m.restarts), (3, 2));
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn a_server_that_will_not_restart_spends_the_retries_too() {
        let (end, m, _) = run(vec![broken], true, false);
        assert!(matches!(end, CropEnd::GiveUp(_)));
        assert_eq!((m.attempts, m.restarts), (1, 2));
    }

    #[test]
    fn a_status_a_deadline_and_a_stop_fail_without_a_retry() {
        let (end, m, _) = run(vec![status], false, false);
        assert!(matches!(end, CropEnd::Failed(_)));
        assert_eq!((m.attempts, m.restarts), (1, 0));
        let (end, m, _) = run(vec![|| anyhow::Error::new(Deadline)], false, false);
        assert!(matches!(end, CropEnd::Failed(_)));
        assert_eq!((m.attempts, m.restarts), (1, 0));
        // The deadline passed, or BOINC asked the task to quit: no new server.
        let (end, m, _) = run(vec![broken], false, true);
        assert!(matches!(end, CropEnd::Failed(_)));
        assert_eq!((m.attempts, m.restarts), (1, 0));
    }

    #[test]
    fn a_stop_that_arrives_during_the_restart_ends_the_crop_without_another_attempt() {
        let mut m = Mock::default();
        let stop = std::cell::Cell::new(false);
        let end = run_crop(
            &mut m,
            |m| {
                m.attempts += 1;
                Err::<(), _>(broken())
            },
            |_| false,
            |m| m.exited,
            |m| {
                m.restarts += 1;
                // The load took the rest of the time, or BOINC asked the task to exit.
                stop.set(true);
                Ok(())
            },
            || stop.get(),
            |_| {},
        );
        assert!(matches!(end, CropEnd::Failed(_)));
        assert_eq!((m.attempts, m.restarts), (1, 1));
    }

    #[test]
    fn an_exited_server_turns_an_odd_error_into_a_retry() {
        let mut m = Mock {
            exited: true,
            ..Mock::default()
        };
        let end = run_crop(
            &mut m,
            |m| {
                m.attempts += 1;
                if m.attempts == 1 {
                    Err(anyhow::anyhow!("chat response JSON"))
                } else {
                    Ok(())
                }
            },
            |_| false,
            |m| m.exited,
            |m| {
                m.restarts += 1;
                m.exited = false;
                Ok(())
            },
            || false,
            |_| {},
        );
        assert!(matches!(end, CropEnd::Done(())));
        assert_eq!((m.attempts, m.restarts), (2, 1));
    }

    /// Runs a crop whose replies come from `replies` (`None` is a broken connection), where a
    /// reply of `""` is blank.
    fn run_blank(replies: Vec<Option<&'static str>>) -> (CropEnd<&'static str>, Mock) {
        let mut m = Mock::default();
        let end = run_crop(
            &mut m,
            |m| {
                m.attempts += 1;
                match replies.get(m.attempts as usize - 1) {
                    Some(Some(text)) => Ok(*text),
                    Some(None) => Err(broken()),
                    None => Ok("late reading"),
                }
            },
            |t| t.is_empty(),
            |m| m.exited,
            |m| {
                m.restarts += 1;
                Ok(())
            },
            || false,
            |_| {},
        );
        (end, m)
    }

    #[test]
    fn a_server_that_dies_mid_crop_and_answers_on_retry_keeps_the_retry() {
        let (end, m) = run_blank(vec![None, Some("text")]);
        assert!(matches!(end, CropEnd::Done("text")));
        assert_eq!((m.attempts, m.restarts), (2, 1));
    }

    #[test]
    fn a_blank_reply_after_a_restart_is_a_failed_attempt_not_a_reading() {
        // Dies, then the restarted server answers nothing: that retry fails and is retried.
        let (end, m) = run_blank(vec![None, Some(""), Some("text")]);
        assert!(matches!(end, CropEnd::Done("text")));
        assert_eq!((m.attempts, m.restarts), (3, 2));
        // Dies, and every retry is blank: the cap ends it, with no empty reading emitted.
        let (end, m) = run_blank(vec![None, Some(""), Some("")]);
        let CropEnd::GiveUp(e) = end else {
            panic!("expected GiveUp, got {end:?}")
        };
        assert!(format!("{e:#}").contains("blank 0-token reply"), "{e:#}");
        assert_eq!((m.attempts, m.restarts), (3, 2));
    }

    #[test]
    fn a_server_that_dies_twice_and_keeps_failing_exceeds_the_cap() {
        let (end, m) = run_blank(vec![None, None, None]);
        assert!(matches!(end, CropEnd::GiveUp(_)));
        assert_eq!((m.attempts, m.restarts), (3, 2));
    }

    #[test]
    fn a_blank_reply_from_a_healthy_server_stays_an_empty_reading() {
        let (end, m) = run_blank(vec![Some("")]);
        assert!(matches!(end, CropEnd::Done("")));
        assert_eq!((m.attempts, m.restarts), (1, 0));
        // A server found dead when the blank reply came is not healthy.
        let mut m = Mock {
            exited: true,
            ..Mock::default()
        };
        let end = run_crop(
            &mut m,
            |m| {
                m.attempts += 1;
                Ok("")
            },
            |t| t.is_empty(),
            |m| m.exited,
            |m| {
                m.restarts += 1;
                m.exited = false;
                Ok(())
            },
            || false,
            |_| {},
        );
        // Blank again after the restart, so the cap ends it.
        assert!(matches!(end, CropEnd::GiveUp(_)));
        assert_eq!((m.attempts, m.restarts), (3, 2));
    }

    #[test]
    fn the_temporary_exit_file_has_the_boinc_format() {
        let tmp = tempfile::tempdir().unwrap();
        write_temporary_exit(tmp.path(), 2, "c07: malformed HTTP response\nsecond line").unwrap();
        let file = std::fs::read_to_string(tmp.path().join(TEMPORARY_EXIT_FILE)).unwrap();
        let mut lines = file.lines();
        assert_eq!(lines.next(), Some("60"));
        assert_eq!(
            lines.next(),
            Some("llama-server connection failed (2/3): c07: malformed HTTP response")
        );
        assert_eq!(lines.next(), None, "not a user notice");
    }
}
