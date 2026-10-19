//! Cross-platform BOINC slot execution for `usnm_quality` (#522, #533).
//!
//! [`compute`] is the whole computation as a library call: the page's
//! features, the readers' pairwise metrics, the hat, the consensus,
//! findability and confusion pairs, with timings. [`run_slot`] wraps it in a
//! BOINC slot on Linux, macOS or Windows: resolves both direct `<copy_file/>`
//! files and `<soft_link>` XML pointers, collects diagnostics, runs the job's
//! checks and computation, writes `result.json`, `confusions.json`, `diag.txt`
//! and `fraction_done`, and finishes by writing `boinc_finish_called` (`"0\n"`).
use crate::{
    features,
    findability::Places,
    report::{self, PageFeatures, PageInputs, ReaderInput},
};
use anyhow::{ensure, Context};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use usnm_alto::{Page, ParseError, ParseOptions};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{
        ConfusionPair, Engine, Host, Job, Outcome, PageResult, ReaderSpec, ResultManifest, Texts,
        Timings, MAX_ALTO, MAX_BUNDLE_ALTO, MAX_BUNDLE_SCAN, MAX_BUNDLE_TEXTS, MAX_DIAG, MAX_ERROR,
        MAX_JOB_JSON, MAX_PLACES, MAX_SCAN, MAX_TEXTS,
    },
};

pub const JOB_FILE: &str = "quality.json";
pub const PAGE_FILE: &str = "page.jpg";
pub const ALTO_FILE: &str = "loc.xml";
pub const TEXTS_FILE: &str = "texts.json";
pub const PLACES_FILE: &str = "places.txt";
pub const RESULT_FILE: &str = "result.json";
pub const CONFUSIONS_FILE: &str = "confusions.json";
pub const DIAG_FILE: &str = "diag.txt";
pub const FRACTION_FILE: &str = "fraction_done";
pub const FINISH_FILE: &str = "boinc_finish_called";
pub const MMAP_FILE: &str = "boinc_mmap_file";
pub const CHECKPOINT_FILE: &str = "quality_checkpoint.json";

/// Embedded BOINC `API_VERSION_` marker (`lib/boinc_api.cpp` convention) so
/// `update_versions` (`strings | grep 'API_VERSION_[0-9]+\.[0-9]+\.[0-9]+$'`)
/// and the BOINC client recognize the 6.0+ file-backed `boinc_mmap_file` protocol.
pub const BOINC_API_VERSION: &str = "8.3.0";
#[used]
pub static BOINC_API_VERSION_MARKER: [u8; 18] = *b"API_VERSION_8.3.0\0";

/// Upstream BOINC `MSG_CHANNEL_SIZE` (`lib/app_ipc.h`).
pub const MSG_CHANNEL_SIZE: usize = 1024;
/// Upstream BOINC `SHARED_MEM` total size: 8 consecutive `MSG_CHANNEL` blocks.
pub const SHARED_MEM_SIZE: usize = MSG_CHANNEL_SIZE * 8;
/// Offset of `SHARED_MEM::process_control_request` (client -> app).
pub const PROCESS_CONTROL_REQUEST_OFFSET: usize = 0;
/// Offset of `SHARED_MEM::process_control_reply` (app -> client).
pub const PROCESS_CONTROL_REPLY_OFFSET: usize = MSG_CHANNEL_SIZE;
/// Offset of `SHARED_MEM::heartbeat` (client -> app).
pub const HEARTBEAT_OFFSET: usize = MSG_CHANNEL_SIZE * 4;
/// Offset of `SHARED_MEM::app_status` (app -> client).
pub const APP_STATUS_OFFSET: usize = MSG_CHANNEL_SIZE * 7;
/// Default BOINC heartbeat loss timeout when heartbeats are active.
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);

/// Lifecycle action applied to a registered child process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildAction {
    Suspend,
    Resume,
    Terminate,
}

pub type ChildHook = Arc<dyn Fn(u32, ChildAction) + Send + Sync>;

