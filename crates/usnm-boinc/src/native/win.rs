//! The Windows side of the native worker: a job object for each unit, in place of the process
//! group, the parent-death signal and the rlimits the other platforms use (`super`).
//!
//! The unit starts suspended, is assigned to the job, and only then runs, so nothing it starts
//! is ever outside the job. The job kills everything in it when its last handle closes, and
//! the worker holds the only handle: if the BOINC client kills the worker, the kernel ends the
//! unit and the engine. The job also gives each process in it a memory limit, a CPU time limit
//! and a process count, which stand in for the container's `--memory`, the engine's CPU
//! rlimit and `--pids-limit`.
//!
//! Every use of the Win32 API is in this file.
#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    os::windows::{
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
        process::CommandExt as _,
    },
    process::{Child, Command},
    sync::{Arc, Mutex},
};
use usnm_ocr_quality::slot::ChildAction;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::{
            Debug::{SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX},
            ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
            JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
            TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOB_OBJECT_LIMIT_PROCESS_TIME,
        },
        Threading::{
            OpenProcess, OpenThread, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED,
            PROCESS_SUSPEND_RESUME, THREAD_SUSPEND_RESUME,
        },
    },
};

#[link(name = "ntdll")]
extern "system" {
    fn NtSuspendProcess(process: HANDLE) -> i32;
    fn NtResumeProcess(process: HANDLE) -> i32;
}

/// The unit, the engine, and one spare.
const ACTIVE_PROCESSES: u32 = 8;
/// Job time limits count 100 ns ticks.
const TICKS_PER_SECOND: i64 = 10_000_000;
/// What `GetExitCodeProcess` reports for a process that hasn't ended.
#[cfg(test)]
const STILL_ACTIVE: u32 = 259;

/// What each process of a job may use.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    /// Committed memory, per process.
    pub memory_bytes: u64,
    /// User-mode CPU time, per process: the deadline of one read, as the engine's CPU rlimit
    /// is on Linux. A suspended task gains none, and the job counts only user-mode time.
    pub cpu_secs: u32,
}

impl Limits {
    /// What the container's `job.toml` gives the engine, for a job that reads in `timeout_secs`.
    pub(super) fn engine(timeout_secs: u32) -> Self {
        Self {
            memory_bytes: super::MEMORY_BYTES,
            cpu_secs: timeout_secs,
        }
    }
}

/// A job object, closed (and its processes killed) when this is dropped.
pub(super) struct Job(OwnedHandle);

