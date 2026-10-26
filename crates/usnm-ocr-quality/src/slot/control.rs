//! BOINC process-control and heartbeat channels.

use super::{resolve_slot_file, BOINC_API_VERSION_MARKER, MMAP_FILE};
use anyhow::ensure;
use std::{
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    ops::ControlFlow,
    path::Path,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

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