/// Extracts the Windows named shared-memory segment from `<comm_obj_name>` in `init_data.xml`.
pub fn parse_comm_obj_name(init_xml: &str) -> Option<String> {
    let start = init_xml.find("<comm_obj_name>")? + "<comm_obj_name>".len();
    let end = init_xml[start..].find("</comm_obj_name>")?;
    let name = init_xml[start..start + end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win_shmem {
    use super::{
        ChildAction, HEARTBEAT_OFFSET, MSG_CHANNEL_SIZE, PROCESS_CONTROL_REQUEST_OFFSET,
        SHARED_MEM_SIZE,
    };
    use anyhow::ensure;
    use std::{
        ffi::CString,
        os::raw::c_void,
        sync::atomic::{fence, Ordering},
    };

    type Handle = *mut c_void;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    const PAGE_READWRITE: u32 = 0x04;
    const FILE_MAP_ALL_ACCESS: u32 = 0x000F_001F;
    const PROCESS_TERMINATE: u32 = 0x0001;
    const PROCESS_SUSPEND_RESUME: u32 = 0x0800;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileMappingA(
            hFile: Handle,
            lpFileMappingAttributes: *mut c_void,
            flProtect: u32,
            dwMaximumSizeHigh: u32,
            dwMaximumSizeLow: u32,
            lpName: *const i8,
        ) -> Handle;
        fn OpenFileMappingA(dwDesiredAccess: u32, bInheritHandle: i32, lpName: *const i8)
            -> Handle;
        fn MapViewOfFile(
            hFileMappingObject: Handle,
            dwDesiredAccess: u32,
            dwFileOffsetHigh: u32,
            dwFileOffsetLow: u32,
            dwNumberOfBytesToMap: usize,
        ) -> *mut u8;
        fn UnmapViewOfFile(lpBaseAddress: *const c_void) -> i32;
        fn CloseHandle(hObject: Handle) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> Handle;
        fn TerminateProcess(hProcess: Handle, uExitCode: u32) -> i32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtSuspendProcess(process_handle: Handle) -> i32;
        fn NtResumeProcess(process_handle: Handle) -> i32;
    }

    /// Win32 named file mapping backing BOINC's `SHARED_MEM` (`<comm_obj_name>`
    /// in `init_data.xml`, `lib/shmem.cpp`).
    pub struct WinShmem {
        handle: Handle,
        base: *mut u8,
    }

    // SAFETY: `WinShmem` owns an OS file-mapping handle and a process-wide
    // virtual view of `SHARED_MEM_SIZE` bytes; accesses go through volatile byte
    // reads/writes with `SeqCst` fences matching BOINC's `MSG_CHANNEL` protocol.
    unsafe impl Send for WinShmem {}
    unsafe impl Sync for WinShmem {}

    impl WinShmem {
        /// Attaches to an existing BOINC named shared-memory segment (`Global\<seg>`
        /// then `<seg>`, matching `attach_shmem` in BOINC `lib/shmem.cpp`), or
        /// creates `<seg>` when `create_if_missing` is true.
        pub fn attach_or_create(seg_name: &str, create_if_missing: bool) -> Option<Self> {
            let global_name = CString::new(format!("Global\\{seg_name}")).ok()?;
            let local_name = CString::new(seg_name).ok()?;
            // SAFETY: `global_name` and `local_name` are valid NUL-terminated strings.
            unsafe {
                let mut h = OpenFileMappingA(FILE_MAP_ALL_ACCESS, 0, global_name.as_ptr());
                if h.is_null() {
                    h = OpenFileMappingA(FILE_MAP_ALL_ACCESS, 0, local_name.as_ptr());
                }
                if h.is_null() && create_if_missing {
                    h = CreateFileMappingA(
                        INVALID_HANDLE_VALUE,
                        std::ptr::null_mut(),
                        PAGE_READWRITE,
                        0,
                        SHARED_MEM_SIZE as u32,
                        local_name.as_ptr(),
                    );
                }
                if h.is_null() {
                    return None;
                }
                let base = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, SHARED_MEM_SIZE);
                if base.is_null() {
                    let _ = CloseHandle(h);
                    return None;
                }
                Some(Self { handle: h, base })
            }
        }

        /// Reads and clears a BOINC `MSG_CHANNEL` at `offset` if byte 0 is non-zero.
        pub fn take_channel(&self, offset: usize) -> Option<String> {
            if offset + MSG_CHANNEL_SIZE > SHARED_MEM_SIZE {
                return None;
            }
            // SAFETY: `self.base` maps `SHARED_MEM_SIZE` bytes and `offset + MSG_CHANNEL_SIZE <= SHARED_MEM_SIZE`.
            unsafe {
                let chan = self.base.add(offset);
                if std::ptr::read_volatile(chan) == 0 {
                    return None;
                }
                fence(Ordering::Acquire);
                let mut buf = [0u8; MSG_CHANNEL_SIZE - 1];
                for (i, slot) in buf.iter_mut().enumerate() {
                    let b = std::ptr::read_volatile(chan.add(1 + i));
                    if b == 0 {
                        let msg = String::from_utf8_lossy(&buf[..i]).into_owned();
                        std::ptr::write_volatile(chan, 0);
                        fence(Ordering::SeqCst);
                        return Some(msg);
                    }
                    *slot = b;
                }
                let msg = String::from_utf8_lossy(&buf).into_owned();
                std::ptr::write_volatile(chan, 0);
                fence(Ordering::SeqCst);
                Some(msg)
            }
        }

        /// Writes a BOINC `MSG_CHANNEL` frame (`byte[0] = 1`, `byte[1..]` = NUL-terminated XML).
        pub fn write_channel(&self, offset: usize, xml: &str) -> anyhow::Result<()> {
            ensure!(
                offset + MSG_CHANNEL_SIZE <= SHARED_MEM_SIZE,
                "invalid MSG_CHANNEL offset {offset}"
            );
            let xml_bytes = xml.as_bytes();
            ensure!(
                xml_bytes.len() < MSG_CHANNEL_SIZE - 1,
                "MSG_CHANNEL payload too large"
            );
            // SAFETY: `self.base` maps `SHARED_MEM_SIZE` bytes and `offset + MSG_CHANNEL_SIZE <= SHARED_MEM_SIZE`.
            unsafe {
                let chan = self.base.add(offset);
                for i in 0..(MSG_CHANNEL_SIZE - 1) {
                    let b = xml_bytes.get(i).copied().unwrap_or(0);
                    std::ptr::write_volatile(chan.add(1 + i), b);
                }
                fence(Ordering::SeqCst);
                std::ptr::write_volatile(chan, 1);
                fence(Ordering::SeqCst);
            }
            Ok(())
        }

        pub fn poll_process_control(&self) -> Option<String> {
            self.take_channel(PROCESS_CONTROL_REQUEST_OFFSET)
        }

        pub fn poll_heartbeat(&self) -> Option<String> {
            self.take_channel(HEARTBEAT_OFFSET)
        }
    }

    impl Drop for WinShmem {
        fn drop(&mut self) {
            // SAFETY: `self.base` and `self.handle` were obtained from `MapViewOfFile`
            // and `OpenFileMappingA`/`CreateFileMappingA` in `attach_or_create`.
            unsafe {
                if !self.base.is_null() {
                    let _ = UnmapViewOfFile(self.base.cast::<c_void>());
                }
                if !self.handle.is_null() {
                    let _ = CloseHandle(self.handle);
                }
            }
        }
    }

    pub fn signal_process(pid: u32, action: ChildAction) {
        // SAFETY: Win32 process handle lifecycle is scoped to this call.
        unsafe {
            match action {
                ChildAction::Suspend => {
                    let h = OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid);
                    if !h.is_null() {
                        let _ = NtSuspendProcess(h);
                        let _ = CloseHandle(h);
                    }
                }
                ChildAction::Resume => {
                    let h = OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid);
                    if !h.is_null() {
                        let _ = NtResumeProcess(h);
                        let _ = CloseHandle(h);
                    }
                }
                ChildAction::Terminate => {
                    let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
                    if !h.is_null() {
                        let _ = TerminateProcess(h, 1);
                        let _ = CloseHandle(h);
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
pub use win_shmem::WinShmem;

fn default_signal_child(pgid: u32, action: ChildAction) {
    #[cfg(unix)]
    {
        let sig = match action {
            ChildAction::Suspend => "-STOP",
            ChildAction::Resume => "-CONT",
            ChildAction::Terminate => "-TERM",
        };
        let group = format!("-{pgid}");
        let ok = std::process::Command::new("kill")
            .args([sig, &group])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            let _ = std::process::Command::new("kill")
                .args([sig, &pgid.to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    #[cfg(windows)]
    {
        win_shmem::signal_process(pgid, action);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pgid, action);
    }
}

struct BoincControlState {
    suspended: bool,
    quit_requested: bool,
    abort_requested: bool,
    heartbeat_lost: bool,
    heartbeat_enabled: bool,
    last_heartbeat: Instant,
    child_pgid: Option<u32>,
    child_hook: Option<ChildHook>,
}

impl BoincControlState {
    fn exit_reason(&self) -> Option<&'static str> {
        if self.abort_requested {
            Some("abort")
        } else if self.quit_requested {
            Some("quit")
        } else if self.heartbeat_lost {
            Some("heartbeat_lost")
        } else {
            None
        }
    }

    fn dispatch_child(&self, action: ChildAction) {
        if let Some(pgid) = self.child_pgid {
            if let Some(ref hook) = self.child_hook {
                hook(pgid, action);
            } else {
                default_signal_child(pgid, action);
            }
        }
    }
}

struct BoincControlInner {
    state: Mutex<BoincControlState>,
    cvar: Condvar,
}

/// Monitors BOINC's shared-memory process-control and heartbeat channels in
/// `boinc_mmap_file` (`lib/app_ipc.h`).
#[derive(Clone)]
pub struct BoincControl {
    inner: Arc<BoincControlInner>,
}

pub type BoincRuntime = BoincControl;

impl BoincControl {
    /// Returns an inert controller for container workers where `docker_wrapper`
    /// already owns `boinc_mmap_file`, `<comm_obj_name>`, and `boinc_finish_called`.
    pub fn noop() -> Self {
        Self {
            inner: Arc::new(BoincControlInner {
                state: Mutex::new(BoincControlState {
                    suspended: false,
                    quit_requested: false,
                    abort_requested: false,
                    heartbeat_lost: false,
                    heartbeat_enabled: false,
                    last_heartbeat: Instant::now(),
                    child_pgid: None,
                    child_hook: None,
                }),
                cvar: Condvar::new(),
            }),
        }
    }

    /// Starts monitoring `boinc_mmap_file` in `work` with the standard 30-second
    /// heartbeat timeout.
    pub fn start(work: &Path) -> Self {
        Self::start_with_heartbeat_timeout(work, HEARTBEAT_TIMEOUT)
    }

    /// Starts monitoring `boinc_mmap_file` (and on Windows `<comm_obj_name>`
    /// named shared memory from `init_data.xml`) in `work` with a custom
    /// heartbeat timeout.
    pub fn start_with_heartbeat_timeout(work: &Path, heartbeat_timeout: Duration) -> Self {
        std::hint::black_box(&BOINC_API_VERSION_MARKER);
        let init_path = resolve_slot_file(work, "init_data.xml");
        let has_init_data = init_path.is_file();
        let init_xml = std::fs::read_to_string(&init_path).ok();
        let no_heartbeat_check = init_xml
            .as_deref()
            .is_some_and(|xml| xml.contains("<no_heartbeat_check"));
        #[cfg(windows)]
        let win_shm = init_xml
            .as_deref()
            .and_then(parse_comm_obj_name)
            .and_then(|seg| WinShmem::attach_or_create(&seg, true));
        let mmap_path = resolve_slot_file(work, MMAP_FILE);
        // Mirror BOINC `boinc_init()` (`create_shmem_mmap`): when running in a
        // BOINC slot (`init_data.xml` present) and `boinc_mmap_file` has not yet
        // been initialized to `SHARED_MEM_SIZE`, create/pad it so the 6.0+
        // file-backed shared-memory channel is always active on startup.
        if has_init_data
            && !mmap_path
                .metadata()
                .is_ok_and(|m| m.is_file() && m.len() >= SHARED_MEM_SIZE as u64)
        {
            if let Ok(f) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&mmap_path)
            {
                let _ = f.set_len(SHARED_MEM_SIZE as u64);
                let _ = f.sync_all();
            }
        }
        let inner = Arc::new(BoincControlInner {
            state: Mutex::new(BoincControlState {
                suspended: false,
                quit_requested: false,
                abort_requested: false,
                heartbeat_lost: false,
                heartbeat_enabled: false,
                last_heartbeat: Instant::now(),
                child_pgid: None,
                child_hook: None,
            }),
            cvar: Condvar::new(),
        });

        let has_mmap = mmap_path
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() >= SHARED_MEM_SIZE as u64);
        #[cfg(windows)]
        let should_poll = has_mmap || win_shm.is_some();
        #[cfg(not(windows))]
        let should_poll = has_mmap;

        if should_poll {
            #[cfg(windows)]
            if let Some(ref shm) = win_shm {
                poll_win_shmem_once(shm, &inner);
            }
            if has_mmap {
                poll_mmap_once(&mmap_path, &inner, heartbeat_timeout, no_heartbeat_check);
            } else {
                check_heartbeat_timeout(&inner, heartbeat_timeout, no_heartbeat_check);
            }
            let weak = Arc::downgrade(&inner);
            let _ = std::thread::Builder::new()
                .name("boinc-mmap-control".into())
                .spawn(move || {
                    while let Some(shared) = weak.upgrade() {
                        #[cfg(windows)]
                        if let Some(ref shm) = win_shm {
                            poll_win_shmem_once(shm, &shared);
                        }
                        poll_mmap_once(&mmap_path, &shared, heartbeat_timeout, no_heartbeat_check);
                        check_heartbeat_timeout(&shared, heartbeat_timeout, no_heartbeat_check);
                        if shared
                            .state
                            .lock()
                            .ok()
                            .and_then(|s| s.exit_reason())
                            .is_some()
                        {
                            break;
                        }
                        drop(shared);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                });
        }

        Self { inner }
    }

    /// Registers a custom callback for suspending, resuming, or terminating a child process group.
    pub fn set_child_hook(&self, hook: Option<ChildHook>) {
        if let Ok(mut s) = self.inner.state.lock() {
            s.child_hook = hook;
        }
    }

    /// Registers (or clears) a child process/group ID for `<suspend/>`, `<resume/>`,
    /// `<quit/>`, `<abort/>`, and heartbeat loss signaling.
    pub fn set_child_pgid(&self, pgid: Option<u32>) {
        if let Ok(mut s) = self.inner.state.lock() {
            s.child_pgid = pgid;
            if pgid.is_some() {
                if s.exit_reason().is_some() {
                    s.dispatch_child(ChildAction::Terminate);
                } else if s.suspended {
                    s.dispatch_child(ChildAction::Suspend);
                }
            }
        }
    }

    /// Returns whether the task is currently suspended by `<suspend/>`.
    pub fn is_suspended(&self) -> bool {
        self.inner.state.lock().is_ok_and(|s| s.suspended)
    }

    /// Returns `Some("quit")`, `Some("abort")`, or `Some("heartbeat_lost")` when
    /// BOINC requested immediate termination or heartbeat was lost.
    pub fn should_exit(&self) -> Option<&'static str> {
        self.inner.state.lock().ok().and_then(|s| s.exit_reason())
    }

    /// Blocks while suspended unless an exit condition (`quit`, `abort`, `heartbeat_lost`)
    /// is triggered.
    pub fn wait_if_suspended_or_should_exit(&self) -> ControlFlow<&'static str, ()> {
        let Ok(mut s) = self.inner.state.lock() else {
            return ControlFlow::Continue(());
        };
        loop {
            if let Some(reason) = s.exit_reason() {
                return ControlFlow::Break(reason);
            }
            if !s.suspended {
                return ControlFlow::Continue(());
            }
            let Ok((guard, _)) = self.inner.cvar.wait_timeout(s, Duration::from_millis(50)) else {
                return ControlFlow::Continue(());
            };
            s = guard;
        }
    }
}

fn apply_process_control_message(inner: &BoincControlInner, msg: &str) {
    if let Ok(mut s) = inner.state.lock() {
        if msg.contains("<quit") {
            s.quit_requested = true;
            s.suspended = false;
            s.dispatch_child(ChildAction::Terminate);
            inner.cvar.notify_all();
        } else if msg.contains("<abort") {
            s.abort_requested = true;
            s.suspended = false;
            s.dispatch_child(ChildAction::Terminate);
            inner.cvar.notify_all();
        } else if msg.contains("<suspend") {
            s.suspended = true;
            s.dispatch_child(ChildAction::Suspend);
            inner.cvar.notify_all();
        } else if msg.contains("<resume") {
            s.suspended = false;
            s.last_heartbeat = Instant::now();
            s.dispatch_child(ChildAction::Resume);
            inner.cvar.notify_all();
        }
    }
}

fn apply_heartbeat_message(inner: &BoincControlInner) {
    if let Ok(mut s) = inner.state.lock() {
        s.last_heartbeat = Instant::now();
        s.heartbeat_enabled = true;
    }
}

fn check_heartbeat_timeout(
    inner: &BoincControlInner,
    heartbeat_timeout: Duration,
    no_heartbeat_check: bool,
) {
    if let Ok(mut s) = inner.state.lock() {
        if !no_heartbeat_check
            && s.heartbeat_enabled
            && !s.suspended
            && !s.heartbeat_lost
            && s.last_heartbeat.elapsed() > heartbeat_timeout
        {
            s.heartbeat_lost = true;
            s.dispatch_child(ChildAction::Terminate);
            inner.cvar.notify_all();
        }
    }
}

#[cfg(windows)]
fn poll_win_shmem_once(shm: &WinShmem, inner: &BoincControlInner) {
    if let Some(msg) = shm.poll_process_control() {
        apply_process_control_message(inner, &msg);
    }
    if shm.poll_heartbeat().is_some() {
        apply_heartbeat_message(inner);
    }
}

fn poll_mmap_once(
    mmap_path: &Path,
    inner: &BoincControlInner,
    heartbeat_timeout: Duration,
    no_heartbeat_check: bool,
) {
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(mmap_path)
    else {
        return;
    };
    let Ok(meta) = file.metadata() else {
        return;
    };
    if !meta.is_file() || meta.len() < SHARED_MEM_SIZE as u64 {
        return;
    }

    let mut req_buf = [0u8; MSG_CHANNEL_SIZE];
    if file
        .seek(SeekFrom::Start(PROCESS_CONTROL_REQUEST_OFFSET as u64))
        .is_ok()
        && file.read_exact(&mut req_buf).is_ok()
        && req_buf[0] != 0
    {
        let end = req_buf[1..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(MSG_CHANNEL_SIZE - 1);
        let msg = String::from_utf8_lossy(&req_buf[1..1 + end]).into_owned();
        if file
            .seek(SeekFrom::Start(PROCESS_CONTROL_REQUEST_OFFSET as u64))
            .is_ok()
        {
            let _ = file.write_all(&[0u8]);
            let _ = file.sync_data();
        }
        apply_process_control_message(inner, &msg);
    }

    let mut hb_buf = [0u8; MSG_CHANNEL_SIZE];
    if file.seek(SeekFrom::Start(HEARTBEAT_OFFSET as u64)).is_ok()
        && file.read_exact(&mut hb_buf).is_ok()
        && hb_buf[0] != 0
    {
        if file.seek(SeekFrom::Start(HEARTBEAT_OFFSET as u64)).is_ok() {
            let _ = file.write_all(&[0u8]);
            let _ = file.sync_data();
        }
        apply_heartbeat_message(inner);
    }

    check_heartbeat_timeout(inner, heartbeat_timeout, no_heartbeat_check);
}

/// Writes a BOINC `MSG_CHANNEL` frame (`byte[0] = 1`, `byte[1..]` = NUL-terminated XML)
/// at `offset` in `boinc_mmap_file` (creating/extending it to 8192 bytes if needed).
pub fn write_mmap_channel(mmap_path: &Path, offset: usize, xml: &str) -> anyhow::Result<()> {
    ensure!(
        offset + MSG_CHANNEL_SIZE <= SHARED_MEM_SIZE,
        "invalid MSG_CHANNEL offset {offset}"
    );
    let xml_bytes = xml.as_bytes();
    ensure!(
        xml_bytes.len() < MSG_CHANNEL_SIZE - 1,
        "MSG_CHANNEL payload too large"
    );
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(mmap_path)?;
    if f.metadata()?.len() < SHARED_MEM_SIZE as u64 {
        f.set_len(SHARED_MEM_SIZE as u64)?;
    }
    let mut payload = vec![0u8; MSG_CHANNEL_SIZE - 1];
    payload[..xml_bytes.len()].copy_from_slice(xml_bytes);
    f.seek(SeekFrom::Start((offset + 1) as u64))?;
    f.write_all(&payload)?;
    f.seek(SeekFrom::Start(offset as u64))?;
    f.write_all(&[1u8])?;
    f.sync_data()?;
    Ok(())
}

/// The output files in output template order.
pub fn outputs() -> [&'static str; 3] {
    [RESULT_FILE, CONFUSIONS_FILE, DIAG_FILE]
}

/// The input files in input template order, with the job last.
pub fn inputs() -> [&'static str; 5] {
    [PAGE_FILE, ALTO_FILE, TEXTS_FILE, PLACES_FILE, JOB_FILE]
}

/// Resolves a logical file name in a BOINC slot directory, supporting both
/// `<copy_file/>` direct files and BOINC `<soft_link>...</soft_link>` XML
/// pseudo-symlinks.
pub fn resolve_slot_file(work: &Path, name: &str) -> PathBuf {
    let p = work.join(name);
    if let Ok(meta) = std::fs::metadata(&p) {
        if meta.is_file() && meta.len() <= 4096 {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let trimmed = text.trim();
                if let Some(rest) = trimmed.strip_prefix("<soft_link>") {
                    if let Some(target) = rest.strip_suffix("</soft_link>") {
                        let target_path = Path::new(target.trim());
                        return if target_path.is_absolute() {
                            target_path.to_path_buf()
                        } else {
                            work.join(target_path)
                        };
                    }
                }
            }
        }
    }
    p
}

/// Bounded regular-file read.
pub fn read_bounded(path: &Path, max: usize) -> anyhow::Result<Vec<u8>> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(meta.is_file(), "{} is not a regular file", path.display());
    ensure!(
        meta.len() <= max as u64,
        "{} is over size limit",
        path.display()
    );
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::new();
    f.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "{} is over size limit", path.display());
    Ok(bytes)
}