impl Job {
    pub(super) fn new(limits: &Limits) -> std::io::Result<Self> {
        // SAFETY: no attributes and no name; the result is checked and owned from here on.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is a handle this call just made and nothing else owns.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        // SAFETY: an all-zero value is a valid, empty set of limits.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        info.BasicLimitInformation.PerProcessUserTimeLimit =
            i64::from(limits.cpu_secs).saturating_mul(TICKS_PER_SECOND);
        info.BasicLimitInformation.ActiveProcessLimit = ACTIVE_PROCESSES;
        info.ProcessMemoryLimit = usize::try_from(limits.memory_bytes).unwrap_or(usize::MAX);
        // SAFETY: `info` is the structure `JobObjectExtendedLimitInformation` takes, and the
        // size is its size.
        let ok = unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast::<c_void>(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }

    fn assign(&self, process: &Child) -> std::io::Result<()> {
        // SAFETY: both handles are open for the length of the call.
        if unsafe { AssignProcessToJobObject(self.raw(), process.as_raw_handle()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn terminate(&self) {
        // SAFETY: the handle is open.
        let _ = unsafe { TerminateJobObject(self.raw(), 1) };
    }

    /// The ids of the processes in the job now.
    pub(super) fn processes(&self) -> Vec<u32> {
        // JOBOBJECT_BASIC_PROCESS_ID_LIST: two u32 counts, then the ids as pointer-sized words.
        // Word 0 holds the counts, and words 1.. the ids, with room for 64.
        let mut words = vec![0u64; 1 + 64];
        // SAFETY: the buffer is writable for the length passed, which is a whole number of
        // words and aligned for the structure.
        let ok = unsafe {
            QueryInformationJobObject(
                self.raw(),
                JobObjectBasicProcessIdList,
                words.as_mut_ptr().cast::<c_void>(),
                (words.len() * std::mem::size_of::<u64>()) as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Vec::new();
        }
        let listed = (words[0] >> 32) as usize;
        words[1..]
            .iter()
            .take(listed)
            .filter_map(|id| u32::try_from(*id).ok())
            .collect()
    }

    fn signal(&self, action: ChildAction) {
        match action {
            ChildAction::Terminate => self.terminate(),
            ChildAction::Suspend | ChildAction::Resume => {
                for pid in self.processes() {
                    suspend_or_resume(pid, action == ChildAction::Suspend);
                }
            }
        }
    }
}

// SAFETY: a job handle is a kernel object handle, valid on every thread.
unsafe impl Send for Job {}
// SAFETY: as above, and every call on it is thread-safe.
unsafe impl Sync for Job {}

/// Suspends or resumes every thread of a process, as the client does for a task.
pub(super) fn suspend_or_resume(pid: u32, suspend: bool) {
    // SAFETY: the handle is closed before returning, and is only passed to the two calls.
    unsafe {
        let handle = OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid);
        if handle.is_null() {
            return;
        }
        if suspend {
            NtSuspendProcess(handle);
        } else {
            NtResumeProcess(handle);
        }
        CloseHandle(handle);
    }
}

/// Whether a process with this id is still running.
#[cfg(test)]
pub(super) fn is_alive(pid: u32) -> bool {
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: the handle is closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

/// The unit's job: the supervisor asks it to stop or suspend what it started, by the id the
/// control channel knows it by. One unit runs at a time, so one job is kept, and starting
/// another replaces it, which kills whatever the previous unit left behind.
static ACTIVE: Mutex<Option<(u32, Arc<Job>)>> = Mutex::new(None);

fn active(pid: u32) -> Option<Arc<Job>> {
    let guard = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .filter(|(id, _)| *id == pid)
        .map(|(_, job)| Arc::clone(job))
}

/// Starts `command` suspended, in a new job with `limits`, with no console window, and lets it
/// run. It is the job's first process, so everything it starts is in the job too.
pub(super) fn spawn_in_job(command: &mut Command, limits: &Limits) -> std::io::Result<Child> {
    let job = Arc::new(Job::new(limits)?);
    let mut child = command
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
        .spawn()?;
    let started = job.assign(&child).and_then(|()| resume(&child));
    if let Err(error) = started {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some((child.id(), job));
    Ok(child)
}

/// Lets a child created suspended run. `std` doesn't give the main thread's handle, so it is
/// found by the process id: a process that was created suspended has only that thread.
fn resume(child: &Child) -> std::io::Result<()> {
    let pid = child.id();
    // SAFETY: the snapshot and thread handles are closed before returning, and the entry is
    // sized as `Thread32First` requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = 0;
        let mut more = Thread32First(snapshot, &mut entry);
        while more != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if !thread.is_null() {
                    if ResumeThread(thread) != u32::MAX {
                        resumed += 1;
                    }
                    CloseHandle(thread);
                }
            }
            more = Thread32Next(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        if resumed == 0 {
            return Err(std::io::Error::other(
                "no thread of the new process could resume",
            ));
        }
    }
    Ok(())
}

/// Ends the unit and everything it started.
pub(super) fn terminate_group(child: &mut Child) {
    match active(child.id()) {
        Some(job) => job.terminate(),
        None => {
            let _ = child.kill();
        }
    }
}

/// What the client's stop, suspend and resume do to the unit with this id.
pub(super) fn signal_group(unit: u32, action: ChildAction) {
    if let Some(job) = active(unit) {
        job.signal(action);
    }
}

/// The ids of the processes of the unit's job.
#[cfg(test)]
pub(super) fn group_processes(unit: u32) -> Vec<u32> {
    active(unit).map(|job| job.processes()).unwrap_or_default()
}

/// A crash in this process or one it starts shows no dialog, which nobody is there to close
/// and which would hold the task. Every process started afterwards inherits the setting.
pub(super) fn silence_crash_dialogs() {
    // SAFETY: only sets this process's error mode.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
}
