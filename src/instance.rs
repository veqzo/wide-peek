//! Named mutexes, so only one launcher, one tray and one stretch session run.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::HSTRING;

pub const LAUNCH: &str = r"Local\widepeek-launch";
pub const TRAY: &str = r"Local\widepeek-tray";
pub const SESSION: &str = r"Local\widepeek-session";

/// An owned named mutex. Drop it on the thread that acquired it.
#[derive(Debug)]
pub struct InstanceLock(HANDLE);

// SAFETY: a mutex handle can move between threads. Drop releases it; if a
// thread that is not the owner drops it, the release fails and the mutex is
// abandoned when the owner thread ends, which the next owner accepts.
unsafe impl Send for InstanceLock {}

/// Takes the mutex without waiting. `None` means another process owns it.
pub fn try_acquire(name: &str) -> anyhow::Result<Option<InstanceLock>> {
    // SAFETY: the name lives until the call returns.
    let handle = unsafe { CreateMutexW(None, false, &HSTRING::from(name)) }?;
    // SAFETY: `handle` is a mutex handle that we own.
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    // A dead owner leaves the mutex abandoned. Its state file covers the rest.
    if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
        Ok(Some(InstanceLock(handle)))
    } else {
        // SAFETY: we own the handle and do not use it again.
        let _ = unsafe { CloseHandle(handle) };
        Ok(None)
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