/// Cross-platform atomic file write (write to temporary sibling, sync, close, rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("output");
    let tmp = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o644));
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Normalizes early NDNP ALTO 1.x files (`http://schema.ccs-gmbh.com/ALTO`)
/// to the LoC ALTO v2 namespace (`http://www.loc.gov/standards/alto/ns-v2#`)
/// so `usnm_alto::check` and `usnm_alto::parse` accept 2008-era NDNP batches.
pub fn normalize_alto_ns(xml: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    const CCS_NS: &[u8] = b"http://schema.ccs-gmbh.com/ALTO";
    const V2_NS: &[u8] = b"http://www.loc.gov/standards/alto/ns-v2#";
    let head_len = xml.len().min(4096);
    if let Some(pos) = xml[..head_len]
        .windows(CCS_NS.len())
        .position(|w| w == CCS_NS)
    {
        let mut out = Vec::with_capacity(xml.len() + V2_NS.len() - CCS_NS.len());
        out.extend_from_slice(&xml[..pos]);
        out.extend_from_slice(V2_NS);
        out.extend_from_slice(&xml[pos + CCS_NS.len()..]);
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(xml)
    }
}

/// Reads an ALTO into a page in the image's pixels. LoC's ALTO is in
/// 1/1200 inch; it is read at 1200 dpi (its own units) and then scaled to
/// the image by the page's width and height, as the gold set's `crops.py`
/// scales it, so the units never matter.
pub fn parse_alto(xml: &[u8], width: f64, height: f64) -> anyhow::Result<Page> {
    ensure!(width > 0.0 && height > 0.0, "page has no size");
    let xml = normalize_alto_ns(xml);
    let opts = ParseOptions::default();
    let mut page = match usnm_alto::parse(&xml, &opts) {
        Ok(p) => p,
        Err(ParseError::NeedsDpi(_)) => usnm_alto::parse(
            &xml,
            &ParseOptions {
                dpi: Some(1200.0),
                ..opts
            },
        )
        .context("ALTO")?,
        Err(e) => return Err(anyhow::anyhow!("ALTO: {e}")),
    };
    ensure!(
        page.width > 0.0 && page.height > 0.0,
        "ALTO page has no size"
    );
    page.rescale(width, height);
    Ok(page)
}

