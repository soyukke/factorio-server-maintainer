//! Process spawn/monitor/terminate helpers — see spec §6.2 and §6.4.
//!
//! Windows behavior:
//! - `CREATE_NEW_CONSOLE` so the child has its own console (Ctrl+C target).
//! - `STARTUPINFOW.wShowWindow = SW_HIDE` so the console window is hidden
//!   from the start. Without this the console flashes briefly before being
//!   hidden, which is observable.
//! - The handles are closed on drop. PID is stable for the lifetime of the
//!   handle (Windows reuses PIDs across processes, never within).

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct SpawnRequest {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

#[cfg(windows)]
mod imp {
    use super::SpawnRequest;
    use std::os::windows::ffi::OsStrExt;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, OpenProcess, TerminateProcess, WaitForSingleObject,
        CREATE_NEW_CONSOLE, PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    // Avoid importing these constants because their module location has
    // shifted across windows-sys versions; the numeric values are stable
    // ABI from Win32 and won't change.
    const WAIT_OBJECT_0: u32 = 0x0000_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;

    pub struct ServerProcess {
        pid: u32,
        process: HANDLE,
        /// Thread handle from CreateProcessW. `None` when we re-attached to
        /// an existing process via OpenProcess (no thread handle available).
        thread: Option<HANDLE>,
    }

    // SAFETY: HANDLE is an opaque kernel handle (raw isize). The Win32 APIs we
    // use are documented as thread-safe, and we never alias the handle from
    // multiple ServerProcess instances.
    unsafe impl Send for ServerProcess {}
    unsafe impl Sync for ServerProcess {}

    impl ServerProcess {
        pub fn spawn(req: &SpawnRequest) -> anyhow::Result<Self> {
            if !req.exe.is_file() {
                anyhow::bail!("executable not found: {}", req.exe.display());
            }
            if !req.cwd.is_dir() {
                anyhow::bail!("working directory not found: {}", req.cwd.display());
            }

            // lpCommandLine must be a writable buffer (CreateProcessW may scribble).
            let mut cmdline = build_command_line(&req.exe, &req.args);
            let cwd_wide = path_to_wide_nul(&req.cwd);

            let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
            si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            si.dwFlags = STARTF_USESHOWWINDOW;
            si.wShowWindow = SW_HIDE as u16;

            let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

            // CREATE_BREAKAWAY_FROM_JOB so the game server child outlives the
            // GUI process even when the GUI itself is in a Windows Job
            // Object (e.g. launched from VS Code's integrated terminal). On
            // Win 8+ this flag is a no-op when the parent is not in a job,
            // and silently ignored when the job permits breakaway. The only
            // case it would fail (ERROR_ACCESS_DENIED, 0x5) is a job that
            // explicitly forbids breakaway — extremely rare for end users.
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            let creation_flags = CREATE_NEW_CONSOLE | CREATE_BREAKAWAY_FROM_JOB;

            let ok = unsafe {
                CreateProcessW(
                    std::ptr::null(),
                    cmdline.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    creation_flags,
                    std::ptr::null(),
                    cwd_wide.as_ptr(),
                    &si,
                    &mut pi,
                )
            };
            if ok == 0 {
                let err = std::io::Error::last_os_error();
                anyhow::bail!("CreateProcessW({}) failed: {err}", req.exe.display());
            }

            Ok(Self {
                pid: pi.dwProcessId,
                process: pi.hProcess,
                thread: Some(pi.hThread),
            })
        }

        /// Open an existing process by PID. Used by re-attach after the GUI
        /// is relaunched while the game server is still running.
        pub fn open_existing(pid: u32) -> anyhow::Result<Self> {
            // PROCESS_TERMINATE for fallback TerminateProcess; SYNCHRONIZE
            // for WaitForSingleObject; PROCESS_QUERY_LIMITED_INFORMATION
            // for GetExitCodeProcess.
            const PROCESS_TERMINATE: u32 = 0x0001;
            const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
            const SYNCHRONIZE: u32 = 0x0010_0000;
            const ACCESS: u32 = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE;

            let h = unsafe { OpenProcess(ACCESS, 0, pid) };
            if h == 0 {
                let err = std::io::Error::last_os_error();
                anyhow::bail!("OpenProcess({pid}) failed: {err}");
            }
            Ok(Self {
                pid,
                process: h,
                thread: None,
            })
        }

        /// True when GetExitCodeProcess reports STILL_ACTIVE.
        pub fn is_alive(&self) -> bool {
            const STILL_ACTIVE: u32 = 259;
            let mut code: u32 = 0;
            let ok = unsafe { GetExitCodeProcess(self.process, &mut code) };
            ok != 0 && code == STILL_ACTIVE
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        /// Wait up to `timeout` for the process to exit. Returns the exit code
        /// when the process has exited, or `None` on timeout.
        pub fn wait_for_exit_with_timeout(&self, timeout: Duration) -> anyhow::Result<Option<u32>> {
            let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            let r = unsafe { WaitForSingleObject(self.process, ms) };
            if r == WAIT_OBJECT_0 {
                let mut code: u32 = 0;
                let ok = unsafe { GetExitCodeProcess(self.process, &mut code) };
                if ok == 0 {
                    let err = std::io::Error::last_os_error();
                    anyhow::bail!("GetExitCodeProcess failed: {err}");
                }
                Ok(Some(code))
            } else if r == WAIT_TIMEOUT {
                Ok(None)
            } else {
                let err = std::io::Error::last_os_error();
                anyhow::bail!("WaitForSingleObject failed: {err}")
            }
        }

        /// Force-kill the process. Last resort per spec §6.3.
        pub fn terminate(&self) -> anyhow::Result<()> {
            let r = unsafe { TerminateProcess(self.process, 1) };
            if r == 0 {
                let err = std::io::Error::last_os_error();
                anyhow::bail!("TerminateProcess failed: {err}")
            }
            Ok(())
        }
    }

    impl Drop for ServerProcess {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.process);
                if let Some(t) = self.thread {
                    CloseHandle(t);
                }
            }
        }
    }

    fn path_to_wide_nul(p: &std::path::Path) -> Vec<u16> {
        let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
        v.push(0);
        v
    }

    /// Build a Windows command line, quoting each argument per
    /// CommandLineToArgvW rules. The executable always gets quoted because
    /// Game server install paths commonly contain spaces.
    fn build_command_line(exe: &std::path::Path, args: &[String]) -> Vec<u16> {
        let mut s = String::new();
        s.push('"');
        s.push_str(&exe.to_string_lossy());
        s.push('"');
        for a in args {
            s.push(' ');
            push_quoted(&mut s, a);
        }
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        v
    }

    fn push_quoted(out: &mut String, arg: &str) {
        // Per CommandLineToArgvW: backslashes followed by `"` must be doubled.
        let needs_quoting =
            arg.is_empty() || arg.chars().any(|c| c == ' ' || c == '\t' || c == '"');
        if !needs_quoting {
            out.push_str(arg);
            return;
        }
        out.push('"');
        let mut backslashes = 0usize;
        for c in arg.chars() {
            if c == '\\' {
                backslashes += 1;
            } else if c == '"' {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            } else {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(c);
            }
        }
        for _ in 0..(backslashes * 2) {
            out.push('\\');
        }
        out.push('"');
    }
}

