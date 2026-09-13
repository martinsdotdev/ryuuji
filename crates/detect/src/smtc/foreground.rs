//! The Windows read behind [`Front`]: which process owns the foreground
//! window. Every failure reads as `Unknown`, which the accrual rule
//! credits, so a read that cannot be made leaves recording as it was.

use windows::Win32::handleapi::CloseHandle;
use windows::Win32::processthreadsapi::OpenProcess;
use windows::Win32::psapi::GetProcessImageFileNameW;
use windows::Win32::winnt::PROCESS_QUERY_LIMITED_INFORMATION;
use windows::Win32::winuser::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::PWSTR;

use crate::session::Front;

/// Room for a device path (`\Device\HarddiskVolume3\...\mpv.exe`); a longer
/// one fails the read and is `Unknown`.
const PATH_CAPACITY: usize = 1024;

pub(crate) fn front() -> Front {
    let mut pid = 0u32;
    let mut path = [0u16; PATH_CAPACITY];
    // SAFETY: plain Win32 calls with valid arguments. `pid` and `path` are
    // live locals for the duration of the calls that write into them, the
    // capacity passed is `path`'s real length, and the process handle is
    // closed after every successful open before it goes out of scope.
    let (own, written) = unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return Front::Unknown;
        }
        GetWindowThreadProcessId(window, Some(&mut pid));
        if pid == 0 {
            return Front::Unknown;
        }
        let own = pid == std::process::id();
        let mut written = 0;
        if !own {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION as u32, false, pid);
            if process.0.is_null() {
                return Front::Unknown;
            }
            written =
                GetProcessImageFileNameW(process, PWSTR(path.as_mut_ptr()), PATH_CAPACITY as u32);
            let _ = CloseHandle(process);
        }
        (own, written)
    };
    let exe = (written > 0).then(|| {
        let path = String::from_utf16_lossy(&path[..written as usize]);
        path.rsplit('\\').next().unwrap_or(&path).to_owned()
    });
    Front::from_reading(own, exe)
}
