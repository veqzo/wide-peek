//! Named mutexes, so only one launcher, one tray and one stretch session run.

use std::time::Duration;

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
    acquire_within(name, Duration::ZERO)
}

/// Takes the mutex and waits at most `timeout` for the owner to release it.
/// `None` means another owner still holds it after the timeout.
pub fn acquire_within(name: &str, timeout: Duration) -> anyhow::Result<Option<InstanceLock>> {
    // INFINITE is u32::MAX, so the wait always ends.
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: the name lives until the call returns.
    let handle = unsafe { CreateMutexW(None, false, &HSTRING::from(name)) }?;
    // SAFETY: `handle` is a mutex handle that we own.
    let wait = unsafe { WaitForSingleObject(handle, millis) };
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn test_name(case: &str) -> String {
        format!(r"Local\widepeek-test-{}-{case}", std::process::id())
    }

    #[test]
    fn another_owner_blocks_the_lock() {
        let name = test_name("blocks");
        let lock = try_acquire(&name).unwrap();
        assert!(lock.is_some());
        let other = name.clone();
        let taken = std::thread::spawn(move || try_acquire(&other).unwrap().is_some())
            .join()
            .unwrap();
        assert!(!taken);
        drop(lock);
        assert!(try_acquire(&name).unwrap().is_some());
    }

    #[test]
    fn waits_until_the_owner_releases() {
        let name = test_name("waits");
        let (held, is_held) = mpsc::channel();
        let owner_name = name.clone();
        let owner = std::thread::spawn(move || {
            let lock = try_acquire(&owner_name).unwrap();
            held.send(lock.is_some()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            drop(lock);
        });
        assert!(is_held.recv().unwrap());
        assert!(try_acquire(&name).unwrap().is_none());
        assert!(
            acquire_within(&name, Duration::from_secs(10))
                .unwrap()
                .is_some()
        );
        owner.join().unwrap();
    }
}