/// The diagnostics text, kept under [`MAX_DIAG`]: each section is cut to its
/// own cap and the whole to the limit, with a note where it was cut.
pub struct Diag(String);
impl Default for Diag {
    fn default() -> Self {
        Self::new()
    }
}
impl Diag {
    pub fn new() -> Self {
        Self(String::new())
    }
    pub fn section(&mut self, title: &str, body: &str, cap: usize) {
        let _ = writeln!(self.0, "== {title}");
        let body = body.trim_end();
        if body.len() > cap {
            let mut end = cap;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            let _ = writeln!(
                self.0,
                "{}\n[cut at {cap} of {} bytes]",
                &body[..end],
                body.len()
            );
        } else if !body.is_empty() {
            let _ = writeln!(self.0, "{body}");
        }
    }
    pub fn finish(mut self) -> Vec<u8> {
        let limit = MAX_DIAG - 64;
        if self.0.len() > limit {
            let mut end = limit;
            while !self.0.is_char_boundary(end) {
                end -= 1;
            }
            self.0.truncate(end);
            self.0.push_str("\n[diagnostics cut at the size limit]\n");
        }
        self.0.into_bytes()
    }
}

/// The last `n` bytes of `s`, from a character boundary.
pub fn tail(s: &str, n: usize) -> &str {
    let mut i = s.len().saturating_sub(n);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(xml[start..end].trim())
}

const INIT_FIELDS: [&str; 10] = [
    "major_version",
    "minor_version",
    "release",
    "app_version",
    "plan_class",
    "gpu_type",
    "gpu_device_num",
    "gpu_opencl_dev_index",
    "ncpus",
    "rsc_memory_bound",
];

pub fn boinc_info(work: &Path, host: &mut Host, diag: &mut Diag) {
    let init_path = resolve_slot_file(work, "init_data.xml");
    let Ok(xml) = std::fs::read_to_string(init_path) else {
        diag.section("boinc", "no init_data.xml in the slot", 256);
        return;
    };
    let mut body = String::new();
    for f in INIT_FIELDS {
        if let Some(v) = tag(&xml, f) {
            if v.len() <= 32
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                let _ = writeln!(body, "{f}: {v}");
            }
        }
    }
    if let (Some(a), Some(b), Some(c)) = (
        tag(&xml, "major_version"),
        tag(&xml, "minor_version"),
        tag(&xml, "release"),
    ) {
        host.boinc_client = Some(format!("{a}.{b}.{c}")).filter(|s| s.len() <= 32);
    }
    host.plan_class = tag(&xml, "plan_class")
        .filter(|s| s.len() <= 32)
        .map(str::to_owned);
    host.ncpus = tag(&xml, "ncpus").and_then(|s| s.parse().ok());
    diag.section("boinc (init_data.xml, selected fields)", &body, 2048);
}

pub fn runtime_info(host: &mut Host, diag: &mut Diag) {
    let mut body = String::new();
    if let Ok(env) = std::fs::read_to_string("/run/.containerenv") {
        for l in env.lines() {
            if let Some(v) = l.strip_prefix("engine=") {
                let v = v.trim_matches('"');
                host.runtime = Some(v.chars().take(32).collect());
                let _ = writeln!(body, "engine: {v}");
            }
            if let Some(v) = l.strip_prefix("rootless=") {
                host.rootless = Some(v.trim() == "1");
                let _ = writeln!(body, "rootless: {v}");
            }
        }
    } else if Path::new("/.dockerenv").exists() {
        host.runtime = Some("docker".into());
        body.push_str("engine: docker (/.dockerenv)\n");
    } else {
        let rt = format!("native-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        host.runtime = Some(rt.chars().take(32).collect());
        let _ = writeln!(body, "engine: {rt}");
    }
    host.cpu_threads = std::thread::available_parallelism()
        .ok()
        .and_then(|n| u32::try_from(n.get()).ok());
    let _ = writeln!(body, "available parallelism: {:?}", host.cpu_threads);
    if Path::new("/sys/fs/cgroup").exists() {
        for (name, path) in [
            ("cgroup memory.max", "/sys/fs/cgroup/memory.max"),
            ("cgroup memory.swap.max", "/sys/fs/cgroup/memory.swap.max"),
            ("cgroup cpu.max", "/sys/fs/cgroup/cpu.max"),
            ("cgroup pids.max", "/sys/fs/cgroup/pids.max"),
        ] {
            let v = std::fs::read_to_string(path).unwrap_or_else(|e| format!("({e})"));
            let _ = writeln!(body, "{name}: {}", v.trim());
        }
    }
    if let Ok(m) = std::fs::read_to_string("/proc/meminfo") {
        for l in m
            .lines()
            .filter(|l| l.starts_with("MemTotal") || l.starts_with("MemAvailable"))
        {
            let _ = writeln!(body, "{l}");
        }
    }
    diag.section("runtime", &body, 2048);
}

/// Default 2 GiB cap for NVIDIA's PTX JIT cache (`CUDA_CACHE_MAXSIZE`), lifted
/// above the 256 MiB driver default so multi-model `ggml-cuda` PTX cubins (~85 MiB)
/// are not evicted between workunits.
pub const DEFAULT_CUDA_CACHE_MAXSIZE: &str = "2147483648";
/// Default 2 GiB cap for Mesa's Vulkan pipeline/shader cache (`MESA_SHADER_CACHE_MAX_SIZE`).
pub const DEFAULT_MESA_SHADER_CACHE_MAX_SIZE: &str = "2G";

fn cache_user_tag() -> String {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(uid) = status
            .lines()
            .find_map(|l| l.strip_prefix("Uid:"))
            .and_then(|rest| rest.split_whitespace().nth(1))
            .filter(|u| !u.is_empty() && u.bytes().all(|b| b.is_ascii_digit()))
        {
            return uid.to_owned();
        }
    }
    for var in ["USER", "LOGNAME", "USERNAME"] {
        if let Ok(val) = std::env::var(var) {
            let clean: String = val
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .take(32)
                .collect();
            if !clean.is_empty() {
                return clean;
            }
        }
    }
    String::from("shared")
}

fn ensure_writable_cache_pair(dir: &Path) -> bool {
    let cuda = dir.join("cuda");
    let mesa = dir.join("mesa");
    if std::fs::create_dir_all(&cuda).is_err() || std::fs::create_dir_all(&mesa).is_err() {
        return false;
    }
    let probe = cuda.join(format!(".writable.{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Resolves a writable directory outside per-invocation scratch `TempDir`s so
/// CUDA (`CUDA_CACHE_PATH`) and Mesa Vulkan (`MESA_SHADER_CACHE_DIR`) keep
/// compiled PTX SASS cubins and SPIR-V shader pipelines across workunits and
/// Surya input modes (#794).
pub fn persistent_gpu_cache_dir(fallback_root: &Path) -> PathBuf {
    let suffix = format!("usnm-gpu-cache-{}", cache_user_tag());
    let candidates = [
        #[cfg(unix)]
        PathBuf::from("/var/tmp").join(&suffix),
        std::env::temp_dir().join(&suffix),
        fallback_root.join("gpu-cache"),
    ];

    for dir in candidates {
        if ensure_writable_cache_pair(&dir) {
            return dir;
        }
    }
    fallback_root.to_path_buf()
}

/// Configures `CUDA_CACHE_PATH`, `CUDA_CACHE_MAXSIZE`, `MESA_SHADER_CACHE_DIR`,
/// and `MESA_SHADER_CACHE_MAX_SIZE` on `cmd` (preserving any explicit caller
/// environment overrides) so overriding `HOME` to an ephemeral `TempDir` does
/// not discard ~85 MiB of compiled GPU kernels after every workunit (#794).
pub fn apply_gpu_cache_env(cmd: &mut std::process::Command, fallback_root: &Path) {
    let root = persistent_gpu_cache_dir(fallback_root);
    if std::env::var_os("CUDA_CACHE_PATH").is_none() {
        cmd.env("CUDA_CACHE_PATH", root.join("cuda"));
    }
    if std::env::var_os("CUDA_CACHE_MAXSIZE").is_none() {
        cmd.env("CUDA_CACHE_MAXSIZE", DEFAULT_CUDA_CACHE_MAXSIZE);
    }
    if std::env::var_os("MESA_SHADER_CACHE_DIR").is_none() {
        cmd.env("MESA_SHADER_CACHE_DIR", root.join("mesa"));
    }
    if std::env::var_os("MESA_SHADER_CACHE_MAX_SIZE").is_none() {
        cmd.env(
            "MESA_SHADER_CACHE_MAX_SIZE",
            DEFAULT_MESA_SHADER_CACHE_MAX_SIZE,
        );
    }
}

/// Selects the CPU worker thread count (`--threads`) when offloading inference
/// to a GPU: honors BOINC's `<ncpus>` allocation when higher, otherwise uses up
/// to 4 available host CPU threads for image decoding and multimodal patch
/// preparation (clamped to `2..=8`) (#794).
pub fn gpu_threads(ncpus: Option<f64>, cpu_threads: Option<u32>) -> u32 {
    let boinc_cpus = ncpus.map(|n| n.round().max(1.0) as u32).unwrap_or(0);
    let hw_cap = cpu_threads.unwrap_or(2).min(4);
    boinc_cpus.max(hw_cap).clamp(2, 8)
}

/// What one unit holds, in memory.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    pub job: &'a Job,
    /// `page.jpg`.
    pub page: &'a [u8],
    /// `loc.xml`.
    pub alto: &'a [u8],
    pub texts: &'a Texts,
    /// `places.txt`.
    pub places: &'a str,
}

/// What [`compute`] gives: the result (host and time of completion left
/// for the caller) and the confusion pairs as the bytes of
/// `confusions.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Computed {
    pub result: ResultManifest,
    pub confusions: Vec<u8>,
}

fn ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// How long the computation may run before the unit returns `timeout`.
pub const DEADLINE: Duration = Duration::from_secs(1800);

/// The process's peak resident set in KiB (`VmHWM`), where `/proc` has it.
pub fn peak_rss_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
}

