//! Standalone helper that sends a real `CTRL_C_EVENT` to another process's
//! console — see spec §6.3.
//!
//! Usage: `ctrlc-helper <server_pid>`
//!
//! Invariants:
//! - This process attaches to the target's console *temporarily*, ignores
//!   Ctrl+C for itself, then generates the event for the whole attached group.
//! - Isolating this into a separate exe means if the Ctrl+C also takes us down,
//!   the GUI process is unaffected (spec §6.3 step 2).

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let pid_str = match args.next() {
        Some(s) => s,
        None => {
            eprintln!("usage: ctrlc-helper <server_pid>");
            return ExitCode::from(2);
        }
    };
    let pid: u32 = match pid_str.parse() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("invalid pid {pid_str:?}: {e}");
            return ExitCode::from(2);
        }
    };

    match run(pid) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ctrlc-helper failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn run(pid: u32) -> anyhow::Result<()> {
    use anyhow::bail;
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler, CTRL_C_EVENT,
    };

    // SAFETY: each FFI call is documented as safe to invoke from any thread,
    // and we are a single-threaded helper. We must drop our own console (if
    // any) before attaching to the target's console.
    unsafe {
        // The return value is intentionally ignored: a freshly spawned
        // helper may have no console to free, which returns 0 with
        // ERROR_INVALID_PARAMETER. That's fine.
        let _ = FreeConsole();

        if AttachConsole(pid) == 0 {
            let err = std::io::Error::last_os_error();
            bail!("AttachConsole({pid}) failed: {err}");
        }

        // Ignore Ctrl+C for ourselves so we survive the event we're about to
        // generate. NULL handler + TRUE means "ignore".
        if SetConsoleCtrlHandler(None, 1) == 0 {
            let err = std::io::Error::last_os_error();
            // Best-effort detach before returning.
            let _ = FreeConsole();
            bail!("SetConsoleCtrlHandler failed: {err}");
        }

        // 0 = deliver to every process attached to this console (the target
        // server process group).
        if GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) == 0 {
            let err = std::io::Error::last_os_error();
            let _ = FreeConsole();
            bail!("GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) failed: {err}");
        }

        // Give the target a moment to receive the event before we detach.
        // 200ms is a guess; tune after the M0 spike.
        std::thread::sleep(std::time::Duration::from_millis(200));

        let _ = FreeConsole();
    }
    Ok(())
}

#[cfg(not(windows))]
fn run(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!("ctrlc-helper is Windows-only (spec §0, §6.3)")
}