#[cfg(not(windows))]
mod imp {
    use super::SpawnRequest;
    use std::time::Duration;

    pub struct ServerProcess;

    impl ServerProcess {
        pub fn spawn(_req: &SpawnRequest) -> anyhow::Result<Self> {
            anyhow::bail!("server process spawning is Windows-only (spec §0)")
        }
        pub fn open_existing(_pid: u32) -> anyhow::Result<Self> {
            anyhow::bail!("open_existing is Windows-only")
        }
        pub fn is_alive(&self) -> bool {
            false
        }
        pub fn pid(&self) -> u32 {
            0
        }
        pub fn wait_for_exit_with_timeout(
            &self,
            _timeout: Duration,
        ) -> anyhow::Result<Option<u32>> {
            anyhow::bail!("not implemented on this platform")
        }
        pub fn terminate(&self) -> anyhow::Result<()> {
            anyhow::bail!("not implemented on this platform")
        }
    }
}

pub use imp::ServerProcess;

impl SpawnRequest {
    pub fn new(exe: impl Into<PathBuf>, args: Vec<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            args,
            cwd: cwd.into(),
        }
    }
}

/// Run a side-helper exe synchronously and return its exit status. Used to
/// invoke `ctrlc-helper.exe` from the manager. On Windows we set
/// `CREATE_NO_WINDOW` so we don't flash a console (the manager itself is a
/// windowed app and has no inherited console to share).
pub fn run_helper_blocking(exe: &Path, args: &[String]) -> anyhow::Result<i32> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let status = cmd.status()?;
    Ok(status.code().unwrap_or(-1))
}