/// The process's CPU time (user plus system) in milliseconds from
/// `/proc/self/stat`, at the kernel's usual 100 ticks a second.
pub fn cpu_ms() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    Some(ticks * 10)
}

/// Per-page checkpoint state persisted atomically in `quality_checkpoint.json`
/// so multi-page workunits resume from the last completed page after BOINC
/// preemption or client restart.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityCheckpoint {
    pub v: u32,
    pub job_id: String,
    pub engine: Engine,
    pub pages: Vec<PageResult>,
    pub confusions: BTreeMap<String, Vec<ConfusionPair>>,
}

impl QualityCheckpoint {
    pub fn load_valid(work: &Path, job: &Job) -> Option<Self> {
        let bytes = std::fs::read(work.join(CHECKPOINT_FILE)).ok()?;
        let cp: Self = serde_json::from_slice(&bytes).ok()?;
        if cp.v != usnm_ocr_contract::quality::VERSION
            || cp.job_id != job.id
            || cp.engine != job.engine
            || cp.pages.len() > job.pages.len()
        {
            return None;
        }
        for (got, want) in cp.pages.iter().zip(&job.pages) {
            if got.doc_id != want.doc_id {
                return None;
            }
        }
        Some(cp)
    }

    pub fn save(&self, work: &Path) -> anyhow::Result<()> {
        write_atomic(
            &work.join(CHECKPOINT_FILE),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

fn compute_page_record(
    doc_id: &str,
    readers_spec: &[ReaderSpec],
    function_words_list: &[String],
    page_bytes: &[u8],
    alto_bytes: &[u8],
    texts: &Texts,
    places: &Places,
) -> anyhow::Result<(PageResult, BTreeMap<String, Vec<ConfusionPair>>)> {
    let started = Instant::now();
    texts.validate_readers(readers_spec)?;
    let t = Instant::now();
    let image = image::load_from_memory(page_bytes)
        .with_context(|| format!("decoding page.jpg for {doc_id}"))?
        .to_luma8();
    let (w, h) = image.dimensions();
    let alto = parse_alto(alto_bytes, f64::from(w), f64::from(h))
        .with_context(|| format!("parsing ALTO for {doc_id}"))?;
    let decode_ms = ms(t);
    let t = Instant::now();
    let page = PageFeatures {
        image: Some(report::round_image(features::image_features(&image))),
        layout: Some(report::round_layout(features::layout_features(&alto))),
    };
    let features_ms = ms(t);
    let t = Instant::now();
    let alto_text = usnm_alto::text(&alto);
    let readers: Vec<ReaderInput> = readers_spec
        .iter()
        .map(|spec| {
            let mut text = texts
                .readers
                .iter()
                .find(|r| r.name == spec.name)
                .map(|r| r.text.clone())
                .unwrap_or_default();
            if (spec.name == "loc" || spec.name == "loc-hist")
                && (text.trim().is_empty() || text.trim().eq_ignore_ascii_case("none"))
            {
                text = alto_text.clone();
            }
            ReaderInput {
                name: spec.name.clone(),
                source: spec.source.clone(),
                text,
            }
        })
        .collect();
    let function_words: BTreeSet<String> = function_words_list
        .iter()
        .map(|w| crate::consensus::fold(w))
        .collect();
    let analysis = report::analyse(&PageInputs {
        image: None,
        alto: None,
        readers,
        function_words,
        places,
    });
    let text_ms = ms(t);
    let r = analysis.report;
    Ok((
        PageResult {
            doc_id: doc_id.to_owned(),
            hat: Some(r.hat),
            legibility: Some(r.legibility),
            outcome: Outcome::Ok,
            page: Some(page),
            readers: r.readers,
            timings: Timings {
                cpu_ms: None,
                decode_ms,
                features_ms,
                peak_rss_kib: None,
                text_ms,
                total_ms: ms(started),
            },
            unit_error: None,
        },
        analysis.confusions,
    ))
}

/// The computation for one unit: deterministic, single-threaded.
pub fn compute(inputs: &Inputs) -> anyhow::Result<Computed> {
    let started = Instant::now();
    let job = inputs.job;
    job.validate()?;
    let places = Places::parse(inputs.places);
    if job.pages.is_empty() {
        let (pr, conf_map) = compute_page_record(
            &job.doc_id,
            &job.readers,
            &job.function_words,
            inputs.page,
            inputs.alto,
            inputs.texts,
            &places,
        )?;
        let confusions = serde_json::to_vec_pretty(&conf_map)?;
        Ok(Computed {
            result: ResultManifest {
                v: usnm_ocr_contract::quality::VERSION,
                job_id: job.id.clone(),
                completed_at: String::new(),
                confusions_bytes: confusions.len() as u64,
                confusions_sha256: sha256(&confusions),
                diag_bytes: 0,
                diag_sha256: sha256(b""),
                engine: job.engine.clone(),
                hat: pr.hat,
                host: Host::default(),
                legibility: pr.legibility,
                outcome: Outcome::Ok,
                page: pr.page,
                pages: vec![],
                readers: pr.readers,
                timings: Timings {
                    total_ms: ms(started),
                    ..pr.timings
                },
                unit_error: None,
            },
            confusions,
        })
    } else {
        let mut page_results = Vec::with_capacity(job.pages.len());
        let mut all_confusions: BTreeMap<String, Vec<ConfusionPair>> = BTreeMap::new();
        let mut timings = Timings::default();
        let raw_texts = serde_json::to_vec(inputs.texts).unwrap_or_default();
        for p in &job.pages {
            let page_bytes = p.inputs.page.extract(inputs.page, "page")?;
            let alto_bytes = p.inputs.alto.extract(inputs.alto, "alto")?;
            let texts_bytes = p.inputs.texts.extract(&raw_texts, "texts");
            let page_texts: Texts = match texts_bytes {
                Ok(b) => serde_json::from_slice(b).context("bundle page texts.json")?,
                Err(_) => inputs.texts.clone(),
            };
            let fw = if p.function_words.is_empty() {
                &job.function_words
            } else {
                &p.function_words
            };
            let (pr, conf_map) = compute_page_record(
                &p.doc_id,
                &p.readers,
                fw,
                page_bytes,
                alto_bytes,
                &page_texts,
                &places,
            )?;
            timings.decode_ms += pr.timings.decode_ms;
            timings.features_ms += pr.timings.features_ms;
            timings.text_ms += pr.timings.text_ms;
            for (k, v) in conf_map {
                all_confusions.insert(format!("{}:{k}", p.doc_id), v);
            }
            page_results.push(pr);
        }
        timings.total_ms = ms(started);
        let confusions = serde_json::to_vec_pretty(&all_confusions)?;
        Ok(Computed {
            result: ResultManifest {
                v: usnm_ocr_contract::quality::VERSION,
                job_id: job.id.clone(),
                completed_at: String::new(),
                confusions_bytes: confusions.len() as u64,
                confusions_sha256: sha256(&confusions),
                diag_bytes: 0,
                diag_sha256: sha256(b""),
                engine: job.engine.clone(),
                hat: None,
                host: Host::default(),
                legibility: None,
                outcome: Outcome::Ok,
                page: None,
                pages: page_results,
                readers: vec![],
                timings,
                unit_error: None,
            },
            confusions,
        })
    }
}

/// The unit's files, read and checked against the job.
pub struct Loaded {
    pub job: Job,
    pub page: Vec<u8>,
    pub alto: Vec<u8>,
    pub texts: Texts,
    pub bundle_texts: Vec<Texts>,
    pub places: String,
}

/// Checks the unit before anything runs: the job and every input's checksum.
pub fn prepare(work: &Path) -> anyhow::Result<Loaded> {
    let job_path = resolve_slot_file(work, JOB_FILE);
    let job: Job = serde_json::from_slice(&read_bounded(&job_path, MAX_JOB_JSON)?)?;
    job.validate()?;
    let (max_scan, max_alto, max_texts) = if job.pages.is_empty() {
        (MAX_SCAN, MAX_ALTO, MAX_TEXTS)
    } else {
        (MAX_BUNDLE_SCAN, MAX_BUNDLE_ALTO, MAX_BUNDLE_TEXTS)
    };
    let page = read_bounded(&resolve_slot_file(work, PAGE_FILE), max_scan)?;
    ensure!(
        job.inputs.page.matches(&page),
        "input page checksum mismatch"
    );
    let alto = read_bounded(&resolve_slot_file(work, ALTO_FILE), max_alto)?;
    ensure!(
        job.inputs.alto.matches(&alto),
        "input ALTO checksum mismatch"
    );
    let texts_bytes = read_bounded(&resolve_slot_file(work, TEXTS_FILE), max_texts)?;
    ensure!(
        job.inputs.texts.matches(&texts_bytes),
        "input texts checksum mismatch"
    );
    let (texts, bundle_texts) = if job.pages.is_empty() {
        let t: Texts = serde_json::from_slice(&texts_bytes).context("texts.json")?;
        t.validate(&job)?;
        (t, vec![])
    } else {
        let mut b_texts = Vec::with_capacity(job.pages.len());
        for p in &job.pages {
            p.inputs.page.extract(&page, "page")?;
            p.inputs.alto.extract(&alto, "alto")?;
            let tb = p.inputs.texts.extract(&texts_bytes, "texts")?;
            let t: Texts = serde_json::from_slice(tb)
                .with_context(|| format!("texts.json for {}", p.doc_id))?;
            t.validate_readers(&p.readers)?;
            b_texts.push(t);
        }
        let first = b_texts.first().cloned().unwrap_or(Texts {
            page: None,
            readers: vec![],
        });
        (first, b_texts)
    };
    let places_bytes = read_bounded(&resolve_slot_file(work, PLACES_FILE), MAX_PLACES)?;
    ensure!(
        job.inputs.places.matches(&places_bytes),
        "input places checksum mismatch"
    );
    let places = String::from_utf8(places_bytes).context("places.txt is not UTF-8")?;
    Ok(Loaded {
        job,
        page,
        alto,
        texts,
        bundle_texts,
        places,
    })
}

fn empty(
    job_id: String,
    engine: Engine,
    outcome: Outcome,
    error: Option<String>,
) -> ResultManifest {
    ResultManifest {
        v: usnm_ocr_contract::quality::VERSION,
        job_id,
        completed_at: String::new(),
        confusions_bytes: 0,
        confusions_sha256: String::new(),
        diag_bytes: 0,
        diag_sha256: String::new(),
        engine,
        hat: None,
        host: Host::default(),
        legibility: None,
        outcome,
        page: None,
        pages: vec![],
        readers: vec![],
        timings: Timings::default(),
        unit_error: error,
    }
}

fn run_on_worker_thread<T, F>(
    job: &Job,
    deadline: Duration,
    control: &BoincControl,
    f: F,
) -> Result<T, Box<(Job, anyhow::Error)>>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        return Err(Box::new((
            job.clone(),
            anyhow::anyhow!("boinc control requested exit ({reason})"),
        )));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("quality-compute".into())
        .stack_size(64 << 20)
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| {
            Box::new((
                job.clone(),
                anyhow::Error::from(e).context("starting the computation"),
            ))
        })?;
    let mut active_elapsed = Duration::ZERO;
    let poll_step = Duration::from_millis(50);
    loop {
        if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
            return Err(Box::new((
                job.clone(),
                anyhow::anyhow!("boinc control requested exit ({reason})"),
            )));
        }
        let remaining = deadline.saturating_sub(active_elapsed);
        if remaining.is_zero() {
            return Err(Box::new((
                job.clone(),
                anyhow::anyhow!("computation exceeded {} s", deadline.as_secs()),
            )));
        }
        let step_start = Instant::now();
        match rx.recv_timeout(poll_step.min(remaining)) {
            Ok(Ok(c)) => {
                if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
                    return Err(Box::new((
                        job.clone(),
                        anyhow::anyhow!("boinc control requested exit ({reason})"),
                    )));
                }
                return Ok(c);
            }
            Ok(Err(e)) => return Err(Box::new((job.clone(), e))),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !control.is_suspended() {
                    active_elapsed += step_start.elapsed();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Box::new((
                    job.clone(),
                    anyhow::anyhow!("computation thread ended without a result"),
                )));
            }
        }
    }
}

