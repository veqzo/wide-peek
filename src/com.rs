//! COM initialization guard for the current thread.

use anyhow::Context;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

/// Drop undoes the init if this guard did it. Declare the guard before the
/// COM objects, so that it drops after them.
#[derive(Debug)]
pub struct Com(bool);

impl Com {
    pub fn init() -> anyhow::Result<Self> {
        // SAFETY: plain COM init for this thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            // COM already runs in another mode on this thread. That works too.
            return Ok(Self(false));
        }
        hr.ok().context("cannot initialize COM")?;
        Ok(Self(true))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx of `init`.
            unsafe { CoUninitialize() };
        }
    }
}