fn compute_with_deadline(
    work: &Path,
    l: Loaded,
    deadline: Duration,
    control: &BoincControl,
    diag: &mut Diag,
) -> Result<Computed, Box<(Job, anyhow::Error)>> {
    if l.job.pages.is_empty() {
        let job = l.job.clone();
        return run_on_worker_thread(&job, deadline, control, move || {
            compute(&Inputs {
                job: &l.job,
                page: &l.page,
                alto: &l.alto,
                texts: &l.texts,
                places: &l.places,
            })
        });
    }

    let started = Instant::now();
    let job = l.job.clone();
    let total_pages = job.pages.len();
    let places = Places::parse(&l.places);
    let mut cp = match QualityCheckpoint::load_valid(work, &job) {
        Some(existing) => {
            if !existing.pages.is_empty() {
                diag.section(
                    "checkpoint",
                    &format!(
                        "resumed from checkpoint: {} of {} pages already complete",
                        existing.pages.len(),
                        total_pages
                    ),
                    256,
                );
                let frac = existing.pages.len() as f64 / total_pages.max(1) as f64;
                let _ = write_atomic(&work.join(FRACTION_FILE), format!("{frac:.6}\n").as_bytes());
            }
            existing
        }
        None => QualityCheckpoint {
            v: usnm_ocr_contract::quality::VERSION,
            job_id: job.id.clone(),
            engine: job.engine.clone(),
            pages: Vec::with_capacity(total_pages),
            confusions: BTreeMap::new(),
        },
    };

    for idx in cp.pages.len()..total_pages {
        if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
            return Err(Box::new((
                job,
                anyhow::anyhow!("boinc control requested exit ({reason})"),
            )));
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(Box::new((
                job,
                anyhow::anyhow!("computation exceeded {} s", deadline.as_secs()),
            )));
        }
        let bp = job.pages[idx].clone();
        let page_bytes = match bp.inputs.page.extract(&l.page, "page") {
            Ok(s) => s.to_vec(),
            Err(e) => return Err(Box::new((job, e))),
        };
        let alto_bytes = match bp.inputs.alto.extract(&l.alto, "alto") {
            Ok(s) => s.to_vec(),
            Err(e) => return Err(Box::new((job, e))),
        };
        let page_texts = l.bundle_texts[idx].clone();
        let fw = if bp.function_words.is_empty() {
            job.function_words.clone()
        } else {
            bp.function_words.clone()
        };
        let places_clone = places.clone();
        let doc_id = bp.doc_id.clone();
        let readers_spec = bp.readers.clone();
        let (pr, conf_map) = run_on_worker_thread(&job, remaining, control, move || {
            compute_page_record(
                &doc_id,
                &readers_spec,
                &fw,
                &page_bytes,
                &alto_bytes,
                &page_texts,
                &places_clone,
            )
        })?;
        for (k, v) in conf_map {
            cp.confusions.insert(format!("{}:{k}", bp.doc_id), v);
        }
        cp.pages.push(pr);
        let _ = cp.save(work);
        let frac = cp.pages.len() as f64 / total_pages.max(1) as f64;
        let _ = write_atomic(&work.join(FRACTION_FILE), format!("{frac:.6}\n").as_bytes());
    }

    let _ = std::fs::remove_file(work.join(CHECKPOINT_FILE));
    let mut timings = Timings::default();
    for p in &cp.pages {
        timings.decode_ms += p.timings.decode_ms;
        timings.features_ms += p.timings.features_ms;
        timings.text_ms += p.timings.text_ms;
        timings.total_ms += p.timings.total_ms;
    }
    let confusions = serde_json::to_vec_pretty(&cp.confusions)
        .map_err(|e| Box::new((job.clone(), anyhow::Error::from(e))))?;
    Ok(Computed {
        result: ResultManifest {
            v: usnm_ocr_contract::quality::VERSION,
            job_id: job.id,
            completed_at: String::new(),
            confusions_bytes: confusions.len() as u64,
            confusions_sha256: sha256(&confusions),
            diag_bytes: 0,
            diag_sha256: sha256(b""),
            engine: job.engine,
            hat: None,
            host: Host::default(),
            legibility: None,
            outcome: Outcome::Ok,
            page: None,
            pages: cp.pages,
            readers: vec![],
            timings,
            unit_error: None,
        },
        confusions,
    })
}

/// Runs a complete native `usnm_quality` unit in `work`, monitoring BOINC's
/// shared-memory lifecycle channels and writing `fraction_done`,
/// `confusions.json`, `diag.txt`, `result.json`, and `boinc_finish_called`.
pub fn run_slot(work: &Path) -> anyhow::Result<()> {
    run_slot_with_mode(work, true)
}

/// Runs a complete container `usnm_quality` unit in `work`, writing
/// `fraction_done`, `confusions.json`, `diag.txt`, and `result.json` while
/// leaving `boinc_mmap_file` and `boinc_finish_called` exclusively to
/// `docker_wrapper`.
pub fn run_container_slot(work: &Path) -> anyhow::Result<()> {
    run_slot_with_mode(work, false)
}

fn run_slot_with_mode(work: &Path, native_lifecycle: bool) -> anyhow::Result<()> {
    let confusions_out = resolve_slot_file(work, CONFUSIONS_FILE);
    let diag_out = resolve_slot_file(work, DIAG_FILE);
    let result_out = resolve_slot_file(work, RESULT_FILE);
    let fraction_out = work.join(FRACTION_FILE);
    let finish_out = work.join(FINISH_FILE);

    for p in [&confusions_out, &diag_out, &result_out, &fraction_out] {
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    if native_lifecycle && finish_out.exists() {
        std::fs::remove_file(&finish_out)?;
    }
    write_atomic(&fraction_out, b"0.0\n")?;

    let control = if native_lifecycle {
        BoincControl::start(work)
    } else {
        BoincControl::noop()
    };
    let mut diag = Diag::new();
    let mut host = Host::default();
    boinc_info(work, &mut host, &mut diag);
    runtime_info(&mut host, &mut diag);

    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        diag.section(
            "boinc control",
            &format!("exiting without boinc_finish_called: {reason}"),
            256,
        );
        write_atomic(&diag_out, &diag.finish())?;
        return Ok(());
    }

    let (mut result, confusions) = match prepare(work) {
        Ok(l) => {
            diag.section(
                "unit",
                &format!(
                    "job {} doc {} group {} language {}: readers {:?}",
                    l.job.id,
                    l.job.doc_id,
                    l.job.group,
                    l.job.language,
                    l.job
                        .readers
                        .iter()
                        .map(|r| r.name.as_str())
                        .collect::<Vec<_>>()
                ),
                1024,
            );
            let started = Instant::now();
            match compute_with_deadline(work, l, DEADLINE, &control, &mut diag) {
                Ok(c) => {
                    diag.section("timings", &format!("{:?}", c.result.timings), 512);
                    (c.result, c.confusions)
                }
                Err(boxed) => {
                    if let Some(reason) = control.should_exit() {
                        diag.section(
                            "boinc control",
                            &format!("exiting without boinc_finish_called: {reason}"),
                            256,
                        );
                        write_atomic(&diag_out, &diag.finish())?;
                        return Ok(());
                    }
                    let (job, e) = *boxed;
                    let outcome = if started.elapsed() >= DEADLINE {
                        Outcome::Timeout
                    } else {
                        Outcome::Failed
                    };
                    diag.section("computation failed", &format!("{e:#}"), 2048);
                    let mut r = empty(job.id.clone(), job.engine.clone(), outcome, None);
                    r.timings.total_ms = ms(started);
                    (r, b"{}".to_vec())
                }
            }
        }
        Err(e) => {
            if let Some(reason) = control.should_exit() {
                diag.section(
                    "boinc control",
                    &format!("exiting without boinc_finish_called: {reason}"),
                    256,
                );
                write_atomic(&diag_out, &diag.finish())?;
                return Ok(());
            }
            let msg = format!("{e:#}");
            diag.section("unit error", &msg, 1024);
            let job_path = resolve_slot_file(work, JOB_FILE);
            let job: Option<serde_json::Value> = std::fs::read(job_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let id = job
                .as_ref()
                .and_then(|j| j.get("id")?.as_str())
                .filter(|s| usnm_ocr_contract::boinc::is_sha256(s))
                .unwrap_or(&"0".repeat(64))
                .to_owned();
            let engine = job
                .and_then(|j| serde_json::from_value::<Engine>(j.get("engine")?.clone()).ok())
                .unwrap_or_else(|| Engine {
                    image: format!("sha256:{}", "0".repeat(64)),
                });
            (
                empty(
                    id,
                    engine,
                    Outcome::UnitError,
                    Some(tail(&msg, MAX_ERROR).to_owned()),
                ),
                b"{}".to_vec(),
            )
        }
    };

    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        diag.section(
            "boinc control",
            &format!("exiting without boinc_finish_called: {reason}"),
            256,
        );
        write_atomic(&diag_out, &diag.finish())?;
        return Ok(());
    }

    result.timings.cpu_ms = cpu_ms();
    result.timings.peak_rss_kib = peak_rss_kib();
    diag.section(
        "process",
        &format!(
            "cpu {:?} ms, peak rss {:?} KiB, deadline {} s",
            result.timings.cpu_ms,
            result.timings.peak_rss_kib,
            DEADLINE.as_secs()
        ),
        256,
    );
    let diag = diag.finish();
    result.host = host;
    result.completed_at = chrono::Utc::now().to_rfc3339();
    result.confusions_bytes = confusions.len() as u64;
    result.confusions_sha256 = sha256(&confusions);
    result.diag_bytes = diag.len() as u64;
    result.diag_sha256 = sha256(&diag);

    write_atomic(&confusions_out, &confusions)?;
    write_atomic(&diag_out, &diag)?;
    write_atomic(&result_out, &serde_json::to_vec_pretty(&result)?)?;
    write_atomic(&fraction_out, b"1.0\n")?;
    if native_lifecycle {
        write_atomic(&finish_out, b"0\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/test-slot-tmp")
                .join(format!("{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn boinc_mmap_suspend_resume_and_quit_consume_messages() {
        let tmp = TestDir::new("suspend-resume");
        let slot = tmp.path();
        let mmap = slot.join(MMAP_FILE);
        std::fs::write(&mmap, vec![0u8; SHARED_MEM_SIZE]).unwrap();

        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<suspend/>").unwrap();
        let control = BoincControl::start(slot);
        assert!(control.is_suspended());
        // Verify byte 0 was cleared to 0 so the BOINC client sees the message was consumed.
        assert_eq!(std::fs::read(&mmap).unwrap()[0], 0);

        let control_wait = control.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let flow = control_wait.wait_if_suspended_or_should_exit();
            tx.send(flow).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(120)).is_err());

        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<resume/>").unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            ControlFlow::Continue(())
        );
        waiter.join().unwrap();
        assert!(!control.is_suspended());
        assert_eq!(std::fs::read(&mmap).unwrap()[0], 0);

        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.should_exit().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.should_exit(), Some("quit"));
        assert_eq!(std::fs::read(&mmap).unwrap()[0], 0);
    }

    #[test]
    fn boinc_mmap_quit_and_heartbeat_loss_omit_boinc_finish_called() {
        let tmp = TestDir::new("quit-heartbeat");
        let quit_slot = tmp.path().join("quit_slot");
        std::fs::create_dir_all(&quit_slot).unwrap();
        let quit_mmap = quit_slot.join(MMAP_FILE);
        write_mmap_channel(&quit_mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        run_slot(&quit_slot).unwrap();
        assert!(!quit_slot.join(FINISH_FILE).exists());
        assert_eq!(std::fs::read(&quit_mmap).unwrap()[0], 0);

        let hb_slot = tmp.path().join("hb_slot");
        std::fs::create_dir_all(&hb_slot).unwrap();
        let hb_mmap = hb_slot.join(MMAP_FILE);
        write_mmap_channel(&hb_mmap, HEARTBEAT_OFFSET, "<heartbeat/>").unwrap();
        let control =
            BoincControl::start_with_heartbeat_timeout(&hb_slot, Duration::from_millis(80));
        assert_eq!(std::fs::read(&hb_mmap).unwrap()[HEARTBEAT_OFFSET], 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.should_exit().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.should_exit(), Some("heartbeat_lost"));
    }

    #[test]
    fn boinc_startup_with_init_data_creates_mmap_channel_and_advertises_api_version() {
        assert_eq!(
            std::str::from_utf8(&BOINC_API_VERSION_MARKER[..17]).unwrap(),
            format!("API_VERSION_{BOINC_API_VERSION}")
        );
        let tmp = TestDir::new("init-creates-mmap");
        let slot = tmp.path();
        std::fs::write(slot.join("init_data.xml"), "<app_init_data/>\n").unwrap();
        let mmap = slot.join(MMAP_FILE);
        assert!(!mmap.exists());

        let control = BoincControl::start(slot);
        assert!(mmap.is_file());
        assert_eq!(mmap.metadata().unwrap().len(), SHARED_MEM_SIZE as u64);

        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.should_exit().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.should_exit(), Some("quit"));
    }

    #[test]
    fn parse_comm_obj_name_extracts_segment_name() {
        assert_eq!(
            parse_comm_obj_name(
                "<app_init_data>\n<comm_obj_name>boinc_usnm_0</comm_obj_name>\n</app_init_data>"
            ),
            Some("boinc_usnm_0".to_string())
        );
        assert_eq!(
            parse_comm_obj_name("<app_init_data><comm_obj_name>  </comm_obj_name></app_init_data>"),
            None
        );
        assert_eq!(parse_comm_obj_name("<app_init_data/>"), None);
    }

    #[test]
    fn container_slot_leaves_mmap_and_finish_file_to_docker_wrapper() {
        let tmp = TestDir::new("container-slot");
        let slot = tmp.path();
        let mmap = slot.join(MMAP_FILE);
        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        write_mmap_channel(&mmap, HEARTBEAT_OFFSET, "<heartbeat/>").unwrap();

        run_container_slot(slot).unwrap();
        // Container slot must not consume docker_wrapper's mmap messages or write boinc_finish_called.
        let bytes = std::fs::read(&mmap).unwrap();
        assert_eq!(bytes[PROCESS_CONTROL_REQUEST_OFFSET], 1);
        assert_eq!(bytes[HEARTBEAT_OFFSET], 1);
        assert!(!slot.join(FINISH_FILE).exists());
        assert!(slot.join(RESULT_FILE).is_file());
    }

    #[test]
    fn multipage_bundle_checkpoints_per_page_and_resumes_cleanly() {
        use usnm_ocr_contract::quality::{
            BundlePage, BundlePageInputs, BundleSlice, Input, Inputs, TextReader, VERSION,
        };

        let mut jpg1 = Vec::new();
        image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(32, 32, image::Luma([220])))
            .write_to(
                &mut std::io::Cursor::new(&mut jpg1),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let mut jpg2 = Vec::new();
        image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(32, 32, image::Luma([200])))
            .write_to(
                &mut std::io::Cursor::new(&mut jpg2),
                image::ImageFormat::Jpeg,
            )
            .unwrap();

        let alto1 = br#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#">
  <Layout><Page ID="P1" WIDTH="32" HEIGHT="32" PHYSICAL_IMG_NR="1">
    <PrintSpace HPOS="0" VPOS="0" WIDTH="32" HEIGHT="32">
      <TextBlock ID="B1" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12">
        <TextLine ID="L1" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12">
          <String ID="S1" CONTENT="washington" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12" WC="0.95"/>
        </TextLine>
      </TextBlock>
    </PrintSpace>
  </Page></Layout>
</alto>"#;
        let alto2 = br#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#">
  <Layout><Page ID="P2" WIDTH="32" HEIGHT="32" PHYSICAL_IMG_NR="2">
    <PrintSpace HPOS="0" VPOS="0" WIDTH="32" HEIGHT="32">
      <TextBlock ID="B1" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12">
        <TextLine ID="L1" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12">
          <String ID="S1" CONTENT="baltimore" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12" WC="0.92"/>
        </TextLine>
      </TextBlock>
    </PrintSpace>
  </Page></Layout>
</alto>"#;

        let readers_spec = vec![
            ReaderSpec {
                name: "loc".into(),
                source: "loc".into(),
            },
            ReaderSpec {
                name: "surya".into(),
                source: "surya".into(),
            },
        ];
        let t1 = serde_json::to_vec(&Texts {
            page: None,
            readers: vec![
                TextReader {
                    name: "loc".into(),
                    source: "loc".into(),
                    text: "the washington news".into(),
                    alto: false,
                    regions: vec![],
                },
                TextReader {
                    name: "surya".into(),
                    source: "surya".into(),
                    text: "the washington news".into(),
                    alto: false,
                    regions: vec![],
                },
            ],
        })
        .unwrap();
        let t2 = serde_json::to_vec(&Texts {
            page: None,
            readers: vec![
                TextReader {
                    name: "loc".into(),
                    source: "loc".into(),
                    text: "the baltimore sun".into(),
                    alto: false,
                    regions: vec![],
                },
                TextReader {
                    name: "surya".into(),
                    source: "surya".into(),
                    text: "the baltimore sun".into(),
                    alto: false,
                    regions: vec![],
                },
            ],
        })
        .unwrap();

        let mut page_blob = Vec::new();
        let p_slice1 = BundleSlice::of(0, &jpg1, "s3://b/p1.jpg");
        page_blob.extend_from_slice(&jpg1);
        let p_slice2 = BundleSlice::of(page_blob.len() as u64, &jpg2, "s3://b/p2.jpg");
        page_blob.extend_from_slice(&jpg2);

        let mut alto_blob = Vec::new();
        let a_slice1 = BundleSlice::of(0, alto1, "s3://b/p1.xml");
        alto_blob.extend_from_slice(alto1);
        let a_slice2 = BundleSlice::of(alto_blob.len() as u64, alto2, "s3://b/p2.xml");
        alto_blob.extend_from_slice(alto2);

        let mut texts_blob = Vec::new();
        let t_slice1 = BundleSlice::of(0, &t1, "s3://b/t1.json");
        texts_blob.extend_from_slice(&t1);
        let t_slice2 = BundleSlice::of(texts_blob.len() as u64, &t2, "s3://b/t2.json");
        texts_blob.extend_from_slice(&t2);

        let places_bytes = b"washington\nbaltimore\n";
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            crop_seed: 7,
            doc_id: "bundle_p1".into(),
            engine: Engine {
                image: format!("sha256:{}", "a".repeat(64)),
            },
            function_words: vec!["the".into()],
            group: "pilot".into(),
            inputs: Inputs {
                alto: Input::of(&alto_blob, "bundle:loc.xml"),
                page: Input::of(&page_blob, "bundle:page.jpg"),
                places: Input::of(places_bytes, "curated:places"),
                texts: Input::of(&texts_blob, "bundle:texts.json"),
            },
            k_crops: 6,
            language: "en".into(),
            pages: vec![
                BundlePage {
                    crop_seed: 7,
                    doc_id: "bundle_p1".into(),
                    function_words: vec![],
                    group: "pilot".into(),
                    inputs: BundlePageInputs {
                        alto: a_slice1,
                        page: p_slice1,
                        texts: t_slice1,
                    },
                    k_crops: 6,
                    language: "en".into(),
                    readers: readers_spec.clone(),
                },
                BundlePage {
                    crop_seed: 7,
                    doc_id: "bundle_p2".into(),
                    function_words: vec![],
                    group: "pilot".into(),
                    inputs: BundlePageInputs {
                        alto: a_slice2,
                        page: p_slice2,
                        texts: t_slice2,
                    },
                    k_crops: 6,
                    language: "en".into(),
                    readers: readers_spec,
                },
            ],
            readers: vec![],
        };
        job.id = job.identity().unwrap();

        let write_slot_inputs = |dir: &Path| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(PAGE_FILE), &page_blob).unwrap();
            std::fs::write(dir.join(ALTO_FILE), &alto_blob).unwrap();
            std::fs::write(dir.join(TEXTS_FILE), &texts_blob).unwrap();
            std::fs::write(dir.join(PLACES_FILE), places_bytes).unwrap();
            std::fs::write(dir.join(JOB_FILE), serde_json::to_vec_pretty(&job).unwrap()).unwrap();
        };

        let tmp = TestDir::new("multipage-bundle");
        let slot_full = tmp.path().join("full");
        write_slot_inputs(&slot_full);
        run_slot(&slot_full).unwrap();
        assert!(slot_full.join(FINISH_FILE).is_file());
        assert!(!slot_full.join(CHECKPOINT_FILE).exists());
        let res_full: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot_full.join(RESULT_FILE)).unwrap()).unwrap();
        let conf_full_bytes = std::fs::read(slot_full.join(CONFUSIONS_FILE)).unwrap();
        let diag_full_bytes = std::fs::read(slot_full.join(DIAG_FILE)).unwrap();
        assert_eq!(res_full.pages.len(), 2);
        res_full.check(&conf_full_bytes, &diag_full_bytes).unwrap();

        // Simulate resuming from a 1-page checkpoint on a second slot.
        let slot_resume = tmp.path().join("resume");
        write_slot_inputs(&slot_resume);
        let full_conf: BTreeMap<String, Vec<ConfusionPair>> =
            serde_json::from_slice(&conf_full_bytes).unwrap();
        let page1_conf: BTreeMap<String, Vec<ConfusionPair>> = full_conf
            .into_iter()
            .filter(|(k, _)| k.starts_with("bundle_p1:"))
            .collect();
        let cp = QualityCheckpoint {
            v: VERSION,
            job_id: job.id.clone(),
            engine: job.engine.clone(),
            pages: vec![res_full.pages[0].clone()],
            confusions: page1_conf,
        };
        cp.save(&slot_resume).unwrap();
        assert!(slot_resume.join(CHECKPOINT_FILE).is_file());

        run_slot(&slot_resume).unwrap();
        assert!(!slot_resume.join(CHECKPOINT_FILE).exists());
        let res_resume: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot_resume.join(RESULT_FILE)).unwrap()).unwrap();
        let conf_resume_bytes = std::fs::read(slot_resume.join(CONFUSIONS_FILE)).unwrap();
        let diag_resume_bytes = std::fs::read(slot_resume.join(DIAG_FILE)).unwrap();
        res_resume
            .check(&conf_resume_bytes, &diag_resume_bytes)
            .unwrap();
        usnm_ocr_contract::quality::agree(&res_full, &res_resume).unwrap();
        let diag_str = std::fs::read_to_string(slot_resume.join(DIAG_FILE)).unwrap();
        assert!(diag_str.contains("resumed from checkpoint: 1 of 2 pages already complete"));
    }

    #[test]
    fn gpu_cache_dir_and_gpu_threads_scale_outside_ephemeral_home() {
        let tmp = TestDir::new("gpu-cache");
        let dir = persistent_gpu_cache_dir(tmp.path());
        assert!(dir.join("cuda").is_dir());
        assert!(dir.join("mesa").is_dir());

        let mut cmd = std::process::Command::new("sh");
        apply_gpu_cache_env(&mut cmd, tmp.path());
        let envs: BTreeMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_owned(), v?.to_str()?.to_owned())))
            .collect();
        if std::env::var_os("CUDA_CACHE_PATH").is_none() {
            assert_eq!(
                envs.get("CUDA_CACHE_PATH").map(String::as_str),
                Some(dir.join("cuda").to_string_lossy().as_ref())
            );
        }
        if std::env::var_os("CUDA_CACHE_MAXSIZE").is_none() {
            assert_eq!(
                envs.get("CUDA_CACHE_MAXSIZE").map(String::as_str),
                Some(DEFAULT_CUDA_CACHE_MAXSIZE)
            );
        }
        if std::env::var_os("MESA_SHADER_CACHE_DIR").is_none() {
            assert_eq!(
                envs.get("MESA_SHADER_CACHE_DIR").map(String::as_str),
                Some(dir.join("mesa").to_string_lossy().as_ref())
            );
        }

        // Default 1-CPU GPU plan class on a 4-core host scales up to 4 threads.
        assert_eq!(gpu_threads(Some(1.0), Some(4)), 4);
        // 2-core host stays at 2 threads.
        assert_eq!(gpu_threads(Some(1.0), Some(2)), 2);
        // 16-core host with default 1-CPU GPU slot caps opportunistic threads at 4,
        // while an explicit <ncpus>6.0</ncpus> allocation scales to 6 (clamped at 8).
        assert_eq!(gpu_threads(Some(1.0), Some(16)), 4);
        assert_eq!(gpu_threads(Some(6.0), Some(16)), 6);
        assert_eq!(gpu_threads(Some(12.0), Some(16)), 8);
        assert_eq!(gpu_threads(None, None), 2);
    }

    #[cfg(windows)]
    #[test]
    fn windows_comm_obj_name_shared_memory_controls_lifecycle() {
        let tmp = TestDir::new("win-comm-obj");
        let slot = tmp.path();
        let seg_name = format!("boinc_usnm_test_{}", std::process::id());
        let client_shm = WinShmem::attach_or_create(&seg_name, true).unwrap();
        std::fs::write(
            slot.join("init_data.xml"),
            format!("<app_init_data><comm_obj_name>{seg_name}</comm_obj_name></app_init_data>\n"),
        )
        .unwrap();

        client_shm
            .write_channel(PROCESS_CONTROL_REQUEST_OFFSET, "<suspend/>")
            .unwrap();
        let control = BoincControl::start(slot);
        assert!(control.is_suspended());

        client_shm
            .write_channel(PROCESS_CONTROL_REQUEST_OFFSET, "<resume/>")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.is_suspended() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!control.is_suspended());

        client_shm
            .write_channel(PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.should_exit().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.should_exit(), Some("quit"));
    }
}
